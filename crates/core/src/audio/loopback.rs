//! The app-owned Multi-Output device that Zoom's Speaker is set to (spec §4.3). It is
//! never made the system default, so a crash leaves nothing to restore.
use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait};

use super::coreaudio;
pub use super::routing::BLACKHOLE_UID;

pub const LOOPBACK_UID: &str = "com.lecturelive.loopback";
pub const LOOPBACK_NAME: &str = "LectureLive Loopback";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceAction {
    Keep,
    Rebuild,
    Create,
}

#[derive(Debug)]
pub struct LoopbackStatus {
    pub present: bool,
    pub members: Vec<String>,
    pub blackhole_present: bool,
    pub default_output_uid: String,
}

pub struct OutputInfo {
    pub name: String,
    pub uid: String,
    pub is_aggregate: bool,
}

pub fn plan_device(existing_members: Option<&[String]>, physical_uid: &str) -> DeviceAction {
    match existing_members {
        None => DeviceAction::Create,
        Some(m) if m.len() == 2 && m.iter().any(|u| u == BLACKHOLE_UID) && m.iter().any(|u| u == physical_uid) => DeviceAction::Keep,
        Some(_) => DeviceAction::Rebuild,
    }
}

pub fn choose_physical(requested: Option<&str>, default_uid: &str, default_is_aggregate: bool) -> Result<String> {
    let uid = match requested {
        Some(u) => u,
        None if default_is_aggregate || default_uid == LOOPBACK_UID => bail!(
            "the system output {default_uid} is itself a multi-output device; name the headphones or speakers with --output <UID> (`lecturelive outputs` lists them)"
        ),
        None => default_uid,
    };
    if uid == BLACKHOLE_UID || uid == LOOPBACK_UID {
        bail!("{uid} cannot be the physical output of {LOOPBACK_NAME}");
    }
    Ok(uid.to_string())
}

/// Creates the device, or rebuilds it under the same UID and name when the physical output
/// changed. Run on request only, never during a recording: a rebuild under a running meeting
/// would move Zoom to another speaker.
pub fn setup(output: Option<&str>) -> Result<(DeviceAction, String)> {
    if coreaudio::device_for_uid(BLACKHOLE_UID)?.is_none() {
        bail!("BlackHole 2ch is not installed (brew install blackhole-2ch)");
    }
    let default_id = coreaudio::default_output_device()?;
    let physical = choose_physical(output, &coreaudio::device_uid(default_id)?, coreaudio::is_aggregate(default_id)?)?;
    let physical_id = coreaudio::device_for_uid(&physical)?.with_context(|| format!("output {physical} not found"))?;
    if coreaudio::is_aggregate(physical_id)? {
        bail!("{physical} is itself a multi-output device; name the headphones or speakers");
    }
    let existing = coreaudio::device_for_uid(LOOPBACK_UID)?;
    let members = existing.map(coreaudio::aggregate_members).transpose()?;
    let action = plan_device(members.as_deref(), &physical);
    if action == DeviceAction::Rebuild {
        coreaudio::destroy_aggregate(existing.expect("rebuild implies an existing device"))?;
    }
    if action != DeviceAction::Keep {
        // BlackHole is the clock: it never disconnects, so headphones leaving cannot take the clock away.
        coreaudio::create_multi_output(LOOPBACK_NAME, LOOPBACK_UID, BLACKHOLE_UID, &[BLACKHOLE_UID, &physical])?;
    }
    Ok((action, physical))
}

pub fn status() -> Result<LoopbackStatus> {
    let existing = coreaudio::device_for_uid(LOOPBACK_UID)?;
    Ok(LoopbackStatus {
        present: existing.is_some(),
        members: existing.map(coreaudio::aggregate_members).transpose()?.unwrap_or_default(),
        blackhole_present: coreaudio::device_for_uid(BLACKHOLE_UID)?.is_some(),
        default_output_uid: coreaudio::device_uid(coreaudio::default_output_device()?)?,
    })
}

