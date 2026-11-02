//! Writes a folder of generated sample images for trying out the app:
//!
//! ```text
//! cargo run --release --example make_samples -- [DIR]    # default: target/samples
//! cargo run --release -- target/samples
//! ```
//!
//! The images are regenerated on every run, so optimizing them in place is harmless.

#[path = "../tests/common/mod.rs"]
mod common;

use std::{fs, io, path::PathBuf};

fn main() -> io::Result<()> {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "target/samples".to_owned()),
    );
    fs::create_dir_all(&dir)?;

    let already_optimized = oxipng::optimize_from_memory(
        &common::few_color_rgba_png(32, 32, 4),
        &oxipng::Options::from_preset(6),
    )
    .map_err(io::Error::other)?;

    let samples: [(&str, Vec<u8>); 12] = [
        ("logo.png", common::few_color_rgba_png(512, 512, 40)),
        ("badge-interlaced.png", common::interlaced_png(512, 512, 40)),
        ("screenshot.png", common::noisy_rgb_png(1600, 1000)),
        ("icon-32.png", already_optimized),
        ("hero-banner.png", common::noisy_rgb_png(1920, 1080)),
        ("diagram.png", common::few_color_rgba_png(1400, 900, 12)),
        ("scan-0042.png", common::noisy_rgb_png(2400, 1600)),
        ("thumbnail.png", common::noisy_rgb_png(320, 200)),
        ("sprite.apng", common::apng(96, 64, 8)),
        ("spinner.gif", common::moving_square_gif(320, 120, 24)),
        ("gradient.webp", common::gradient_webp(800, 500)),
        ("notes.jpg", b"\xff\xd8\xff\xe0 not really a JPEG".to_vec()),
    ];
    for (name, bytes) in samples {
        fs::write(dir.join(name), bytes)?;
    }
    println!("Wrote samples to {}", dir.display());
    Ok(())
}
