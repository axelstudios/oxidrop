//! The settings panel on the left. Changes apply to files that haven't started, and queued
//! files are queued again with the new settings.

use std::path::PathBuf;

use egui::{Align, ComboBox, DragValue, Layout, RichText, Slider, TextEdit, Ui, vec2};

use super::{OxidropApp, style};
use crate::{
    format,
    settings::{Interlacing, MetadataMode, OutputTarget, Settings, available_cores},
};

impl OxidropApp {
    pub(super) fn settings_panel(&mut self, ui: &mut Ui) {
        let before = self.settings.clone();
        let Self {
            settings: s,
            output_dir_text,
            ..
        } = self;
        ui.add_space(4.0);

        section(ui, "COMPRESSION");
        setting_row(ui, "Level", |ui, label| {
            labelled_slider(ui, &mut s.level, 0..=Settings::MAX_LEVEL, label);
        });
        hint(ui, "0 is fastest, 6 is smallest and much slower");
        ui.checkbox(&mut s.zopfli, option_text(ui, "Use Zopfli"))
            .on_hover_text("Much slower compression that usually saves another 3–8%");
        ui.add_enabled_ui(s.zopfli, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                let label = ui.label("Iterations");
                ui.add(
                    number_field(&mut s.zopfli_iterations)
                        .range(1..=Settings::MAX_ZOPFLI_ITERATIONS),
                )
                .labelled_by(label.id);
            });
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                let checkbox = ui
                    .checkbox(&mut s.zopfli_stop_early, option_text(ui, "Stop after"))
                    .on_hover_text(
                        "Move on to the next block after this many passes without a gain. \
                         Faster with high iteration counts; the details panel shows when \
                         gains usually stop.",
                    );
                ui.add_enabled(
                    s.zopfli_stop_early,
                    number_field(&mut s.zopfli_stop_after)
                        .range(1..=Settings::MAX_ZOPFLI_ITERATIONS),
                )
                .labelled_by(checkbox.id);
                ui.label("idle passes");
            });
        });
        ui.separator();

        section(ui, "IMAGE");
        setting_row(ui, "Metadata", |ui, label| {
            ComboBox::from_id_salt("metadata")
                .width(ui.available_width())
                .selected_text(s.metadata.label())
                .show_ui(ui, |ui| {
                    for mode in MetadataMode::ALL {
                        ui.selectable_value(&mut s.metadata, mode, mode.label());
                    }
                })
                .response
                .labelled_by(label);
        })
        .response
        .on_hover_text(
            "Safe stripping removes text, time, and EXIF chunks but keeps color profiles. A \
             profile that is just standard sRGB is swapped for the small sRGB tag that means \
             the same, and that tag is dropped if the image is stored as grayscale; neither \
             changes how the image looks. Keep all leaves every chunk as it is. Strip all also \
             removes color profiles, which can change how colors look. Every choice keeps an \
             animation's frames.",
        );
        setting_row(ui, "Interlacing", |ui, label| {
            ComboBox::from_id_salt("interlacing")
                .width(ui.available_width())
                .selected_text(s.interlacing.label())
                .show_ui(ui, |ui| {
                    for mode in Interlacing::ALL {
                        ui.selectable_value(&mut s.interlacing, mode, mode.label());
                    }
                })
                .response
                .labelled_by(label);
        })
        .response
        .on_hover_text(
            "Interlaced (Adam7) images can show a rough preview while they download, but are \
             usually larger.\n\n\
             Remove: save without interlacing, which is almost always smaller.\n\
             Keep as is: leave each file the way it is.\n\
             Add (Adam7): interlace files, unless that would make them larger.",
        );
        ui.checkbox(
            &mut s.optimize_alpha,
            option_text(ui, "Optimize transparent pixels"),
        )
        .on_hover_text(
            "Lets oxipng change the hidden color of fully transparent pixels. The image looks \
             the same, but the color data under transparent areas is lost.",
        );
        ui.separator();

        section(ui, "OUTPUT");
        ui.radio_value(
            &mut s.output,
            OutputTarget::InPlace,
            option_text(ui, "Overwrite originals (only if smaller)"),
        )
        .on_hover_text(
            "GIF and WebP files are never overwritten: their PNG is saved next to them, as \
             name.png, or name (2).png if that is taken.",
        );
        let folder_label = ui
            .radio_value(
                &mut s.output,
                OutputTarget::Folder,
                option_text(ui, "Save to folder"),
            )
            .id;
        ui.add_enabled_ui(s.output == OutputTarget::Folder, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                // Right-to-left so the text box takes exactly the width left after the
                // button; sizing it from the full width would keep widening the panel.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Browse…").clicked()
                        && let Some(dir) = rfd::FileDialog::new()
                            .set_title("Save optimized files to")
                            .pick_folder()
                    {
                        *output_dir_text = dir.display().to_string();
                        s.output_dir = dir;
                    }
                    let edit = ui
                        .add(
                            TextEdit::singleline(output_dir_text)
                                .hint_text("Choose a folder")
                                .desired_width(ui.available_width()),
                        )
                        .labelled_by(folder_label);
                    if edit.changed() {
                        s.output_dir = PathBuf::from(output_dir_text.trim());
                    }
                });
            });
        });
        ui.checkbox(
            &mut s.preserve_modified_time,
            option_text(ui, "Keep original modified time"),
        );
        ui.separator();

        section(ui, "PERFORMANCE");
        setting_row(ui, "Files at a time", |ui, label| {
            labelled_slider(ui, &mut s.parallel_files, 1..=available_cores(), label);
        });
        ui.checkbox(
            &mut s.auto_start,
            option_text(ui, "Start when files are added"),
        );
        ui.horizontal(|ui| {
            let checkbox = ui
                .checkbox(&mut s.time_limit, option_text(ui, "Time limit per file"))
                .on_hover_text(
                    "After this long, oxipng starts no new trials and stops any Zopfli \
                     compression still under way. The smallest result that finished in time is \
                     kept; if none did, the file stays as it was.",
                );
            ui.add_enabled(
                s.time_limit,
                number_field(&mut s.time_limit_secs)
                    .range(1..=Settings::MAX_TIME_LIMIT_SECS)
                    .suffix(" s"),
            )
            .labelled_by(checkbox.id);
        });
        ui.add_space(8.0);
        hint(ui, "Changes apply to files that haven't started yet.");

        if self.settings.parallel_files != before.parallel_files {
            self.runner
                .set_parallel_files(self.settings.parallel_files());
        }
        let affects_output = Settings {
            parallel_files: before.parallel_files,
            auto_start: before.auto_start,
            ..self.settings.clone()
        } != before;
        if affects_output {
            // Queued files captured the old settings; queue them again with the new ones.
            let ids = self.runner.cancel_queued();
            self.submit(&ids);
        }
    }
}

