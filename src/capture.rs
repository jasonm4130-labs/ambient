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
    kAudioDevicePropertyStreamConfiguration, kAudioObjectPropertyScopeInput,
    AudioObjectGetPropertyDataSize,
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceMainSubDeviceKey,
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceSubDeviceListKey,
    kAudioDevicePropertyDeviceUID, kAudioHardwarePropertyDefaultInputDevice,
    kAudioObjectSystemObject, kAudioSubDeviceDriftCompensationKey, kAudioSubDeviceUIDKey,
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

    /// Interleave up to `n` source buffers frame by frame. Stack-only, so it
    /// is safe to call from the realtime thread.
    #[inline]
    fn push_interleaved(&self, srcs: &[&[f32]], frames: usize) {
        let cap = self.buf.len();
        let mut w = self.write.load(Ordering::Relaxed);
        for f in 0..frames {
            for src in srcs {
                let v = if f < src.len() { src[f] } else { 0.0 };
                unsafe { *self.buf[w % cap].get() = v };
                w += 1;
            }
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

/// UID of the current default input device, and its object id.
unsafe fn default_input() -> Result<(AudioObjectID, Retained<NSString>)> {
    let mut a = addr(kAudioHardwarePropertyDefaultInputDevice);
    let mut size = std::mem::size_of::<AudioObjectID>() as u32;
    let mut dev: AudioObjectID = 0;
    let st = AudioObjectGetPropertyData(
        kAudioObjectSystemObject as AudioObjectID,
        NonNull::from(&mut a),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::new(&mut dev as *mut _ as *mut c_void).unwrap(),
    );
    if st != 0 || dev == 0 {
        bail!("no default input device: OSStatus {st}");
    }

    let mut a = addr(kAudioDevicePropertyDeviceUID);
    let mut size = std::mem::size_of::<*const c_void>() as u32;
    let mut raw: *const NSString = std::ptr::null();
    let st = AudioObjectGetPropertyData(
        dev,
        NonNull::from(&mut a),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::new(&mut raw as *mut *const NSString as *mut c_void).unwrap(),
    );
    if st != 0 || raw.is_null() {
        bail!("kAudioDevicePropertyDeviceUID failed: OSStatus {st}");
    }
    Ok((dev, Retained::from_raw(raw as *mut NSString).unwrap()))
}

/// Number of input channels a device exposes.
unsafe fn input_channel_count(dev: AudioObjectID) -> Result<u32> {
    let mut a = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyStreamConfiguration,
        mScope: kAudioObjectPropertyScopeInput,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut size: u32 = 0;
    let st = AudioObjectGetPropertyDataSize(
        dev,
        NonNull::from(&mut a),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
    );
    if st != 0 {
        bail!("stream config size failed: OSStatus {st}");
    }
    let mut raw = vec![0u8; size as usize];
    let st = AudioObjectGetPropertyData(
        dev,
        NonNull::from(&mut a),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::new(raw.as_mut_ptr() as *mut c_void).unwrap(),
    );
    if st != 0 {
        bail!("stream config failed: OSStatus {st}");
    }
    let list = &*(raw.as_ptr() as *const objc2_core_audio_types::AudioBufferList);
    let n = list.mNumberBuffers as usize;
    let bufs = std::slice::from_raw_parts(list.mBuffers.as_ptr(), n);
    Ok(bufs.iter().map(|b| b.mNumberChannels).sum::<u32>().max(1))
}

pub struct ProcessTap {
    tap: AudioObjectID,
    device: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    pub ring: Arc<Ring>,
    pub sample_rate: f64,
    /// Total channels the IOProc delivers, mic buffers first then the tap.
    pub channels: u32,
    /// How many of those leading channels are the microphone.
    pub mic_channels: u32,
}

impl ProcessTap {
    /// Tap the given bundle IDs, mixed down to mono. An empty list taps
    /// everything the machine is playing.
    pub fn start(bundle_ids: &[String], ring_seconds: usize, include_mic: bool) -> Result<Self> {
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

            // Putting the microphone in the same aggregate as the tap is what
            // keeps the two tracks sample-aligned: one IOProc, one clock, with
            // Core Audio doing drift compensation between them. Two separate
            // streams would slowly slide apart over a long meeting.
            let (mic_dev, mic_uid) = if include_mic {
                let (d, u) = default_input()?;
                (Some(d), Some(u))
            } else {
                (None, None)
            };

            let sub_list = match &mic_uid {
                Some(uid) => {
                    let sub = NSDictionary::from_slices(
                        &[
                            &*NSString::from_str(kAudioSubDeviceUIDKey.to_str().unwrap()),
                            &*NSString::from_str(
                                kAudioSubDeviceDriftCompensationKey.to_str().unwrap(),
                            ),
                        ],
                        &[
                            &**uid as &objc2::runtime::AnyObject,
                            &*one as &objc2::runtime::AnyObject,
                        ],
                    );
                    NSArray::from_slice(&[&*sub as &objc2::runtime::AnyObject])
                }
                None => NSArray::from_slice(&[]),
            };

            let mut keys: Vec<&NSString> = Vec::new();
            let mut vals: Vec<&objc2::runtime::AnyObject> = Vec::new();
            let k_name = NSString::from_str(kAudioAggregateDeviceNameKey.to_str().unwrap());
            let k_uid = NSString::from_str(kAudioAggregateDeviceUIDKey.to_str().unwrap());
            let k_priv = NSString::from_str(kAudioAggregateDeviceIsPrivateKey.to_str().unwrap());
            let k_auto =
                NSString::from_str(kAudioAggregateDeviceTapAutoStartKey.to_str().unwrap());
            let k_taps = NSString::from_str(kAudioAggregateDeviceTapListKey.to_str().unwrap());
            let k_subs = NSString::from_str(kAudioAggregateDeviceSubDeviceListKey.to_str().unwrap());
            let k_main = NSString::from_str(kAudioAggregateDeviceMainSubDeviceKey.to_str().unwrap());

            keys.push(&k_name); vals.push(&*name);
            keys.push(&k_uid);  vals.push(&*agg_uid);
            keys.push(&k_priv); vals.push(&*one);
            keys.push(&k_auto); vals.push(&*one);
            keys.push(&k_taps); vals.push(&*tap_list);
            keys.push(&k_subs); vals.push(&*sub_list);
            if let Some(uid) = &mic_uid {
                // Clock the aggregate off the real hardware, not the tap.
                keys.push(&k_main);
                vals.push(&**uid);
            }

            let dict = NSDictionary::from_slices(&keys, &vals);

            let mut device: AudioObjectID = 0;
            let cf_dict: &objc2_core_foundation::CFDictionary =
                &*(Retained::as_ptr(&dict) as *const objc2_core_foundation::CFDictionary);
            let st = AudioHardwareCreateAggregateDevice(cf_dict, NonNull::from(&mut device));
            if st != 0 {
                AudioHardwareDestroyProcessTap(tap);
                bail!("AudioHardwareCreateAggregateDevice failed: OSStatus {st}");
            }

            let tap_channels = asbd.mChannelsPerFrame.max(1);
            let mic_channels = match mic_dev {
                Some(d) => input_channel_count(d).unwrap_or(1),
                None => 0,
            };
            let channels = mic_channels + tap_channels;
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
                    let n = (list.mNumberBuffers as usize).min(8);
                    let buffers = std::slice::from_raw_parts(list.mBuffers.as_ptr(), n);

                    // Fixed-capacity, stack-allocated: no heap work on the
                    // realtime thread.
                    let mut srcs: [&[f32]; 8] = [&[]; 8];
                    let mut used = 0usize;
                    let mut frames = 0usize;
                    for b in buffers {
                        if b.mData.is_null() || b.mNumberChannels == 0 {
                            continue;
                        }
                        let count = b.mDataByteSize as usize / std::mem::size_of::<f32>();
                        let data = std::slice::from_raw_parts(b.mData as *const f32, count);
                        let f = count / b.mNumberChannels as usize;
                        frames = frames.max(f);
                        srcs[used] = data;
                        used += 1;
                        if used == 8 {
                            break;
                        }
                    }
                    if used > 0 && frames > 0 {
                        ring_cb.push_interleaved(&srcs[..used], frames);
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

            Ok(Self { tap, device, proc_id, ring, sample_rate, channels, mic_channels })
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
