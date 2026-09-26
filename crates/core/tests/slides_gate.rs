//! The M5 detector gate (docs/milestones.md): ≥95% recall of stable states visible ≥3 s and ≤1 false
//! capture per 10 minutes, on a synthetic lecture here and on recorded Zoom fixtures.
mod support;

use std::path::Path;

use lecturelive_core::capture::detect::Thresholds;
use support::frames::{align, evaluate, load_samples, schedule, synthetic_lecture, write_states, Sample, State};

#[test]
fn alignment_finds_where_the_deck_started_in_a_recording() {
    let (samples, states) = synthetic_lecture();
    // As a recording would hold it: the deck started 37 s after the recording did.
    let shifted: Vec<Sample> = samples.iter().map(|s| Sample { t: s.t + 37_000, frame: s.frame.clone() }).collect();
    let plan: Vec<(State, bool)> = states.iter().map(|s| (s.clone(), s.id.ends_with("b0"))).collect();
    let (offset, residuals) = align(&shifted, &plan);
    let mut r: Vec<i64> = residuals.iter().map(|d| d.abs()).collect();
    r.sort();
    assert!((offset as i64 - 37_000).abs() <= 1_000, "offset {offset}");
    assert!(r[r.len() / 2] < 1_000, "median residual {} of {residuals:?}", r[r.len() / 2]);
}

#[test]
fn synthetic_lecture_meets_the_gate() {
    let (samples, states) = synthetic_lecture();
    let m = evaluate(samples, states, Thresholds::default());
    println!("synthetic: {} of {} states recalled ({:.1}%), {} false in {:.1} min; missed {:?}; false {:?}", m.recalled, m.counted, m.recall() * 100.0, m.false_captures.len(), m.minutes, m.missed, m.false_captures);
    assert!(m.counted >= 40, "enough states to measure: {}", m.counted);
    assert!(m.recall() >= 0.95, "recall {:.3}", m.recall());
    assert!(m.false_per_10_min() <= 1.0, "false {:?}", m.false_captures);
    // The measure can fail: a blind detector misses states, and one without masks captures animations.
    let blind = evaluate(samples, states, Thresholds { change: 0.5, ..Thresholds::default() });
    assert!(blind.recall() < 0.95, "a blind detector still recalled {:.3}", blind.recall());
    let unmasked = evaluate(samples, states, Thresholds { animated: 1000, ..Thresholds::default() });
    assert!(unmasked.false_captures.len() > m.false_captures.len(), "the masks removed no false capture: {:?}", unmasked.false_captures);
}

/// Annotates a recording of the synthetic deck (Task 12):
/// M5_RECORDING=<folder> M5_SCHEDULE=apps/desktop/src/lib/deck.json cargo test -p lecturelive-core --test slides_gate annotate -- --ignored --nocapture
#[test]
#[ignore]
fn annotate() {
    let rec = std::env::var("M5_RECORDING").expect("M5_RECORDING: a recorded folder");
    let deck = std::env::var("M5_SCHEDULE").expect("M5_SCHEDULE: the deck's deck.json");
    let samples = load_samples(Path::new(&rec));
    let plan = schedule(Path::new(&deck));
    let (offset, residuals) = align(&samples, &plan);
    let mut sorted = residuals.clone();
    sorted.sort();
    println!("offset {offset} ms; slide-boundary residuals (ms) {residuals:?}; median {}", sorted.get(sorted.len() / 2).copied().unwrap_or(0));
    write_states(Path::new(&rec), &plan, offset, samples.last().map_or(0, |s| s.t) + 1000);
}

#[test]
/// The committed fixtures are the synthetic deck recorded from a window by the live worker; the recorded
/// Zoom lecture is measured by `measure_recording` on this machine only, since its content is not synthetic.
fn recorded_fixtures_meet_the_gate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/slides");
    let dirs: Vec<_> = std::fs::read_dir(&root).map(|d| d.flatten().map(|e| e.path()).filter(|p| p.join("states.json").exists()).collect()).unwrap_or_default();
    assert!(!dirs.is_empty(), "no recorded fixture under {}", root.display());
    for dir in dirs {
        let (samples, states) = support::frames::load(&dir);
        let m = evaluate(&samples, &states, Thresholds::default());
        println!("{}: {} of {} states recalled ({:.1}%), {} false in {:.1} min ({:.2} per 10 min); missed {:?}; false {:?}", dir.display(), m.recalled, m.counted, m.recall() * 100.0, m.false_captures.len(), m.minutes, m.false_per_10_min(), m.missed, m.false_captures);
        assert!(m.recall() >= 0.95 && m.false_per_10_min() <= 1.0);
    }
}

