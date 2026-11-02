//! The About window: the app and oxipng versions, and the graphics adapter for bug reports.

use super::{OxidropApp, style};

/// The version of oxipng being built, read from `Cargo.lock` by the build script.
const OXIPNG_VERSION: &str = env!("OXIPNG_VERSION");

impl OxidropApp {
    /// Escape closes the About window, as it does a dialog, unless it's for an open menu or a
    /// text box. Called first in the frame: a menu only closes on Escape later in the frame,
    /// but a text box loses the keyboard before the frame starts, so that's as of the last one.
    pub(super) fn close_about_on_escape(&mut self, ctx: &egui::Context) {
        if self.show_about
            && !egui::Popup::is_any_open(ctx)
            && !self.was_typing
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.show_about = false;
        }
    }

    pub(super) fn about_window(&mut self, ctx: &egui::Context) {
        egui::Window::new("About Oxidrop")
            .open(&mut self.show_about)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                // As wide as the longest line, so none of them wraps.
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.label(style::heading(
                    format!("Oxidrop {}", env!("CARGO_PKG_VERSION")),
                    16.0,
                ));
                ui.label("Drag-and-drop PNG and APNG optimizer");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    // egui can't link part of a label, so this line is three widgets, a
                    // space apart.
                    let body = egui::TextStyle::Body.resolve(ui.style());
                    ui.spacing_mut().item_spacing.x =
                        ui.ctx().fonts_mut(|fonts| fonts.glyph_width(&body, ' '));
                    ui.label("Optimization via");
                    ui.hyperlink_to("oxipng", "https://github.com/oxipng/oxipng");
                    ui.label(OXIPNG_VERSION);
                });
                ui.add_space(6.0);
                ui.weak(format!("Graphics: {}", self.graphics));
            });
    }
}
