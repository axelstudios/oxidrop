//! Turns oxipng and zopfli log lines into typed signals.
//!
//! The patterns match oxipng 10.2.1 and zopfli 0.8.4; `examples/log_probe.rs` prints the raw
//! stream. Unknown lines are ignored, so a format change in a future oxipng degrades to
//! indeterminate progress and a thinner report instead of breaking optimization.

/// One event observed in the log stream of a single file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// `    1200x900 pixels, PNG format` — the input has been decoded.
    ImageSize { width: u32, height: u32 },
    /// `    8-bit RGB, non-interlaced` for the input, or
    /// `Transformed image to 8-bit Indexed (40 colors), interlaced` after reductions.
    Format {
        bits_per_pixel: u32,
        interlaced: bool,
        transformed: bool,
    },
    /// One filter/reduction trial finished (`Eval: …`).
    TrialFinished(Trial),
    /// `Evaluating 4 filters` (fast evaluation) or `Trying 6 filters with zc = 12`
    /// (full trials, where each trial is a final-quality compression).
    TrialsPlanned { count: usize, final_quality: bool },
    /// `Trying filter Entropy with zopfli, zi = 5` — the single final compression.
    FinalCompression { zopfli: bool },
    /// `    zopfli, zi = 5, f = Entropy` after `Found better result:`.
    Chosen { deflater: String, filter: String },
    /// `block split points: 6170 16451 (hex: 181a 4043)` — layout of a 1 MB zopfli block.
    ZopfliSplit { points: Vec<usize> },
    /// `Iteration 3: 15257 bit` — one zopfli optimization pass over a block. Passes that did
    /// not beat the best so far are logged with `(no improvement)`.
    ZopfliIteration {
        index: u32,
        bits: u64,
        improved: bool,
    },
    /// `compressed block size: 1855 (1k) (unc: 6134)` — a zopfli block was written.
    ZopfliBlockWritten { uncompressed: usize },
    /// `Recompressed fdAT #3 : …` — an APNG frame was recompressed.
    FrameRecompressed,
    /// A warning from oxipng, e.g. `APNG detected, disabling all reductions`.
    Warning(String),
    /// Something oxipng decided that explains the result, e.g.
    /// `Replacing iCCP chunk with equivalent sRGB chunk`.
    Note(String),
}

/// One logged trial: an image variant compressed with one filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trial {
    /// Bit depth and color type or reduction, e.g. `8-bit Indexed (luma sort)`.
    pub image: String,
    pub filter: String,
    pub size: TrialSize,
}

/// Estimated output size of a trial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TrialSize {
    Exact(usize),
    /// The trial was cut short once it exceeded the best size found so far.
    Over(usize),
}

impl TrialSize {
    /// The exact size, or the size it was known to exceed.
    #[must_use]
    pub const fn bytes(self) -> usize {
        match self {
            Self::Exact(n) | Self::Over(n) => n,
        }
    }
}

/// Parses one log record. Returns `None` for lines that carry no useful information.
#[must_use]
pub fn parse(target: &str, level: log::Level, message: &str) -> Option<Signal> {
    if target.starts_with("zopfli") {
        return parse_zopfli(message);
    }
    if !target.starts_with("oxipng") {
        return None;
    }
    let line = message.trim();
    if level == log::Level::Warn {
        return Some(Signal::Warning(line.to_owned()));
    }

    if let Some(rest) = line.strip_prefix("Eval:") {
        return parse_trial(rest).map(Signal::TrialFinished);
    }
    if let Some(rest) = line.strip_prefix("Evaluating ") {
        let count = rest.strip_suffix(" filters").and_then(|n| n.parse().ok())?;
        return Some(Signal::TrialsPlanned {
            count,
            final_quality: false,
        });
    }
    if let Some(rest) = line.strip_prefix("Trying ") {
        if rest.starts_with("filter ") {
            return Some(Signal::FinalCompression {
                zopfli: rest.contains("with zopfli"),
            });
        }
        let count = rest
            .split_once(" filters with ")
            .and_then(|(n, _)| n.parse().ok())?;
        return Some(Signal::TrialsPlanned {
            count,
            final_quality: true,
        });
    }
    if let Some(rest) = line.strip_prefix("Transformed image to ") {
        return parse_format(rest, true);
    }
    if line.starts_with("Recompressed fdAT") {
        return Some(Signal::FrameRecompressed);
    }
    if let Some(dims) = line.strip_suffix(" pixels, PNG format") {
        let (w, h) = dims.split_once('x')?;
        return Some(Signal::ImageSize {
            width: w.parse().ok()?,
            height: h.parse().ok()?,
        });
    }
    if (line.starts_with("zc = ") || line.starts_with("zopfli, "))
        && let Some((deflater, filter)) = line.rsplit_once(", f = ")
    {
        return Some(Signal::Chosen {
            deflater: deflater.to_owned(),
            filter: filter.to_owned(),
        });
    }
    if is_note(line) {
        return Some(Signal::Note(line.to_owned()));
    }
    if line.starts_with(|c: char| c.is_ascii_digit()) && line.contains("-bit ") {
        return parse_format(line, false);
    }
    None
}