/// Every `M5_EVERY`th distinct frame of a recording on one sheet, to look at before committing it:
/// M5_RECORDING=<folder> M5_SHEET=<png outside the repository> cargo test -p lecturelive-core --test slides_gate contact_sheet -- --ignored
#[test]
#[ignore]
fn contact_sheet() {
    let rec = std::env::var("M5_RECORDING").expect("M5_RECORDING");
    let out = std::env::var("M5_SHEET").expect("M5_SHEET");
    let every: usize = std::env::var("M5_EVERY").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
    let mut frames: Vec<_> = std::fs::read_dir(Path::new(&rec).join("frames")).unwrap().flatten().map(|e| e.path()).collect();
    frames.sort();
    let picked: Vec<_> = frames.iter().step_by(every).collect();
    let cols = 6u32;
    let rows = (picked.len() as u32).div_ceil(cols);
    let mut sheet = image::GrayImage::from_pixel(cols * 260, rows * 148, image::Luma([0]));
    for (i, p) in picked.iter().enumerate() {
        let f = image::open(p).unwrap().to_luma8();
        image::imageops::overlay(&mut sheet, &f, (i as u32 % cols * 260) as i64, (i as u32 / cols * 148) as i64);
    }
    sheet.save(&out).unwrap();
    println!("{} of {} frames on {out}", picked.len(), frames.len());
}

/// Proposes the stable states of a recording with no schedule, for checking by eye:
/// M5_RECORDING=<folder> M5_SHEET=<png outside the repository> cargo test -p lecturelive-core --test slides_gate propose -- --ignored --nocapture
/// Writes `proposed.json` beside the recording and a sheet of each run's frame, in order, six to a row.
#[test]
#[ignore]
fn propose_states() {
    let rec = std::env::var("M5_RECORDING").expect("M5_RECORDING");
    let samples = load_samples(Path::new(&rec));
    // M5_IGNORE: tiles that are not the slide, as row:col (a camera thumbnail drawn over it).
    let mut ignore = vec![false; 144];
    for rc in std::env::var("M5_IGNORE").unwrap_or_default().split(',').filter(|v| !v.is_empty()) {
        let (r, c) = rc.split_once(':').expect("row:col");
        ignore[r.parse::<usize>().unwrap() * 16 + c.parse::<usize>().unwrap()] = true;
    }
    let (runs, busy) = support::frames::still_runs(&samples, 0.02, &ignore, 0.06);
    println!("{} busy tiles left out (moving in most samples)", busy.iter().filter(|&&b| b).count());
    for (i, r) in runs.iter().enumerate() {
        println!("r{i:03} {:>6.1}–{:>6.1} s ({:>5.1} s)  vs previous: max {:.3}, {:>3} tiles", r.from as f64 / 1000.0, r.to as f64 / 1000.0, (r.to - r.from) as f64 / 1000.0, r.diff, r.tiles);
    }
    std::fs::write(Path::new(&rec).join("proposed.json"), serde_json::to_vec_pretty(&runs).unwrap()).unwrap();
    if let Ok(out) = std::env::var("M5_SHEET") {
        let cols = 6u32;
        let rows = (runs.len() as u32).div_ceil(cols);
        let mut sheet = image::GrayImage::from_pixel(cols * 260, rows * 148, image::Luma([0]));
        for (i, r) in runs.iter().enumerate() {
            image::imageops::overlay(&mut sheet, samples[r.sample].frame.as_ref().unwrap(), (i as u32 % cols * 260) as i64, (i as u32 / cols * 148) as i64);
        }
        sheet.save(out).unwrap();
    }
}

