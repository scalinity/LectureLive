//! Redundant captures (spec §7.7): a build step or a frame of a fade whose finished slide was also taken, the same
//! slide taken again, and a camera taken for a slide. Each is judged from the pictures alone, with no request.
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local};

use crate::capture::compare::{self, Look, Order, Relation};
use crate::notes::timeline::embed_line;
use crate::session::coordinator::Store;
use crate::session::files::LectureFiles;
use crate::session::notesfile;
use crate::session::sidecar::{Sidecar, SlideEntry};
use crate::session::slides::{self, REMOVED_DIR};

/// An earlier state is looked for among captures this close in time: a build is revealed over a few minutes.
const BUILD_MINUTES: i64 = 10;
/// A camera is a run of at least this many photographs of one view...
const CAMERA_RUN: usize = 3;
/// ...in which little moves: the typical change from one to the next is at most this share of the frame. A classroom
/// camera's captures changed by 0.2–13 %, typically 0.5 % (M7.6 class); slides of different photographs change throughout.
const CAMERA_STILL: f32 = 0.02;

/// Why a capture is not needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redundant {
    /// A step of a build or a frame of a fade: this slide shows all of it, and more.
    EarlierStateOf(u32),
    /// The same picture as this slide, taken again.
    SameAs(u32),
}

impl fmt::Display for Redundant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Redundant::EarlierStateOf(n) => write!(f, "an earlier state of slide {n}"),
            Redundant::SameAs(n) => write!(f, "the same as slide {n}"),
        }
    }
}

/// One registered capture as the judge sees it.
pub struct Capture {
    pub index: u32,
    pub shown_at: DateTime<Local>,
    /// Whether it may be removed: an automatic capture the sweep may touch and that undo has not put back. Every
    /// capture, candidate or not, can be the one a candidate is judged against.
    pub candidate: bool,
    /// None when its image could not be read: it is never judged and never judged against.
    pub look: Option<Arc<Look>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    pub index: u32,
    pub why: Redundant,
}

/// Relations already measured, by the two slides' numbers (a slide's number never names another image).
#[derive(Default)]
pub struct Relations(HashMap<(u32, u32), Relation>);

impl Relations {
    fn get(&mut self, a: &Capture, b: &Capture) -> Option<Relation> {
        let (la, lb) = (a.look.as_ref()?, b.look.as_ref()?);
        if a.index > b.index {
            return self.get(b, a).map(|r| Relation { lost: r.added, added: r.lost });
        }
        Some(*self.0.entry((a.index, b.index)).or_insert_with(|| compare::relate(la, lb)))
    }

    /// What `b` is to `a`. A photograph's detail is not ink, so it is never the earlier state of anything and is only
    /// ever the same as an identical picture; a slide can still be an earlier state of one that adds a photograph.
    fn order(&mut self, a: &Capture, b: &Capture) -> Order {
        let (Some(la), Some(lb)) = (a.look.as_deref(), b.look.as_deref()) else { return Order::Apart };
        if la.photographic() && lb.photographic() {
            return compare::order_photographs(la, lb);
        }
        match self.get(a, b).map_or(Order::Apart, |r| compare::order(&r)) {
            Order::Earlier if la.photographic() => Order::Apart,
            Order::Later if lb.photographic() => Order::Apart,
            Order::Same if la.photographic() || lb.photographic() => compare::order_photographs(la, lb),
            o => o,
        }
    }

    fn lost(&mut self, a: &Capture, b: &Capture) -> f32 {
        self.get(a, b).map_or(1.0, |r| r.lost)
    }
}

/// Runs that look like a camera left on screen: at least three photographs of one fixed view in which little moves,
/// not all the same picture. Only reported: from saved pictures alone such a run cannot be told from a photograph slide
/// whose caption or highlight changes, so it goes only on the person's word (`lecture tidy --drop`). The scene gate
/// keeps a camera from being captured in the first place (spec §7.2). Each run is its first and last slide number.
pub fn camera_runs(caps: &[Capture]) -> Vec<(u32, u32)> {
    let mut caps: Vec<&Capture> = caps.iter().collect();
    caps.sort_by_key(|c| c.index);
    let look = |i: usize| caps[i].look.as_deref();
    let photo = |i: usize| look(i).is_some_and(Look::photographic);
    let (n, mut out, mut i) = (caps.len(), Vec::new(), 0);
    while i < n {
        let mut j = i + 1;
        if photo(i) {
            while j < n && photo(j) && compare::same_view(look(j - 1).unwrap(), look(j).unwrap()) {
                j += 1;
            }
            let identical = (i + 1..j).all(|k| compare::order_photographs(look(i).unwrap(), look(k).unwrap()) == Order::Same);
            let mut steps: Vec<f32> = (i + 1..j).map(|k| compare::changed(look(k - 1).unwrap(), look(k).unwrap())).collect();
            steps.sort_by(f32::total_cmp);
            let fixed = steps.get(steps.len() / 2).is_some_and(|&c| c <= CAMERA_STILL);
            if j - i >= CAMERA_RUN && !identical && fixed {
                out.push((caps[i].index, caps[j - 1].index));
            }
        }
        i = j;
    }
    out
}

