//! The status bar: how many files are in each state, how much the finished ones saved, and how
//! long the batch has taken.

use std::time::Instant;

use egui::{Align, Layout, Ui};

use super::OxidropApp;
use crate::format;

impl OxidropApp {
    pub(super) fn status_bar(&self, ui: &mut Ui) {
        let totals = self.files.totals();
        ui.horizontal(|ui| {
            if totals.files == 0 {
                ui.label("No files yet");
            } else {
                let mut parts = vec![format::count(totals.files, "file", "files")];
                for (n, label) in [
                    (totals.done, "done"),
                    (totals.running, "running"),
                    (totals.queued, "queued"),
                    (totals.pending, "not started"),
                    (totals.stopped, "stopped"),
                    (totals.failed, "failed"),
                ] {
                    if n > 0 {
                        parts.push(format!("{} {label}", format::grouped(n as u64)));
                    }
                }
                ui.label(parts.join(" · "));
                if totals.done > 0 {
                    ui.separator();
                    let (original, result) = (totals.original_bytes, totals.result_bytes);
                    ui.strong(if result <= original {
                        let percent = (original - result) as f64 / original.max(1) as f64 * 100.0;
                        format!(
                            "Saved {} of {} ({percent:.1}%)",
                            format::bytes(original - result),
                            format::bytes(original)
                        )
                    } else {
                        // Possible when converted files come out larger than their originals.
                        format!(
                            "{} larger than the {} originals ({})",
                            format::bytes(result - original),
                            format::bytes(original),
                            format::savings(original, result)
                        )
                    });
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(started) = self.batch_started {
                    let end = self.batch_finished.unwrap_or_else(Instant::now);
                    ui.label(format!("Elapsed {}", format::clock(end - started)));
                }
            });
        });
    }
}
