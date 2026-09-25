use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::coreaudio;
use crate::fsutil::write_atomic;

pub const AGGREGATE_UID: &str = "com.lecturelive.multioutput";
pub const BLACKHOLE_UID: &str = "BlackHole2ch_UID";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteState {
    pub previous_output_uid: String,
    pub aggregate_uid: String,
}

#[derive(Debug)]
pub struct RouteStatus {
    pub default_output_uid: String,
    pub saved: Option<RouteState>,
    pub abandoned: bool,
    pub blackhole_present: bool,
}

pub fn restore_target(state: &RouteState, current_default_uid: &str) -> Option<String> {
    (current_default_uid == state.aggregate_uid).then(|| state.previous_output_uid.clone())
}

pub fn load_state(path: &Path) -> Result<Option<RouteState>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(serde_json::from_slice(&b).context("route state")?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn save_state(path: &Path, state: &RouteState) -> Result<()> {
    write_atomic(path, &serde_json::to_vec_pretty(state)?)
}

pub fn enable_loopback(state_path: &Path) -> Result<RouteState> {
    let current = coreaudio::device_uid(coreaudio::default_output_device()?)?;
    if current == AGGREGATE_UID {
        bail!("already routed; run disable first");
    }
    if coreaudio::device_for_uid(BLACKHOLE_UID)?.is_none() {
        bail!("BlackHole 2ch is not installed (brew install blackhole-2ch)");
    }
    let state = RouteState { previous_output_uid: current.clone(), aggregate_uid: AGGREGATE_UID.into() };
    save_state(state_path, &state)?; // persisted before anything changes, so a crash can be undone
    if let Some(stale) = coreaudio::device_for_uid(AGGREGATE_UID)? {
        coreaudio::destroy_aggregate(stale)?;
    }
    let agg = coreaudio::create_multi_output("LectureLive Output", AGGREGATE_UID, &current, &[&current, BLACKHOLE_UID])?;
    coreaudio::set_default_output(agg)?;
    Ok(state)
}

pub fn disable_loopback(state_path: &Path) -> Result<bool> {
    let Some(state) = load_state(state_path)? else { return Ok(false) };
    let current = coreaudio::device_uid(coreaudio::default_output_device()?)?;
    let mut restored = false;
    if let Some(prev) = restore_target(&state, &current) {
        let id = coreaudio::device_for_uid(&prev)?.with_context(|| {
            format!("previous output {prev} is gone: choose an output in System Settings → Sound → Output, then run `route off` again")
        })?;
        coreaudio::set_default_output(id)?;
        restored = true;
    }
    if let Some(agg) = coreaudio::device_for_uid(AGGREGATE_UID)? {
        coreaudio::destroy_aggregate(agg)?;
    }
    std::fs::remove_file(state_path)?;
    Ok(restored)
}

pub fn route_status(state_path: &Path) -> Result<RouteStatus> {
    let default_output_uid = coreaudio::device_uid(coreaudio::default_output_device()?)?;
    let saved = load_state(state_path)?;
    let abandoned = saved.as_ref().is_some_and(|s| s.aggregate_uid == default_output_uid);
    let blackhole_present = coreaudio::device_for_uid(BLACKHOLE_UID)?.is_some();
    Ok(RouteStatus { default_output_uid, saved, abandoned, blackhole_present })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> RouteState {
        RouteState { previous_output_uid: "BuiltInSpeakerDevice".into(), aggregate_uid: AGGREGATE_UID.into() }
    }

    #[test]
    fn restores_when_our_aggregate_is_still_default() {
        assert_eq!(restore_target(&state(), AGGREGATE_UID), Some("BuiltInSpeakerDevice".to_string()));
    }

    #[test]
    fn respects_a_later_user_change() {
        assert_eq!(restore_target(&state(), "AirPodsUID"), None);
    }

    #[test]
    fn state_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("route.json");
        save_state(&p, &state()).unwrap();
        assert_eq!(load_state(&p).unwrap(), Some(state()));
        std::fs::remove_file(&p).unwrap();
        assert_eq!(load_state(&p).unwrap(), None);
    }

    /// Changes the real default output; run by hand: cargo test -p lecturelive-core routing -- --ignored
    #[test]
    #[ignore]
    fn enable_then_disable_restores_the_original_output() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("route.json");
        let before = coreaudio::device_uid(coreaudio::default_output_device().unwrap()).unwrap();
        enable_loopback(&p).unwrap();
        assert_eq!(coreaudio::device_uid(coreaudio::default_output_device().unwrap()).unwrap(), AGGREGATE_UID);
        assert!(disable_loopback(&p).unwrap());
        assert_eq!(coreaudio::device_uid(coreaudio::default_output_device().unwrap()).unwrap(), before);
        assert!(coreaudio::device_for_uid(AGGREGATE_UID).unwrap().is_none());
    }
}