/// Which candidates are redundant, in index order. A capture is only ever removed in favour of one that stays.
pub fn judge(caps: &[Capture], memo: &mut Relations) -> Vec<Verdict> {
    let mut caps: Vec<&Capture> = caps.iter().collect();
    caps.sort_by_key(|c| c.index);
    let n = caps.len();
    let look = |i: usize| caps[i].look.as_deref();
    let mut why: Vec<Option<Redundant>> = vec![None; n];

    // The same picture, however long apart: each group keeps one, the first shown unless one must stay.
    let pool: Vec<usize> = (0..n).filter(|&i| look(i).is_some()).collect();
    let mut group: Vec<usize> = (0..n).collect();
    fn root(group: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while group[r] != r {
            r = group[r];
        }
        group[i] = r;
        r
    }
    for (p, &a) in pool.iter().enumerate() {
        for &b in &pool[p + 1..] {
            if compare::near(look(a).unwrap(), look(b).unwrap()) && memo.order(caps[a], caps[b]) == Order::Same {
                let (ra, rb) = (root(&mut group, a), root(&mut group, b));
                group[ra.max(rb)] = ra.min(rb);
            }
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for &i in &pool {
        let r = root(&mut group, i);
        groups.entry(r).or_default().push(i);
    }
    let mut keeper_of: HashMap<usize, usize> = HashMap::new();
    for members in groups.values() {
        // The first showing, unless one must stay, or the first had Zoom's control bar over it and another did not.
        let keep = members.iter().copied().find(|&i| !caps[i].candidate).or_else(|| members.iter().copied().find(|&i| look(i).is_some_and(|l| l.bar.is_none()))).unwrap_or(members[0]);
        for &m in members {
            // A group is only where to look: the measure is not transitive, so each goes only if it matches the very
            // capture that stays, and otherwise stands on its own.
            if m != keep && caps[m].candidate && memo.order(caps[m], caps[keep]) == Order::Same {
                keeper_of.insert(m, keep);
                why[m] = Some(Redundant::SameAs(caps[keep].index));
            } else {
                keeper_of.insert(m, m);
            }
        }
    }

    // Earlier states, from the most ink down: a keeper is settled before anything it could stand for.
    let mut survivors: Vec<usize> = pool.iter().copied().filter(|&i| keeper_of[&i] == i).collect();
    survivors.sort_by_key(|&i| (std::cmp::Reverse(look(i).unwrap().ink), caps[i].index));
    let mut kept: Vec<usize> = Vec::new();
    for s in survivors {
        if caps[s].candidate {
            // The keeper that holds it most closely, then the nearest in time.
            let mut best: Option<(f32, i64, usize)> = None;
            for &k in &kept {
                let apart = (caps[s].shown_at - caps[k].shown_at).num_milliseconds().abs();
                if apart > BUILD_MINUTES * 60_000 || memo.order(caps[s], caps[k]) != Order::Earlier {
                    continue;
                }
                let lost = memo.lost(caps[s], caps[k]);
                if best.is_none_or(|(l, a, _)| lost < l || (lost == l && apart < a)) {
                    best = Some((lost, apart, k));
                }
            }
            let best = best.map(|(_, _, k)| k);
            if let Some(k) = best {
                let to = Redundant::EarlierStateOf(caps[k].index);
                why[s] = Some(to);
                // What stood in for this capture follows it only if the new keeper holds it as well.
                let followers: Vec<usize> = keeper_of.iter().filter(|&(&m, &keep)| keep == s && m != s).map(|(&m, _)| m).collect();
                for m in followers {
                    why[m] = matches!(memo.order(caps[m], caps[k]), Order::Earlier | Order::Same).then_some(to);
                }
                continue;
            }
        }
        kept.push(s);
    }

    (0..n).filter_map(|i| why[i].map(|why| Verdict { index: caps[i].index, why })).collect()
}

/// Which captures a sweep may take out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Only what no snapshot has used: during a lecture and before each snapshot.
    Pending,
    /// Also what the notes already embed, whose embed goes with it: after the lecture, before its polish.
    All,
}

/// One lecture's captures as the judge saw them, kept between sweeps: decoding a capture takes longer than judging
/// every capture of the lecture.
#[derive(Default)]
pub struct Tidier {
    looks: HashMap<PathBuf, (Option<(u64, SystemTime)>, Option<Arc<Look>>)>,
    memo: Relations,
}

/// The lecture being tidied: its capture and its snapshots share one cache, and one sweep runs at a time, so two
/// sweeps cannot each take out one of a pair.
static CURRENT: std::sync::Mutex<Option<(PathBuf, Arc<tokio::sync::Mutex<Tidier>>)>> = std::sync::Mutex::new(None);

fn tidier(files: &LectureFiles) -> Arc<tokio::sync::Mutex<Tidier>> {
    let mut current = CURRENT.lock().expect("the tidier");
    if let Some((_, t)) = current.as_ref().filter(|(dir, _)| *dir == files.dir) {
        return t.clone();
    }
    let t = Arc::new(tokio::sync::Mutex::new(Tidier::default()));
    *current = Some((files.dir.clone(), t.clone()));
    t
}

fn stamp(p: &Path) -> Option<(u64, SystemTime)> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.len(), m.modified().ok()?))
}

