//! User-facing optimization settings and their mapping onto [`oxipng::Options`].

use std::{
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
    time::Duration,
};

use oxipng::{Deflater, Options, StripChunks, ZopfliOptions};
use serde::{Deserialize, Serialize};

/// Which metadata chunks to remove.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MetadataMode {
    /// Keep every chunk.
    Keep,
    /// Remove chunks that don't affect how the image is displayed (oxipng `--strip safe`).
    #[default]
    StripSafe,
    /// Remove every non-critical chunk, including color profiles, but keep the animation.
    /// (oxipng's `--strip all` would also remove that, leaving an APNG only its first frame.)
    StripAll,
}

/// The APNG chunks: the frame count, then each frame's position and delay, and its pixels.
const ANIMATION_CHUNKS: [[u8; 4]; 3] = [*b"acTL", *b"fcTL", *b"fdAT"];

impl MetadataMode {
    pub const ALL: [Self; 3] = [Self::StripSafe, Self::Keep, Self::StripAll];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Keep => "Keep all",
            Self::StripSafe => "Strip safe chunks",
            Self::StripAll => "Strip all",
        }
    }
}

/// What to do with Adam7 interlacing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Interlacing {
    /// Leave interlacing as it is.
    Keep,
    /// Remove interlacing; almost always smaller.
    #[default]
    Remove,
    /// Interlace the output (only applied when it doesn't make the file bigger).
    Add,
}

impl Interlacing {
    pub const ALL: [Self; 3] = [Self::Remove, Self::Keep, Self::Add];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Keep => "Keep as is",
            Self::Remove => "Remove",
            Self::Add => "Add (Adam7)",
        }
    }
}

/// Where optimized files go.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputTarget {
    /// Replace the original, but only when the result is smaller.
    #[default]
    InPlace,
    /// Write into [`Settings::output_dir`], keeping the file name.
    Folder,
}

/// Everything the user can configure in the settings panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent checkboxes in the settings panel"
)]
pub struct Settings {
    /// oxipng preset level, 0 (fastest) to 6 (smallest).
    pub level: u8,
    pub zopfli: bool,
    pub zopfli_iterations: u32,
    /// Move on to the next Zopfli block after this many passes without a gain
    /// (oxipng's `--ziwi`), instead of always running every iteration.
    pub zopfli_stop_early: bool,
    pub zopfli_stop_after: u32,
    pub metadata: MetadataMode,
    pub interlacing: Interlacing,
    /// Allow changing the color of fully transparent pixels.
    pub optimize_alpha: bool,
    pub output: OutputTarget,
    pub output_dir: PathBuf,
    pub preserve_modified_time: bool,
    /// How many files are optimized at the same time.
    pub parallel_files: usize,
    /// Start optimizing as soon as files are added.
    pub auto_start: bool,
    /// Stop starting new trials once a file has run for `time_limit_secs` (oxipng's
    /// `--timeout`); the best result found so far is kept. oxipng doesn't stop a Zopfli
    /// compression under way, so the runner does.
    pub time_limit: bool,
    pub time_limit_secs: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            level: 2,
            zopfli: false,
            zopfli_iterations: Self::DEFAULT_ZOPFLI_ITERATIONS,
            zopfli_stop_early: false,
            zopfli_stop_after: Self::DEFAULT_ZOPFLI_STOP_AFTER,
            metadata: MetadataMode::default(),
            interlacing: Interlacing::default(),
            optimize_alpha: false,
            output: OutputTarget::default(),
            output_dir: PathBuf::new(),
            preserve_modified_time: true,
            parallel_files: available_cores(),
            auto_start: false,
            time_limit: false,
            time_limit_secs: Self::DEFAULT_TIME_LIMIT_SECS,
        }
    }
}

impl Settings {
    pub const MAX_LEVEL: u8 = 6;
    pub const DEFAULT_ZOPFLI_ITERATIONS: u32 = 15;
    pub const MAX_ZOPFLI_ITERATIONS: u32 = 1000;
    pub const DEFAULT_ZOPFLI_STOP_AFTER: u32 = 5;
    pub const DEFAULT_TIME_LIMIT_SECS: u32 = 60;
    pub const MAX_TIME_LIMIT_SECS: u32 = 24 * 60 * 60;

    /// Builds the oxipng options for these settings.
    #[must_use]
    pub fn oxipng_options(&self) -> Options {
        let mut opts = Options::from_preset(self.level.min(Self::MAX_LEVEL));
        if self.zopfli {
            let count = |n: u32| NonZeroU64::new(u64::from(n)).unwrap_or(NonZeroU64::MIN);
            opts.deflater = Deflater::Zopfli(ZopfliOptions {
                iteration_count: count(self.zopfli_iterations()),
                iterations_without_improvement: match self.zopfli_stop_after() {
                    Some(passes) => count(passes),
                    None => NonZeroU64::MAX,
                },
                ..ZopfliOptions::default()
            });
        }
        opts.strip = match self.metadata {
            MetadataMode::Keep => StripChunks::None,
            MetadataMode::StripSafe => StripChunks::Safe,
            MetadataMode::StripAll => StripChunks::Keep(ANIMATION_CHUNKS.into()),
        };
        opts.interlace = match self.interlacing {
            Interlacing::Keep => None,
            Interlacing::Remove => Some(false),
            Interlacing::Add => Some(true),
        };
        opts.optimize_alpha = self.optimize_alpha;
        opts.timeout = self.time_limit();
        opts
    }

    /// The time limit per file, when it is on.
    #[must_use]
    pub fn time_limit(&self) -> Option<Duration> {
        self.time_limit.then(|| {
            Duration::from_secs(u64::from(
                self.time_limit_secs.clamp(1, Self::MAX_TIME_LIMIT_SECS),
            ))
        })
    }

