//! Converts GIF and WebP files to PNG/APNG the way the app does and writes the results to a
//! folder, so they can be checked against another decoder. With `--optimize`, oxipng (default
//! settings) runs on each result too, and its time is reported:
//!
//! ```text
//! cargo run --release --example convert_probe -- FILE... [--out DIR] [--optimize]
//! ```
//!
//! The default folder is `target/converted`.

use std::{fs, path::PathBuf, time::Instant};

use oxidrop::{
    convert::{self, FileType},
    format,
    settings::Settings,
};

fn main() -> std::io::Result<()> {
    let mut out_dir = PathBuf::from("target/converted");
    let mut optimize = false;
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out_dir = args.next().map(PathBuf::from).unwrap_or(out_dir),
            "--optimize" => optimize = true,
            _ => files.push(PathBuf::from(arg)),
        }
    }
    fs::create_dir_all(&out_dir)?;

    for path in files {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let data = fs::read(&path)?;
        let Some(file_type) = FileType::detect(&data).filter(|t| t.is_converted()) else {
            println!("{name}: not a GIF or WebP file");
            continue;
        };
        let started = Instant::now();
        let converted =
            match convert::to_png(&data, file_type, convert::Options::default(), &mut |_| {}) {
                Ok(converted) => converted,
                Err(err) => {
                    println!("{name}: {err}");
                    continue;
                }
            };
        let source = &converted.source;
        println!(
            "{name}: {source} {}×{} · {} to {} in {}",
            source.width,
            source.height,
            format::bytes(data.len() as u64),
            format::bytes(converted.png.len() as u64),
            format::duration(started.elapsed()),
        );
        for note in &source.notes {
            println!("    {note}");
        }

        let mut png = converted.png;
        if optimize {
            let started = Instant::now();
            match oxipng::optimize_from_memory(&png, &Settings::default().oxipng_options()) {
                Ok(optimized) => {
                    println!(
                        "    oxipng: {} in {}",
                        format::bytes(optimized.len() as u64),
                        format::duration(started.elapsed())
                    );
                    png = optimized;
                }
                Err(err) => println!("    oxipng failed: {err}"),
            }
        }
        let target = out_dir.join(format!("{name}.png"));
        fs::write(&target, &png)?;
        println!("    → {}", target.display());
    }
    Ok(())
}