impl Tidier {
    /// Looks at every capture not seen yet, or changed since, on threads of their own.
    async fn refresh(&mut self, files: &LectureFiles, slides: &[SlideEntry]) {
        let stale: Vec<PathBuf> = slides.iter().map(|s| files.dir.join(&s.file)).filter(|p| self.looks.get(p).is_none_or(|(st, _)| *st != stamp(p))).collect();
        if stale.is_empty() {
            return;
        }
        // Relations are kept by slide number: once an image a relation was measured on has changed, none is trusted.
        if stale.iter().any(|p| self.looks.contains_key(p)) {
            self.memo = Relations::default();
        }
        let decoded = tokio::task::spawn_blocking(move || {
            let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
            let chunk = stale.len().div_ceil(threads).max(1);
            std::thread::scope(|scope| {
                let parts: Vec<_> = stale.chunks(chunk).map(|part| scope.spawn(move || part.iter().map(|p| (p.clone(), stamp(p), Look::load(p).map(Arc::new))).collect::<Vec<_>>())).collect();
                parts.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect::<Vec<_>>()
            })
        })
        .await
        .unwrap_or_default();
        for (p, st, look) in decoded {
            self.looks.insert(p, (st, look));
        }
    }

    fn captures(&self, files: &LectureFiles, sc: &Sidecar, scope: Scope) -> Vec<Capture> {
        sc.slides
            .iter()
            .map(|s| {
                let path = files.dir.join(&s.file);
                let look = self.looks.get(&path).and_then(|(_, l)| l.clone());
                let candidate = s.auto && !sc.pinned_slides.contains(&s.index) && (scope == Scope::All || s.index > sc.notes.slide_index);
                Capture { index: s.index, shown_at: s.shown_at, candidate, look }
            })
            .collect()
    }
}

/// What a sweep would take out, without taking anything: `lecture tidy` before `--apply`.
pub async fn plan(files: &LectureFiles, sc: &Sidecar, scope: Scope) -> Vec<Verdict> {
    let t = tidier(files);
    let mut t = t.lock().await;
    t.refresh(files, &sc.slides).await;
    let caps = t.captures(files, sc, scope);
    judge(&caps, &mut t.memo)
}

/// Runs among the lecture's captures that look like a camera (`camera_runs`), for `lecture tidy` to report.
pub async fn camera_report(files: &LectureFiles, sc: &Sidecar) -> Vec<(u32, u32)> {
    let t = tidier(files);
    let mut t = t.lock().await;
    t.refresh(files, &sc.slides).await;
    camera_runs(&t.captures(files, sc, Scope::All))
}

/// Takes the redundant captures `scope` covers out of the lecture (spec §7.7), through the same removal as the
/// person's, so undo restores them; says which went and why. `guard` is a running lecture's snapshot flag: a sweep
/// from capture gives way to a snapshot, which sweeps for itself before it takes its batch.
pub async fn sweep(files: &LectureFiles, store: &Store, scope: Scope, guard: Option<Arc<AtomicBool>>) -> Result<Vec<(SlideEntry, Redundant)>> {
    let held = |g: &Option<Arc<AtomicBool>>| g.as_ref().is_some_and(|g| g.load(Ordering::SeqCst));
    if held(&guard) {
        return Ok(Vec::new());
    }
    let t = tidier(files);
    let mut t = t.lock().await;
    let sc = store.read().await?;
    t.refresh(files, &sc.slides).await;
    let caps = t.captures(files, &sc, scope);
    let verdicts = judge(&caps, &mut t.memo);
    if verdicts.is_empty() {
        return Ok(Vec::new());
    }
    let files = files.clone();
    store
        .update(move |sc| {
            if held(&guard) {
                return Ok(Vec::new());
            }
            // Judged from a copy: what changed since (a removal, an undo, a snapshot) is looked at again here.
            let registered = |sc: &Sidecar, i: u32| sc.slides.iter().any(|s| s.index == i);
            let still: Vec<Verdict> = verdicts
                .into_iter()
                .filter(|v| registered(sc, v.index) && !sc.pinned_slides.contains(&v.index) && (scope == Scope::All || v.index > sc.notes.slide_index))
                .filter(|v| match v.why {
                    Redundant::EarlierStateOf(k) | Redundant::SameAs(k) => registered(sc, k),
                })
                .collect();
            let gone = take_out(&files, sc, &still.iter().map(|v| v.index).collect::<Vec<_>>())?;
            Ok(gone.into_iter().map(|e| { let why = still.iter().find(|v| v.index == e.index).expect("taken out from the verdicts").why; (e, why) }).collect::<Vec<_>>())
        })
        .await
}