    /// Zopfli iteration count, clamped to the supported range.
    #[must_use]
    pub fn zopfli_iterations(&self) -> u32 {
        self.zopfli_iterations.clamp(1, Self::MAX_ZOPFLI_ITERATIONS)
    }

    /// Passes without a gain after which Zopfli moves on, when stopping early is enabled
    /// and the limit is below the iteration count (otherwise it would never be reached).
    #[must_use]
    pub fn zopfli_stop_after(&self) -> Option<u32> {
        let limit = self.zopfli_stop_after.max(1);
        (self.zopfli_stop_early && limit < self.zopfli_iterations()).then_some(limit)
    }

    /// Files to optimize at once, clamped to at least one.
    #[must_use]
    pub fn parallel_files(&self) -> usize {
        self.parallel_files.max(1)
    }

    /// One-line summary for the toolbar, e.g. `Level 4 · Zopfli ×15 · 8 files at a time`.
    #[must_use]
    pub fn summary(&self) -> String {
        let deflater = if self.zopfli {
            format!(
                "Zopfli ×{}",
                crate::format::grouped(u64::from(self.zopfli_iterations()))
            )
        } else {
            "libdeflate".to_owned()
        };
        let files = match self.parallel_files() {
            1 => "1 file at a time".to_owned(),
            n => format!("{} files at a time", crate::format::grouped(n as u64)),
        };
        format!(
            "Level {} · {deflater} · {files}",
            self.level.min(Self::MAX_LEVEL)
        )
    }
}

/// Number of logical CPU cores, or 1 if unknown.
#[must_use]
pub fn available_cores() -> usize {
    std::thread::available_parallelism().map_or(1, NonZeroUsize::get)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_oxipng_level_2_with_safe_stripping() {
        let opts = Settings::default().oxipng_options();
        let preset = Options::from_preset(2);
        assert_eq!(opts.filters, preset.filters);
        assert_eq!(opts.deflater, preset.deflater);
        assert_eq!(opts.strip, StripChunks::Safe);
        assert_eq!(opts.interlace, Some(false));
        assert!(!opts.optimize_alpha);
    }

    #[test]
    fn zopfli_replaces_the_deflater_and_keeps_the_preset_filters() {
        let settings = Settings {
            level: 4,
            zopfli: true,
            zopfli_iterations: 30,
            ..Settings::default()
        };
        let opts = settings.oxipng_options();
        assert_eq!(opts.filters, Options::from_preset(4).filters);
        let Deflater::Zopfli(zopfli) = opts.deflater else {
            panic!("expected zopfli, got {:?}", opts.deflater);
        };
        assert_eq!(zopfli.iteration_count.get(), 30);
    }

    #[test]
    fn zopfli_can_stop_a_block_after_passes_without_gain() {
        let zopfli = |settings: &Settings| match settings.oxipng_options().deflater {
            Deflater::Zopfli(options) => options.iterations_without_improvement,
            other @ Deflater::Libdeflater { .. } => panic!("expected zopfli, got {other:?}"),
        };
        let mut settings = Settings {
            zopfli: true,
            zopfli_iterations: 50,
            ..Settings::default()
        };
        assert_eq!(zopfli(&settings), NonZeroU64::MAX, "off by default");

        settings.zopfli_stop_early = true;
        settings.zopfli_stop_after = 8;
        assert_eq!(zopfli(&settings).get(), 8);

        settings.zopfli_iterations = 8;
        assert_eq!(
            settings.zopfli_stop_after(),
            None,
            "a limit of 8 of 8 is never reached"
        );
        assert_eq!(zopfli(&settings), NonZeroU64::MAX);
    }

    #[test]
    fn time_limit_maps_to_the_oxipng_timeout() {
        let mut settings = Settings::default();
        assert_eq!(settings.oxipng_options().timeout, None, "off by default");
        settings.time_limit = true;
        assert_eq!(
            settings.oxipng_options().timeout,
            Some(Duration::from_secs(60))
        );
        settings.time_limit_secs = 0;
        assert_eq!(
            settings.oxipng_options().timeout,
            Some(Duration::from_secs(1))
        );
    }

    #[test]
    fn clamps_out_of_range_values() {
        let settings = Settings {
            level: 42,
            zopfli: true,
            zopfli_iterations: 0,
            parallel_files: 0,
            ..Settings::default()
        };
        assert_eq!(settings.zopfli_iterations(), 1);
        assert_eq!(settings.parallel_files(), 1);
        assert_eq!(settings.summary(), "Level 6 · Zopfli ×1 · 1 file at a time");
        let many = Settings {
            zopfli_iterations: 1_000,
            ..settings.clone()
        };
        assert_eq!(many.summary(), "Level 6 · Zopfli ×1,000 · 1 file at a time");
        assert_eq!(
            settings.oxipng_options().filters,
            Options::from_preset(6).filters
        );
    }

    #[test]
    fn maps_metadata_and_interlacing_modes() {
        let keep = Settings {
            metadata: MetadataMode::Keep,
            interlacing: Interlacing::Keep,
            ..Settings::default()
        };
        assert_eq!(keep.oxipng_options().strip, StripChunks::None);
        assert_eq!(keep.oxipng_options().interlace, None);

        let all = Settings {
            metadata: MetadataMode::StripAll,
            interlacing: Interlacing::Add,
            ..Settings::default()
        };
        assert_eq!(
            all.oxipng_options().strip,
            StripChunks::Keep(oxipng::indexset![*b"acTL", *b"fcTL", *b"fdAT"])
        );
        assert_eq!(all.oxipng_options().interlace, Some(true));
    }
}
