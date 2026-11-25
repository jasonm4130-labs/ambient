//! Core Audio process-tap probe: the fastest check that a machine can be tapped.

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

/// The Core Audio process objects rendering output now; empty if the list
/// cannot be read.
fn rendering_output() -> Vec<AudioObjectID> {
    let Ok(processes) = (unsafe {
        get_array::<AudioObjectID>(
            kAudioObjectSystemObject as AudioObjectID,
            kAudioHardwarePropertyProcessObjectList,
        )
    }) else {
        return Vec::new();
    };
    processes
        .into_iter()
        .filter(|&p| {
            let on: u32 =
                unsafe { get_scalar(p, kAudioProcessPropertyIsRunningOutput).unwrap_or(0) };
            on != 0
        })
        .collect()
}

pub fn bundles_rendering_output() -> Vec<String> {
    rendering_output()
        .into_iter()
        .filter_map(|p| unsafe { get_bundle_id(p) })
        .collect()
}

/// A denied tap looks like an idle one: Core Audio reports success and fills
/// the buffers with zeros. This count tells the two apart.
pub fn processes_rendering_output() -> usize {
    rendering_output().len()
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

    Ok(())
}
