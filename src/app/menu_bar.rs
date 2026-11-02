//! The menu bar (File, View, and Help), with access keys as on Windows: Alt and a menu's
//! underlined letter opens it, and an item's letter picks it.

use egui::{Button, Key, KeyboardShortcut, Modifiers, Stroke, Ui};

use super::{ADD_FILES, OxidropApp};

/// The menus. Each marks its access key with `&`: Alt (Option on macOS) with that letter
/// opens the menu, as on Windows and Linux.
const MENUS: [&str; 3] = ["&File", "&View", "&Help"];

impl OxidropApp {
    pub(super) fn menu_bar(&mut self, ui: &mut Ui) {
        let pressed = menu_key(ui);
        // Like Windows, underline the access keys while Alt is held, and while a menu
        // opened from the keyboard is in use.
        let shown = self.menu_from_keyboard || ui.input(|i| i.modifiers.alt);
        egui::MenuBar::new().ui(ui, |ui| {
            let [file, view, help] = MENUS.map(|title| access_text(ui, title, shown));
            let file = ui.menu_button(file, |ui| {
                let shortcut = ui.ctx().format_shortcut(&ADD_FILES);
                let (text, pressed) = menu_item(ui, "Add &files…", shown);
                if ui.add(Button::new(text).shortcut_text(shortcut)).clicked() || pressed {
                    self.pick_files();
                }
                let (text, pressed) = menu_item(ui, "Add f&older…", shown);
                if ui.button(text).clicked() || pressed {
                    self.pick_folder();
                }
                ui.separator();
                let (text, pressed) = menu_item(ui, "&Quit", shown);
                if ui.button(text).clicked() || pressed {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            let view = ui.menu_button(view, |ui| {
                ui.label("Theme");
                // Not egui's theme buttons: their emoji aren't in the app's fonts.
                let mut theme = ui.options(|o| o.theme_preference);
                for (value, label) in [
                    (egui::ThemePreference::System, "&Follow system"),
                    (egui::ThemePreference::Light, "&Light"),
                    (egui::ThemePreference::Dark, "&Dark"),
                ] {
                    let (text, pressed) = menu_item(ui, label, shown);
                    ui.radio_value(&mut theme, value, text);
                    if pressed {
                        theme = value;
                    }
                }
                ui.ctx().set_theme(theme);
            });
            let help = ui.menu_button(help, |ui| {
                let (text, pressed) = menu_item(ui, "&About Oxidrop", shown);
                if ui.button(text).clicked() || pressed {
                    self.show_about = true;
                }
            });
            let buttons = [&file.response, &view.response, &help.response];
            for (title, button) in MENUS.into_iter().zip(buttons) {
                if pressed.is_some() && pressed == access_key(title) {
                    self.menu_from_keyboard |= toggle_menu(ui, button);
                }
            }
            let any_open = buttons.into_iter().any(|button| {
                egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(button))
            });
            self.menu_from_keyboard &= any_open;
        });
    }
}

/// The menu key pressed with Alt this frame, if any. On macOS, Option with a letter types a
/// character, so not while a text field has the keyboard. (Not `egui_wants_keyboard_input`:
/// that's any focused widget, such as the menu button a keyboard-opened menu leaves focused.)
fn menu_key(ui: &Ui) -> Option<Key> {
    if ui.ctx().os() == egui::os::OperatingSystem::Mac && ui.ctx().text_edit_focused() {
        return None;
    }
    ui.input_mut(|i| {
        MENUS
            .into_iter()
            .filter_map(access_key)
            .find(|&key| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::ALT, key)))
    })
}

/// Opens the menu of `button`, or closes it if it is open; true if it opened. The button
/// gets the keyboard focus, so the arrow keys move into the menu, Enter picks an entry, and
/// Escape closes it.
fn toggle_menu(ui: &Ui, button: &egui::Response) -> bool {
    let popup = egui::Popup::default_response_id(button);
    let was_open = egui::Popup::is_id_open(ui.ctx(), popup);
    egui::Popup::toggle_id(ui.ctx(), popup);
    if !was_open {
        button.request_focus();
    }
    !was_open
}

/// The text of a menu item whose label marks its access key with `&` (see [`access_text`]),
/// and whether that key was pressed this frame, with or without Alt. A press closes the menu,
/// as clicking the item does.
fn menu_item(ui: &Ui, label: &str, shown: bool) -> (egui::WidgetText, bool) {
    let pressed =
        access_key(label).is_some_and(|key| ui.input_mut(|i| i.consume_key(Modifiers::NONE, key)));
    if pressed {
        ui.close();
    }
    (access_text(ui, label, shown), pressed)
}

/// The key a label marks with `&`, as in "&Quit".
fn access_key(label: &str) -> Option<Key> {
    let (_, after) = label.split_once('&')?;
    Key::from_name(&after.chars().next()?.to_string())
}

/// A label that marks its access key with `&`, as in "&Quit", without the `&`; the key's
/// letter is underlined when `shown` is set. The text keeps the widget's own colors.
fn access_text(ui: &Ui, label: &str, shown: bool) -> egui::WidgetText {
    let Some((before, after)) = label.split_once('&') else {
        return label.into();
    };
    if !shown {
        return format!("{before}{after}").into();
    }
    let plain = egui::TextFormat::simple(
        egui::TextStyle::Button.resolve(ui.style()),
        egui::Color32::PLACEHOLDER,
    );
    let key = after.chars().next().map_or(0, char::len_utf8);
    let mut job = egui::text::LayoutJob::default();
    job.append(before, 0.0, plain.clone());
    job.append(
        &after[..key],
        0.0,
        egui::TextFormat {
            underline: Stroke::new(1.0, egui::Color32::PLACEHOLDER),
            ..plain.clone()
        },
    );
    job.append(&after[key..], 0.0, plain);
    job.into()
}
