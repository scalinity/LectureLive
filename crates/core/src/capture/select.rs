//! The saved window and region (spec §7.1): a descriptor (bundle id, title, size) and the slide's
//! rectangle, kept per course. On start it is revalidated; anything but one exact match asks.
use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::capture::detect::Region;
use crate::capture::window::WindowInfo;
use crate::fsutil::write_atomic;

/// How much a window's size may differ and still be the same window.
const SIZE_TOLERANCE: f64 = 0.02;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Descriptor {
    pub bundle_id: Option<String>,
    pub app: String,
    pub title: String,
    pub width: u32,
    pub height: u32,
}

impl Descriptor {
    pub fn of(w: &WindowInfo) -> Self {
        Self { bundle_id: w.bundle_id.clone(), app: w.app.clone(), title: w.title.clone(), width: w.width, height: w.height }
    }

    /// The same app: by bundle id when both have one, else by the app's name.
    pub fn same_app(&self, w: &WindowInfo) -> bool {
        match (&self.bundle_id, &w.bundle_id) {
            (Some(a), Some(b)) => a == b,
            _ => self.app == w.app,
        }
    }

    pub fn same_size(&self, w: &WindowInfo) -> bool {
        let near = |a: u32, b: u32| (a as f64 - b as f64).abs() <= SIZE_TOLERANCE * a.max(1) as f64;
        near(self.width, w.width) && near(self.height, w.height)
    }

    pub fn matches(&self, w: &WindowInfo) -> bool {
        self.same_app(w) && self.title == w.title && self.same_size(w)
    }

