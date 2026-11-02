//! What oxipng tried and chose for one file, collected from its log stream.

use std::time::Duration;

use super::{Calibration, Stage, Trial, TrialSize};

/// One zopfli optimization pass over a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZopfliPass {
    /// Estimated compressed size of the block after this pass.
    pub bits: u64,
    /// Whether this pass beat every earlier pass on the block.
    pub improved: bool,
}

/// The compressor and filter oxipng settled on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    /// As oxipng prints it: `zc = 11` or `zopfli, zi = 15`.
    pub deflater: String,
    pub filter: String,
}

impl Chosen {
    /// Zopfli's iteration count, e.g. 15 for `zopfli, zi = 15`.
    #[must_use]
    pub fn zopfli_iterations(&self) -> Option<usize> {
        self.deflater.strip_prefix("zopfli, zi = ")?.parse().ok()
    }

    /// Human-readable compressor, e.g. `libdeflate level 11` or `Zopfli ×15`.
    #[must_use]
    pub fn compressor(&self) -> String {
        if let Some(level) = self.deflater.strip_prefix("zc = ") {
            format!("libdeflate level {level}")
        } else if let Some(iterations) = self.deflater.strip_prefix("zopfli, zi = ") {
            // Grouped like every other number shown, e.g. `Zopfli ×1,000`.
            let iterations = iterations
                .parse()
                .map_or_else(|_| iterations.to_owned(), crate::format::grouped);
            format!("Zopfli ×{iterations}")
        } else {
            self.deflater.clone()
        }
    }
}

/// Everything learned while optimizing one file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    /// Time spent per stage, in the order the stages were entered.
    pub stages: Vec<(Stage, Duration)>,
    /// oxipng's warnings and notes, without duplicates.
    pub notes: Vec<String>,
    /// Trials of reduced image variants (color type, bit depth, palette order).
    pub reduction_trials: Vec<Trial>,
    /// Trials of filters on the chosen image variant.
    pub filter_trials: Vec<Trial>,
    pub chosen: Option<Chosen>,
    /// Passes per zopfli block of the final compression, in block order.
    pub zopfli_blocks: Vec<Vec<ZopfliPass>>,
    /// Timings measured on this file, to estimate later files with.
    pub calibration: Calibration,
    /// The CPU threads oxipng had for this file.
    pub threads: Option<Threads>,
}

/// The CPU threads a file gets: an equal share of them, decided when it starts and kept until
/// it finishes. oxipng runs its trials side by side on them; a single compression (such as one
/// Zopfli run) uses one thread at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Threads {
    /// Threads in the file's own pool.
    pub used: usize,
    /// The CPU's threads (logical cores).
    pub available: usize,
    /// Files running or waiting to run when it started, which split the CPU's threads.
    pub sharing: usize,
}

/// The best trial for one image variant or one filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Image variant (for reductions) or filter name (for filters).
    pub label: String,
    pub size: TrialSize,
}

/// Summary of the zopfli passes that produced the image data.
#[derive(Debug, Clone, PartialEq)]
pub struct ZopfliSummary {
    /// Blocks zopfli split the image data into; it optimizes each one on its own, one after
    /// another.
    pub blocks: usize,
    /// The image data's size after each pass, in bytes: every block's best size after that
    /// many of its own passes, added up, with blocks that stopped earlier at their final size.
    /// The last entry matches the compressed image data to within a few bytes (zlib's header
    /// and checksum).
    pub totals: Vec<u64>,
    /// How many blocks got smaller on each pass; on the first pass every block counts.
    pub gains: Vec<usize>,
    /// 1-based pass of the last gain in any block.
    pub last_gain: usize,
    /// For every block: how many passes it ran, and the 1-based pass of its last gain.
    /// Blocks run different numbers of passes when "Stop after" ends them early.
    pub block_ends: Vec<(usize, usize)>,
    /// The longest run of passes without a gain that still ended in one, in any block. When
    /// zopfli gets stuck it shuffles its statistics and keeps searching, so a run of passes
    /// without a gain doesn't mean the block is done.
    pub longest_gap: usize,
    /// The iteration count zopfli ran with, when oxipng's log says.
    pub limit: Option<usize>,
}

impl ZopfliSummary {
    /// How much smaller the image data is after the last pass than after the first, as a
    /// fraction.
    #[must_use]
    pub fn gain(&self) -> f64 {
        match (self.totals.first(), self.totals.last()) {
            (Some(&first), Some(&last)) if first > 0 => {
                first.saturating_sub(last) as f64 / first as f64
            }
            _ => 0.0,
        }
    }

