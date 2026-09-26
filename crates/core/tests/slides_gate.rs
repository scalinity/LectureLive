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
