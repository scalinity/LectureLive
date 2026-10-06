//! Prints the scene measures of every detector frame in a verification recording (LECTURELIVE_RECORD), with
//! when each first appeared, to calibrate `capture::scene` against a class whose frames are known.
//!
//!   cargo run -p lecturelive-core --example scene_stats -- <recording dir>
use lecturelive_core::capture::scene::{classify, features};

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("<recording dir>"));
    let text = std::fs::read_to_string(dir.join("samples.jsonl")).expect("samples.jsonl");
    // When each frame first appeared, and for how many samples.
    let mut seen: std::collections::BTreeMap<u64, (String, u32)> = Default::default();
    for v in text.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()) {
        let (Some(at), Some(f)) = (v.get("at").and_then(|a| a.as_str()), v.get("frame").and_then(|f| f.as_u64())) else { continue };
        seen.entry(f).or_insert_with(|| (at[11..19].to_string(), 0)).1 += 1;
    }
    println!("frame\tfirst\tsecs\tmean\tdominant\tflat\tsoft\thard\tscene\tdelta");
    let mut before: Option<image::GrayImage> = None;
    for (frame, (at, secs)) in seen {
        let Ok(img) = image::open(dir.join(format!("frames/{frame:05}.png"))) else { continue };
        let g = img.to_luma8();
        let f = features(&g);
        // The mean change from the frame before it, 0–1: a camera shimmers, a held page does not.
        let delta = before.as_ref().filter(|b| b.dimensions() == g.dimensions()).map(|b| g.as_raw().iter().zip(b.as_raw()).map(|(x, y)| x.abs_diff(*y) as u64).sum::<u64>() as f32 / g.as_raw().len() as f32 / 255.0).unwrap_or(0.0);
        println!("{frame}\t{at}\t{secs}\t{:.0}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:?}\t{delta:.4}", f.mean, f.dominant, f.flat, f.soft, f.hard, classify(&f, true));
        before = Some(g);
    }
}