    /// Advice on the iteration count and "Stop after", when the passes give a clear signal.
    /// Only blocks that ran every allowed pass say anything about the iteration count; the
    /// others were stopped early by "Stop after", that many passes after their last gain.
    #[must_use]
    pub fn hint(&self) -> Option<String> {
        let number = |n: usize| crate::format::grouped(n as u64);
        let limit = self
            .limit
            .or_else(|| self.block_ends.iter().map(|&(passes, _)| passes).max())?;
        if limit < 2 {
            return None;
        }
        let Some(idle) = self
            .block_ends
            .iter()
            .filter(|&&(passes, _)| passes >= limit)
            .map(|&(passes, last_gain)| passes.saturating_sub(last_gain))
            .min()
        else {
            // Zopfli is deterministic, so with the same "Stop after" every block would stop
            // at the same pass again.
            let stop = self
                .block_ends
                .iter()
                .map(|&(passes, last_gain)| passes.saturating_sub(last_gain))
                .max()
                .unwrap_or(0);
            return Some(format!(
                "Every block stopped after {} passes without a gain, before reaching {} \
                 iterations. More iterations alone won't change the result, but a higher \
                 \"Stop after\" might: zopfli can still improve after a run like that.",
                number(stop),
                number(limit)
            ));
        };
        if idle == 0 {
            Some("Still improving at the last pass, so more iterations may help.".to_owned())
        } else if self.longest_gap >= idle {
            Some(format!(
                "No gains in the last {} passes, but gains came after runs of up to {} passes \
                 without one, so more iterations may help.",
                number(idle),
                number(self.longest_gap)
            ))
        } else if idle >= 3 {
            Some(format!(
                "No gains in the last {} passes, longer than any earlier run without a gain, \
                 so more iterations are unlikely to help.",
                number(idle)
            ))
        } else {
            None
        }
    }
}

impl Report {
    /// The best result per reduced image variant, smallest first.
    #[must_use]
    pub fn reductions(&self) -> Vec<Candidate> {
        best_by(&self.reduction_trials, |t| &t.image)
    }

    /// Filters tried on the image variant oxipng kept, smallest first. When several tie for
    /// smallest, the one oxipng chose comes first.
    ///
    /// With fast evaluation the two quick filters (None and Bigrams) are tried during
    /// reductions, and only the remaining filters in the filter stage, so both are merged.
    #[must_use]
    pub fn filters(&self) -> Vec<Candidate> {
        let winner = self.reductions().into_iter().next().map(|c| c.label);
        let trials: Vec<Trial> = self
            .reduction_trials
            .iter()
            .filter(|t| winner.as_deref() == Some(t.image.as_str()))
            .chain(&self.filter_trials)
            .cloned()
            .collect();
        let mut filters = best_by(&trials, |t| &t.filter);
        if let (Some(chosen), Some(best)) = (&self.chosen, filters.first().map(|c| c.size))
            && let Some(index) = filters
                .iter()
                .position(|c| c.label == chosen.filter && c.size == best)
        {
            let tied = filters.remove(index);
            filters.insert(0, tied);
        }
        filters
    }

    /// Statistics about the zopfli passes, if zopfli was used.
    #[must_use]
    pub fn zopfli(&self) -> Option<ZopfliSummary> {
        let blocks: Vec<&Vec<ZopfliPass>> = self
            .zopfli_blocks
            .iter()
            .filter(|b| !b.is_empty())
            .collect();
        let passes = blocks.iter().map(|b| b.len()).max()?;
        let mut bits = vec![0_u64; passes];
        let mut gains = vec![0_usize; passes];
        let mut block_ends = Vec::with_capacity(blocks.len());
        let mut longest_gap = 0;
        for block in &blocks {
            let mut best = u64::MAX;
            let mut last_gain = 1;
            for (index, total) in bits.iter_mut().enumerate() {
                if let Some(pass) = block.get(index)
                    && pass.bits < best
                {
                    best = pass.bits;
                    if index > 0 {
                        gains[index] += 1;
                        // Passes between the previous gain and this one (pass `index + 1`).
                        longest_gap = longest_gap.max(index - last_gain);
                        last_gain = index + 1;
                    }
                }
                *total += best;
            }
            block_ends.push((block.len(), last_gain));
        }
        gains[0] = blocks.len();
        Some(ZopfliSummary {
            blocks: blocks.len(),
            // Blocks aren't byte-aligned; only the whole stream is padded to a byte.
            totals: bits.iter().map(|b| b.div_ceil(8)).collect(),
            last_gain: gains.iter().rposition(|&g| g > 0).map_or(1, |i| i + 1),
            gains,
            block_ends,
            longest_gap,
            limit: self.chosen.as_ref().and_then(Chosen::zopfli_iterations),
        })
    }
}

