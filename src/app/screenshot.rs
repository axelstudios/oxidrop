//! Development aid for checking the UI without clicking around:
//!
//! ```text
//! OXIDROP_SCREENSHOT=shot.png [OXIDROP_SCREENSHOT_DELAY=4] [OXIDROP_SCREENSHOT_THEME=light]
//!     [OXIDROP_SCREENSHOT_SIZE=1120x860] oxidrop some/folder
//! ```
//!
//! starts with demo settings (Zopfli on, so progress bars stay visible) and throwaway storage,
//! selects the largest finished file, saves a screenshot after the delay, and closes the window.
//! The theme follows the system unless `OXIDROP_SCREENSHOT_THEME` is `light` or `dark`, and the
//! window has its usual size unless `OXIDROP_SCREENSHOT_SIZE` gives one, in points.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use egui::{ColorImage, Context, ThemePreference, ViewportCommand};

use crate::settings::Settings;

/// How long before the capture [`Screenshot::is_due`] turns true.
const SETTLE_TIME: Duration = Duration::from_millis(500);

pub(super) struct Screenshot {
    path: PathBuf,
    at: Instant,
    theme: ThemePreference,
    window_size: Option<[f32; 2]>,
    requested: bool,
}

impl Screenshot {
    /// Reads `OXIDROP_SCREENSHOT`, `OXIDROP_SCREENSHOT_DELAY` (seconds, default 3),
    /// `OXIDROP_SCREENSHOT_THEME`, and `OXIDROP_SCREENSHOT_SIZE`.
    #[must_use]
    pub(super) fn from_env() -> Option<Self> {
        let path = std::env::var_os("OXIDROP_SCREENSHOT")?;
        let delay = std::env::var("OXIDROP_SCREENSHOT_DELAY")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|s| s.is_finite() && *s >= 0.0)
            .unwrap_or(3.0);
        let theme = match std::env::var("OXIDROP_SCREENSHOT_THEME").as_deref() {
            Ok("light") => ThemePreference::Light,
            Ok("dark") => ThemePreference::Dark,
            _ => ThemePreference::System,
        };
        let window_size = std::env::var("OXIDROP_SCREENSHOT_SIZE")
            .ok()
            .and_then(|s| parse_size(&s));
        Some(Self {
            path: path.into(),
            at: Instant::now() + Duration::from_secs_f64(delay),
            theme,
            window_size,
            requested: false,
        })
    }

    /// The theme to capture.
    #[must_use]
    pub(super) fn theme(&self) -> ThemePreference {
        self.theme
    }

    /// The window's inner size to capture, in points, if not the usual one.
    #[must_use]
    pub(super) fn window_size(&self) -> Option<[f32; 2]> {
        self.window_size
    }

    /// A fresh storage file for eframe, so screenshots don't depend on saved window and panel
    /// sizes and don't overwrite them. (`NativeOptions::persistence_path` is the RON file
    /// itself, not a folder.)
    #[must_use]
    pub(super) fn fresh_storage_file() -> PathBuf {
        let file = std::env::temp_dir().join("oxidrop-screenshot.ron");
        // Missing on the first run; anything else shows up as stale state in the screenshot.
        let _ = std::fs::remove_file(&file);
        file
    }

    /// Settings used in screenshot mode instead of the saved ones: files start as they are
    /// added (that's off by default), with Zopfli and a small parallelism limit, so running and
    /// queued files are both visible.
    #[must_use]
    pub(super) fn settings() -> Settings {
        Settings {
            zopfli: true,
            parallel_files: 3,
            auto_start: true,
            ..Settings::default()
        }
    }

    /// True shortly before the screenshot is taken: time to set up what it should show, with
    /// a few frames left for panels to settle.
    #[must_use]
    pub(super) fn is_due(&self) -> bool {
        !self.requested && Instant::now() + SETTLE_TIME >= self.at
    }

    /// Requests the screenshot once the delay has passed.
    pub(super) fn tick(&mut self, ctx: &Context) {
        if self.requested {
            return;
        }
        let now = Instant::now();
        if now < self.at {
            ctx.request_repaint_after(self.at - now);
            return;
        }
        self.requested = true;
        let path = self.path.clone();
        let closer = ctx.clone();
        ctx.request_screenshot(move |image| {
            match save_png(&path, &image) {
                Ok(()) => eprintln!("screenshot saved to {}", path.display()),
                Err(err) => eprintln!("could not save screenshot: {err}"),
            }
            closer.send_viewport_cmd(ViewportCommand::Close);
            closer.request_repaint();
        });
        ctx.request_repaint();
    }
}

/// A size such as `1120x860`, both sides positive.
fn parse_size(text: &str) -> Option<[f32; 2]> {
    let (width, height) = text.split_once('x')?;
    let side = |s: &str| {
        s.trim()
            .parse::<f32>()
            .ok()
            .filter(|n| n.is_finite() && *n > 0.0)
    };
    Some([side(width)?, side(height)?])
}

/// Encodes the screenshot with oxipng itself, so no extra image crate is needed.
fn save_png(path: &Path, image: &ColorImage) -> Result<(), String> {
    let [width, height] = image.size.map(|n| u32::try_from(n).unwrap_or(u32::MAX));
    let rgba = image
        .pixels
        .iter()
        .flat_map(egui::Color32::to_srgba_unmultiplied)
        .collect();
    let raw = oxipng::RawImage::new(
        width,
        height,
        oxipng::ColorType::RGBA,
        oxipng::BitDepth::Eight,
        rgba,
    )
    .map_err(|e| e.to_string())?;
    let png = raw
        .create_optimized_png(&oxipng::Options::from_preset(1))
        .map_err(|e| e.to_string())?;
    std::fs::write(path, png).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_window_sizes() {
        assert_eq!(parse_size("1120x860"), Some([1120.0, 860.0]));
        assert_eq!(parse_size("1120"), None);
        assert_eq!(parse_size("0x860"), None);
        assert_eq!(parse_size("widexhigh"), None);
    }
}
