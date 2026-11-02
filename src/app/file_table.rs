//! The file table in the middle of the window, sortable by column, and the drop area shown in
//! its place while the list is empty.

use std::time::Instant;

use egui::{Align, Button, Key, Label, Layout, Rect, RichText, Sense, Stroke, Ui, UiBuilder, vec2};
use egui_extras::{Column, TableBuilder, TableRow};

use super::{
    FileEntry, OxidropApp, Sort, SortKey, Status, reveal, style,
    widgets::{self, StatusIcon},
};
use crate::{format, optimize::Outcome, png_info, progress::Report};

impl OxidropApp {
    /// The drop area of an empty window, as in the mockup: a drop icon and instructions in a
    /// dashed frame. Adding files by hand is left to the toolbar and the File menu.
    pub(super) fn empty_state(ui: &mut Ui) {
        let area = ui.available_rect_before_wrap();
        let size = vec2((area.width() - 32.0).clamp(200.0, 520.0), 196.0);
        let frame = Rect::from_center_size(area.center(), size);
        let color = ui.visuals().widgets.noninteractive.bg_stroke.color;
        widgets::dashed_frame(ui.painter(), frame, 6.0, Stroke::new(2.0, color));
        ui.scope_builder(UiBuilder::new().max_rect(frame.shrink(16.0)), |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                widgets::drop_icon(ui, 48.0);
                ui.add_space(10.0);
                ui.label(style::heading(
                    "Drop PNG, APNG, GIF, or WebP files here",
                    18.0,
                ));
                ui.add_space(4.0);
                ui.label("Folders are searched too.");
                ui.label("Originals are only replaced when the result is smaller.");
                ui.label("GIF and WebP files are converted to new PNG files.");
            });
        });
    }

    pub(super) fn file_table(&mut self, ui: &mut Ui) {
        let now = Instant::now();
        let row_height = ui.spacing().interact_size.y + 6.0;
        let mut clicked = None;
        let mut removed = None;
        let mut started = None;
        let mut stopped = None;
        let entries = self.files.entries();
        let order = self.files.sorted_indices(self.sort, now);
        let sort = &mut self.sort;

        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::exact(22.0))
            .column(Column::initial(200.0).at_least(80.0).clip(true))
            .column(Column::initial(76.0).at_least(56.0))
            .column(Column::initial(76.0).at_least(56.0))
            .column(Column::initial(66.0).at_least(50.0))
            .column(Column::initial(62.0).at_least(50.0))
            .column(Column::remainder().at_least(160.0).clip(true))
            .header(row_height, |mut header| {
                header.col(|_| {});
                sort_column(&mut header, "File", SortKey::Name, sort, false);
                for (title, key) in [
                    ("Original", SortKey::Original),
                    ("Optimized", SortKey::Optimized),
                    ("Saved", SortKey::Saved),
                    ("Time", SortKey::Time),
                ] {
                    sort_column(&mut header, title, key, sort, true);
                }
                header.col(|ui| {
                    ui.strong("Progress / changes");
                });
            })
            .body(|body| {
                body.rows(row_height, entries.len(), |mut row| {
                    let entry = &entries[order[row.index()]];
                    row.set_selected(self.selected == Some(entry.id));
                    let view = RowView::new(entry, now);

                    row.col(|ui| match view.icon {
                        Some(icon) => {
                            widgets::status_icon(ui, icon);
                        }
                        None => {
                            widgets::spinner(ui, 14.0);
                        }
                    });
                    row.col(|ui| {
                        ui.add(Label::new(&entry.name).truncate().selectable(false))
                            .on_hover_text(entry.path.display().to_string());
                    });
                    for text in [&view.original, &view.optimized, &view.saved, &view.time] {
                        row.col(|ui| {
                            right_aligned(ui, |ui| {
                                ui.add(Label::new(text.as_str()).selectable(false));
                            });
                        });
                    }
                    row.col(|ui| match &entry.status {
                        Status::Running { stopping: true, .. } => {
                            widgets::progress_bar(ui, None, "Stopping…", None);
                        }
                        Status::Running { snapshot, .. } => match snapshot {
                            Some(snapshot) => {
                                // Projections keep the bar moving between log lines.
                                let fraction = snapshot.fraction_at(now);
                                let estimate = fraction
                                    .filter(|_| snapshot.estimated)
                                    .map(|f| format!("~{:.0}%", f * 100.0));
                                widgets::progress_bar(
                                    ui,
                                    fraction,
                                    &snapshot.detail,
                                    estimate.as_deref(),
                                );
                            }
                            None => {
                                widgets::progress_bar(ui, None, "Starting", None);
                            }
                        },
                        Status::Failed { error } => {
                            let color = ui.visuals().error_fg_color;
                            ui.add(
                                Label::new(RichText::new(error).color(color))
                                    .truncate()
                                    .selectable(false),
                            );
                        }
                        _ => {
                            let text = RichText::new(&view.result);
                            let text = if view.weak_result { text.weak() } else { text };
                            ui.add(Label::new(text).truncate().selectable(false))
                                .on_hover_text(&view.result);
                        }
                    });

                    let response = row.response();
                    if response.clicked() {
                        clicked = Some(entry.id);
                    }
                    response.context_menu(|ui| {
                        let (start_label, start_hint) = if entry.status.is_finished() {
                            (
                                "Start again",
                                "Optimize this file again with the current settings",
                            )
                        } else {
                            ("Start", "Optimize this file now")
                        };
                        if ui
                            .add_enabled(!entry.status.is_active(), Button::new(start_label))
                            .on_hover_text(start_hint)
                            .clicked()
                        {
                            started = Some(entry.id);
                        }
                        let stoppable = matches!(
                            entry.status,
                            Status::Queued
                                | Status::Running {
                                    stopping: false,
                                    ..
                                }
                        );
                        if ui
                            .add_enabled(stoppable, Button::new("Stop"))
                            .on_hover_text("Cancel this file without writing anything")
                            .clicked()
                        {
                            stopped = Some(entry.id);
                        }
                        ui.separator();
                        if ui.button("Open containing folder").clicked() {
                            // Show the written copy in folder mode, otherwise the original.
                            let shown = match &entry.status {
                                Status::Done { outcome, .. } => {
                                    outcome.written_to.as_deref().unwrap_or(&entry.path)
                                }
                                _ => &entry.path,
                            };
                            if let Err(err) = reveal::show_in_folder(shown) {
                                log::warn!(
                                    "could not open the folder of {}: {err}",
                                    shown.display()
                                );
                            }
                        }
                        if ui.button("Copy path").clicked() {
                            ui.ctx().copy_text(entry.path.display().to_string());
                        }
                        if ui
                            .add_enabled(!entry.status.is_active(), Button::new("Remove from list"))
                            .clicked()
                        {
                            removed = Some(entry.id);
                        }
                    });
                });
            });

        if let Some(id) = clicked {
            self.selected = if self.selected == Some(id) {
                None
            } else {
                Some(id)
            };
        }
        // Unless typing: any other focused widget, such as a button, ignores Delete.
        let delete_pressed =
            !ui.ctx().text_edit_focused() && ui.input(|i| i.key_pressed(Key::Delete));
        if let Some(id) = removed.or(self.selected.filter(|_| delete_pressed)) {
            self.files.remove(id);
        }
        if let Some(id) = started {
            self.restart(id);
        }
        if let Some(id) = stopped {
            self.stop_one(id);
        }
    }
}