/// Keeps the best trial per key (exact sizes beat cut-short ones), smallest first.
fn best_by<'a>(trials: &'a [Trial], key: impl Fn(&'a Trial) -> &'a String) -> Vec<Candidate> {
    let mut best: Vec<Candidate> = Vec::new();
    for trial in trials {
        let label = key(trial);
        match best.iter_mut().find(|c| &c.label == label) {
            Some(existing) if trial.size < existing.size => existing.size = trial.size,
            Some(_) => {}
            None => best.push(Candidate {
                label: label.clone(),
                size: trial.size,
            }),
        }
    }
    // `TrialSize` orders every exact size before every cut-short one.
    best.sort_by_key(|c| c.size);
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trial(image: &str, filter: &str, size: TrialSize) -> Trial {
        Trial {
            image: image.to_owned(),
            filter: filter.to_owned(),
            size,
        }
    }

    fn passes(bits_and_gains: &[(u64, bool)]) -> Vec<ZopfliPass> {
        bits_and_gains
            .iter()
            .map(|&(bits, improved)| ZopfliPass { bits, improved })
            .collect()
    }

    /// The trials oxipng logged for a 40-color image in `examples/log_probe.rs`.
    fn few_color_report() -> Report {
        use TrialSize::{Exact, Over};
        Report {
            reduction_trials: vec![
                trial("8-bit Indexed (40 colors)", "None", Exact(6269)),
                trial("8-bit Indexed (ezeng sort)", "None", Exact(6270)),
                trial("8-bit Indexed (40 colors)", "Bigrams", Exact(3311)),
                trial("8-bit Indexed (ezeng sort)", "Bigrams", Exact(3317)),
            ],
            filter_trials: vec![
                trial("8-bit Indexed (40 colors)", "Sub", Over(3311)),
                trial("8-bit Indexed (40 colors)", "Entropy", Over(3311)),
            ],
            chosen: Some(Chosen {
                deflater: "zc = 11".to_owned(),
                filter: "Bigrams".to_owned(),
            }),
            ..Report::default()
        }
    }

    #[test]
    fn ranks_reduction_variants_by_their_best_trial() {
        let reductions = few_color_report().reductions();
        assert_eq!(
            reductions,
            [
                Candidate {
                    label: "8-bit Indexed (40 colors)".to_owned(),
                    size: TrialSize::Exact(3311)
                },
                Candidate {
                    label: "8-bit Indexed (ezeng sort)".to_owned(),
                    size: TrialSize::Exact(3317)
                },
            ]
        );
    }

    #[test]
    fn merges_quick_filters_from_reductions_with_the_filter_stage() {
        let filters: Vec<(String, TrialSize)> = few_color_report()
            .filters()
            .into_iter()
            .map(|c| (c.label, c.size))
            .collect();
        assert_eq!(
            filters,
            [
                ("Bigrams".to_owned(), TrialSize::Exact(3311)),
                ("None".to_owned(), TrialSize::Exact(6269)),
                ("Sub".to_owned(), TrialSize::Over(3311)),
                ("Entropy".to_owned(), TrialSize::Over(3311)),
            ]
        );
    }

    #[test]
    fn puts_the_chosen_filter_first_when_sizes_tie() {
        use TrialSize::Exact;
        let report = Report {
            reduction_trials: vec![
                trial("4-bit Indexed (12 colors)", "None", Exact(4000)),
                trial("4-bit Indexed (12 colors)", "Bigrams", Exact(3779)),
            ],
            filter_trials: vec![
                trial("4-bit Indexed (12 colors)", "Entropy", Exact(3779)),
                trial("4-bit Indexed (12 colors)", "Sub", Exact(3900)),
            ],
            chosen: Some(Chosen {
                deflater: "zc = 11".to_owned(),
                filter: "Entropy".to_owned(),
            }),
            ..Report::default()
        };
        let order: Vec<String> = report.filters().into_iter().map(|c| c.label).collect();
        assert_eq!(order, ["Entropy", "Bigrams", "Sub", "None"]);
    }

    #[test]
    fn describes_the_compressor() {
        let chosen = |deflater: &str| Chosen {
            deflater: deflater.to_owned(),
            filter: "Sub".to_owned(),
        };
        assert_eq!(chosen("zc = 12").compressor(), "libdeflate level 12");
        assert_eq!(chosen("zopfli, zi = 15").compressor(), "Zopfli ×15");
    }

    #[test]
    fn summarizes_zopfli_passes() {
        let report = Report {
            zopfli_blocks: vec![
                passes(&[(3026, true), (3028, false), (3016, true)]),
                passes(&[
                    (2_047_600, true),
                    (2_031_000, true),
                    (2_031_500, false),
                    (2_015_500, true),
                    (2_016_000, false),
                    (2_015_900, false),
                    (2_015_800, false),
                ]),
            ],
            ..Report::default()
        };
        let summary = report.zopfli().expect("zopfli was used");
        assert_eq!(summary.blocks, 2);
        // Each block's best so far, added up, in bytes; the short block keeps its final size.
        assert_eq!(
            summary.totals,
            [
                256_329, 254_254, 254_252, 252_315, 252_315, 252_315, 252_315
            ]
        );
        assert_eq!(summary.gains, [2, 1, 1, 1, 0, 0, 0]);
        assert_eq!(summary.last_gain, 4);
        assert_eq!(summary.block_ends, [(3, 3), (7, 4)]);
        assert_eq!(summary.longest_gap, 1);
        assert!((summary.gain() - (256_329.0 - 252_315.0) / 256_329.0).abs() < 1e-9);
        assert_eq!(
            summary.hint().as_deref(),
            Some(
                "No gains in the last 3 passes, longer than any earlier run without a gain, so \
                 more iterations are unlikely to help."
            )
        );
    }

    #[test]
    fn hints_when_zopfli_is_still_improving() {
        let report = Report {
            zopfli_blocks: vec![
                passes(&[(500, true), (510, false), (520, false)]),
                passes(&[(100, true), (90, true), (80, true)]),
            ],
            ..Report::default()
        };
        let summary = report.zopfli().expect("zopfli was used");
        assert_eq!(summary.last_gain, 3, "the small block still improved");
        assert_eq!(
            summary.hint().as_deref(),
            Some("Still improving at the last pass, so more iterations may help.")
        );
        assert_eq!(Report::default().zopfli(), None);
    }

    #[test]
    fn hints_when_earlier_gains_came_after_longer_runs_without_one() {
        // A gain after five passes without one, then four passes without one at the limit.
        let mut block: Vec<(u64, bool)> = vec![(1000, true), (990, true)];
        block.extend([(995, false); 5]);
        block.push((980, true));
        block.extend([(985, false); 4]);
        let report = Report {
            zopfli_blocks: vec![passes(&block)],
            ..Report::default()
        };
        let summary = report.zopfli().expect("zopfli was used");
        assert_eq!(summary.longest_gap, 5);
        assert_eq!(
            summary.hint().as_deref(),
            Some(
                "No gains in the last 4 passes, but gains came after runs of up to 5 passes \
                 without one, so more iterations may help."
            )
        );
    }

    #[test]
    fn blocks_stopped_early_point_to_stop_after() {
        // "Stop after 20" with 1,000 iterations: each block ends 20 passes after its own last
        // gain, the largest after 24 passes and another after 82.
        let mut largest: Vec<(u64, bool)> = (0..4).map(|i| (900_000 - i * 100, true)).collect();
        largest.extend([(905_000, false); 20]);
        let mut other: Vec<(u64, bool)> = (0..62).map(|i| (10_000 - i, true)).collect();
        other.extend([(10_100, false); 20]);
        let mut report = Report {
            zopfli_blocks: vec![passes(&largest), passes(&other)],
            chosen: Some(Chosen {
                deflater: "zopfli, zi = 1000".to_owned(),
                filter: "None".to_owned(),
            }),
            ..Report::default()
        };
        let summary = report.zopfli().expect("zopfli was used");
        assert_eq!(summary.limit, Some(1000));
        assert_eq!(
            summary.hint().as_deref(),
            Some(
                "Every block stopped after 20 passes without a gain, before reaching 1,000 \
                 iterations. More iterations alone won't change the result, but a higher \
                 \"Stop after\" might: zopfli can still improve after a run like that."
            )
        );

        // Without the iteration count, the longest block stands in for it.
        report.chosen = None;
        assert_eq!(
            report.zopfli().expect("zopfli was used").hint().as_deref(),
            Some(
                "No gains in the last 20 passes, longer than any earlier run without a gain, \
                 so more iterations are unlikely to help."
            )
        );
    }
}
