//! Phase 0 probe: prove the two risky assumptions before building on them.
//! Kept because it is also the fastest way to confirm a new machine is viable.
//!
//!   1. The Core Audio process-tap API is reachable from Rust with no C shim.
//!   2. ONNX Runtime's CoreML execution provider is available from a prebuilt
//!      binary, and can be asked for the Neural Engine specifically.
//!
//! Neither is a substitute for measuring the real model — see `docs/phase-0.md`.

use anyhow::{bail, Result};
use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_audio::{
    kAudioHardwarePropertyProcessObjectList, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioProcessPropertyBundleID,
    kAudioProcessPropertyIsRunningOutput, kAudioProcessPropertyPID, AudioObjectGetPropertyData,
    AudioObjectGetPropertyDataSize, AudioObjectID, AudioObjectPropertyAddress,
};
use objc2_core_foundation::{CFRetained, CFString};

fn addr(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Read a property whose payload is a flat array of `T`.
unsafe fn get_array<T: Copy + Default>(object: AudioObjectID, selector: u32) -> Result<Vec<T>> {
    let mut a = addr(selector);
    let mut size: u32 = 0;

    let status = AudioObjectGetPropertyDataSize(
        object,
        NonNull::from(&mut a),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
    );
    if status != 0 {
        bail!("AudioObjectGetPropertyDataSize failed: OSStatus {status}");
    }

    let count = size as usize / std::mem::size_of::<T>();
    let mut buf: Vec<T> = vec![T::default(); count];
    if count == 0 {
        return Ok(buf);
    }

    let status = AudioObjectGetPropertyData(
        object,
        NonNull::from(&mut a),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::new(buf.as_mut_ptr() as *mut c_void).unwrap(),
    );
    if status != 0 {
        bail!("AudioObjectGetPropertyData failed: OSStatus {status}");
    }
    Ok(buf)
}

/// Read a scalar property.
unsafe fn get_scalar<T: Copy + Default>(object: AudioObjectID, selector: u32) -> Result<T> {
    let mut a = addr(selector);
    let mut size = std::mem::size_of::<T>() as u32;
    let mut value = T::default();

    let status = AudioObjectGetPropertyData(
        object,
        NonNull::from(&mut a),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::new(&mut value as *mut T as *mut c_void).unwrap(),
    );
    if status != 0 {
        bail!("AudioObjectGetPropertyData failed: OSStatus {status}");
    }
    Ok(value)
}

/// Bundle ID comes back as a CFStringRef the caller owns.
unsafe fn get_bundle_id(object: AudioObjectID) -> Option<String> {
    let mut a = addr(kAudioProcessPropertyBundleID);
    let mut size = std::mem::size_of::<*const c_void>() as u32;
    let mut raw: *const CFString = std::ptr::null();

    let status = AudioObjectGetPropertyData(
        object,
        NonNull::from(&mut a),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::new(&mut raw as *mut *const CFString as *mut c_void).unwrap(),
    );
    if status != 0 || raw.is_null() {
        return None;
    }
    let s = CFRetained::from_raw(NonNull::new(raw as *mut CFString)?);
    Some(s.to_string())
}

pub fn probe_ort() {
    use ort::ep::coreml::ComputeUnits;
    use ort::ep::{CoreML, ExecutionProvider};

    println!("\n── inference ──────────────────────────────────────────");
    ort::init().commit();
    let ane = CoreML::default().with_compute_units(ComputeUnits::CPUAndNeuralEngine);
    match ane.is_available() {
        Ok(true) => println!("CoreML EP        available (CPUAndNeuralEngine requestable)"),
        Ok(false) => println!("CoreML EP        NOT available — inference would fall back to CPU"),
        Err(e) => println!("CoreML EP        probe failed: {e}"),
    }
    println!("note             availability is not placement; a graph can still");
    println!("                 partition onto CPU. Measure with the real model.");
}

/// How many processes are rendering output right now.
///
/// Used to tell two identical-looking failures apart: a tap that is silent
/// because nothing was playing, and a tap that is silent because it was denied.
/// The second is the dangerous one — Core Audio reports success, delivers
/// correctly-shaped buffers, and fills them with zeros.
/// Bundle IDs of the processes currently producing audio.
///
/// `processes_rendering_output` counts them for the capture warning; this names
/// them, which is what lets the menu bar notice that a watched call app has
/// started talking. Processes without a bundle ID (daemons, the system itself)
/// are dropped rather than reported as anonymous.
pub fn bundles_rendering_output() -> Vec<String> {
    let processes: Vec<AudioObjectID> = match unsafe {
        get_array(
            kAudioObjectSystemObject as AudioObjectID,
            kAudioHardwarePropertyProcessObjectList,
        )
    } {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    processes
        .into_iter()
        .filter(|&p| {
            let on: u32 =
                unsafe { get_scalar(p, kAudioProcessPropertyIsRunningOutput).unwrap_or(0) };
            on != 0
        })
        .filter_map(|p| unsafe { get_bundle_id(p) })
        .collect()
}

pub fn processes_rendering_output() -> usize {
    let processes: Vec<AudioObjectID> = match unsafe {
        get_array(
            kAudioObjectSystemObject as AudioObjectID,
            kAudioHardwarePropertyProcessObjectList,
        )
    } {
        Ok(p) => p,
        Err(_) => return 0,
    };
    processes
        .into_iter()
        .filter(|&p| {
            let on: u32 =
                unsafe { get_scalar(p, kAudioProcessPropertyIsRunningOutput).unwrap_or(0) };
            on != 0
        })
        .count()
}

pub fn run() -> Result<()> {
    println!("── capture ────────────────────────────────────────────");

    let processes: Vec<AudioObjectID> = unsafe {
        get_array(
            kAudioObjectSystemObject as AudioObjectID,
            kAudioHardwarePropertyProcessObjectList,
        )?
    };

    println!("{} audio processes known to Core Audio\n", processes.len());
    println!("{:<8} {:<8} {:<10} BUNDLE", "OBJ", "PID", "OUTPUT");

    let mut tappable = 0;
    for p in processes {
        let pid: i32 = unsafe { get_scalar(p, kAudioProcessPropertyPID).unwrap_or(-1) };
        let running_out: u32 =
            unsafe { get_scalar(p, kAudioProcessPropertyIsRunningOutput).unwrap_or(0) };
        let bundle = unsafe { get_bundle_id(p) }.unwrap_or_else(|| "—".into());

        if running_out != 0 {
            tappable += 1;
        }
        println!(
            "{:<8} {:<8} {:<10} {}",
            p,
            pid,
            if running_out != 0 { "playing" } else { "" },
            bundle
        );
    }

    println!("\n{tappable} process(es) currently rendering output — these are tappable.");

    probe_ort();
    Ok(())
}