    /// What the person calls it: the window's title, else its app.
    pub fn label(&self) -> String {
        if self.title.is_empty() { self.app.clone() } else { self.title.clone() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Selection {
    pub descriptor: Descriptor,
    pub region: Region,
    /// Parts of the region that are not the slide (a speaker's camera drawn over it), as fractions of the
    /// region: the detector never reacts to them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub leave_out: Vec<Region>,
    /// The region at the window's other sizes (full screen and back), found again or chosen there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sizes: Vec<SizedRegion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SizedRegion {
    pub width: u32,
    pub height: u32,
    pub region: Region,
}

impl Selection {
    /// The same window: its app and title, at its size or at a size seen before.
    pub fn matches(&self, w: &WindowInfo) -> bool {
        let d = &self.descriptor;
        d.same_app(w) && d.title == w.title && self.at_size(w.width, w.height).is_some()
    }

    /// This selection with the window at `width`×`height`: its region for that size, if the size was seen.
    pub fn at_size(&self, width: u32, height: u32) -> Option<Selection> {
        let probe = WindowInfo { id: 0, app: String::new(), bundle_id: None, title: String::new(), width, height, on_screen: true };
        if self.descriptor.same_size(&probe) {
            return Some(self.clone());
        }
        let s = self.sizes.iter().find(|s| Descriptor { width: s.width, height: s.height, ..self.descriptor.clone() }.same_size(&probe))?;
        Some(self.with_size(s.width, s.height, s.region))
    }

    /// This selection at a new size with its region there; the size it was at is remembered.
    pub fn with_size(&self, width: u32, height: u32, region: Region) -> Selection {
        let mut sizes: Vec<SizedRegion> = self.sizes.iter().filter(|s| (s.width, s.height) != (width, height)).copied().collect();
        if (self.descriptor.width, self.descriptor.height) != (width, height) {
            sizes.push(SizedRegion { width: self.descriptor.width, height: self.descriptor.height, region: self.region });
        }
        Selection { descriptor: Descriptor { width, height, ..self.descriptor.clone() }, region, leave_out: self.leave_out.clone(), sizes }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Revalidation {
    Match(WindowInfo),
    /// Why the saved window was not found as it was, and the windows the person may mean.
    Ask { reason: String, candidates: Vec<WindowInfo> },
}

/// Spec §7.1: exactly one window matching bundle id, title and a size it had binds; anything else asks.
pub fn revalidate(sel: &Selection, windows: &[WindowInfo]) -> Revalidation {
    let d = &sel.descriptor;
    let exact: Vec<WindowInfo> = windows.iter().filter(|w| sel.matches(w)).cloned().collect();
    match exact.len() {
        1 => return Revalidation::Match(exact[0].clone()),
        n if n > 1 => return Revalidation::Ask { reason: format!("{n} windows match {}; choose one", d.label()), candidates: exact },
        _ => {}
    }
    let same_app: Vec<WindowInfo> = windows.iter().filter(|w| d.same_app(w)).cloned().collect();
    let reason = if same_app.is_empty() {
        format!("No {} window is open", d.app)
    } else if let Some(w) = same_app.iter().find(|w| w.title == d.title) {
        format!("{} is {} × {} now; it was {} × {}", d.label(), w.width, w.height, d.width, d.height)
    } else {
        format!("{} has no “{}” window", d.app, d.title)
    };
    Revalidation::Ask { reason, candidates: same_app }
}

/// The saved selections by course, in the app's data folder (`capture.json`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Selections(pub BTreeMap<String, Selection>);

impl Selections {
    /// A missing file is no selections.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(b) => serde_json::from_slice(&b).with_context(|| format!("read {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_atomic(path, &serde_json::to_vec_pretty(self)?).with_context(|| format!("write {}", path.display()))
    }

    pub fn get(&self, course: &str) -> Option<&Selection> {
        self.0.get(course)
    }

    pub fn set(&mut self, course: &str, sel: Selection) {
        self.0.insert(course.to_string(), sel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::detect::Region;
    use crate::capture::window::WindowInfo;

    fn win(id: u32, bundle: &str, title: &str, w: u32, h: u32) -> WindowInfo {
        WindowInfo { id, app: "zoom.us".into(), bundle_id: Some(bundle.into()), title: title.into(), width: w, height: h, on_screen: true }
    }

    fn saved() -> Selection {
        Selection { descriptor: Descriptor::of(&win(1, "us.zoom.xos", "Zoom Meeting", 1600, 900)), region: Region { x: 0.1, y: 0.1, w: 0.8, h: 0.8 }, leave_out: vec![], sizes: vec![] }
    }

    #[test]
    fn exactly_one_matching_window_binds() {
        let ws = [win(7, "us.zoom.xos", "Zoom Workplace", 900, 600), win(42, "us.zoom.xos", "Zoom Meeting", 1610, 895)];
        assert_eq!(revalidate(&saved(), &ws), Revalidation::Match(ws[1].clone()), "within 2% is the same size");
    }

    #[test]
    fn a_mismatch_asks_with_the_app_s_windows_and_says_why() {
        let resized = [win(42, "us.zoom.xos", "Zoom Meeting", 1280, 800)];
        let Revalidation::Ask { reason, candidates } = revalidate(&saved(), &resized) else { panic!() };
        assert!(reason.contains("1280 × 800") && reason.contains("1600 × 900"), "{reason}");
        assert_eq!(candidates, resized.to_vec());

        let other_title = [win(7, "us.zoom.xos", "Zoom Workplace", 900, 600)];
        let Revalidation::Ask { reason, candidates } = revalidate(&saved(), &other_title) else { panic!() };
        assert!(reason.contains("Zoom Meeting"), "{reason}");
        assert_eq!(candidates.len(), 1);

        let Revalidation::Ask { reason, candidates } = revalidate(&saved(), &[win(9, "com.google.Chrome", "Slides", 1600, 900)]) else { panic!() };
        assert!(reason.contains("zoom.us") && candidates.is_empty(), "{reason}");

        let two = [win(42, "us.zoom.xos", "Zoom Meeting", 1600, 900), win(43, "us.zoom.xos", "Zoom Meeting", 1600, 900)];
        let Revalidation::Ask { reason, candidates } = revalidate(&saved(), &two) else { panic!() };
        assert!(reason.contains("2 "), "{reason}");
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn a_window_outside_a_bundle_is_named_by_its_app() {
        let dev = WindowInfo { bundle_id: None, app: "desktop".into(), ..win(5, "", "LectureLive deck", 1280, 720) };
        let sel = Selection { descriptor: Descriptor::of(&dev), region: Region::WHOLE, leave_out: vec![], sizes: vec![] };
        assert_eq!(revalidate(&sel, &[dev.clone()]), Revalidation::Match(dev));
    }

    /// Full screen and back: one window, two sizes, the slide in a different place in each.
    #[test]
    fn a_selection_remembers_a_region_per_window_size_and_matches_either() {
        let sel = saved(); // 1600 × 900, region (0.1, 0.1, 0.8, 0.8)
        let full = Region { x: 0.05, y: 0.12, w: 0.9, h: 0.7 };
        let both = sel.with_size(1920, 1200, full);
        let at_full = both.at_size(1920, 1200).unwrap();
        assert_eq!((at_full.descriptor.width, at_full.descriptor.height, at_full.region), (1920, 1200, full));
        let back = at_full.at_size(1600, 900).unwrap();
        assert_eq!((back.descriptor.width, back.region), (1600, sel.region));
        assert_eq!(both.at_size(1280, 800), None);
        let fullscreen = win(42, "us.zoom.xos", "Zoom Meeting", 1920, 1200);
        assert_eq!(revalidate(&both, &[fullscreen.clone()]), Revalidation::Match(fullscreen), "a size seen before is the same window");
        let old = r#"{"descriptor":{"bundle_id":"us.zoom.xos","app":"zoom.us","title":"Zoom Meeting","width":1600,"height":900},"region":{"x":0.1,"y":0.1,"w":0.8,"h":0.8}}"#;
        let loaded: Selection = serde_json::from_str(old).unwrap();
        assert!(loaded.sizes.is_empty() && loaded.leave_out.is_empty(), "an earlier capture.json still loads");
    }

    #[test]
    fn selections_are_kept_per_course_and_a_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.json");
        assert_eq!(Selections::load(&path).unwrap(), Selections::default());
        let mut s = Selections::default();
        s.set("Machine Learning", saved());
        s.save(&path).unwrap();
        let back = Selections::load(&path).unwrap();
        assert_eq!(back.get("Machine Learning"), Some(&saved()));
        assert_eq!(back.get("Statistics"), None);
    }
}