/// Text shown in one table row, derived from the file's status.
struct RowView {
    /// `None` shows a spinner.
    icon: Option<StatusIcon>,
    original: String,
    optimized: String,
    saved: String,
    time: String,
    result: String,
    weak_result: bool,
}

impl RowView {
    fn new(entry: &FileEntry, now: Instant) -> Self {
        let dash = || "—".to_owned();
        let original = entry.original_size.map_or_else(dash, format::bytes);
        let blank = |icon, result: &str| Self {
            icon,
            original: original.clone(),
            optimized: dash(),
            saved: dash(),
            time: dash(),
            result: result.to_owned(),
            weak_result: true,
        };
        match &entry.status {
            Status::Pending => blank(Some(StatusIcon::Pending), "Not started"),
            Status::Stopped => blank(Some(StatusIcon::Pending), "Stopped"),
            Status::Queued => blank(Some(StatusIcon::Queued), "Queued"),
            Status::Failed { error } => blank(Some(StatusIcon::Failed), error),
            Status::Running { started, .. } => Self {
                time: format::duration(now.saturating_duration_since(*started)),
                ..blank(None, "")
            },
            Status::Done {
                outcome,
                elapsed,
                report,
            } => Self {
                icon: Some(match (outcome.improved(), outcome.converted()) {
                    (true, _) => StatusIcon::Improved,
                    (false, true) => StatusIcon::Converted,
                    (false, false) => StatusIcon::Unchanged,
                }),
                original: format::bytes(outcome.original_size),
                optimized: format::bytes(outcome.optimized_size),
                saved: format::savings(outcome.original_size, outcome.optimized_size),
                time: format::duration(*elapsed),
                result: result_text(outcome, report),
                weak_result: !outcome.improved() && !outcome.converted(),
            },
        }
    }
}

fn result_text(outcome: &Outcome, report: &Report) -> String {
    // oxipng warns "Timed out after N second(s)" when the time limit cut trials short.
    let timed_out = report.notes.iter().any(|n| n.starts_with("Timed out"));
    if outcome.improved() || outcome.converted() {
        let text = if outcome.changes.is_empty() {
            "Recompressed".to_owned()
        } else {
            png_info::summarize(&outcome.changes)
        };
        if timed_out {
            format!("{text} · stopped at the time limit")
        } else {
            text
        }
    } else {
        match (timed_out, &outcome.written_to) {
            (true, _) => "Time limit reached before anything smaller was found".to_owned(),
            (false, Some(_)) => "Already optimized — copied unchanged".to_owned(),
            (false, None) => "Already optimized — file left untouched".to_owned(),
        }
    }
}

fn right_aligned(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.with_layout(Layout::right_to_left(Align::Center), add);
}

const SORT_HINT: &str = "Sort by this column; click again to reverse, a third time to reset";

/// A sortable column: its title, with an arrow when sorted, and the whole header cell (not
/// just the title) changes the sort when clicked.
fn sort_column(
    header: &mut TableRow<'_, '_>,
    title: &str,
    key: SortKey,
    sort: &mut Option<Sort>,
    right: bool,
) {
    let current = *sort;
    let mut title_clicked = false;
    let (_, cell) = header.col(|ui| {
        let mut add = |ui: &mut Ui| title_clicked = sort_title(ui, title, key, current);
        if right {
            right_aligned(ui, add);
        } else {
            add(ui);
        }
    });
    if title_clicked || cell.on_hover_text(SORT_HINT).clicked() {
        *sort = Sort::after_click(*sort, key);
    }
}

/// A column title with an arrow on the sorted column; true when it was clicked.
fn sort_title(ui: &mut Ui, title: &str, key: SortKey, sort: Option<Sort>) -> bool {
    let text = RichText::new(title).strong();
    let button = match sort {
        Some(current) if current.key == key => {
            Button::new((text, widgets::sort_arrow(current.ascending)))
        }
        _ => Button::new(text),
    };
    ui.add(button.frame(false))
        .on_hover_text(SORT_HINT)
        .clicked()
}
