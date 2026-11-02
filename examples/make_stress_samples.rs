//! Writes large, long animations for checking conversion time and memory:
//!
//! ```text
//! cargo run --release --example make_stress_samples -- [DIR] [--long]    # default: target/stress
//! cargo run --release --example convert_probe -- target/stress/* --optimize
//! ```
//!
//! - `screencast.gif`: 1280×720, 300 frames, each stored in full (as simple encoders do), with
//!   only a small moving box and a ticking counter changing; 8 colors
//! - `video.gif`: 960×540, 200 frames where every pixel changes, each frame with its own
//!   palette, so the animation as a whole has thousands of colors
//! - `scrolling.gif`: 1280×720, 200 frames of a 64-color pattern scrolling, so every pixel
//!   changes but a palette still fits
//! - `slideshow.webp`: 1280×720 lossless, 120 full frames: a gradient with a moving circle
//! - with `--long`, also `long-video.gif`: like `video.gif` at 1920×1080 with 300 frames
//!   (about 300 MB, and 2.5 GB of changed pixels)

#![allow(
    clippy::cast_possible_truncation,
    reason = "pixel patterns wrap on purpose"
)]

use std::{borrow::Cow, fs, io, path::PathBuf};

/// A file name and the function that makes its contents.
type Sample = (&'static str, fn() -> Vec<u8>);

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let long = args.iter().any(|a| a == "--long");
    let dir = PathBuf::from(
        args.iter()
            .find(|a| !a.starts_with("--"))
            .map_or("target/stress", String::as_str),
    );
    fs::create_dir_all(&dir)?;
    let mut samples: Vec<Sample> = vec![
        ("screencast.gif", || screencast_gif(1280, 720, 300)),
        ("video.gif", || video_gif(960, 540, 200)),
        ("scrolling.gif", || scrolling_gif(1280, 720, 200)),
        ("slideshow.webp", || slideshow_webp(1280, 720, 120)),
    ];
    if long {
        samples.push(("long-video.gif", || video_gif(1920, 1080, 300)));
    }
    for (name, make) in samples {
        let bytes = make();
        println!("{name}: {} bytes", bytes.len());
        fs::write(dir.join(name), bytes)?;
    }
    Ok(())
}

fn screencast_gif(width: u16, height: u16, frames: u16) -> Vec<u8> {
    let palette: Vec<u8> = [
        [245, 245, 245],
        [220, 224, 230],
        [60, 64, 72],
        [30, 120, 220],
        [230, 80, 60],
        [40, 170, 90],
        [250, 200, 40],
        [0, 0, 0],
    ]
    .concat();
    let (w, h) = (usize::from(width), usize::from(height));
    let mut out = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut out, width, height, &palette).expect("encoder");
        encoder.set_repeat(gif::Repeat::Infinite).expect("repeat");
        for t in 0..usize::from(frames) {
            // A window with a title bar and text-like stripes.
            let mut pixels: Vec<u8> = (0..w * h)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    match (x, y) {
                        (_, 0..40) => 2,
                        (40..1240, 80..680) if y % 24 < 12 && (x / 7) % 9 != 0 => 1,
                        _ => 0,
                    }
                })
                .collect();
            // A box moving across, and a counter that changes every 10 frames.
            let left = 40 + (t * 4) % (w - 120);
            for y in 300..364 {
                pixels[y * w + left..y * w + left + 64].fill(3 + (t / 50 % 3) as u8);
            }
            for y in 8..32 {
                for x in 1100..1240 {
                    if (x / 6 + t / 10) % 3 == 0 {
                        pixels[y * w + x] = 6;
                    }
                }
            }
            let frame = gif::Frame {
                width,
                height,
                delay: 3,
                buffer: Cow::Owned(pixels),
                ..gif::Frame::default()
            };
            encoder.write_frame(&frame).expect("frame");
        }
    }
    out
}