/// Writes `states.json` for a recording with no schedule, from `proposed.json` and the runs checked by eye:
/// M5_MERGE lists the runs that show the same slide as the run before them (a pointer moved, a clock ticked),
/// M5_FROM drops what came before (ms: the player being set up).
/// M5_RECORDING=<folder> M5_MERGE=26,27,28 M5_FROM=33000 cargo test -p lecturelive-core --test slides_gate annotate_recording -- --ignored --nocapture
#[test]
#[ignore]
fn annotate_recording() {
    let rec = std::env::var("M5_RECORDING").expect("M5_RECORDING");
    let from: u64 = std::env::var("M5_FROM").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let merge: Vec<usize> = std::env::var("M5_MERGE").unwrap_or_default().split(',').filter_map(|v| v.trim().parse().ok()).collect();
    let runs: Vec<support::frames::Run> = serde_json::from_slice(&std::fs::read(Path::new(&rec).join("proposed.json")).unwrap()).unwrap();
    let mut states: Vec<State> = Vec::new();
    for (i, r) in runs.iter().enumerate() {
        match states.last_mut() {
            Some(last) if merge.contains(&i) => last.to = r.to,
            _ => states.push(State { id: format!("r{i:03}"), from: r.from, to: r.to, kind: "slide".into() }),
        }
    }
    states.retain(|s| s.from >= from);
    let counted = states.iter().filter(|s| s.to - s.from >= 3000).count();
    println!("{} states, {counted} visible for 3 s or more", states.len());
    std::fs::write(Path::new(&rec).join("states.json"), serde_json::to_vec_pretty(&serde_json::json!({ "states": states })).unwrap()).unwrap();
}

/// Replays a recording through the detector at spec §7.2's values and across a calibration grid:
/// M5_RECORDING=<folder with states.json> M5_FROM=<ms> cargo test -p lecturelive-core --test slides_gate measure_recording -- --ignored --nocapture
#[test]
#[ignore]
fn measure_recording() {
    let rec = std::env::var("M5_RECORDING").expect("M5_RECORDING");
    let from: u64 = std::env::var("M5_FROM").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let (samples, states) = support::frames::load(Path::new(&rec));
    let samples: Vec<Sample> = samples.into_iter().filter(|s| s.t >= from).collect();
    let m = evaluate(&samples, &states, Thresholds::default());
    println!("spec values: {} of {} recalled ({:.1}%), {} false in {:.1} min ({:.2} per 10 min)\n  missed {:?}\n  false {:?}", m.recalled, m.counted, m.recall() * 100.0, m.false_captures.len(), m.minutes, m.false_per_10_min(), m.missed, m.false_captures);
    println!("change settle animated: recall false/10min");
    let grid = |samples: &[Sample], label: &str| {
        for change in [0.03f32, 0.04, 0.05, 0.06, 0.08] {
            for settle in [0.02f32, 0.03, 0.04] {
                for animated in [2u32, 3, 4] {
                    let m = evaluate(samples, &states, Thresholds { change, settle, animated, ..Thresholds::default() });
                    println!("{label} {change:.2} {settle:.2} {animated}: {:.3} {:.2}", m.recall(), m.false_per_10_min());
                }
            }
        }
    };
    grid(&samples, "as drawn");
    // M5_LEAVE_OUT (x,y,w,h in fractions of the region): the same frames with that part left out, through
    // the detector's own blanking, as the picker's "leave out a part" does.
    if let Ok(part) = std::env::var("M5_LEAVE_OUT") {
        let v: Vec<f64> = part.split(',').map(|x| x.trim().parse().expect("x,y,w,h")).collect();
        let part = lecturelive_core::capture::detect::Region { x: v[0], y: v[1], w: v[2], h: v[3] };
        let left_out: Vec<Sample> = samples
            .iter()
            .map(|s| Sample {
                t: s.t,
                frame: s.frame.as_ref().map(|f| {
                    let mut f = f.clone();
                    lecturelive_core::capture::detect::leave_out(&mut f, &[part]);
                    f
                }),
            })
            .collect();
        let m = evaluate(&left_out, &states, Thresholds::default());
        println!("left out, spec values: {} of {} recalled ({:.1}%), {} false in {:.1} min ({:.2} per 10 min); missed {:?}; false {:?}", m.recalled, m.counted, m.recall() * 100.0, m.false_captures.len(), m.minutes, m.false_per_10_min(), m.missed, m.false_captures);
        grid(&left_out, "left out");
    }
}