/// Takes these registered slides out, inside the sidecar's writer: the notes' embeds of those a snapshot used first (an
/// embed must never name a file that has moved), only where each stands once on its own line outside code, and a slide
/// whose embed could not be taken out stays. Returns what went.
fn take_out(files: &LectureFiles, sc: &mut Sidecar, indices: &[u32]) -> Result<Vec<SlideEntry>> {
    let noted: Vec<SlideEntry> = indices.iter().filter(|&&i| i <= sc.notes.slide_index).filter_map(|&i| sc.slides.iter().find(|s| s.index == i).cloned()).collect();
    let mut stay: HashSet<u32> = HashSet::new();
    if !noted.is_empty() {
        let changes: Vec<(String, Option<String>)> = noted.iter().map(|s| (embed_line(s, &files.dir, files.notes_dir()), None)).collect();
        let made = notesfile::change_embeds(files, sc, &changes, Local::now())?;
        stay = noted.iter().enumerate().filter(|(k, _)| !made.contains(k)).map(|(_, s)| s.index).collect();
    }
    Ok(indices.iter().filter(|i| !stay.contains(i)).filter_map(|&i| slides::remove_in(files, sc, i).ok()).collect())
}

/// Takes out the slides the person names (`lecture tidy --drop`), as tidying does: a camera run the judge only reports,
/// say. Outside a lecture only. Returns what went; a number not registered, or whose embed is not on a line of its own,
/// is left.
pub async fn drop_slides(files: &LectureFiles, store: &Store, indices: &[u32]) -> Result<Vec<SlideEntry>> {
    let (files, indices) = (files.clone(), indices.to_vec());
    store.update(move |sc| take_out(&files, sc, &indices)).await
}

/// Puts back `complete`, discarded by hand, in place of `partial`, kept by hand, when `partial` is an earlier state
/// of it: the complete slide takes the partial's embed in the notes. Used once, to correct a folder pruned by hand
/// before tidying existed (`lecture tidy --swap`); outside a lecture only.
pub async fn swap(files: &LectureFiles, store: &Store, partial: u32, complete: u32) -> Result<()> {
    let sc = store.read().await?;
    let a = sc.slides.iter().find(|s| s.index == partial).cloned().with_context(|| format!("slide {partial} is not in this lecture"))?;
    let b = sc.removed_slides.iter().find(|s| s.index == complete).cloned().with_context(|| format!("slide {complete} was not removed"))?;
    let name = Path::new(&b.file).file_name().context("the removed slide has no filename")?.to_owned();
    let (from, to) = (files.slides.join(REMOVED_DIR).join(&name), files.dir.join(&b.file));
    let (la, lb) = (Look::load(&files.dir.join(&a.file)), Look::load(&from));
    let earlier = la.zip(lb).is_some_and(|(la, lb)| compare::order(&compare::relate(&la, &lb)) == Order::Earlier);
    if !earlier {
        bail!("slide {partial} is not an earlier state of slide {complete}, so nothing was swapped");
    }
    if (a.index <= sc.notes.slide_index) != (b.index <= sc.notes.slide_index) {
        bail!("slides {partial} and {complete} are on either side of what the notes hold, so nothing was swapped");
    }
    if to.exists() || to.parent() != Some(files.slides.as_path()) {
        bail!("slide {complete} cannot go back to {}", to.display());
    }
    let files = files.clone();
    store
        .update(move |sc| {
            if a.index <= sc.notes.slide_index {
                let line = |s: &SlideEntry| embed_line(s, &files.dir, files.notes_dir());
                let made = notesfile::change_embeds(&files, sc, &[(line(&a), Some(line(&b)))], Local::now())?;
                if made.is_empty() {
                    bail!("slide {partial}'s embed is not on a line of its own in the notes, so nothing was swapped");
                }
            }
            std::fs::rename(&from, &to).with_context(|| format!("put back {}", to.display()))?;
            sc.removed_slides.retain(|s| s.index != b.index);
            sc.slides.push(b.clone());
            sc.slides.sort_by_key(|s| s.index);
            sc.pinned_slides.push(b.index);
            slides::remove_in(&files, sc, a.index)?;
            Ok(())
        })
        .await
}

/// Kept automatic captures that are an earlier state of one taken out by hand within a build's time: the partial kept
/// and the finished slide discarded, which `swap` corrects.
pub fn mistakes(files: &LectureFiles, sc: &Sidecar) -> Vec<(u32, u32)> {
    let out_dir = files.slides.join(REMOVED_DIR);
    let taken_out: Vec<(&SlideEntry, Look)> = sc.removed_slides.iter().filter_map(|s| Some((s, Look::load(&out_dir.join(Path::new(&s.file).file_name()?))?))).filter(|(_, l)| !l.photographic()).collect();
    let mut found = Vec::new();
    for kept in sc.slides.iter().filter(|s| s.auto) {
        let Some(look) = Look::load(&files.dir.join(&kept.file)).filter(|l| !l.photographic()) else { continue };
        for (gone, other) in &taken_out {
            if (kept.shown_at - gone.shown_at).num_minutes().abs() <= BUILD_MINUTES && compare::order(&compare::relate(&look, other)) == Order::Earlier {
                found.push((kept.index, gone.index));
            }
        }
    }
    found
}

