//! System-audio capture via Core Audio process taps, which need only the System
//! Audio Recording permission, not Screen Recording.
//!
//! The IO block runs on a realtime thread: nothing in it allocates, locks or
//! logs.

use anyhow::{bail, Result};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::AllocAnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceMainSubDeviceKey,
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceSubDeviceListKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey, kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertyStreamConfiguration,
    kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain, kAudioObjectPropertyName,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput, kAudioObjectSystemObject,
    kAudioSubDeviceDriftCompensationKey, kAudioSubDeviceUIDKey, kAudioSubTapUIDKey,
    kAudioTapPropertyFormat, kAudioTapPropertyUID, AudioDeviceCreateIOProcIDWithBlock,
    AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, CATapDescription,
};
use objc2_core_audio_types::AudioStreamBasicDescription;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

/// A peak below this is silence: no signal, or a tap macOS has denied.
pub const SILENT_PEAK: f32 = 1e-4;

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
            buf: (0..capacity)
                .map(|_| std::cell::UnsafeCell::new(0.0))
                .collect(),
            write: AtomicUsize::new(0),
        }
    }

    /// Stack-only, so safe on the realtime thread.
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

/// Names, not UIDs, are stored: a name is what a person picking a microphone
/// recognises.
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

/// A name that no longer matches falls back to the default loudly: a silent
/// fallback records nothing and reports success.
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

/// A tap observes audio on its way to an output device, so that device must be
/// in the aggregate; without it the tap delivers silence, not an error.
unsafe fn default_output() -> Result<(AudioObjectID, Retained<NSString>)> {
    default_device(kAudioHardwarePropertyDefaultOutputDevice)
}

/// Unclamped, unlike `input_channel_count`, so `input_devices` can tell which
/// devices cannot record.
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

/// A denied tap and an idle machine both produce zeros; whether anything was
/// rendering during capture is the only thing that tells them apart.
pub fn silent_tap_advice(
    call_peak: f32,
    max_rendering: usize,
    bundles: &[String],
) -> Option<String> {
    if call_peak >= SILENT_PEAK {
        return None;
    }
    // A filtered tap that caught nothing is the filter working, not a denial,
    // whatever else was playing.
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
    // Launched by LaunchServices, the parent is launchd (pid 1). From a shell,
    // TCC attributes the request to the terminal: the commonest silent tap.
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
    /// Stalls whenever nothing is playing.
    pub call_ring: Arc<Ring>,
    /// On its own device and clock, so nothing the tap does can stop it.
    pub mic_ring: Option<Arc<Ring>>,
    pub call_rate: f64,
    pub mic_rate: f64,
    pub call_channels: u32,
    pub mic_channels: u32,
    started: std::time::Instant,
}

#[derive(Default)]
pub struct Drain {
    mic_cursor: usize,
    call_cursor: usize,
    mic_out: u64,
    call_out: u64,
    /// Frames that came from a device, excluding `drain`'s padding; without the
    /// split a tap that stalled throughout reads as healthy.
    mic_real: u64,
    call_real: u64,
}

/// Below this a shortfall is the ring lagging the clock by a tick; padding it
/// would slowly stretch the track.
const PAD_THRESHOLD_S: f64 = 0.05;

