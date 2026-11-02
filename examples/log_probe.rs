//! Prints the oxipng/zopfli log stream that Oxidrop's progress inference is built on.
//!
//! Run this after upgrading oxipng to check that the log lines parsed in
//! `src/progress/signal.rs` still look the same:
//!
//! ```text
//! cargo run --release --example log_probe -- [FILE.png] [--level N] [--zopfli ITERATIONS] [--threads N]
//! ```
//!
//! Without a file, a 300x200 noisy RGB test image is generated.

#![allow(
    clippy::cast_possible_truncation,
    reason = "the test image wraps pixel values on purpose"
)]

use std::{num::NonZeroU64, sync::Mutex, time::Instant};

use oxipng::{BitDepth, ColorType, Deflater, Options, RawImage, ZopfliOptions};

static START: Mutex<Option<Instant>> = Mutex::new(None);

struct Probe;

impl log::Log for Probe {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("oxipng") || metadata.target().starts_with("zopfli")
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let elapsed = START
            .lock()
            .ok()
            .and_then(|s| *s)
            .map_or(0, |s| s.elapsed().as_millis());
        let thread = std::thread::current();
        println!(
            "{elapsed:>7}ms [{}] {:5} {:24} | {}",
            thread.name().unwrap_or("?"),
            record.level(),
            record.target(),
            record.args()
        );
    }

    fn flush(&self) {}
}

fn noisy_rgb(width: u32, height: u32) -> Vec<u8> {
    let mut seed = 0x1234_5678_u32;
    let mut data = Vec::new();
    for y in 0..height {
        for x in 0..width {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 28) as u8;
            data.extend_from_slice(&[(x as u8) ^ noise, y as u8, ((x + y) % 64) as u8]);
        }
    }
    let raw = RawImage::new(
        width,
        height,
        ColorType::RGB {
            transparent_color: None,
        },
        BitDepth::Eight,
        data,
    )
    .expect("valid raw image");
    let mut opts = Options::from_preset(0);
    opts.color_type_reduction = false;
    raw.create_optimized_png(&opts).expect("encodable")
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut file, mut level, mut zopfli, mut threads) = (None, 2, None, 4);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .and_then(|v| v.parse().ok())
                .expect("missing number")
        };
        match arg.as_str() {
            "--level" => level = value(),
            "--zopfli" => zopfli = Some(value()),
            "--threads" => threads = usize::try_from(value()).expect("thread count"),
            path => file = Some(path.to_owned()),
        }
    }
    let input = match &file {
        Some(path) => std::fs::read(path).expect("readable file"),
        None => noisy_rgb(300, 200),
    };
    let mut opts = Options::from_preset(u8::try_from(level).expect("level 0-6"));
    if let Some(iterations) = zopfli {
        opts.deflater = Deflater::Zopfli(ZopfliOptions {
            iteration_count: NonZeroU64::new(iterations).expect("at least one iteration"),
            ..ZopfliOptions::default()
        });
    }

    log::set_logger(&Probe).expect("no other logger");
    log::set_max_level(log::LevelFilter::Trace);
    *START.lock().expect("start lock") = Some(Instant::now());

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("worker-{i}"))
        .build()
        .expect("thread pool");
    let output = pool
        .install(|| oxipng::optimize_from_memory(&input, &opts))
        .expect("optimizable");
    println!("{} -> {} bytes", input.len(), output.len());
}