/// What a finished page let go (spec §7.6).
#[derive(Debug, Default)]
pub struct Retired {
    /// Captures the page holds, and images of slides taken out of the lecture.
    pub images: crate::notes::archive::Cleanup,
    pub recordings: usize,
    pub recording_bytes: u64,
}

/// Spec §7.6: once the study page is finished it is what the lecture keeps, with the notes. The captures it holds are
/// deleted after each is checked against it, then the images of slides taken out of the lecture and any copies of them
/// an earlier version kept in the page, and, when no session is recording (`live` false), the recordings. `store` is
/// the lecture's sidecar, None for a folder that has none.
pub async fn retire(files: &LectureFiles, page: &Path, store: Option<&Store>, live: bool) -> Result<Retired> {
    use crate::notes::archive;
    let html = std::fs::read_to_string(page).with_context(|| format!("read {}", page.display()))?;
    if let Some(lighter) = archive::without_removed(&html) {
        crate::fsutil::write_atomic(page, lighter.as_bytes()).with_context(|| format!("write {}", page.display()))?;
    }
    let mut out = Retired::default();
    if archive::script(&html, "capture-files").is_some() {
        out.images = archive::clear(page)?;
    }
    let removed = archive::clear_removed(&files.dir);
    out.images.files += removed.files;
    out.images.bytes += removed.bytes;
    out.images.kept.extend(removed.kept);
    if let Some(store) = store {
        let files = files.clone();
        let (n, bytes) = store
            .update(move |sc| {
                slides::forget_cleared(&files, sc);
                if live { Ok((0, 0)) } else { crate::session::launch::retire_recordings(&files.dir, sc) }
            })
            .await?;
        (out.recordings, out.recording_bytes) = (n, bytes);
    }
    Ok(out)
}

