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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::AllocAnyThread;
use objc2_core_audio::{
    kAudioDevicePropertyStreamConfiguration, kAudioObjectPropertyScopeInput,
    AudioObjectGetPropertyDataSize,
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceMainSubDeviceKey,
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceSubDeviceListKey,
    kAudioDevicePropertyDeviceUID, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDefaultOutputDevice,
    kAudioObjectSystemObject, kAudioSubDeviceDriftCompensationKey, kAudioSubDeviceUIDKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey, kAudioObjectPropertyElementMain,
    kAudioHardwarePropertyDevices, kAudioObjectPropertyName,
    kAudioObjectPropertyScopeGlobal, kAudioSubTapUIDKey, kAudioTapPropertyFormat,
    kAudioDevicePropertyNominalSampleRate, kAudioTapPropertyUID,
    AudioDeviceCreateIOProcIDWithBlock, AudioDeviceDestroyIOProcID,
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

/// UID of a default device, and its object id.
unsafe fn default_device(selector: u32) -> Result<(AudioObjectID, Retained<NSString>)> {
    let mut a = addr(selector);
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
        bail!("no default device for selector {selector}: OSStatus {st}");
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

unsafe fn default_input() -> Result<(AudioObjectID, Retained<NSString>)> {
    default_device(kAudioHardwarePropertyDefaultInputDevice)
}

/// A device's human-readable name — what the settings window shows, and what
/// `Config::input_device` stores. The UID is stabler but unreadable, and a
/// name is what somebody picking a microphone recognises.
unsafe fn device_name(dev: AudioObjectID) -> Option<String> {
    let mut a = addr(kAudioObjectPropertyName);
    let mut size = std::mem::size_of::<*const c_void>() as u32;
    let mut raw: *const NSString = std::ptr::null();
    let st = AudioObjectGetPropertyData(
        dev,
        NonNull::from(&mut a),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::new(&mut raw as *mut *const NSString as *mut c_void)?,
    );
    if st != 0 || raw.is_null() {
        return None;
    }
    Some(Retained::from_raw(raw as *mut NSString)?.to_string())
}

/// Every device that can record, by name. The settings window lists these; a
/// device with no input channels is not a microphone and is left out.
pub fn input_devices() -> Vec<(AudioObjectID, String)> {
    unsafe {
        let mut a = addr(kAudioHardwarePropertyDevices);
        let mut size: u32 = 0;
        if AudioObjectGetPropertyDataSize(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&mut a),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        ) != 0
        {
            return Vec::new();
        }
        let n = size as usize / std::mem::size_of::<AudioObjectID>();
        let mut devices = vec![0 as AudioObjectID; n];
        if AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&mut a),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(devices.as_mut_ptr() as *mut c_void).unwrap(),
        ) != 0
        {
            return Vec::new();
        }
        devices
            .into_iter()
            .filter(|&d| input_channels_raw(d).unwrap_or(0) > 0)
            .filter_map(|d| device_name(d).map(|n| (d, n)))
            .collect()
    }
}