fn section(ui: &mut Ui, title: &str) {
    ui.label(style::heading(title, 14.0));
}

/// Width of the settings panel's label column, so the controls after it line up.
const LABEL_WIDTH: f32 = 92.0;

/// A settings row: `label` in the label column, then `add_control` with the rest of the
/// width and the label's id, which names the control for screen readers.
fn setting_row<R>(
    ui: &mut Ui,
    label: &str,
    add_control: impl FnOnce(&mut Ui, egui::Id) -> R,
) -> egui::InnerResponse<R> {
    ui.horizontal(|ui| {
        let height = ui.spacing().interact_size.y;
        let label = ui
            .allocate_ui_with_layout(
                vec2(LABEL_WIDTH, height),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_min_width(LABEL_WIDTH);
                    ui.label(label)
                },
            )
            .inner;
        add_control(ui, label.id)
    })
}

/// Checkbox and radio text in the normal text color, as in the mockup, rather than as bright
/// as button text.
fn option_text(ui: &Ui, text: &str) -> RichText {
    RichText::new(text).color(ui.visuals().text_color())
}

/// A small explanatory line in the settings panel.
fn hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).weak().size(12.0));
}

/// A field for a whole number, shown with thousands separators (`1,000`) and accepting them
/// when typed.
fn number_field<Num: egui::emath::Numeric>(value: &mut Num) -> DragValue<'_> {
    DragValue::new(value)
        .custom_formatter(|n, _| {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "the fields hold small positive whole numbers"
            )]
            let whole = n.round().max(0.0) as u64;
            format::grouped(whole)
        })
        .custom_parser(|text| text.replace(',', "").trim().parse().ok())
}

/// A slider without its built-in number box, plus a number field, both named by `label`
/// for screen readers.
fn labelled_slider<Num: egui::emath::Numeric>(
    ui: &mut Ui,
    value: &mut Num,
    range: std::ops::RangeInclusive<Num>,
    label: egui::Id,
) {
    // The slider takes the width the number field leaves, so rows line up on the right too.
    let field_width = ui.spacing().interact_size.x;
    ui.spacing_mut().slider_width =
        (ui.available_width() - field_width - ui.spacing().item_spacing.x).max(field_width);
    ui.add(Slider::new(&mut *value, range.clone()).show_value(false))
        .labelled_by(label);
    ui.add(DragValue::new(value).range(range))
        .labelled_by(label);
}
