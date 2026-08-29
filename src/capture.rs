//! System-audio capture via Core Audio process taps.
//!
//! This is the mechanism that lets us record the far side of a Teams call
//! without anything joining the meeting: macOS hands us the audio a named
//! process is rendering. It needs the "System Audio Recording" permission,
//! which is strictly less than the Screen Recording that ScreenCaptureKit
//! would demand — the difference that matters on a managed Mac.
//!
//! The IO block runs on a realtime thread. Nothing in it allocates, locks or
//! logs; samples go into a preallocated ring and are drained elsewhere.

use anyhow::{bail, Result};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::AllocAnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioSubTapUIDKey, kAudioTapPropertyFormat,
    kAudioTapPropertyUID, AudioDeviceCreateIOProcIDWithBlock, AudioDeviceDestroyIOProcID,
    AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop, AudioHardwareCreateAggregateDevice,
    AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
    AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData, AudioObjectID,
    AudioObjectPropertyAddress, CATapDescription,
};
use objc2_core_audio_types::AudioStreamBasicDescription;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

/// Lock-free single-producer ring the realtime callback writes into.
pub struct Ring {
    buf: Vec<std::cell::UnsafeCell<f32>>,
    write: AtomicUsize,
}
unsafe impl Sync for Ring {}
unsafe impl Send for Ring {}

impl Ring {
    fn new(capacity: usize) -> Self {
        Self {
            buf: (0..capacity).map(|_| std::cell::UnsafeCell::new(0.0)).collect(),
            write: AtomicUsize::new(0),
        }
    }

    /// Called from the realtime thread. No allocation, no locking.
    #[inline]
    fn push(&self, samples: &[f32]) {
        let cap = self.buf.len();
        let mut w = self.write.load(Ordering::Relaxed);
        for &s in samples {
            unsafe { *self.buf[w % cap].get() = s };
            w += 1;
        }
        self.write.store(w, Ordering::Release);
    }

    pub fn written(&self) -> usize {
        self.write.load(Ordering::Acquire)
    }

    /// Copy out everything between `from` and the current write position.
    pub fn drain_from(&self, from: usize) -> (Vec<f32>, usize) {
        let w = self.written();
        let cap = self.buf.len();
        let start = from.max(w.saturating_sub(cap));
        let mut out = Vec::with_capacity(w - start);
        for i in start..w {
            out.push(unsafe { *self.buf[i % cap].get() });
        }
        (out, w)
    }
}

