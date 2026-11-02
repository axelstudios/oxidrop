//! The UI font: Inter, for all text.
//!
//! Inter 4.001 Variable (SIL Open Font License 1.1, see `assets/fonts/Inter-LICENSE.txt`), so
//! text can take any weight on its `wght` axis (100 to 900, regular is 400): headings are
//! semibold (see `style::heading`). egui's "strong" text is only a brighter color.
//!
//! egui's bundled fonts (Ubuntu Light, and Hack for monospace) are left out of the build: Inter
//! has every character they add that the UI uses, and eframe's system font fallback finds the
//! rest, such as emoji and CJK in file names. Nothing in the UI is monospace; Inter stands in.

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

const INTER: &[u8] = include_bytes!("../../assets/fonts/InterVariable.ttf");

/// Inter as the only font of both families.
#[must_use]
pub(super) fn definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::empty();
    fonts
        .font_data
        .insert("Inter".to_owned(), Arc::new(FontData::from_static(INTER)));
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts.families.insert(family, vec!["Inter".to_owned()]);
    }
    fonts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inter_is_the_only_font() {
        let fonts = definitions();
        assert_eq!(fonts.font_data.keys().collect::<Vec<_>>(), ["Inter"]);
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            assert_eq!(fonts.families[&family], ["Inter"], "{family:?}");
        }
        // A TrueType font file.
        assert_eq!(INTER.get(..4), Some([0, 1, 0, 0].as_slice()));
    }

    #[test]
    fn egui_bundled_fonts_are_not_built_in() {
        // eframe's "default_fonts" feature is off (see Cargo.toml).
        assert!(FontDefinitions::default().font_data.is_empty());
    }
}