fn video_gif(width: u16, height: u16, frames: u16) -> Vec<u8> {
    let (w, h) = (usize::from(width), usize::from(height));
    let mut out = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut out, width, height, &[]).expect("encoder");
        encoder.set_repeat(gif::Repeat::Infinite).expect("repeat");
        for t in 0..usize::from(frames) {
            // A palette whose hues drift every frame.
            let palette: Vec<u8> = (0..256)
                .flat_map(|i| {
                    let v = (i + t * 3) as u8;
                    [v, v.wrapping_mul(3).wrapping_add(40), 255 - v]
                })
                .collect();
            let pixels: Vec<u8> = (0..w * h)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    ((x / 3 + t * 2) ^ (y / 3 + t)) as u8
                })
                .collect();
            let frame = gif::Frame {
                width,
                height,
                delay: 4,
                palette: Some(palette),
                buffer: Cow::Owned(pixels),
                ..gif::Frame::default()
            };
            encoder.write_frame(&frame).expect("frame");
        }
    }
    out
}

fn scrolling_gif(width: u16, height: u16, frames: u16) -> Vec<u8> {
    let palette: Vec<u8> = (0..64_u8)
        .flat_map(|i| [i * 4, 255 - i * 3, (i * 7) % 255])
        .collect();
    let (w, h) = (usize::from(width), usize::from(height));
    let mut out = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut out, width, height, &palette).expect("encoder");
        encoder.set_repeat(gif::Repeat::Infinite).expect("repeat");
        for t in 0..usize::from(frames) {
            let pixels: Vec<u8> = (0..w * h)
                .map(|i| {
                    let (x, y) = (i % w + t * 5, i / w);
                    ((x / 16 + y / 16) % 64) as u8
                })
                .collect();
            let frame = gif::Frame {
                width,
                height,
                delay: 4,
                buffer: Cow::Owned(pixels),
                ..gif::Frame::default()
            };
            encoder.write_frame(&frame).expect("frame");
        }
    }
    out
}

fn slideshow_webp(width: u32, height: u32, frames: u32) -> Vec<u8> {
    fn chunk(name: [u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = name.to_vec();
        out.extend_from_slice(&u32::try_from(data.len()).expect("small").to_le_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
        out
    }
    fn u24(value: u32) -> [u8; 3] {
        let [a, b, c, _] = value.to_le_bytes();
        [a, b, c]
    }

    let mut vp8x = vec![0x02, 0, 0, 0]; // animation, no alpha
    vp8x.extend(u24(width - 1));
    vp8x.extend(u24(height - 1));
    let mut body = b"WEBP".to_vec();
    body.extend(chunk(*b"VP8X", &vp8x));
    body.extend(chunk(*b"ANIM", &[0, 0, 0, 0, 0, 0]));
    for t in 0..frames {
        let (cx, cy) = (100 + t * 9 % (width - 200), height / 2);
        let rgba: Vec<u8> = (0..width * height)
            .flat_map(|i| {
                let (x, y) = (i % width, i / width);
                let (dx, dy) = (x.abs_diff(cx), y.abs_diff(cy));
                if dx * dx + dy * dy < 80 * 80 {
                    [250, 220, 60, 255]
                } else {
                    [(x * 255 / width) as u8, (y * 255 / height) as u8, 120, 255]
                }
            })
            .collect();
        let mut encoded = Vec::new();
        image_webp::WebPEncoder::new(&mut encoded)
            .encode(&rgba, width, height, image_webp::ColorType::Rgba8)
            .expect("encodable");
        let mut anmf = Vec::new();
        for value in [0, 0, width - 1, height - 1, 50] {
            anmf.extend(u24(value));
        }
        anmf.push(0b10); // no blending, no disposal
        anmf.extend_from_slice(&encoded[12..]);
        body.extend(chunk(*b"ANMF", &anmf));
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&u32::try_from(body.len()).expect("small").to_le_bytes());
    out.extend(body);
    out
}