fn addr(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

unsafe fn tap_uid(tap: AudioObjectID) -> Result<Retained<NSString>> {
    let mut a = addr(kAudioTapPropertyUID);
    let mut size = std::mem::size_of::<*const c_void>() as u32;
    let mut raw: *const NSString = std::ptr::null();
    let st = AudioObjectGetPropertyData(
        tap,
        NonNull::from(&mut a),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::new(&mut raw as *mut *const NSString as *mut c_void).unwrap(),
    );
    if st != 0 || raw.is_null() {
        bail!("kAudioTapPropertyUID failed: OSStatus {st}");
    }
    Ok(Retained::from_raw(raw as *mut NSString).unwrap())
}

unsafe fn tap_format(tap: AudioObjectID) -> Result<AudioStreamBasicDescription> {
    let mut a = addr(kAudioTapPropertyFormat);
    let mut size = std::mem::size_of::<AudioStreamBasicDescription>() as u32;
    let mut asbd: AudioStreamBasicDescription = std::mem::zeroed();
    let st = AudioObjectGetPropertyData(
        tap,
        NonNull::from(&mut a),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::new(&mut asbd as *mut _ as *mut c_void).unwrap(),
    );
    if st != 0 {
        bail!("kAudioTapPropertyFormat failed: OSStatus {st}");
    }
    Ok(asbd)
}

pub struct ProcessTap {
    tap: AudioObjectID,
    device: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    pub ring: Arc<Ring>,
    pub sample_rate: f64,
    pub channels: u32,
}

impl ProcessTap {
    /// Tap the given bundle IDs, mixed down to mono. An empty list taps
    /// everything the machine is playing.
    pub fn start(bundle_ids: &[String], ring_seconds: usize) -> Result<Self> {
        unsafe {
            let ids: Vec<Retained<NSString>> =
                bundle_ids.iter().map(|s| NSString::from_str(s)).collect();
            let ns_ids = NSArray::from_retained_slice(&ids);

            // Use the documented initialisers rather than bare init plus
            // setters: a description built by hand can end up selecting no
            // processes at all, which surfaces as a working tap full of
            // silence rather than as an error.
            let desc = if bundle_ids.is_empty() {
                let none = NSArray::from_retained_slice(&[] as &[Retained<NSNumber>]);
                CATapDescription::initMonoGlobalTapButExcludeProcesses(
                    CATapDescription::alloc(),
                    &none,
                )
            } else {
                let d = CATapDescription::init(CATapDescription::alloc());
                d.setBundleIDs(&ns_ids);
                d.setMono(true);
                d.setMixdown(true);
                d
            };
            desc.setName(&NSString::from_str("ambient"));
            desc.setPrivate(true); // don't show up as a device to other apps
            desc.setMuteBehavior(objc2_core_audio::CATapMuteBehavior(0)); // unmuted

            let mut tap: AudioObjectID = 0;
            let st = AudioHardwareCreateProcessTap(Some(&desc), &mut tap);
            if st != 0 {
                bail!(
                    "AudioHardwareCreateProcessTap failed: OSStatus {st}\n\
                     If this is -4 or a permission error, grant System Audio Recording \
                     in System Settings > Privacy & Security."
                );
            }

            let uid = tap_uid(tap)?;
            let asbd = tap_format(tap)?;

            // Aggregate device wrapping the tap. NSDictionary is toll-free
            // bridged to CFDictionary, which is what Core Audio wants.
            let sub_tap = NSDictionary::from_slices(
                &[&*NSString::from_str(kAudioSubTapUIDKey.to_str().unwrap())],
                &[&*uid as &objc2::runtime::AnyObject],
            );
            let tap_list = NSArray::from_slice(&[&*sub_tap as &objc2::runtime::AnyObject]);
            let agg_uid = NSString::from_str(&format!("uk.ambient.tap.{}", std::process::id()));
            let name = NSString::from_str("ambient aggregate");
            let one = NSNumber::new_i32(1);

            let dict = NSDictionary::from_slices(
                &[
                    &*NSString::from_str(kAudioAggregateDeviceNameKey.to_str().unwrap()),
                    &*NSString::from_str(kAudioAggregateDeviceUIDKey.to_str().unwrap()),
                    &*NSString::from_str(kAudioAggregateDeviceIsPrivateKey.to_str().unwrap()),
                    &*NSString::from_str(kAudioAggregateDeviceTapAutoStartKey.to_str().unwrap()),
                    &*NSString::from_str(kAudioAggregateDeviceTapListKey.to_str().unwrap()),
                ],
                &[
                    &*name as &objc2::runtime::AnyObject,
                    &*agg_uid as &objc2::runtime::AnyObject,
                    &*one as &objc2::runtime::AnyObject,
                    &*one as &objc2::runtime::AnyObject,
                    &*tap_list as &objc2::runtime::AnyObject,
                ],
            );

            let mut device: AudioObjectID = 0;
            let cf_dict: &objc2_core_foundation::CFDictionary =
                &*(Retained::as_ptr(&dict) as *const objc2_core_foundation::CFDictionary);
            let st = AudioHardwareCreateAggregateDevice(cf_dict, NonNull::from(&mut device));
            if st != 0 {
                AudioHardwareDestroyProcessTap(tap);
                bail!("AudioHardwareCreateAggregateDevice failed: OSStatus {st}");
            }

            let channels = asbd.mChannelsPerFrame.max(1);
            let sample_rate = asbd.mSampleRate;
            let ring = Arc::new(Ring::new(
                (sample_rate as usize) * channels as usize * ring_seconds,
            ));

            let ring_cb = ring.clone();
            let block = block2::RcBlock::new(
                move |_now: NonNull<objc2_core_audio_types::AudioTimeStamp>,
                      input: NonNull<objc2_core_audio_types::AudioBufferList>,
                      _in_time: NonNull<objc2_core_audio_types::AudioTimeStamp>,
                      _out: NonNull<objc2_core_audio_types::AudioBufferList>,
                      _out_time: NonNull<objc2_core_audio_types::AudioTimeStamp>| {
                    let list = input.as_ref();
                    let n = list.mNumberBuffers as usize;
                    let buffers = std::slice::from_raw_parts(list.mBuffers.as_ptr(), n);
                    for b in buffers {
                        if b.mData.is_null() {
                            continue;
                        }
                        let count = b.mDataByteSize as usize / std::mem::size_of::<f32>();
                        let data = std::slice::from_raw_parts(b.mData as *const f32, count);
                        ring_cb.push(data);
                    }
                },
            );

            let mut proc_id: AudioDeviceIOProcID = None;
            let st = AudioDeviceCreateIOProcIDWithBlock(
                NonNull::from(&mut proc_id),
                device,
                None,
                &*block as *const _ as *mut _,
            );
            if st != 0 {
                AudioHardwareDestroyAggregateDevice(device);
                AudioHardwareDestroyProcessTap(tap);
                bail!("AudioDeviceCreateIOProcIDWithBlock failed: OSStatus {st}");
            }
            std::mem::forget(block); // the IOProc holds it for its lifetime

            let st = AudioDeviceStart(device, proc_id);
            if st != 0 {
                bail!("AudioDeviceStart failed: OSStatus {st}");
            }

            Ok(Self { tap, device, proc_id, ring, sample_rate, channels })
        }
    }
}

impl Drop for ProcessTap {
    fn drop(&mut self) {
        unsafe {
            AudioDeviceStop(self.device, self.proc_id);
            AudioDeviceDestroyIOProcID(self.device, self.proc_id);
            AudioHardwareDestroyAggregateDevice(self.device);
            AudioHardwareDestroyProcessTap(self.tap);
        }
    }
}
