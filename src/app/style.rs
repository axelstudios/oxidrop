//! The look of buttons, fields, and text, following the mockup: nearly square corners, the
//! mockup's widget fills in the dark theme (egui 0.37's own are brighter), semibold headings,
//! and text that can't be selected. Text has more contrast than in the mockup.

use egui::{Color32, Context, CornerRadius, RichText, Theme};

/// Corner radius of buttons, fields, checkboxes, and dropdowns.
const WIDGET_RADIUS: u8 = 2;

/// Weak text (hints, the toolbar summary) at 80% of the normal text color, as in the mockup.
const WEAK_TEXT_ALPHA: f32 = 0.8;

/// Normal text and button text. The mockup's and egui's grays (140 and 80) have only 5:1 and
/// 7.6:1 contrast with the panels, which makes small text look faded; these have 10:1 or
/// more. Headings and values use the strong color: white, or black in the light theme.
const DARK_TEXT: u8 = 200;
const DARK_BUTTON_TEXT: u8 = 210;
const LIGHT_TEXT: u8 = 50;
const LIGHT_BUTTON_TEXT: u8 = 40;

/// Weight of headings on Inter's `wght` axis: semibold. Regular text is 400.
const HEADING_WEIGHT: f32 = 600.0;

/// A heading of `size` points: semibold, in the strong text color. egui's "strong" alone is
/// only the brighter color.
pub(super) fn heading(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text)
        .strong()
        .size(size)
        .variation("wght", HEADING_WEIGHT)
}

/// Applies the app's style to both themes.
pub(super) fn apply(ctx: &Context) {
    ctx.all_styles_mut(|style| {
        // Text here is read, not copied (rows have "Copy path"), and selectable labels would
        // turn clicks and drags on rows into text selections.
        style.interaction.selectable_labels = false;
        style.visuals.weak_text_alpha = WEAK_TEXT_ALPHA;
        let widgets = &mut style.visuals.widgets;
        for visuals in [
            &mut widgets.noninteractive,
            &mut widgets.inactive,
            &mut widgets.hovered,
            &mut widgets.active,
            &mut widgets.open,
        ] {
            visuals.corner_radius = CornerRadius::same(WIDGET_RADIUS);
        }
    });
    ctx.style_mut_of(Theme::Dark, |style| {
        let widgets = &mut style.visuals.widgets;
        widgets.noninteractive.fg_stroke.color = Color32::from_gray(DARK_TEXT);
        // Button and dropdown fill and text.
        widgets.inactive.weak_bg_fill = Color32::from_gray(60);
        widgets.inactive.fg_stroke.color = Color32::from_gray(DARK_BUTTON_TEXT);
        widgets.hovered.weak_bg_fill = Color32::from_gray(70);
        widgets.hovered.fg_stroke.color = Color32::from_gray(240);
    });
    ctx.style_mut_of(Theme::Light, |style| {
        let widgets = &mut style.visuals.widgets;
        widgets.noninteractive.fg_stroke.color = Color32::from_gray(LIGHT_TEXT);
        widgets.inactive.weak_bg_fill = Color32::from_gray(226);
        widgets.inactive.fg_stroke.color = Color32::from_gray(LIGHT_BUTTON_TEXT);
        widgets.hovered.weak_bg_fill = Color32::from_gray(210);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squares_off_widgets_and_turns_off_text_selection_in_both_themes() {
        let ctx = Context::default();
        apply(&ctx);
        for theme in [Theme::Dark, Theme::Light] {
            let style = ctx.style_of(theme);
            assert!(!style.interaction.selectable_labels);
            assert_eq!(
                style.visuals.widgets.inactive.corner_radius,
                CornerRadius::same(WIDGET_RADIUS)
            );
        }
    }

    #[test]
    fn text_has_more_contrast_than_egui_gives_it() {
        let ctx = Context::default();
        apply(&ctx);
        for (theme, text, button) in [
            (Theme::Dark, DARK_TEXT, DARK_BUTTON_TEXT),
            (Theme::Light, LIGHT_TEXT, LIGHT_BUTTON_TEXT),
        ] {
            let visuals = &ctx.style_of(theme).visuals;
            assert_eq!(visuals.text_color(), Color32::from_gray(text), "{theme:?}");
            assert_eq!(
                visuals.widgets.inactive.text_color(),
                Color32::from_gray(button),
                "{theme:?}"
            );
        }
    }

    #[test]
    fn headings_are_semibold() {
        let job = egui::WidgetText::from(heading("Output", 14.0)).into_layout_job(
            &egui::Style::default(),
            egui::FontSelection::Default,
            egui::Align::Center,
        );
        let coords: Vec<(String, f32)> = job.sections[0]
            .format
            .coords
            .as_ref()
            .iter()
            .map(|(tag, value)| (tag.to_string(), *value))
            .collect();
        assert_eq!(coords, [("wght".to_owned(), HEADING_WEIGHT)]);
    }
}
