//! The details panel: what a file is, before and after optimizing, and what oxipng tried.
//!
//! Laid out like the mockup: a before/after table with two equally wide value columns, then
//! the optimization report as up to three blocks side by side (facts, trials, Zopfli passes)
//! that wrap onto more rows in a narrow panel. Labels use the normal text color and values
//! the strong one (white in the dark theme).

mod before_after;
mod facts;
mod trials;
mod zopfli_passes;

use std::{path::Path, time::Duration};

use egui::{Align, Label, Layout, RichText, ScrollArea, Ui, vec2};

use super::{OxidropApp, Status, style};
use crate::{format, optimize::Outcome, progress::Report};

use facts::facts;
use trials::trials;
use zopfli_passes::zopfli_passes;

/// Gap between the rows of a table.
const ROW_GAP: f32 = 3.0;
/// Gap between the optimization blocks.
const BLOCK_GAP: f32 = 24.0;
/// Smallest widths of the facts, trials, and Zopfli blocks; extra room is shared equally.
const FACTS_WIDTH: f32 = 250.0;
const TRIALS_WIDTH: f32 = 280.0;
const ZOPFLI_WIDTH: f32 = 232.0;
/// Shown for values that aren't known (yet).
const UNKNOWN: &str = "—";

impl OxidropApp {
    pub(super) fn details_panel(&mut self, ui: &mut Ui) {
        let Some(entry) = self.selected.and_then(|id| self.files.get(id)) else {
            return;
        };
        let mut close = false;
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(style::heading(format!("Details — {}", entry.name), 14.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                close = ui
                    .small_button("×")
                    .on_hover_text("Close details")
                    .clicked();
            });
        });
        ui.add_space(2.0);
        let facts = entry.facts.as_ref();
        // Fill the panel: egui panels are only as tall as their contents, so a scroll area
        // that shrank to fit would keep the panel from being dragged taller than, say, a
        // running file's few rows, and the Optimization section would be cut off once it ends.
        ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| match &entry.status {
                Status::Done {
                    outcome,
                    elapsed,
                    report,
                } => finished(ui, &entry.path, outcome, *elapsed, report),
                Status::Running {
                    started,
                    snapshot,
                    stopping,
                    threads,
                } => {
                    let mut status = format!(
                        "Running for {} on {}",
                        format::duration(started.elapsed()),
                        format::count(threads.used, "thread", "threads")
                    );
                    if *stopping {
                        status = format!("{status} · stopping");
                    } else if let Some(snapshot) = snapshot {
                        status = format!("{status} · {}", snapshot.detail);
                    }
                    before_after::unfinished(ui, &entry.path, facts, &status, false);
                }
                Status::Stopped => {
                    before_after::unfinished(ui, &entry.path, facts, "Stopped", false);
                }
                Status::Failed { error } => {
                    before_after::unfinished(ui, &entry.path, facts, error, true);
                }
                Status::Pending => {
                    before_after::unfinished(ui, &entry.path, facts, "Not started", false);
                }
                Status::Queued => before_after::unfinished(ui, &entry.path, facts, "Queued", false),
            });
        if close {
            self.selected = None;
        }
    }
}

/// A finished file: the before/after table, then the optimization report.
fn finished(ui: &mut Ui, path: &Path, outcome: &Outcome, elapsed: Duration, report: &Report) {
    before_after::finished(ui, path, outcome, elapsed, report);
    ui.add_space(2.0);
    ui.separator();
    ui.label(style::heading("Optimization", 13.0));
    ui.add_space(2.0);

    let reductions = report.reductions();
    let filters = report.filters();
    let zopfli = report.zopfli();
    let mut blocks: Vec<(f32, Block<'_>)> =
        vec![(FACTS_WIDTH, Box::new(|ui| facts(ui, outcome, report)))];
    if reductions.len() > 1 || !filters.is_empty() {
        blocks.push((
            TRIALS_WIDTH,
            Box::new(|ui| trials(ui, &reductions, &filters)),
        ));
    }
    if let Some(summary) = &zopfli {
        blocks.push((ZOPFLI_WIDTH, Box::new(|ui| zopfli_passes(ui, summary))));
    }
    wrap_blocks(ui, blocks);
}

/// Draws one block of the optimization report.
type Block<'a> = Box<dyn FnOnce(&mut Ui) + 'a>;

/// Lays blocks out side by side, as many per row as fit at their smallest widths, sharing
/// the rest of each row's width equally (like CSS `flex-wrap` with `flex: 1 1 <width>`).
fn wrap_blocks(ui: &mut Ui, blocks: Vec<(f32, Block<'_>)>) {
    let available = ui.available_width();
    let mut rows: Vec<Vec<(f32, Block<'_>)>> = Vec::new();
    let mut used = 0.0;
    for (width, block) in blocks {
        match rows.last_mut() {
            Some(row) if used + BLOCK_GAP + width <= available => {
                used += BLOCK_GAP + width;
                row.push((width, block));
            }
            _ => {
                used = width;
                rows.push(vec![(width, block)]);
            }
        }
    }
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            ui.add_space(10.0);
        }
        let gaps = BLOCK_GAP * (row.len() - 1) as f32;
        let smallest: f32 = row.iter().map(|(width, _)| width).sum();
        let extra = ((available - gaps - smallest) / row.len() as f32).max(0.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = BLOCK_GAP;
            for (width, block) in row {
                ui.vertical(|ui| {
                    ui.set_width((width + extra).min(available));
                    block(ui);
                });
            }
        });
    }
}

/// A value in a table: the strong text color, white in the dark theme.
fn value(text: impl Into<String>) -> RichText {
    RichText::new(text).strong()
}

/// One row of fixed-width cells, `gap` apart. Text wraps within its cell and every cell
/// starts at the top of the row, as in the mockup. (egui's `Grid` would center shorter cells
/// vertically, so a label would drift down next to a value that wraps.)
fn cells<const N: usize>(ui: &mut Ui, gap: f32, cells: [(f32, RichText); N]) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (width, text) in cells {
            cell(ui, width, text);
        }
    });
}

/// A fixed-width cell whose text wraps, so both value columns stay equally wide.
fn cell(ui: &mut Ui, width: f32, text: RichText) {
    ui.allocate_ui_with_layout(vec2(width, 0.0), Layout::top_down(Align::Min), |ui| {
        ui.set_width(width);
        ui.add(Label::new(text).wrap());
    });
}
