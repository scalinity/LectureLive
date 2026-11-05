//! Scores the redundant-capture judge (spec §7.7) against a lecture the person pruned by hand: the sidecar's `slides`
//! are what they kept, its `removed_slides` what they discarded. Every capture is judged as if none had been removed.
//! Prints numbers only; `--save DIR` also writes each capture's 256×144 grayscale and the labels, for calibrating
//! again after the originals are gone. Lecture images are never committed.
//!
//!   cargo run --release -p lecturelive-core --example tidy_score -- <sidecar.v2.json> <slides dir> [--save DIR]
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lecturelive_core::capture::compare::{self, Look, Order};
use lecturelive_core::session::sidecar::{Sidecar, SlideEntry};
use lecturelive_core::session::tidy::{judge, Capture, Redundant, Relations};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(args.len() >= 2, "usage: tidy_score <sidecar.v2.json> <slides dir> [--save DIR]");
    let sc = Sidecar::load(Path::new(&args[0]))?.ok_or_else(|| anyhow::anyhow!("no sidecar at {}", args[0]))?;
    let slides = PathBuf::from(&args[1]);
    let save = args.iter().position(|a| a == "--save").and_then(|i| args.get(i + 1)).map(PathBuf::from);
    let why: Option<(u32, u32)> = args.iter().position(|a| a == "--why").and_then(|i| Some((args.get(i + 1)?.parse().ok()?, args.get(i + 2)?.parse().ok()?)));

    let mut entries: Vec<(SlideEntry, bool)> = sc.slides.iter().map(|s| (s.clone(), true)).chain(sc.removed_slides.iter().map(|s| (s.clone(), false))).collect();
    entries.sort_by_key(|(s, _)| s.index);
    let file_of = |s: &SlideEntry| {
        let name = Path::new(&s.file).file_name().unwrap().to_owned();
        [slides.join(&name), slides.join("removed").join(&name)].into_iter().find(|p| p.exists())
    };
    let looks: Vec<Option<Arc<Look>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = entries.iter().map(|(s, _)| {
            let f = file_of(s);
            scope.spawn(move || f.and_then(|f| Look::load(&f)).map(Arc::new))
        }).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let caps: Vec<Capture> = entries.iter().zip(&looks).map(|((s, _), l)| Capture { index: s.index, shown_at: s.shown_at, candidate: s.auto, look: l.clone() }).collect();
    let kept: HashMap<u32, bool> = entries.iter().map(|(s, k)| (s.index, *k)).collect();
    let by_index: HashMap<u32, &Capture> = caps.iter().map(|c| (c.index, c)).collect();

    if let Some((a, b)) = why {
        let (la, lb) = (by_index[&a].look.as_deref().unwrap(), by_index[&b].look.as_deref().unwrap());
        let r = compare::relate(la, lb);
        println!("{a} against {b}: lost {:.4} added {:.4}; {a}: soft {:.2} dominant {:.2} photographic {} bar {:?}; {b}: soft {:.2} dominant {:.2} photographic {} bar {:?}", r.lost, r.added, la.features.soft, la.features.dominant, la.photographic(), la.bar, lb.features.soft, lb.features.dominant, lb.photographic(), lb.bar);
        print!("{}", compare::map(la, lb));
        return Ok(());
    }
    let started = std::time::Instant::now();
    let verdicts = judge(&caps, &mut Relations::default());
    let took = started.elapsed();
    let removed: HashMap<u32, Redundant> = verdicts.iter().map(|v| (v.index, v.why)).collect();

    let unread = caps.iter().filter(|c| c.look.is_none()).count();
    let count = |keep: bool, rem: bool| entries.iter().filter(|(s, k)| *k == keep && removed.contains_key(&s.index) == rem).count();
    println!("captures {} (kept by hand {}, discarded by hand {}), unreadable {unread}, judged in {:.2?}", entries.len(), sc.slides.len(), sc.removed_slides.len(), took);
    println!("                 judge removes   judge keeps");
    println!("  discarded      {:>13}   {:>11}", count(false, true), count(false, false));
    println!("  kept           {:>13}   {:>11}", count(true, true), count(true, false));
    let mut kinds: HashMap<&str, usize> = HashMap::new();
    for why in removed.values() {
        *kinds.entry(match why { Redundant::EarlierStateOf(_) => "earlier state", Redundant::SameAs(_) => "same", Redundant::Camera => "camera" }).or_default() += 1;
    }
    println!("  by reason: {kinds:?}");

    let rel = |a: u32, b: u32| {
        let (la, lb) = (by_index[&a].look.as_deref().unwrap(), by_index[&b].look.as_deref().unwrap());
        let r = compare::relate(la, lb);
        format!("lost {:.4} added {:.4} ink {}→{}", r.lost, r.added, la.ink, lb.ink)
    };
    println!("\nremovals (index, hand label, reason, against its keeper):");
    for v in &verdicts {
        let label = if kept[&v.index] { "KEPT by hand" } else { "discarded" };
        let detail = match v.why {
            Redundant::EarlierStateOf(k) | Redundant::SameAs(k) => format!("{}{}", rel(v.index, k), if kept.get(&k) == Some(&false) { "  (keeper discarded by hand)" } else { "" }),
            Redundant::Camera => by_index[&v.index].look.as_deref().map(|l| format!("soft {:.2} dominant {:.2}", l.features.soft, l.features.dominant)).unwrap_or_default(),
        };
        println!("  {:>4} {:<12} {:<26} {detail}", v.index, label, v.why.to_string());
    }

    println!("\ndiscarded by hand, kept by the judge (neighbours within 6):");
    let order_of = |i: usize, j: usize| match (caps[i].look.as_deref(), caps[j].look.as_deref()) {
        (Some(a), Some(b)) if a.photographic() || b.photographic() => compare::order_photographs(a, b),
        (Some(a), Some(b)) => compare::order(&compare::relate(a, b)),
        _ => Order::Apart,
    };
    for (i, (s, k)) in entries.iter().enumerate() {
        if *k || removed.contains_key(&s.index) {
            continue;
        }
        let near: Vec<String> = (i.saturating_sub(6)..(i + 7).min(entries.len())).filter(|&j| j != i).map(|j| {
            let r = caps[i].look.as_deref().zip(caps[j].look.as_deref()).map(|(a, b)| compare::relate(a, b));
            format!("{}{}:{:?}{}", entries[j].0.index, if entries[j].1 { "K" } else { "x" }, order_of(i, j), r.map(|r| format!("({:.4}/{:.4})", r.lost, r.added)).unwrap_or_default())
        }).collect();
        println!("  {:>4} {}", s.index, near.join(" "));
    }

    println!("\nkept by hand but an earlier state of one discarded by hand:");
    for (i, (s, k)) in entries.iter().enumerate() {
        if !*k {
            continue;
        }
        for (j, (t, tk)) in entries.iter().enumerate() {
            if !*tk && (s.shown_at - t.shown_at).num_minutes().abs() <= 10 && order_of(i, j) == Order::Earlier {
                println!("  {} ⊑ {}  ({})", s.index, t.index, rel(s.index, t.index));
            }
        }
    }

    if let Some(dir) = save {
        std::fs::create_dir_all(&dir)?;
        let mut labels = serde_json::Map::new();
        for ((s, k), l) in entries.iter().zip(&looks) {
            if let Some(l) = l {
                l.gray().save(dir.join(format!("{:03}.png", s.index)))?;
            }
            labels.insert(s.index.to_string(), serde_json::json!({ "kept": k, "shown_at": s.shown_at, "auto": s.auto }));
        }
        std::fs::write(dir.join("labels.json"), serde_json::to_string_pretty(&labels)?)?;
        println!("\nsaved {} grayscale captures and labels to {}", looks.iter().flatten().count(), dir.display());
    }
    Ok(())
}
