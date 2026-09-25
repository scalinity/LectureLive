//! Thin CoreAudio wrappers for default-output routing (macOS only).
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::null;

use anyhow::{bail, Result};
use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use coreaudio_sys::*;

fn addr(selector: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn check(status: OSStatus, what: &str) -> Result<()> {
    if status != 0 {
        bail!("{what} failed: OSStatus {status}");
    }
    Ok(())
}

pub fn default_output_device() -> Result<AudioObjectID> {
    let a = addr(kAudioHardwarePropertyDefaultOutputDevice);
    let mut id: AudioObjectID = 0;
    let mut size = size_of::<AudioObjectID>() as u32;
    check(
        unsafe { AudioObjectGetPropertyData(kAudioObjectSystemObject, &a, 0, null(), &mut size, &mut id as *mut _ as *mut c_void) },
        "get default output",
    )?;
    Ok(id)
}

pub fn set_default_output(id: AudioObjectID) -> Result<()> {
    let a = addr(kAudioHardwarePropertyDefaultOutputDevice);
    check(
        unsafe {
            AudioObjectSetPropertyData(kAudioObjectSystemObject, &a, 0, null(), size_of::<AudioObjectID>() as u32, &id as *const _ as *const c_void)
        },
        "set default output",
    )
}

pub fn device_uid(id: AudioObjectID) -> Result<String> {
    let a = addr(kAudioDevicePropertyDeviceUID);
    let mut uid: CFStringRef = std::ptr::null();
    let mut size = size_of::<CFStringRef>() as u32;
    check(
        unsafe { AudioObjectGetPropertyData(id, &a, 0, null(), &mut size, &mut uid as *mut _ as *mut c_void) },
        "get device uid",
    )?;
    Ok(unsafe { CFString::wrap_under_create_rule(uid) }.to_string())
}

pub fn device_for_uid(uid: &str) -> Result<Option<AudioObjectID>> {
    let a = addr(kAudioHardwarePropertyTranslateUIDToDevice);
    let cf_uid = CFString::new(uid);
    let qualifier = cf_uid.as_concrete_TypeRef();
    let mut id: AudioObjectID = kAudioObjectUnknown;
    let mut size = size_of::<AudioObjectID>() as u32;
    check(
        unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject,
                &a,
                size_of::<CFStringRef>() as u32,
                &qualifier as *const _ as *const c_void,
                &mut size,
                &mut id as *mut _ as *mut c_void,
            )
        },
        "translate uid",
    )?;
    Ok((id != kAudioObjectUnknown).then_some(id))
}

/// Public stacked (multi-output) aggregate: every subdevice plays the same audio.
pub fn create_multi_output(name: &str, uid: &str, clock_uid: &str, sub_uids: &[&str]) -> Result<AudioObjectID> {
    let subs: Vec<CFDictionary<CFString, CFType>> = sub_uids
        .iter()
        .map(|u| {
            CFDictionary::from_CFType_pairs(&[
                (CFString::new("uid"), CFString::new(u).as_CFType()),
                (CFString::new("drift"), CFNumber::from(i32::from(*u != clock_uid)).as_CFType()),
            ])
        })
        .collect();
    let desc = CFDictionary::from_CFType_pairs(&[
        (CFString::new("name"), CFString::new(name).as_CFType()),
        (CFString::new("uid"), CFString::new(uid).as_CFType()),
        (CFString::new("subdevices"), CFArray::from_CFTypes(&subs).as_CFType()),
        (CFString::new("master"), CFString::new(clock_uid).as_CFType()),
        (CFString::new("stacked"), CFNumber::from(1).as_CFType()),
        (CFString::new("private"), CFNumber::from(0).as_CFType()),
    ]);
    let mut id: AudioObjectID = 0;
    check(
        unsafe { AudioHardwareCreateAggregateDevice(desc.as_concrete_TypeRef() as _, &mut id) },
        "create aggregate",
    )?;
    Ok(id)
}

/// Returns once the device no longer resolves: the HAL removes it asynchronously
/// (about 13 ms on macOS 27), and callers re-check the device list straight after.
pub fn destroy_aggregate(id: AudioObjectID) -> Result<()> {
    let uid = device_uid(id)?;
    check(unsafe { AudioHardwareDestroyAggregateDevice(id) }, "destroy aggregate")?;
    for _ in 0..200 {
        if device_for_uid(&uid)?.is_none() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    bail!("aggregate {uid} still present 2 s after destroy")
}
