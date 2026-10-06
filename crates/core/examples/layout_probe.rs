//! Prints where `capture::layout` finds the shared content in whole-window copies of a verification recording.
//!
//!   cargo run -p lecturelive-core --example layout_probe -- <recording dir> <window index>...
use image::imageops::FilterType;
use lecturelive_core::capture::layout::content_of;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("<recording dir>"));
    for index in args {
        let n: u32 = index.parse().expect("a window index");
        let Ok(img) = image::open(dir.join(format!("window/{n:05}.png"))) else {
            println!("{n}\tno such frame");
            continue;
        };
        let g = img.to_luma8();
        let h = (g.height() as f64 * 160.0 / g.width() as f64).round() as u32;
        let small = image::DynamicImage::ImageLuma8(g).resize_exact(160, h, FilterType::Triangle).into_luma8();
        match content_of(&small) {
            Some(r) => println!("{n}\tx {:.3}  y {:.3}  w {:.3}  h {:.3}", r.x, r.y, r.w, r.h),
            None => println!("{n}\tno content found"),
        }
    }
}