/// Informational lines about chunks and skipped reductions that explain a result.
fn is_note(line: &str) -> bool {
    line.starts_with("Replacing iCCP")
        || line.starts_with("Recompressed iCCP")
        || line.starts_with("Disabling ")
        || (line.starts_with("Removing ") && line.contains(" chunk"))
}

/// Parses ` 8-bit Indexed (40 colors)     Sub       >3311 bytes`.
fn parse_trial(rest: &str) -> Option<Trial> {
    let rest = rest.trim().strip_suffix(" bytes")?;
    let (head, size) = rest.rsplit_once(char::is_whitespace)?;
    let size = match size.strip_prefix('>') {
        Some(bound) => TrialSize::Over(bound.parse().ok()?),
        None => TrialSize::Exact(size.parse().ok()?),
    };
    let (image, filter) = head.trim_end().rsplit_once(char::is_whitespace)?;
    Some(Trial {
        image: image.trim().to_owned(),
        filter: filter.to_owned(),
        size,
    })
}

/// Parses `8-bit Indexed (40 colors), interlaced`.
fn parse_format(text: &str, transformed: bool) -> Option<Signal> {
    let (format, interlacing) = text.rsplit_once(", ")?;
    let (depth, color) = format.split_once("-bit ")?;
    let depth: u32 = depth.parse().ok()?;
    let channels = if color.starts_with("RGB + Alpha") {
        4
    } else if color.starts_with("RGB") {
        3
    } else if color.starts_with("Grayscale + Alpha") {
        2
    } else if color.starts_with("Grayscale") || color.starts_with("Indexed") {
        1
    } else {
        return None;
    };
    Some(Signal::Format {
        bits_per_pixel: depth * channels,
        interlaced: interlacing == "interlaced",
        transformed,
    })
}

