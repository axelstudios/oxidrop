//! The toolbar: adding files and folders, starting and stopping, and clearing the list, with
//! a summary of the settings on the right.

use egui::{Align, Button, Layout, Ui};

use super::{
    OxidropApp, START,
    widgets::{self, ToolIcon},
};

impl OxidropApp {
    pub(super) fn toolbar(&mut self, ui: &mut Ui) {
        let totals = self.files.totals();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(widgets::tool_button(ToolIcon::Add, "Add files…"))
                .clicked()
            {
                self.pick_files();
            }
            if ui
                .add(widgets::tool_button(ToolIcon::Folder, "Add folder…"))
                .clicked()
            {
                self.pick_folder();
            }
            ui.separator();
            let start = ui
                .add_enabled(
                    totals.pending + totals.stopped > 0,
                    widgets::tool_button(ToolIcon::Start, "Start"),
                )
                .on_hover_text(format!(
                    "Optimize the files that haven't started ({})",
                    ui.ctx().format_shortcut(&START)
                ));
            if start.clicked() {
                self.start_pending();
            }
            let stop = ui
                .add_enabled(
                    totals.queued + totals.running > 0,
                    widgets::tool_button(ToolIcon::Stop, "Stop"),
                )
                .on_hover_text(
                    "Stop now: running files are cancelled without writing anything, and \
                     queued ones don't start.",
                );
            if stop.clicked() {
                self.stop();
            }
            ui.separator();
            if ui
                .add_enabled(
                    totals.done + totals.failed > 0,
                    Button::new("Clear finished"),
                )
                .clicked()
            {
                self.files.clear_finished();
            }
            let idle = totals.files - totals.running - totals.queued;
            if ui
                .add_enabled(idle > 0, Button::new("Remove all"))
                .clicked()
            {
                self.files.clear_idle();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.weak(self.settings.summary());
            });
        });
    }
}
