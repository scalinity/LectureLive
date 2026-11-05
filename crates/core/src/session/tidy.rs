//! Redundant captures (spec §7.7): a build step or a frame of a fade whose finished slide was also taken, the same
//! slide taken again, and a camera taken for a slide. Each is judged from the pictures alone, with no request.
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Local};

use crate::capture::compare::{self, Look, Order, Relation};

/// An earlier state is looked for among captures this close in time: a build is revealed over a few minutes.
const BUILD_MINUTES: i64 = 10;

/// Why a capture is not needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redundant {
    /// A step of a build or a frame of a fade: this slide shows all of it, and more.
    EarlierStateOf(u32),
    /// The same picture as this slide, taken again.
    SameAs(u32),
    /// A camera, not a slide.
    Camera,
}

impl fmt::Display for Redundant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Redundant::EarlierStateOf(n) => write!(f, "an earlier state of slide {n}"),
            Redundant::SameAs(n) => write!(f, "the same as slide {n}"),
            Redundant::Camera => write!(f, "a camera, not a slide"),
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

/// Which candidates are redundant, in index order. A capture is only ever removed in favour of one that stays.
pub fn judge(caps: &[Capture], memo: &mut Relations) -> Vec<Verdict> {
    let mut caps: Vec<&Capture> = caps.iter().collect();
    caps.sort_by_key(|c| c.index);
    let n = caps.len();
    let look = |i: usize| caps[i].look.as_deref();
    let mut why: Vec<Option<Redundant>> = vec![None; n];

    // A camera: a run of photographs of one view. A single photograph between slides is a slide that has one.
    let photo = |i: usize| look(i).is_some_and(Look::photographic);
    let mut in_run = vec![false; n];
    let mut i = 0;
    while i < n {
        let mut j = i + 1;
        if photo(i) {
            while j < n && photo(j) && compare::same_view(look(j - 1).unwrap(), look(j).unwrap()) {
                j += 1;
            }
            if j - i >= 2 {
                for k in i..j {
                    in_run[k] = true;
                    if caps[k].candidate {
                        why[k] = Some(Redundant::Camera);
                    }
                }
            }
        }
        i = j;
    }

    // The same picture, however long apart: each group keeps one, the first shown unless one must stay.
    let pool: Vec<usize> = (0..n).filter(|&i| !in_run[i] && look(i).is_some()).collect();
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
        let keep = members.iter().copied().find(|&i| !caps[i].candidate).unwrap_or(members[0]);
        for &m in members {
            keeper_of.insert(m, keep);
            if m != keep && caps[m].candidate {
                why[m] = Some(Redundant::SameAs(caps[keep].index));
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
                for (&m, &keep) in &keeper_of {
                    if keep == s && m != s && caps[m].candidate {
                        why[m] = Some(to);
                    }
                }
                continue;
            }
        }
        kept.push(s);
    }

    (0..n).filter_map(|i| why[i].map(|why| Verdict { index: caps[i].index, why })).collect()
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

    fn room(shift: u32) -> GrayImage {
        GrayImage::from_fn(W, H, |x, y| Luma([(60 + x * 90 / W + y * 70 / H + (x * 7 + y * 13) % 11 + if (100..130).contains(&x) && y > 40 { shift } else { 0 }).min(255) as u8]))
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
    fn a_camera_run_goes_and_a_lone_photograph_between_slides_stays() {
        let caps = [cap(1, 0, slide(0, 3)), cap(2, 1, room(0)), cap(3, 2, slide(3, 3)), cap(4, 50, room(0)), cap(5, 51, room(40)), cap(6, 52, room(10))];
        assert_eq!(judged(&caps), [(4, Redundant::Camera), (5, Redundant::Camera), (6, Redundant::Camera)]);
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