/// The device to record the room with. A name that no longer matches anything
/// falls back to the default — loudly, because this project's recurring
/// failure is a capture that silently records nothing and reports success.
unsafe fn input_device(preferred: Option<&str>) -> Result<AudioObjectID> {
    let Some(name) = preferred else {
        return Ok(default_input()?.0);
    };
    if let Some((dev, _)) = input_devices().into_iter().find(|(_, n)| n == name) {
        return Ok(dev);
    }
    let fallback = default_input()?.0;
    eprintln!(
        "  WARNING: no input device named {name:?} — recording with the system default \
         instead. Available: {}",
        input_devices()
            .into_iter()
            .map(|(_, n)| n)
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(fallback)
}

/// The device the machine is playing through. A process tap observes audio on
/// its way to an output device, so that device has to be in the aggregate:
/// without it the tap is attached to nothing and delivers silence rather than
/// an error.
unsafe fn default_output() -> Result<(AudioObjectID, Retained<NSString>)> {
    default_device(kAudioHardwarePropertyDefaultOutputDevice)
}

/// Number of input channels a device exposes.
/// Input channels as the device actually reports them, zero included.
/// `input_channel_count` clamps to 1 so the capture path always has a usable
/// count; that clamp makes it useless for deciding whether a device can record
/// at all, which is what `input_devices` needs.
unsafe fn input_channels_raw(dev: AudioObjectID) -> Result<u32> {
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
    Ok(bufs.iter().map(|b| b.mNumberChannels).sum::<u32>())
}

unsafe fn input_channel_count(dev: AudioObjectID) -> Result<u32> {
    Ok(input_channels_raw(dev)?.max(1))
}

static DUMPED: AtomicBool = AtomicBool::new(false);

/// Explain a silent call track, or stay quiet if there is nothing to explain.
///
/// A denied system-audio tap and an idle machine produce byte-identical output:
/// zeros. The only thing that separates them is whether anything was actually
/// rendering audio at the time, which is why that gets sampled during capture.
/// Getting this wrong costs hours — the same silence also looks exactly like an
/// MDM policy block on a managed Mac.
pub fn silent_tap_advice(
    call_peak: f32,
    max_rendering: usize,
    bundles: &[String],
) -> Option<String> {
    if call_peak >= 1e-4 {
        return None;
    }
    // A filtered tap that caught nothing is the filter working, not a denial.
    // Something else playing says nothing about whether *these* apps were.
    if !bundles.is_empty() {
        return Some(format!(
            "The call track is silent. The tap was limited to {}, so this means those \
             app(s) played no audio during the recording — {} other process(es) were \
             playing, and were excluded as asked.",
            bundles.join(", "),
            max_rendering
        ));
    }
    if max_rendering == 0 {
        return Some(
            "The call track is silent, but nothing was rendering audio during the \
             recording — so there was simply nothing to capture."
                .into(),
        );
    }
    // Parent is launchd (pid 1) when LaunchServices started us, a shell
    // otherwise. This is the difference between TCC attributing the request to
    // the app and attributing it to the terminal, and it is by far the more
    // common cause of a silent tap.
    let via_launch_services = std::os::unix::process::parent_id() == 1;
    if !via_launch_services {
        return Some(format!(
            "The call track is silent while {max_rendering} process(es) were playing audio, \
             and this process was started from a shell rather than through LaunchServices.\n\
             \n\
             That is almost certainly the cause. TCC attributes a capture request to the \
             *responsible process* — your terminal — which holds no audio-capture grant and \
             is never prompted, so the tap is created successfully and handed nothing but \
             zeros. Relaunch as a bundle:\n\
             \n\
               open -a \"$PWD/build/Ambient.app\" --args <verb> ...\n\
             \n\
             If it is still silent that way, the grant itself is suspect — see the cdhash \
             check in the README."
        ));
    }
    Some(format!(
        "The call track is silent while {max_rendering} process(es) were playing audio.\n\
         \n\
         That is what a DENIED system-audio tap looks like: Core Audio returns success, \
         delivers correctly-shaped buffers, and fills them with zeros. It never reports \
         an error and never prompts.\n\
         \n\
         Most likely the TCC grant is bound to an older build. An ad-hoc signature's \
         designated requirement is the binary's own cdhash, so every rebuild orphans the \
         grant while leaving the row in place, still reading 'allowed'. Check:\n\
         \n\
           codesign -dvvv build/Ambient.app 2>&1 | grep CDHash\n\
           sqlite3 ~/Library/Application\\ Support/com.apple.TCC/TCC.db \\\n\
             \"select service,hex(csreq) from access where client='uk.ambient.cli'\"\n\
         \n\
         The last 20 bytes of csreq are the cdhash TCC expects. If they differ, either \
         sign with a stable identity, or re-grant once with:\n\
         \n\
           tccutil reset AudioCapture uk.ambient.cli\n\
         \n\
         On a managed Mac a PPPC profile denying kTCCServiceAudioCapture looks identical, \
         so rule the cdhash out before blaming IT."
    ))
}


pub struct ProcessTap {
    tap: AudioObjectID,
    agg: AudioObjectID,
    agg_proc: AudioDeviceIOProcID,
    mic_dev: Option<AudioObjectID>,
    mic_proc: AudioDeviceIOProcID,
    /// System audio. Stalls whenever nothing is playing — see `start`.
    pub call_ring: Arc<Ring>,
    /// The microphone, on its own device and its own clock, so that nothing
    /// the tap does can stop it.
    pub mic_ring: Option<Arc<Ring>>,
    pub call_rate: f64,
    pub mic_rate: f64,
    pub call_channels: u32,
    pub mic_channels: u32,
    started: std::time::Instant,
}

/// Per-consumer position in both rings. Separate from `ProcessTap` because the
/// capture is shared and the reading is not.
#[derive(Default)]
pub struct Drain {
    mic_cursor: usize,
    call_cursor: usize,
    mic_out: u64,
    call_out: u64,
    /// Frames that actually arrived from a device, as opposed to the silence
    /// `drain` inserts to keep the two tracks level. Without this split a tap
    /// that stalled for the whole recording reports the same duration as one
    /// that worked, and reads as healthy.
    mic_real: u64,
    call_real: u64,
}

/// Below this, a shortfall is just the ring lagging the clock by a tick rather
/// than a genuine gap, and padding it would slowly stretch the track.
const PAD_THRESHOLD_S: f64 = 0.05;

impl ProcessTap {
    /// The microphone alone, with no tap and no aggregate — the isolation case
    /// for diagnosing whether the input IOProc blocks on its own.
    pub fn start_mic_only() -> Result<Self> {
        unsafe {
            let dev = default_input()?.0;
            let ch = input_channel_count(dev).unwrap_or(1);
            let rate = nominal_sample_rate(dev).unwrap_or(48_000.0);
            eprintln!("[mic-only] device {dev}, {ch} ch, {rate} Hz — starting IOProc");
            let ring = Arc::new(Ring::new((rate as usize) * ch as usize * 30));
            let proc = start_ioproc(dev, ring.clone())?;
            eprintln!("[mic-only] IOProc running");
            Ok(Self {
                tap: 0,
                agg: 0,
                agg_proc: None,
                mic_dev: Some(dev),
                mic_proc: proc,
                call_ring: Arc::new(Ring::new(16)),
                mic_ring: Some(ring),
                call_rate: rate,
                mic_rate: rate,
                call_channels: 0,
                mic_channels: ch,
                started: std::time::Instant::now(),
            })
        }
    }

    /// Tap the given bundle IDs, mixed down to mono. An empty list taps
    /// everything the machine is playing.
    pub fn start(
        bundle_ids: &[String],
        ring_seconds: usize,
        include_mic: bool,
        mic_name: Option<&str>,
    ) -> Result<Self> {
        unsafe {
            // The microphone goes first, before the tap exists. Starting an
            // IOProc on the input device while the tap's aggregate is already
            // running blocks indefinitely — measured: the mic alone starts
            // instantly, the same call after the aggregate never returns.
            let (mic_ring, mic_proc, mic_rate, mic_channels, mic_dev) = if include_mic {
                let d = input_device(mic_name)?;
                let ch = input_channel_count(d).unwrap_or(1);
                let rate = nominal_sample_rate(d).unwrap_or(48_000.0);
                let ring = Arc::new(Ring::new((rate as usize) * ch as usize * ring_seconds));
                let proc = start_ioproc(d, ring.clone())?;
                (Some(ring), proc, rate, ch, Some(d))
            } else {
                (None, None, 48_000.0, 0, None)
            };
            // From here the mic is live, so any early return must stop it.
            let stop_mic = |dev: Option<AudioObjectID>, proc: AudioDeviceIOProcID| {
                if let Some(d) = dev {
                    AudioDeviceStop(d, proc);
                    AudioDeviceDestroyIOProcID(d, proc);
                }
            };

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
                stop_mic(mic_dev, mic_proc);
                bail!(
                    "AudioHardwareCreateProcessTap failed: OSStatus {st}\n\
                     If this is -4 or a permission error, grant System Audio Recording \
                     in System Settings > Privacy & Security."
                );
            }

            let uid = tap_uid(tap)?;
            let asbd = tap_format(tap)?;

            if std::env::var_os("AMBIENT_DEBUG_TAP").is_some() {
                // Read the description back: what we asked for and what the
                // system actually built are different questions.
                eprintln!(
                    "[tap] id={tap} uid={uid} rate={} ch={} fmt_flags={:#x}",
                    asbd.mSampleRate, asbd.mChannelsPerFrame, asbd.mFormatFlags
                );
                eprintln!(
                    "[tap] asked: mono={} mixdown={} private={} exclude_empty={}",
                    desc.isMono(),
                    desc.isMixdown(),
                    desc.isPrivate(),
                    bundle_ids.is_empty()
                );
                let procs = desc.processes();
                eprintln!("[tap] description processes: {}", procs.len());
            }

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

            // The microphone is deliberately NOT in this aggregate. Sharing one
            // meant sharing a clock, which is sample-accurate and also fatal: a
            // tap gates the clock of the aggregate holding it, so with nothing
            // playing the whole device stops delivering and the microphone dies
            // with it. Measured: zero frames in 6 s, and continuous frames the
            // moment the tap is removed. Alignment is rebuilt from elapsed time
            // in `drain` instead.


            // The output device must be in the aggregate for the tap to have
            // anything to observe. It must NOT be the clock master: an output
            // device only clocks while something is playing, and the aggregate
            // then delivers no callbacks at all — starving the microphone too,
            // so a room conversation with nothing playing records absolute
            // silence and reports success. Measured: zero frames in 6 s with
            // nothing rendering. The microphone runs continuously, so it is the
            // clock, and the output is the master only when there is no mic.
            //
            // This does NOT fix the stall: with a tap in the aggregate, nothing
            // is delivered while no process renders, whichever device is master
            // and whether or not `kAudioAggregateDeviceClockDeviceKey` is set —
            // both measured. See README, "The tap gates the clock".
            let (_out_dev, out_uid) = default_output()?;

            let k_sub_uid = NSString::from_str(kAudioSubDeviceUIDKey.to_str().unwrap());
            let k_drift = NSString::from_str(kAudioSubDeviceDriftCompensationKey.to_str().unwrap());
            let sub_of = |uid: &NSString| {
                NSDictionary::from_slices(
                    &[&*k_sub_uid, &*k_drift],
                    &[
                        uid as &objc2::runtime::AnyObject,
                        &*one as &objc2::runtime::AnyObject,
                    ],
                )
            };

            // Mic first: the IOProc hands back sub-device buffers in list
            // order, and the channel accounting below assumes mic before tap.
            let sub_out = sub_of(&out_uid);
            let sub_list = NSArray::from_slice(&[&*sub_out as &objc2::runtime::AnyObject]);

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
            keys.push(&k_main);
            vals.push(&*out_uid);

            let dict = NSDictionary::from_slices(&keys, &vals);

            let mut device: AudioObjectID = 0;
            let cf_dict: &objc2_core_foundation::CFDictionary =
                &*(Retained::as_ptr(&dict) as *const objc2_core_foundation::CFDictionary);
            let st = AudioHardwareCreateAggregateDevice(cf_dict, NonNull::from(&mut device));
            if st != 0 {
                stop_mic(mic_dev, mic_proc);
                AudioHardwareDestroyProcessTap(tap);
                bail!("AudioHardwareCreateAggregateDevice failed: OSStatus {st}");
            }

            let call_channels = asbd.mChannelsPerFrame.max(1);
            let call_rate = asbd.mSampleRate;
            let call_ring = Arc::new(Ring::new(
                (call_rate as usize) * call_channels as usize * ring_seconds,
            ));
            let agg_proc = match start_ioproc(device, call_ring.clone()) {
                Ok(p) => p,
                Err(e) => {
                    stop_mic(mic_dev, mic_proc);
                    AudioHardwareDestroyAggregateDevice(device);
                    AudioHardwareDestroyProcessTap(tap);
                    return Err(e);
                }
            };

            // The microphone runs on the input device itself: no aggregate, no
            // tap, nothing that can gate its clock.
            Ok(Self {
                tap,
                agg: device,
                agg_proc,
                mic_dev,
                mic_proc,
                call_ring,
                mic_ring,
                call_rate,
                mic_rate,
                call_channels,
                mic_channels,
                started: std::time::Instant::now(),
            })
        }
    }

    /// Both tracks since the last call, downmixed to mono, each at its own rate.
    ///
    /// The tap stops delivering whenever nothing is playing, so its stream has
    /// holes where the mic's does not. Elapsed wall-clock time — independent of
    /// either device clock — says how many frames each track should have by now,
    /// and a short track is padded with silence to put the gap back. Without
    /// that, everything after a pause slides earlier by the length of the pause.
    ///
    /// A track that has run long is never trimmed: dropping captured audio to
    /// satisfy a clock estimate is the wrong way round.
    /// Seconds of genuine device audio behind each track, against the total
    /// each track spans. `(mic_real, mic_total, call_real, call_total)`.
    pub fn real_seconds(&self, d: &Drain) -> (f64, f64, f64, f64) {
        (
            d.mic_real as f64 / self.mic_rate,
            d.mic_out as f64 / self.mic_rate,
            d.call_real as f64 / self.call_rate,
            d.call_out as f64 / self.call_rate,
        )
    }

    pub fn drain(&self, d: &mut Drain) -> (Vec<f32>, Vec<f32>) {
        let elapsed = self.started.elapsed().as_secs_f64();

        let take = |ring: Option<&Arc<Ring>>,
                        cursor: &mut usize,
                        emitted: &mut u64,
                        real: &mut u64,
                        channels: usize,
                        rate: f64|
         -> Vec<f32> {
            let Some(ring) = ring else {
                return Vec::new();
            };
            let (raw, next) = ring.drain_from(*cursor);
            *cursor = next;
            let mono = crate::resample::downmix(&raw, channels.max(1), 0..channels.max(1));

            let target = (elapsed * rate) as u64;
            let have = *emitted + mono.len() as u64;
            let mut out = Vec::with_capacity(mono.len());
            if target > have && (target - have) as f64 / rate > PAD_THRESHOLD_S {
                // The gap happened before these samples, so the silence goes
                // in front of them.
                out.resize((target - have) as usize, 0.0);
            }
            out.extend_from_slice(&mono);
            *real += mono.len() as u64;
            *emitted += out.len() as u64;
            out
        };

        let mic = take(
            self.mic_ring.as_ref(),
            &mut d.mic_cursor,
            &mut d.mic_out,
            &mut d.mic_real,
            self.mic_channels as usize,
            self.mic_rate,
        );
        let call = take(
            Some(&self.call_ring),
            &mut d.call_cursor,
            &mut d.call_out,
            &mut d.call_real,
            self.call_channels as usize,
            self.call_rate,
        );
        (mic, call)
    }
}