fn parse_zopfli(message: &str) -> Option<Signal> {
    if let Some(rest) = message.strip_prefix("Iteration ") {
        let (index, cost) = rest.split_once(": ")?;
        let (bits, improved) = match cost.strip_suffix(" bit (no improvement)") {
            Some(bits) => (bits, false),
            None => (cost.strip_suffix(" bit")?, true),
        };
        let bits: f64 = bits.parse().ok()?;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "bit costs are positive and far below 2^53"
        )]
        let bits = bits.round() as u64;
        return Some(Signal::ZopfliIteration {
            index: index.parse().ok()?,
            bits,
            improved,
        });
    }
    if let Some(rest) = message.strip_prefix("block split points:") {
        let decimal = rest.split_once("(hex:").map_or(rest, |(points, _)| points);
        let points = decimal
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        return Some(Signal::ZopfliSplit { points });
    }
    if message.starts_with("compressed block size:") {
        let unc = message.split_once("(unc: ")?.1.split_once(')')?.0;
        return Some(Signal::ZopfliBlockWritten {
            uncompressed: unc.parse().ok()?,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Level::{Debug, Info, Trace, Warn};

    fn oxipng(level: log::Level, line: &str) -> Option<Signal> {
        parse("oxipng", level, line)
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "mirrors the return type of `parse`"
    )]
    fn trial(image: &str, filter: &str, size: TrialSize) -> Option<Signal> {
        Some(Signal::TrialFinished(Trial {
            image: image.to_owned(),
            filter: filter.to_owned(),
            size,
        }))
    }

    #[test]
    fn parses_header_lines() {
        assert_eq!(
            oxipng(Debug, "    1200x900 pixels, PNG format"),
            Some(Signal::ImageSize {
                width: 1200,
                height: 900
            })
        );
        assert_eq!(
            oxipng(Debug, "    8-bit RGB, non-interlaced"),
            Some(Signal::Format {
                bits_per_pixel: 24,
                interlaced: false,
                transformed: false
            })
        );
        assert_eq!(
            oxipng(Debug, "    16-bit RGB + Alpha, interlaced"),
            Some(Signal::Format {
                bits_per_pixel: 64,
                interlaced: true,
                transformed: false
            })
        );
        assert_eq!(
            oxipng(
                Debug,
                "Transformed image to 8-bit Indexed (40 colors), interlaced"
            ),
            Some(Signal::Format {
                bits_per_pixel: 8,
                interlaced: true,
                transformed: true
            })
        );
        assert_eq!(
            oxipng(
                Debug,
                "Transformed image to 2-bit Grayscale, non-interlaced"
            ),
            Some(Signal::Format {
                bits_per_pixel: 2,
                interlaced: false,
                transformed: true
            })
        );
    }

    #[test]
    fn parses_trial_lines() {
        assert_eq!(
            oxipng(Trace, "Evaluating 4 filters"),
            Some(Signal::TrialsPlanned {
                count: 4,
                final_quality: false
            })
        );
        assert_eq!(
            oxipng(Debug, "Trying 6 filters with zc = 12"),
            Some(Signal::TrialsPlanned {
                count: 6,
                final_quality: true
            })
        );
        assert_eq!(
            parse(
                "oxipng::evaluate",
                Trace,
                "Eval: 8-bit RGB                     Sub        929820 bytes"
            ),
            trial("8-bit RGB", "Sub", TrialSize::Exact(929_820))
        );
        assert_eq!(
            parse(
                "oxipng::evaluate",
                Trace,
                "Eval: 8-bit Indexed (40 colors)     Sub       >3311 bytes"
            ),
            trial("8-bit Indexed (40 colors)", "Sub", TrialSize::Over(3311))
        );
        assert_eq!(
            parse(
                "oxipng::evaluate",
                Trace,
                "Eval: 8-bit Indexed (battiato sort) None       9872 bytes"
            ),
            trial(
                "8-bit Indexed (battiato sort)",
                "None",
                TrialSize::Exact(9872)
            )
        );
        assert_eq!(
            parse(
                "oxipng::evaluate",
                Trace,
                "Eval: 16-bit RGB + Alpha             BigEnt     831741 bytes"
            ),
            trial("16-bit RGB + Alpha", "BigEnt", TrialSize::Exact(831_741))
        );
        assert_eq!(
            oxipng(Debug, "Trying filter Entropy with zc = 11"),
            Some(Signal::FinalCompression { zopfli: false })
        );
        assert_eq!(
            oxipng(Debug, "Trying filter Bigrams with zopfli, zi = 3"),
            Some(Signal::FinalCompression { zopfli: true })
        );
        assert_eq!(
            oxipng(Debug, "Recompressed fdAT #3 : 1200 (40 bytes decrease)"),
            Some(Signal::FrameRecompressed)
        );
    }

    #[test]
    fn parses_the_chosen_result() {
        assert_eq!(
            oxipng(Debug, "    zc = 11, f = Entropy"),
            Some(Signal::Chosen {
                deflater: "zc = 11".to_owned(),
                filter: "Entropy".to_owned()
            })
        );
        assert_eq!(
            oxipng(Debug, "    zopfli, zi = 15, f = Bigrams"),
            Some(Signal::Chosen {
                deflater: "zopfli, zi = 15".to_owned(),
                filter: "Bigrams".to_owned()
            })
        );
    }

    #[test]
    fn parses_zopfli_lines() {
        let z = |target, line| parse(target, Debug, line);
        assert_eq!(
            z(
                "zopfli::blocksplitter",
                "block split points: 6170 16451 (hex: 181a 4043)"
            ),
            Some(Signal::ZopfliSplit {
                points: vec![6170, 16451]
            })
        );
        assert_eq!(
            z("zopfli::blocksplitter", "block split points:  (hex: )"),
            Some(Signal::ZopfliSplit { points: vec![] })
        );
        assert_eq!(
            z("zopfli::squeeze", "Iteration 3: 15257 bit"),
            Some(Signal::ZopfliIteration {
                index: 3,
                bits: 15257,
                improved: true
            })
        );
        assert_eq!(
            parse(
                "zopfli::squeeze",
                Trace,
                "Iteration 4: 15263.5 bit (no improvement)"
            ),
            Some(Signal::ZopfliIteration {
                index: 4,
                bits: 15264,
                improved: false
            })
        );
        assert_eq!(
            z(
                "zopfli::deflate",
                "compressed block size: 248268 (242k) (unc: 971064)"
            ),
            Some(Signal::ZopfliBlockWritten {
                uncompressed: 971_064
            })
        );
        assert_eq!(z("zopfli::deflate", "treesize: 42"), None);
    }

    #[test]
    fn keeps_warnings_and_notes_and_ignores_noise() {
        assert_eq!(
            oxipng(Warn, "APNG detected, disabling all reductions"),
            Some(Signal::Warning(
                "APNG detected, disabling all reductions".to_owned()
            ))
        );
        for note in [
            "Replacing iCCP chunk with equivalent sRGB chunk",
            "Removing iCCP chunk due to conflict with sRGB chunk",
            "Recompressed iCCP chunk: 2050 (1024 bytes decrease)",
            "Disabling grayscale reduction due to presence of sRGB or iCCP chunk",
            "Removing bKGD chunk as it no longer matches the color type",
        ] {
            assert_eq!(oxipng(Trace, note), Some(Signal::Note(note.to_owned())));
        }
        assert_eq!(oxipng(Info, "Processing from memory"), None);
        assert_eq!(oxipng(Debug, "    IDAT size = 918505 bytes"), None);
        assert_eq!(oxipng(Debug, "Found better result:"), None);
        assert_eq!(
            oxipng(
                Debug,
                "    file size = 834402 bytes (84160 bytes = 9.16% decrease)"
            ),
            None
        );
        assert_eq!(
            parse("wgpu_core::device", Debug, "Iteration 3: 1 bit"),
            None
        );
    }
}