/// How often each tile moves in a recording (share of samples), as a 16×9 grid:
/// M5_RECORDING=<folder> cargo test -p lecturelive-core --test slides_gate tile_activity -- --ignored --nocapture
#[test]
#[ignore]
fn tile_activity() {
    let rec = std::env::var("M5_RECORDING").expect("M5_RECORDING");
    let samples = load_samples(Path::new(&rec));
    let frames: Vec<&image::GrayImage> = samples.iter().filter_map(|s| s.frame.as_ref()).collect();
    let mut moved = vec![0usize; 144];
    for w in frames.windows(2) {
        for (i, v) in support::frames::tile_diffs(w[0], w[1]).into_iter().enumerate() {
            if v >= 0.02 {
                moved[i] += 1;
            }
        }
    }
    for row in 0..9 {
        println!("{}", (0..16).map(|c| format!("{:3}", moved[row * 16 + c] * 100 / frames.len().max(1))).collect::<Vec<_>>().join(" "));
    }
}

/// Chosen runs' frames side by side, to decide by eye whether neighbours show the same slide:
/// M5_RECORDING=<folder> M5_RUNS=32,33,34,35 M5_SHEET=<png outside the repository> cargo test -p lecturelive-core --test slides_gate runs_sheet -- --ignored
#[test]
#[ignore]
fn runs_sheet() {
    let rec = std::env::var("M5_RECORDING").expect("M5_RECORDING");
    let out = std::env::var("M5_SHEET").expect("M5_SHEET");
    let ids: Vec<usize> = std::env::var("M5_RUNS").expect("M5_RUNS").split(',').filter_map(|v| v.trim().parse().ok()).collect();
    let samples = load_samples(Path::new(&rec));
    let runs: Vec<support::frames::Run> = serde_json::from_slice(&std::fs::read(Path::new(&rec).join("proposed.json")).unwrap()).unwrap();
    let cols = 2u32;
    let rows = (ids.len() as u32).div_ceil(cols);
    let mut sheet = image::GrayImage::from_pixel(cols * 520, rows * 292, image::Luma([0]));
    for (k, &i) in ids.iter().enumerate() {
        let f = image::DynamicImage::ImageLuma8(samples[runs[i].sample].frame.clone().unwrap()).resize_exact(512, 288, image::imageops::FilterType::Nearest).into_luma8();
        image::imageops::overlay(&mut sheet, &f, (k as u32 % cols * 520) as i64, (k as u32 / cols * 292) as i64);
    }
    sheet.save(out).unwrap();
}

/// Each capture of a recording with the tiles it changed against the capture before (row:col over 0.08):
/// M5_RECORDING=<folder> M5_FROM=<ms> cargo test -p lecturelive-core --test slides_gate explain_captures -- --ignored --nocapture
#[test]
#[ignore]
fn explain_captures() {
    let rec = std::env::var("M5_RECORDING").expect("M5_RECORDING");
    let from: u64 = std::env::var("M5_FROM").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let (samples, states) = support::frames::load(Path::new(&rec));
    let samples: Vec<Sample> = samples.into_iter().filter(|s| s.t >= from).collect();
    let mut d = lecturelive_core::capture::detect::Detector::<usize>::new(Thresholds::default());
    let mut last: Option<usize> = None;
    for (i, s) in samples.iter().enumerate() {
        let Some(f) = &s.frame else { continue };
        if d.observe(i, f.clone()).is_some() {
            let state = states.iter().find(|st| st.from <= s.t && s.t < st.to).map_or("-".to_string(), |st| st.id.clone());
            let tiles: Vec<String> = match last {
                Some(k) => support::frames::tile_diffs(samples[k].frame.as_ref().unwrap(), f).iter().enumerate().filter(|(_, v)| **v > 0.08).map(|(t, v)| format!("{}:{}={:.2}", t / 16, t % 16, v)).collect(),
                None => vec![],
            };
            println!("{:>8} {state:>5} {} tiles {:?}", s.t, tiles.len(), tiles.iter().take(8).collect::<Vec<_>>());
            last = Some(i);
        }
    }
}