pub fn list_outputs() -> Result<Vec<OutputInfo>> {
    let mut out = Vec::new();
    for d in cpal::default_host().output_devices()? {
        let uid = d.id()?.id().to_string();
        let is_aggregate = match coreaudio::device_for_uid(&uid)? {
            Some(id) => coreaudio::is_aggregate(id)?,
            None => false,
        };
        out.push(OutputInfo { name: d.description()?.name().to_string(), uid, is_aggregate });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::tone;

    fn members(a: &str, b: &str) -> Vec<String> {
        vec![a.to_string(), b.to_string()]
    }

    #[test]
    fn creates_keeps_or_rebuilds_by_members() {
        assert_eq!(plan_device(None, "BuiltInSpeakerDevice"), DeviceAction::Create);
        assert_eq!(plan_device(Some(&members(BLACKHOLE_UID, "BuiltInSpeakerDevice")), "BuiltInSpeakerDevice"), DeviceAction::Keep);
        assert_eq!(plan_device(Some(&members("BuiltInSpeakerDevice", BLACKHOLE_UID)), "BuiltInSpeakerDevice"), DeviceAction::Keep);
        assert_eq!(plan_device(Some(&members(BLACKHOLE_UID, "AirPods_UID")), "BuiltInSpeakerDevice"), DeviceAction::Rebuild);
        assert_eq!(plan_device(Some(&[BLACKHOLE_UID.to_string()]), "BuiltInSpeakerDevice"), DeviceAction::Rebuild);
    }

    #[test]
    fn a_physical_default_is_used_when_nothing_is_named() {
        assert_eq!(choose_physical(None, "BuiltInSpeakerDevice", false).unwrap(), "BuiltInSpeakerDevice");
        assert_eq!(choose_physical(Some("AirPods_UID"), "BuiltInSpeakerDevice", false).unwrap(), "AirPods_UID");
    }

    #[test]
    fn refuses_an_aggregate_default_as_the_physical_output() {
        let err = choose_physical(None, "~:AMS2_StackedOutput:1", true).unwrap_err();
        assert!(format!("{err}").contains("--output"), "{err}");
        assert_eq!(choose_physical(Some("BuiltInSpeakerDevice"), "~:AMS2_StackedOutput:1", true).unwrap(), "BuiltInSpeakerDevice");
    }

    #[test]
    fn refuses_blackhole_or_itself_as_the_physical_output() {
        assert!(choose_physical(Some(BLACKHOLE_UID), "BuiltInSpeakerDevice", false).is_err());
        assert!(choose_physical(Some(LOOPBACK_UID), "BuiltInSpeakerDevice", false).is_err());
        assert!(choose_physical(None, LOOPBACK_UID, true).is_err());
    }

    fn loudest_on_blackhole_while(play: impl FnOnce() + Send + 'static) -> f32 {
        let dir = tempfile::tempdir().unwrap();
        let player = std::thread::spawn(play);
        let mut loudest = 0f32;
        crate::audio::input::record_for("BlackHole 2ch", std::time::Duration::from_secs(3), dir.path(), |l| loudest = loudest.max(l)).unwrap();
        player.join().unwrap();
        crate::audio::level::dbfs(loudest)
    }

    /// Creates or keeps the real device and plays tones; the system output must not change.
    /// cargo test -p lecturelive-core loopback -- --ignored --nocapture
    #[test]
    #[ignore]
    fn device_carries_its_input_to_blackhole_without_touching_the_default_output() {
        let before = coreaudio::device_uid(coreaudio::default_output_device().unwrap()).unwrap();
        let (action, physical) = setup(Some("BuiltInSpeakerDevice")).unwrap();
        println!("setup: {action:?} with {physical}");
        let s = status().unwrap();
        assert!(s.present && s.members.contains(&BLACKHOLE_UID.to_string()) && s.members.contains(&physical));
        assert_eq!(s.default_output_uid, before, "setup must not change the system output");
        let via_device = loudest_on_blackhole_while(|| tone::play_tone(LOOPBACK_UID, 2.0, 0.05).unwrap());
        let direct = loudest_on_blackhole_while(|| tone::play_tone("BuiltInSpeakerDevice", 2.0, 0.05).unwrap());
        println!("BlackHole loudest: via {LOOPBACK_NAME} {via_device:.1} dBFS, straight to the speakers {direct:.1} dBFS");
        assert!(via_device > -60.0, "the spec §4.3 preflight threshold");
        assert!(direct < -90.0);
        assert_eq!(coreaudio::device_uid(coreaudio::default_output_device().unwrap()).unwrap(), before);
        assert_eq!(setup(Some("BuiltInSpeakerDevice")).unwrap().0, DeviceAction::Keep);
    }
}
