//! End-to-end optimization of real files with oxipng.

#![allow(clippy::unwrap_used, reason = "a test should fail loudly")]

mod common;

use std::fs;

use oxidrop::{
    convert::Source,
    optimize::optimize_path,
    png_info::{self, Change, ColorType},
    settings::{OutputTarget, Settings},
};

#[test]
fn reduces_few_color_rgba_to_indexed_and_strips_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("logo.png");
    fs::write(&path, common::few_color_rgba_png(96, 64, 12)).unwrap();

    let outcome = optimize_path(&path, &Settings::default()).unwrap();

    assert!(outcome.improved(), "expected a smaller file: {outcome:?}");
    assert_eq!(outcome.before.color_type, ColorType::Rgba);
    assert_eq!(outcome.after.color_type, ColorType::Indexed);
    assert!(outcome.after.palette_len.is_some_and(|n| n <= 12));
    assert!(
        outcome.changes.iter().any(
            |c| matches!(c, Change::Removed(chunks) if chunks.iter().any(|c| c.name == "tEXt"))
        ),
        "tEXt should be stripped: {:?}",
        outcome.changes
    );

    let on_disk = fs::read(&path).unwrap();
    assert_eq!(on_disk.len() as u64, outcome.optimized_size);
    assert_eq!(png_info::parse(&on_disk).unwrap(), outcome.after);
}

#[test]
fn already_optimized_file_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("icon.png");
    fs::write(&path, common::few_color_rgba_png(32, 32, 4)).unwrap();
    optimize_path(&path, &Settings::default()).unwrap();
    let first_pass = fs::read(&path).unwrap();

    let outcome = optimize_path(&path, &Settings::default()).unwrap();

    assert!(!outcome.improved());
    assert_eq!(outcome.written_to, None);
    assert_eq!(outcome.changes, []);
    assert_eq!(fs::read(&path).unwrap(), first_pass);
}

#[test]
fn zopfli_output_is_valid_and_smaller() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("noise.png");
    let original = common::noisy_rgb_png(64, 48);
    fs::write(&path, &original).unwrap();
    let settings = Settings {
        zopfli: true,
        zopfli_iterations: 2,
        ..Settings::default()
    };

    let outcome = optimize_path(&path, &settings).unwrap();

    assert!(outcome.improved());
    assert!(outcome.optimized_size < original.len() as u64);
    assert_eq!((outcome.after.width, outcome.after.height), (64, 48));
}

#[test]
fn apng_keeps_its_animation_and_shrinks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sprite.apng");
    fs::write(&path, common::apng(48, 32, 3)).unwrap();

    let outcome = optimize_path(&path, &Settings::default()).unwrap();

    assert!(outcome.before.is_apng());
    assert_eq!(outcome.before.frame_count, Some(3));
    assert!(
        outcome.improved(),
        "stored frames should compress: {outcome:?}"
    );
    assert_eq!(outcome.after.frame_count, Some(3), "animation must survive");
    assert!(
        outcome
            .after
            .chunks
            .iter()
            .any(|c| c.name == "fdAT" && c.count == 2)
    );
    assert!(outcome.after.image_data < outcome.before.image_data);
}

#[test]
fn converts_an_animated_gif_to_a_smaller_apng_next_to_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spinner.gif");
    let gif = common::moving_square_gif(160, 48, 8);
    fs::write(&path, &gif).unwrap();

    let outcome = optimize_path(&path, &Settings::default()).unwrap();

    assert!(outcome.converted());
    assert!(outcome.improved(), "expected a smaller file: {outcome:?}");
    assert_eq!(outcome.after.frame_count, Some(8));
    assert_eq!(outcome.after.color_type, ColorType::Indexed);
    assert!(
        matches!(&outcome.changes[0], Change::Converted { from, to, .. } if from == "GIF" && to == "APNG"),
        "{:?}",
        outcome.changes
    );
    let written = dir.path().join("spinner.png");
    assert_eq!(outcome.written_to.as_deref(), Some(written.as_path()));
    assert_eq!(
        png_info::parse(&fs::read(&written).unwrap()).unwrap(),
        outcome.after
    );
    assert_eq!(
        fs::read(&path).unwrap(),
        gif,
        "the GIF itself is never changed"
    );

    // Running again keeps the first PNG and picks a free name.
    let again = optimize_path(&path, &Settings::default()).unwrap();
    assert_eq!(again.written_to, Some(dir.path().join("spinner (2).png")));
}

#[test]
fn converts_a_webp_into_the_output_folder() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("photo.webp");
    fs::write(&path, common::gradient_webp(48, 32)).unwrap();
    let settings = Settings {
        output: OutputTarget::Folder,
        output_dir: dir.path().join("out"),
        ..Settings::default()
    };

    let outcome = optimize_path(&path, &settings).unwrap();

    assert_eq!(
        outcome.source.as_ref().map(Source::format_name),
        Some("WebP (lossless)")
    );
    assert!(!outcome.after.is_apng());
    assert_eq!(
        outcome.written_to,
        Some(dir.path().join("out").join("photo.png"))
    );
}
