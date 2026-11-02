//! The facts block of the optimization report: what was chosen, where the savings came from,
//! and oxipng's notes.

use egui::{RichText, Ui};

use super::{ROW_GAP, cells, value};
use crate::{
    format,
    optimize::Outcome,
    png_info::{ChunkCount, PngInfo},
    progress::{Report, Threads},
};

/// Width of the label column of the optimization facts.
const FACT_LABEL_WIDTH: f32 = 96.0;
/// Chunks that make up the image rather than describe it; left out of "Other chunks".
const IMAGE_CHUNKS: [&str; 6] = ["IHDR", "IDAT", "IEND", "acTL", "fcTL", "fdAT"];

/// What was chosen, where the savings came from, and oxipng's notes.
pub(super) fn facts(ui: &mut Ui, outcome: &Outcome, report: &Report) {
    let (before, after) = (&outcome.before, &outcome.after);
    let chosen = match (&report.chosen, outcome.improved() || outcome.converted()) {
        (Some(chosen), true) => {
            let image = report
                .reductions()
                .into_iter()
                .next()
                .map_or_else(|| after.format_summary(), |c| c.label);
            format!(
                "{image} · filter {} · {}",
                chosen.filter,
                chosen.compressor()
            )
        }
        (None, true) if outcome.converted() => {
            "Kept as converted: oxipng found nothing smaller".to_owned()
        }
        (None, true) => "Image data kept; only chunks changed".to_owned(),
        (_, false) => "Original kept: oxipng found nothing smaller".to_owned(),
    };
    let other_before = before.file_size.saturating_sub(before.image_data);
    let other_after = after.file_size.saturating_sub(after.image_data);
    // A converted file's "before" is the converter's quick PNG; only the result means much.
    let (image_data, other) = if outcome.converted() {
        (
            format!("{} compressed pixels", format::bytes(after.image_data)),
            format::bytes(other_after),
        )
    } else {
        let mut other = format::bytes_change(other_before, other_after);
        let removed = chunk_difference(before, after);
        let added = chunk_difference(after, before);
        if !removed.is_empty() {
            other = format!("{other} · {} removed", list(&removed));
        }
        if !added.is_empty() {
            let separator = if removed.is_empty() { " · " } else { ", " };
            other = format!("{other}{separator}{} added", list(&added));
        }
        (
            format!(
                "IDAT {} to {} ({})",
                format::bytes(before.image_data),
                format::bytes(after.image_data),
                format::bytes_change(before.image_data, after.image_data)
            ),
            other,
        )
    };
    let notes: Vec<&str> = outcome
        .source
        .iter()
        .flat_map(|s| &s.notes)
        .chain(&report.notes)
        .map(String::as_str)
        .collect();

    let gap = 12.0;
    let value_width = ui.available_width() - FACT_LABEL_WIDTH - gap;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = ROW_GAP;
        let mut row = |label: &str, text: String| {
            cells(
                ui,
                gap,
                [
                    (FACT_LABEL_WIDTH, RichText::new(label)),
                    (value_width, value(text)),
                ],
            );
        };
        row("Chosen", chosen);
        row("Image data", image_data);
        row("Other chunks", other);
        if let Some(threads) = report.threads {
            row("Threads", threads_text(threads));
        }
        if !notes.is_empty() {
            row("Notes", notes.join("\n"));
        }
    });
}

/// e.g. `8 of 24 CPU threads, shared by 3 files`.
fn threads_text(threads: Threads) -> String {
    let share = format!(
        "{} of {} CPU threads",
        format::grouped(threads.used as u64),
        format::grouped(threads.available as u64)
    );
    if threads.sharing > 1 {
        format!(
            "{share}, shared by {}",
            format::count(threads.sharing, "file", "files")
        )
    } else {
        share
    }
}

/// Chunks in `a` more often than in `b`, leaving out those that make up the image.
fn chunk_difference(a: &PngInfo, b: &PngInfo) -> Vec<ChunkCount> {
    a.chunks
        .iter()
        .filter(|c| !IMAGE_CHUNKS.contains(&c.name.as_str()))
        .filter_map(|c| {
            let in_b = b
                .chunks
                .iter()
                .find(|other| other.name == c.name)
                .map_or(0, |other| other.count);
            let count = c.count.saturating_sub(in_b);
            (count > 0).then(|| ChunkCount {
                name: c.name.clone(),
                count,
            })
        })
        .collect()
}

/// `tEXt ×3 and tIME`, or `iCCP, tEXt, and tIME`.
fn list(chunks: &[ChunkCount]) -> String {
    let names: Vec<String> = chunks
        .iter()
        .map(|c| {
            if c.count == 1 {
                c.name.clone()
            } else {
                format!("{} ×{}", c.name, format::grouped(c.count as u64))
            }
        })
        .collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_the_thread_share() {
        let shared = Threads {
            used: 8,
            available: 24,
            sharing: 3,
        };
        assert_eq!(
            threads_text(shared),
            "8 of 24 CPU threads, shared by 3 files"
        );
        let alone = Threads {
            used: 24,
            available: 24,
            sharing: 1,
        };
        assert_eq!(threads_text(alone), "24 of 24 CPU threads");
    }

    fn chunks(names: &[(&str, usize)]) -> Vec<ChunkCount> {
        names
            .iter()
            .map(|&(name, count)| ChunkCount {
                name: name.to_owned(),
                count,
            })
            .collect()
    }

    #[test]
    fn lists_chunks_with_the_oxford_comma() {
        assert_eq!(
            list(&chunks(&[("tEXt", 3), ("tIME", 1)])),
            "tEXt ×3 and tIME"
        );
        assert_eq!(
            list(&chunks(&[("iCCP", 1), ("tEXt", 1), ("tIME", 1)])),
            "iCCP, tEXt, and tIME"
        );
        assert_eq!(list(&chunks(&[("PLTE", 1)])), "PLTE");
    }
}
