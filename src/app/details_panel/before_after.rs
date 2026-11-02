//! The before/after table: what the file was and what it became, how long that took, and
//! where the result went.

use std::{path::Path, time::Duration};

use egui::{RichText, Ui};

use super::{ROW_GAP, UNKNOWN, cells, value};
use crate::{
    convert::Source, format, inspect::Facts, optimize::Outcome, png_info::PngInfo, progress::Report,
};

/// Width of the before/after table's label column.
const LABEL_WIDTH: f32 = 110.0;
/// Gap between the table's columns.
const COLUMN_GAP: f32 = 16.0;
/// Narrowest the Before and After columns get.
const MIN_VALUE_WIDTH: f32 = 160.0;
/// Stages shorter than this are left out of the time breakdown.
const MIN_STAGE_TIME: Duration = Duration::from_millis(5);

/// A file that hasn't finished: what it is now, with the After column still empty.
pub(super) fn unfinished(
    ui: &mut Ui,
    path: &Path,
    facts: Option<&Facts>,
    status: &str,
    failed: bool,
) {
    let (color_label, before) = match facts {
        Some(Facts::Png(info)) => ("Color", Column::png(info, "")),
        Some(Facts::Converted(source)) => ("Format", Column::source(source)),
        None => ("Color", Column::unknown()),
    };
    let status = if failed {
        RichText::new(status).color(ui.visuals().error_fg_color)
    } else {
        RichText::new(status).strong()
    };
    let extra = [
        ("Status", RichText::new(""), status),
        ("Output", value(path.display().to_string()), value(UNKNOWN)),
    ];
    // A file that couldn't be read has nothing to show but the error.
    if facts.is_none() && failed {
        table(ui, None, extra);
    } else {
        table(ui, Some((color_label, &before, &Column::unknown())), extra);
    }
}

/// The table of a finished file: before and after, the time each stage took, and where the result went.
pub(super) fn finished(
    ui: &mut Ui,
    path: &Path,
    outcome: &Outcome,
    elapsed: Duration,
    report: &Report,
) {
    let (color_label, mut before, prefix) = match &outcome.source {
        // The PNG the converter wrote is an intermediate step; show the source file.
        Some(source) => {
            let kind = if outcome.after.is_apng() {
                "APNG · "
            } else {
                "PNG · "
            };
            ("Format", Column::source(source), kind)
        }
        None => ("Color", Column::png(&outcome.before, ""), ""),
    };
    let mut after = Column::png(&outcome.after, prefix);
    before.size = file_size_text(outcome.original_size);
    after.size = format!(
        "{} · {}",
        file_size_text(outcome.optimized_size),
        format::savings(outcome.original_size, outcome.optimized_size)
    );

    let stages = report
        .stages
        .iter()
        .filter(|(_, spent)| *spent >= MIN_STAGE_TIME)
        .map(|(stage, spent)| format!("{} {}", stage.label(), format::duration(*spent)))
        .collect::<Vec<_>>();
    let mut time = format::duration(elapsed);
    if !stages.is_empty() {
        time = format!("{time} · {}", stages.join(" · "));
    }
    let output = match &outcome.written_to {
        Some(_) if outcome.replaced_original => "Overwritten in place".to_owned(),
        Some(path) if outcome.converted() => format!("New file {}", path.display()),
        Some(path) => format!("Saved to {}", path.display()),
        None => "Nothing written (already optimized)".to_owned(),
    };
    table(
        ui,
        Some((color_label, &before, &after)),
        [
            ("Time", value(UNKNOWN), value(time)),
            ("Output", value(path.display().to_string()), value(output)),
        ],
    );
}

/// One side of the before/after table.
struct Column {
    size: String,
    dimensions: String,
    /// The pixel format; for a converted file's source, the file format.
    color: String,
    interlacing: String,
    frames: String,
    animated: bool,
    chunks: String,
}

impl Column {
    /// A PNG or APNG; `prefix` goes before the pixel format, e.g. `APNG · `.
    fn png(info: &PngInfo, prefix: &str) -> Self {
        Self {
            size: file_size_text(info.file_size),
            dimensions: dimensions(info.width, info.height),
            color: format!("{prefix}{}", info.format_summary()),
            interlacing: if info.interlaced { "Adam7" } else { "None" }.to_owned(),
            frames: format::grouped(u64::from(info.frame_count.unwrap_or(1))),
            animated: info.is_apng(),
            chunks: info.chunk_summary(),
        }
    }

    /// A GIF or WebP file that is converted to PNG/APNG.
    fn source(source: &Source) -> Self {
        Self {
            size: file_size_text(source.file_size),
            dimensions: dimensions(source.width, source.height),
            color: source.format_name().to_owned(),
            // GIF has its own line interlacing and WebP none; neither is PNG's Adam7.
            interlacing: UNKNOWN.to_owned(),
            frames: format::grouped(u64::from(source.frames)),
            animated: source.is_animated(),
            chunks: if source.metadata.is_empty() {
                "no metadata".to_owned()
            } else {
                source.metadata.join(", ")
            },
        }
    }

    fn unknown() -> Self {
        Self {
            size: UNKNOWN.to_owned(),
            dimensions: UNKNOWN.to_owned(),
            color: UNKNOWN.to_owned(),
            interlacing: UNKNOWN.to_owned(),
            frames: UNKNOWN.to_owned(),
            animated: false,
            chunks: UNKNOWN.to_owned(),
        }
    }
}

fn file_size_text(bytes: u64) -> String {
    format!(
        "{} ({} bytes)",
        format::bytes(bytes),
        format::grouped(bytes)
    )
}

fn dimensions(width: u32, height: u32) -> String {
    format!(
        "{} × {}",
        format::grouped(u64::from(width)),
        format::grouped(u64::from(height))
    )
}

/// The before/after table: the rows of the two columns, if there are any (`color_label`
/// names the pixel format row, `Format` for converted files), then `extra` rows.
fn table<const N: usize>(
    ui: &mut Ui,
    columns: Option<(&str, &Column, &Column)>,
    extra: [(&str, RichText, RichText); N],
) {
    let value_width =
        ((ui.available_width() - LABEL_WIDTH - 2.0 * COLUMN_GAP) / 2.0).max(MIN_VALUE_WIDTH);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = ROW_GAP;
        let row = |ui: &mut Ui, label: &str, before: RichText, after: RichText| {
            cells(
                ui,
                COLUMN_GAP,
                [
                    (LABEL_WIDTH, RichText::new(label)),
                    (value_width, before),
                    (value_width, after),
                ],
            );
        };
        row(ui, "", value("Before"), value("After"));
        if let Some((color_label, before, after)) = columns {
            let pair = |ui: &mut Ui, label: &str, before: &str, after: &str| {
                row(ui, label, value(before), value(after));
            };
            pair(ui, "Size", &before.size, &after.size);
            pair(ui, "Dimensions", &before.dimensions, &after.dimensions);
            pair(ui, color_label, &before.color, &after.color);
            pair(ui, "Interlacing", &before.interlacing, &after.interlacing);
            if before.animated || after.animated {
                pair(ui, "Frames", &before.frames, &after.frames);
            }
            pair(ui, "Chunks", &before.chunks, &after.chunks);
        }
        for (label, before, after) in extra {
            row(ui, label, before, after);
        }
    });
}
