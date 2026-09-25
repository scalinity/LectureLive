//! Slide images for tests: a small chart, so `sips` has real pixels to convert.
use std::path::Path;

pub fn png(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut img = image::RgbImage::from_pixel(320, 180, image::Rgb([255, 255, 255]));
    for x in 20..300u32 {
        let y = 160 - ((x as f32 - 160.0).powi(2) / 180.0) as u32;
        img.put_pixel(x, y.min(179), image::Rgb([20, 20, 20]));
    }
    img.save(path).unwrap();
}