/// Attach a realtime IOProc to `device` that pushes every input buffer into
/// `ring`, and start it. Used for both the tap's aggregate and the microphone.
unsafe fn start_ioproc(device: AudioObjectID, ring: Arc<Ring>) -> Result<AudioDeviceIOProcID> {
    let block = block2::RcBlock::new(
        move |_now: NonNull<objc2_core_audio_types::AudioTimeStamp>,
              input: NonNull<objc2_core_audio_types::AudioBufferList>,
              _in_time: NonNull<objc2_core_audio_types::AudioTimeStamp>,
              _out: NonNull<objc2_core_audio_types::AudioBufferList>,
              _out_time: NonNull<objc2_core_audio_types::AudioTimeStamp>| {
            let list = input.as_ref();
            let n = (list.mNumberBuffers as usize).min(8);
            let buffers = std::slice::from_raw_parts(list.mBuffers.as_ptr(), n);

            if std::env::var_os("AMBIENT_DEBUG_TAP").is_some()
                && !DUMPED.swap(true, Ordering::Relaxed)
            {
                eprintln!("[ioproc] mNumberBuffers={}", list.mNumberBuffers);
                for (i, b) in buffers.iter().enumerate() {
                    let count = b.mDataByteSize as usize / std::mem::size_of::<f32>();
                    let peak = if b.mData.is_null() {
                        -1.0
                    } else {
                        std::slice::from_raw_parts(b.mData as *const f32, count)
                            .iter()
                            .fold(0.0f32, |a, s| a.max(s.abs()))
                    };
                    eprintln!(
                        "[ioproc]   buf{i}: ch={} bytes={} null={} peak={peak:.4}",
                        b.mNumberChannels,
                        b.mDataByteSize,
                        b.mData.is_null()
                    );
                }
            }

            // Fixed-capacity, stack-allocated: no heap work on the realtime
            // thread.
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
                ring.push_interleaved(&srcs[..used], frames);
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
        bail!("AudioDeviceCreateIOProcIDWithBlock failed: OSStatus {st}");
    }
    std::mem::forget(block); // the IOProc holds it for its lifetime

    let st = AudioDeviceStart(device, proc_id);
    if st != 0 {
        bail!("AudioDeviceStart failed: OSStatus {st}");
    }
    Ok(proc_id)
}

/// The device's current sample rate. The input and output devices need not
/// agree, and since they no longer share an aggregate, nothing makes them.
unsafe fn nominal_sample_rate(device: AudioObjectID) -> Result<f64> {
    let mut rate: f64 = 0.0;
    let mut size = std::mem::size_of::<f64>() as u32;
    let a = addr(kAudioDevicePropertyNominalSampleRate);
    let st = AudioObjectGetPropertyData(
        device,
        NonNull::from(&a),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::new(&mut rate as *mut f64 as *mut std::ffi::c_void).unwrap(),
    );
    if st != 0 || rate <= 0.0 {
        bail!("could not read the sample rate of device {device}: OSStatus {st}");
    }
    Ok(rate)
}

impl Drop for ProcessTap {
    fn drop(&mut self) {
        unsafe {
            if let Some(mic) = self.mic_dev {
                AudioDeviceStop(mic, self.mic_proc);
                AudioDeviceDestroyIOProcID(mic, self.mic_proc);
            }
            if self.agg != 0 {
                AudioDeviceStop(self.agg, self.agg_proc);
                AudioDeviceDestroyIOProcID(self.agg, self.agg_proc);
                AudioHardwareDestroyAggregateDevice(self.agg);
            }
            if self.tap != 0 {
                AudioHardwareDestroyProcessTap(self.tap);
            }
        }
    }
}