/// Whether a study page already on disk is finished, so `lecture tidy` may let go of what it replaces (spec §7.6):
/// typeset with every part §6.4 promises, the same rule as after a polish, and holding every slide the notes embed. Says
/// why not otherwise.
pub fn finished(files: &LectureFiles, page: &Path) -> Result<(), String> {
    if !page.exists() {
        return Err("no study page yet".into());
    }
    match crate::notes::page::cached_parts_missing(&files.page_cache()) {
        None => return Err("its typeset parts are not saved, so whether it is complete is not known".into()),
        Some(missing) if !missing.is_empty() => return Err(format!("it lacks {}", missing.join(", "))),
        Some(_) => {}
    }
    let (Ok(html), Ok(doc)) = (std::fs::read_to_string(page), std::fs::read_to_string(&files.notes)) else { return Err("it or the notes cannot be read".into()) };
    let Some(gallery) = crate::notes::archive::script(&html, "slides").and_then(|s| serde_json::from_str::<HashMap<String, String>>(&s).ok()) else { return Err("it has no slide gallery".into()) };
    let held = crate::notes::embeds::embeds_in(&doc).iter().filter_map(|e| e.strip_prefix("![Slide ")?.split(']').next().map(str::to_string)).all(|n| gallery.get(&n).is_some_and(|u| u.starts_with("data:image/")));
    if held { Ok(()) } else { Err("it lacks slides the notes embed".into()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::compare::tests::{bar, slide};
    use crate::capture::detect::{H, W};
    use chrono::TimeZone;
    use image::{GrayImage, Luma};

    fn at(min: i64) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, 5, 12, 0, 0).unwrap() + chrono::Duration::seconds(min * 60)
    }

    fn cap(index: u32, min: i64, g: GrayImage) -> Capture {
        Capture { index, shown_at: at(min), candidate: true, look: Some(Arc::new(Look::of(g))) }
    }

    fn judged(caps: &[Capture]) -> Vec<(u32, Redundant)> {
        judge(caps, &mut Relations::default()).into_iter().map(|v| (v.index, v.why)).collect()
    }

    /// A fixed camera's view with someone standing in a different place.
    fn room(shift: u32) -> GrayImage {
        GrayImage::from_fn(W, H, |x, y| Luma([(60 + x * 90 / W + y * 70 / H + (x * 7 + y * 13) % 11 + if (100..110).contains(&x) && (60..90).contains(&y) { shift } else { 0 }).min(255) as u8]))
    }

    /// Different photographs of one kind: alike overall, different throughout.
    fn scan(k: u32) -> GrayImage {
        GrayImage::from_fn(W, H, |x, y| Luma([(60 + x * 90 / W + y * 70 / H + (x * (7 + 2 * k) + y * 13) % 41) as u8]))
    }

    mod sweeps {
        use super::super::*;

        #[test]
        fn only_a_complete_page_holding_the_notes_slides_is_finished() {
            // F004: a page lacking a part, or one whose parts are unknown, lets nothing go.
            let dir = tempfile::tempdir().unwrap();
            let files = LectureFiles::standard(dir.path(), chrono::NaiveDate::from_ymd_opt(2026, 11, 5).unwrap());
            std::fs::create_dir_all(files.state_dir()).unwrap();
            std::fs::write(&files.notes, "# T\n\nNo slides.\n").unwrap();
            let page = dir.path().join("Page.html");
            std::fs::write(&page, "<script type=\"application/json\" id=\"slides\">{}</script>").unwrap();
            assert!(finished(&files, &page).unwrap_err().contains("not saved"), "no saved parts, even with no slides to hold");
            let parts = |content: &str| serde_json::json!({"source": "k", "fills": {"keystone": "k", "summary": "s", "content": content, "slides": "{}"}, "words": 1, "budget": 1}).to_string();
            std::fs::write(files.page_cache(), parts("<section class=\"part\">x</section>")).unwrap();
            assert!(finished(&files, &page).unwrap_err().contains("lacks"), "parts missing");
            let all = ["widget", "eli5", "everyday", "glossary", "takeaways", "quiz"].iter().map(|c| format!("<p class=\"{c}\"></p>")).collect::<String>();
            std::fs::write(files.page_cache(), parts(&format!("<section class=\"part\">{all}</section>"))).unwrap();
            assert_eq!(finished(&files, &page), Ok(()));
            std::fs::write(&files.notes, "# T\n\n![Slide 3](slides/slide_03_100000.png)\n").unwrap();
            assert!(finished(&files, &page).unwrap_err().contains("slides the notes embed"));
        }
        use crate::capture::compare::tests::slide;
        use crate::session::slides::{register, undo_remove, SlideMeta};
        use chrono::TimeZone;

        struct Folder {
            _dir: tempfile::TempDir,
            files: LectureFiles,
            store: Store,
        }

        fn folder() -> Folder {
            let dir = tempfile::tempdir().unwrap();
            let files = LectureFiles::standard(dir.path(), chrono::NaiveDate::from_ymd_opt(2026, 11, 5).unwrap());
            std::fs::create_dir_all(&files.slides).unwrap();
            std::fs::create_dir_all(files.state_dir()).unwrap();
            let store = Store::offline(Sidecar::default(), files.sidecar());
            Folder { _dir: dir, files, store }
        }

        /// Registers a build of `lines` lines, one capture per line, each a minute apart.
        async fn build(f: &Folder, lines: usize) -> Vec<SlideEntry> {
            let mut out = Vec::new();
            for n in 1..=lines {
                let tmp = f.files.slides.join(format!(".capture-{n}.png.tmp"));
                slide(0, n).save_with_format(&tmp, image::ImageFormat::Png).unwrap();
                let at = Local.with_ymd_and_hms(2026, 11, 5, 12, n as u32, 0).unwrap();
                out.push(register(&f.files, &f.store, &tmp, SlideMeta { shown_at: at, auto: true, uncertain: false }).await.unwrap().unwrap());
            }
            out
        }

        fn indexes(v: &[(SlideEntry, Redundant)]) -> Vec<(u32, Redundant)> {
            v.iter().map(|(s, w)| (s.index, *w)).collect()
        }

        #[tokio::test]
        async fn a_sweep_takes_build_steps_out_and_what_undo_puts_back_stays() {
            let f = folder();
            build(&f, 3).await;
            let gone = sweep(&f.files, &f.store, Scope::Pending, None).await.unwrap();
            assert_eq!(indexes(&gone), [(1, Redundant::EarlierStateOf(3)), (2, Redundant::EarlierStateOf(3))]);
            assert!(f.files.slides.join(REMOVED_DIR).join(Path::new(&gone[0].0.file).file_name().unwrap()).exists());
            let restored = undo_remove(&f.files, &f.store, Arc::new(AtomicBool::new(false))).await.unwrap();
            assert_eq!(restored.index, 2);
            assert!(sweep(&f.files, &f.store, Scope::Pending, None).await.unwrap().is_empty(), "a restored slide is the person's");
            assert_eq!(f.store.read().await.unwrap().pinned_slides, [2]);
        }

        #[tokio::test]
        async fn a_capture_whose_image_changes_is_judged_on_its_new_image() {
            // F005: relations measured on the old image are not reused.
            let f = folder();
            let steps = build(&f, 2).await;
            let sc = f.store.read().await.unwrap();
            let mut t = Tidier::default();
            t.refresh(&f.files, &sc.slides).await;
            let first = judge(&t.captures(&f.files, &sc, Scope::All), &mut t.memo);
            assert_eq!(first, [Verdict { index: 1, why: Redundant::EarlierStateOf(2) }]);
            std::thread::sleep(std::time::Duration::from_millis(20));
            slide(3, 1).save_with_format(f.files.dir.join(&steps[0].file), image::ImageFormat::Png).unwrap();
            t.refresh(&f.files, &sc.slides).await;
            assert!(judge(&t.captures(&f.files, &sc, Scope::All), &mut t.memo).is_empty(), "another title now: apart");
        }

        #[tokio::test]
        async fn slides_named_by_the_person_go_with_their_embeds() {
            let f = folder();
            let steps = build(&f, 3).await;
            let line = |s: &SlideEntry| embed_line(s, &f.files.dir, f.files.notes_dir());
            std::fs::write(&f.files.notes, format!("# T\n\n{}\n\n{}\n", line(&steps[0]), line(&steps[2]))).unwrap();
            let files = f.files.clone();
            f.store.update(move |sc| { notesfile::accept_external_edit(&files, sc)?; sc.notes.slide_index = 3; Ok(()) }).await.unwrap();
            let gone = drop_slides(&f.files, &f.store, &[1, 2, 9]).await.unwrap();
            assert_eq!(gone.iter().map(|s| s.index).collect::<Vec<_>>(), [1], "2 is used but its embed is not in the notes, 9 is not a slide");
            assert_eq!(std::fs::read_to_string(&f.files.notes).unwrap(), format!("# T\n\n{}\n", line(&steps[2])));
        }

        #[tokio::test]
        async fn a_sweep_gives_way_to_a_running_snapshot() {
            let f = folder();
            build(&f, 2).await;
            assert!(sweep(&f.files, &f.store, Scope::Pending, Some(Arc::new(AtomicBool::new(true)))).await.unwrap().is_empty());
            assert_eq!(f.store.read().await.unwrap().slides.len(), 2);
        }

        #[tokio::test]
        async fn after_the_lecture_a_used_step_goes_with_its_embed_and_before_it_stays() {
            let f = folder();
            let steps = build(&f, 3).await;
            let line = |s: &SlideEntry| embed_line(s, &f.files.dir, f.files.notes_dir());
            std::fs::write(&f.files.notes, format!("# T\n\n{}\n\nFirst.\n\n{}\n\n{}\n", line(&steps[0]), line(&steps[1]), line(&steps[2]))).unwrap();
            let files = f.files.clone();
            f.store.update(move |sc| { notesfile::accept_external_edit(&files, sc)?; sc.notes.slide_index = 3; Ok(()) }).await.unwrap();
            assert!(sweep(&f.files, &f.store, Scope::Pending, None).await.unwrap().is_empty(), "the notes hold them");
            let gone = sweep(&f.files, &f.store, Scope::All, None).await.unwrap();
            assert_eq!(indexes(&gone), [(1, Redundant::EarlierStateOf(3)), (2, Redundant::EarlierStateOf(3))]);
            assert_eq!(std::fs::read_to_string(&f.files.notes).unwrap(), format!("# T\n\nFirst.\n\n{}\n", line(&steps[2])));
            assert!(std::fs::read_dir(f.files.state_dir()).unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".md")), "backed up");
        }

        #[tokio::test]
        async fn a_kept_partial_swaps_for_its_discarded_complete_slide_in_place() {
            let f = folder();
            let steps = build(&f, 2).await;
            let line = |s: &SlideEntry| embed_line(s, &f.files.dir, f.files.notes_dir());
            std::fs::write(&f.files.notes, format!("# T\n\n{}\n\nThe slide.\n\n{}\n", line(&steps[0]), line(&steps[1]))).unwrap();
            let files = f.files.clone();
            f.store.update(move |sc| { notesfile::accept_external_edit(&files, sc)?; sc.notes.slide_index = 2; Ok(()) }).await.unwrap();
            // Pruned by hand the wrong way round: the complete step out, its partial kept.
            let files = f.files.clone();
            std::fs::write(&f.files.notes, format!("# T\n\n{}\n\nThe slide.\n", line(&steps[0]))).unwrap();
            f.store.update(move |sc| { notesfile::accept_external_edit(&files, sc)?; slides::remove_in(&files, sc, 2).map(|_| ()) }).await.unwrap();
            assert!(swap(&f.files, &f.store, 2, 1).await.is_err(), "only a partial for its complete slide");
            swap(&f.files, &f.store, 1, 2).await.unwrap();
            let sc = f.store.read().await.unwrap();
            assert_eq!((sc.slides.iter().map(|s| s.index).collect::<Vec<_>>(), sc.pinned_slides.clone()), (vec![2], vec![2]));
            assert_eq!(std::fs::read_to_string(&f.files.notes).unwrap(), format!("# T\n\n{}\n\nThe slide.\n", line(&steps[1])));
            assert!(f.files.dir.join(&steps[1].file).exists() && !f.files.dir.join(&steps[0].file).exists());
        }
    }

    #[test]
    fn a_build_keeps_only_its_finished_slide_and_a_next_slide_stays() {
        let caps = [cap(1, 0, slide(0, 1)), cap(2, 1, slide(0, 2)), cap(3, 2, slide(0, 3)), cap(4, 3, slide(3, 2))];
        assert_eq!(judged(&caps), [(1, Redundant::EarlierStateOf(3)), (2, Redundant::EarlierStateOf(3))]);
    }

    #[test]
    fn a_slide_shown_again_later_keeps_its_first_showing() {
        let caps = [cap(1, 0, slide(0, 3)), cap(2, 5, slide(3, 2)), cap(3, 40, slide(0, 3))];
        assert_eq!(judged(&caps), [(3, Redundant::SameAs(1))]);
    }

    #[test]
    fn an_earlier_state_long_before_the_finished_slide_is_left_alone() {
        let caps = [cap(1, 0, slide(0, 1)), cap(2, 30, slide(0, 3))];
        assert!(judged(&caps).is_empty());
    }

    #[test]
    fn what_must_stay_is_kept_and_still_stands_for_the_others() {
        let mut caps = [cap(1, 0, slide(0, 1)), cap(2, 1, slide(0, 3)), cap(3, 2, slide(0, 3))];
        caps[0].candidate = false;
        caps[2].candidate = false; // a manual capture: the group keeps it, not the first showing
        assert_eq!(judged(&caps), [(2, Redundant::SameAs(3))]);
    }

    #[test]
    fn a_chain_of_near_copies_never_removes_one_its_keeper_does_not_match() {
        // F002: A ≈ B and B ≈ C within the tolerance, A and C twice as far apart.
        let marked = |marks: &[(u32, u32)]| {
            let mut g = slide(0, 2);
            for &(y, x) in marks {
                bar(&mut g, y, x, 20, 6, 20);
            }
            g
        };
        let (p1, p2, q1, q2) = ((90, 30), (90, 120), (110, 30), (110, 120));
        let caps = [cap(1, 0, marked(&[p1, p2])), cap(2, 1, marked(&[p2, q1])), cap(3, 2, marked(&[q1, q2]))];
        let mut memo = Relations::default();
        let (a, b, c) = (&caps[0], &caps[1], &caps[2]);
        assert_eq!((memo.order(a, b), memo.order(b, c), memo.order(a, c)), (Order::Same, Order::Same, Order::Apart), "the shape under test");
        assert_eq!(judged(&caps), [(2, Redundant::SameAs(1))]);
    }

    #[test]
    fn the_same_slide_without_zooms_bar_is_kept_whichever_came_first() {
        use crate::capture::compare::tests::with_bar;
        let caps = [cap(1, 0, with_bar(&slide(0, 3))), cap(2, 1, slide(0, 3))];
        assert_eq!(judged(&caps), [(1, Redundant::SameAs(2))]);
        let caps = [cap(1, 0, slide(0, 3)), cap(2, 1, with_bar(&slide(0, 3)))];
        assert_eq!(judged(&caps), [(2, Redundant::SameAs(1))]);
    }

    #[test]
    fn a_camera_run_is_reported_and_never_removed_by_the_judge() {
        // F003: from saved pictures a camera cannot be told from a photograph slide whose caption changes.
        let caps = [cap(1, 0, slide(0, 3)), cap(2, 1, room(0)), cap(3, 2, slide(3, 3)), cap(4, 50, room(0)), cap(5, 51, room(40)), cap(6, 52, room(80))];
        assert_eq!(camera_runs(&caps), [(4, 6)]);
        assert!(judged(&caps).iter().all(|(i, _)| *i != 5 && *i != 6), "{:?}", judged(&caps));
    }

    #[test]
    fn copies_of_one_photograph_keep_one_and_a_series_of_photographs_stays() {
        // F003: a photograph slide taken again is one slide, not a camera.
        let caps = [cap(1, 0, slide(0, 3)), cap(2, 1, room(0)), cap(3, 30, room(0))];
        assert_eq!(judged(&caps), [(3, Redundant::SameAs(2))]);
        let two = [cap(1, 0, room(0)), cap(2, 1, room(40))];
        assert!(judged(&two).is_empty() && camera_runs(&two).is_empty(), "two photographs are not a camera");
        let series = [cap(1, 0, scan(0)), cap(2, 1, scan(1)), cap(3, 2, scan(2)), cap(4, 3, scan(3))];
        assert!(series.iter().all(|c| c.look.as_ref().unwrap().photographic()));
        assert!(judged(&series).is_empty() && camera_runs(&series).is_empty(), "photographs that change throughout are slides");
    }

    #[test]
    fn a_pale_photograph_is_not_an_earlier_state_of_the_slide_after_it() {
        let photo = GrayImage::from_fn(W, H, |x, y| Luma([(150 + x * 40 / W + (x * 7 + y * 13) % 11) as u8]));
        let caps = [cap(1, 0, photo), cap(2, 1, slide(0, 3))];
        assert!(judged(&caps).is_empty());
    }

    #[test]
    fn an_unreadable_capture_is_never_judged() {
        let mut caps = [cap(1, 0, slide(0, 1)), cap(2, 1, slide(0, 3))];
        caps[0].look = None;
        assert!(judged(&caps).is_empty());
    }

    #[test]
    fn steps_that_are_the_same_picture_follow_their_keeper_into_the_finished_slide() {
        let mut half = slide(0, 2);
        bar(&mut half, 120, 200, 2, 4, 0); // the pointer, on one of two identical steps
        let caps = [cap(1, 0, slide(0, 2)), cap(2, 1, half), cap(3, 2, slide(0, 4))];
        assert_eq!(judged(&caps), [(1, Redundant::EarlierStateOf(3)), (2, Redundant::EarlierStateOf(3))]);
    }
}