impl ProcessTap {
    /// No tap and no aggregate; `drain` returns an empty call stream.
    pub fn start_mic_only(ring_seconds: usize, mic_name: Option<&str>) -> Result<Self> {
        unsafe {
            let dev = input_device(mic_name)?;
            let ch = input_channel_count(dev).unwrap_or(1);
            let rate = nominal_sample_rate(dev).unwrap_or(48_000.0);
            let ring = Arc::new(Ring::new((rate as usize) * ch as usize * ring_seconds));
            let proc = start_ioproc(dev, ring.clone())?;
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

    pub fn has_call_track(&self) -> bool {
        self.call_channels > 0
    }

    /// An empty `bundle_ids` taps everything the machine is playing.
    pub fn start(
        bundle_ids: &[String],
        ring_seconds: usize,
        include_mic: bool,
        mic_name: Option<&str>,
    ) -> Result<Self> {
        unsafe {
            // The mic starts before the tap exists: an input IOProc started
            // after the tap's aggregate is running never returns (measured).
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

            // The documented initialisers, not init plus setters: a hand-built
            // description can select no processes and yield a silent tap.
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

            // NSDictionary is toll-free bridged to the CFDictionary Core Audio
            // wants.
            let sub_tap = NSDictionary::from_slices(
                &[&*NSString::from_str(kAudioSubTapUIDKey.to_str().unwrap())],
                &[&*uid as &objc2::runtime::AnyObject],
            );
            let tap_list = NSArray::from_slice(&[&*sub_tap as &objc2::runtime::AnyObject]);
            let agg_uid = NSString::from_str(&format!("uk.ambient.tap.{}", std::process::id()));
            let name = NSString::from_str("ambient aggregate");
            let one = NSNumber::new_i32(1);

            // The mic stays out of this aggregate: a tap gates the aggregate's
            // clock, so with nothing playing the mic would stop too (measured:
            // zero frames in 6 s). `drain` realigns from elapsed time instead.

            // The output device must be in the aggregate for the tap to observe
            // anything, but not as clock master when there is a mic: an output
            // device clocks only while something plays (measured: zero frames
            // in 6 s). The tap stalls whichever device is master; see
            // docs/adr/0003-two-ioprocs-wall-clock.md.
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
            // order, and the channel accounting assumes mic before tap.
            let sub_out = sub_of(&out_uid);
            let sub_list = NSArray::from_slice(&[&*sub_out as &objc2::runtime::AnyObject]);

            let mut keys: Vec<&NSString> = Vec::new();
            let mut vals: Vec<&objc2::runtime::AnyObject> = Vec::new();
            let k_name = NSString::from_str(kAudioAggregateDeviceNameKey.to_str().unwrap());
            let k_uid = NSString::from_str(kAudioAggregateDeviceUIDKey.to_str().unwrap());
            let k_priv = NSString::from_str(kAudioAggregateDeviceIsPrivateKey.to_str().unwrap());
            let k_auto = NSString::from_str(kAudioAggregateDeviceTapAutoStartKey.to_str().unwrap());
            let k_taps = NSString::from_str(kAudioAggregateDeviceTapListKey.to_str().unwrap());
            let k_subs =
                NSString::from_str(kAudioAggregateDeviceSubDeviceListKey.to_str().unwrap());
            let k_main =
                NSString::from_str(kAudioAggregateDeviceMainSubDeviceKey.to_str().unwrap());

            keys.push(&k_name);
            vals.push(&*name);
            keys.push(&k_uid);
            vals.push(&*agg_uid);
            keys.push(&k_priv);
            vals.push(&*one);
            keys.push(&k_auto);
            vals.push(&*one);
            keys.push(&k_taps);
            vals.push(&*tap_list);
            keys.push(&k_subs);
            vals.push(&*sub_list);
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

    /// `(mic_real, mic_total, call_real, call_total)` in seconds.
    pub fn real_seconds(&self, d: &Drain) -> (f64, f64, f64, f64) {
        (
            d.mic_real as f64 / self.mic_rate,
            d.mic_out as f64 / self.mic_rate,
            d.call_real as f64 / self.call_rate,
            d.call_out as f64 / self.call_rate,
        )
    }

    /// The tap stops delivering while nothing plays, so wall-clock time sets
    /// each track's expected length and a short track is padded with silence;
    /// otherwise everything after a pause slides earlier. A long track is never
    /// trimmed: dropping captured audio to fit a clock estimate loses data.
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
            self.has_call_track().then_some(&self.call_ring),
            &mut d.call_cursor,
            &mut d.call_out,
            &mut d.call_real,
            self.call_channels as usize,
            self.call_rate,
        );
        (mic, call)
    }
}

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

            // Stack-allocated: no heap work on the realtime thread.
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

/// Input and output devices need not agree on rate.
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
