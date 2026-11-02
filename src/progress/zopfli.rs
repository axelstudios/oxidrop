//! Zopfli progress from its block layout, its pass log, and measured pass times.
//!
//! Zopfli compresses its input in independent 1 MB master blocks. For each one it logs the
//! split points, runs `iterations` passes over each sub-block, then logs every sub-block it
//! writes. Progress is weighted by bytes: finished master blocks count fully, and the current
//! one counts the sub-blocks before the active one plus the active one's share of passes.
//!
//! A block's first pass also builds zopfli's match cache, so it costs far more than later
//! passes: about 32× on photographic data and about 2× on flat graphics (measured with
//! `examples/log_probe.rs`). That ratio is steady within a file, so it is measured on the first
//! large block and used for the rest; until then a middle-of-the-road default applies.

use std::time::Instant;

use super::{Signal, calibration::ZopfliTiming};

/// Zopfli compresses its input in independent master blocks of this many bytes.
pub(super) const MASTER_BLOCK: usize = 1_000_000;

/// First-pass weight before anything has been measured (between the ~2× of flat graphics and
/// the ~32× of photos).
const DEFAULT_FIRST_PASS_WEIGHT: f64 = 8.0;

/// Smaller blocks give noisy timings, so they don't calibrate anything.
const MIN_CALIBRATION_BYTES: usize = 64 * 1024;

/// Where zopfli is, as a fraction of the whole compression, plus where it is expected to go.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Position {
    pub fraction: f32,
    /// From `since`, the fraction grows by `per_second` until it reaches `until`.
    pub projection: Option<(Instant, f32, f32)>,
}

#[derive(Debug)]
pub(super) struct ZopfliEstimate {
    total: usize,
    iterations: u32,
    written: usize,
    pending_split: Option<Vec<usize>>,
    split_points: Vec<usize>,
    master_start: usize,
    block: Option<usize>,
    last_index: Option<u32>,
    /// The current master block's passes are done and its sub-blocks are being written.
    writing: bool,
    /// Highest fraction reported so far; the estimate never moves backwards.
    reported: f32,
    /// When the work on the current block began, and when each of its passes finished.
    block_started: Instant,
    pass_times: Vec<Instant>,
    /// End of the last unit of work; the next block starts from here.
    idle_since: Instant,
    /// Timings from earlier files, and what this file has measured so far.
    prior: Option<ZopfliTiming>,
    learned: Option<ZopfliTiming>,
}

impl ZopfliEstimate {
    pub(super) fn new(
        total: usize,
        iterations: u32,
        prior: Option<ZopfliTiming>,
        now: Instant,
    ) -> Self {
        Self {
            total: total.max(1),
            iterations: iterations.max(1),
            written: 0,
            pending_split: None,
            split_points: Vec::new(),
            master_start: 0,
            block: None,
            last_index: None,
            writing: false,
            reported: 0.0,
            block_started: now,
            pass_times: Vec::new(),
            idle_since: now,
            prior,
            learned: None,
        }
    }

    pub(super) fn apply(&mut self, signal: &Signal, now: Instant) {
        match signal {
            // Zopfli logs a master block's split points before its passes, and sometimes a
            // second attempt after them. The second is dropped once blocks are written, so a
            // pending split always describes the master block about to start.
            Signal::ZopfliSplit { points } => self.pending_split = Some(points.clone()),
            &Signal::ZopfliIteration { index, .. } => {
                if self.last_index.is_none_or(|last| index <= last) {
                    self.start_block();
                }
                self.last_index = Some(index);
                self.pass_times.push(now);
            }
            &Signal::ZopfliBlockWritten { uncompressed } => {
                if !self.writing {
                    self.calibrate();
                }
                self.written += uncompressed;
                self.writing = true;
                self.pending_split = None;
                self.block = None;
                self.last_index = None;
                self.pass_times.clear();
                self.idle_since = now;
            }
            _ => {}
        }
        self.reported = self.reported.max(self.base_fraction());
    }

    /// Timings measured in this file, for later files.
    pub(super) const fn learned(&self) -> Option<ZopfliTiming> {
        self.learned
    }

    fn timing(&self) -> Option<ZopfliTiming> {
        self.learned.or(self.prior)
    }

    fn first_pass_weight(&self) -> f64 {
        self.timing()
            .map_or(DEFAULT_FIRST_PASS_WEIGHT, |t| t.first_pass_weight)
    }

    fn start_block(&mut self) {
        self.calibrate();
        // The next block's work began when the previous one's last pass ended (or, for the
        // first block of a master, when the previous master was written).
        self.block_started = self.pass_times.last().copied().unwrap_or(self.idle_since);
        self.pass_times.clear();
        if let Some(points) = self.pending_split.take() {
            self.start_master(points);
        } else if let Some(block) = self.block {
            self.block = Some((block + 1).min(self.split_points.len()));
        } else {
            self.start_master(Vec::new());
        }
    }

    fn start_master(&mut self, split_points: Vec<usize>) {
        self.split_points = split_points;
        self.master_start = self.written;
        self.block = Some(0);
        self.writing = false;
    }

    /// Learns pass timings from the block that just ended.
    fn calibrate(&mut self) {
        let (Some(block), [first, .., last]) = (self.block, self.pass_times.as_slice()) else {
            return;
        };
        let (start, end) = self.block_bounds(block);
        let bytes = end - start;
        let first_pass = first
            .saturating_duration_since(self.block_started)
            .as_secs_f64();
        let later_pass = last.saturating_duration_since(*first).as_secs_f64()
            / (self.pass_times.len() - 1) as f64;
        if bytes < MIN_CALIBRATION_BYTES || later_pass <= 0.0 {
            return;
        }
        self.learned = Some(ZopfliTiming {
            first_pass_weight: (first_pass / later_pass).clamp(1.0, 200.0),
            first_pass_per_byte: first_pass / bytes as f64,
            later_pass_per_byte: later_pass / bytes as f64,
        });
    }

    fn master_len(&self) -> usize {
        self.total
            .saturating_sub(self.master_start)
            .min(MASTER_BLOCK)
    }

    /// Where the next master block starts, while none is under way.
    fn next_master_start(&self) -> usize {
        if self.writing {
            (self.master_start + self.master_len()).max(self.written)
        } else {
            self.written
        }
    }

    /// The sub-block under way (1-based) and how many its master block has, once zopfli has
    /// logged that master block's split points.
    fn block_position(&self) -> Option<(usize, usize)> {
        if let Some(block) = self.block.filter(|_| !self.writing) {
            return Some((
                block + 1,
                block_count(&self.split_points, self.master_len()),
            ));
        }
        let len = self
            .total
            .saturating_sub(self.next_master_start())
            .min(MASTER_BLOCK);
        let points = self.pending_split.as_ref()?;
        Some((1, block_count(points, len)))
    }

    /// Start and end offset of a sub-block within the current master block.
    fn block_bounds(&self, block: usize) -> (usize, usize) {
        let len = self.master_len();
        let mut edges = vec![0];
        edges.extend(
            self.split_points
                .iter()
                .copied()
                .filter(|&p| p > 0 && p < len),
        );
        edges.push(len);
        let block = block.min(edges.len() - 2);
        (edges[block], edges[block + 1])
    }

    /// Work units of a block: the first pass counts as `weight` later passes.
    fn units(&self, passes: usize) -> f64 {
        if passes == 0 {
            0.0
        } else {
            self.first_pass_weight() + (passes.min(self.iterations as usize) - 1) as f64
        }
    }

    fn total_units(&self) -> f64 {
        self.first_pass_weight() + f64::from(self.iterations - 1)
    }

    fn base_fraction(&self) -> f32 {
        let done = if self.writing {
            // Writing happens after all of the master block's passes.
            (self.master_start + self.master_len()).max(self.written) as f64
        } else {
            let partial = self.block.map_or(0.0, |block| {
                let (start, end) = self.block_bounds(block);
                start as f64
                    + (end - start) as f64 * self.units(self.pass_times.len()) / self.total_units()
            });
            self.master_start.max(self.written) as f64 + partial
        };
        let fraction =
            (done.min(self.total as f64) / self.total as f64).min(f64::from(super::MAX_FRACTION));
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a fraction between 0 and 1"
        )]
        let fraction = fraction as f32;
        fraction
    }

    /// The current fraction and how it is expected to grow until the next pass is logged.
    pub(super) fn position(&self) -> Position {
        Position {
            fraction: self.reported,
            projection: self.projection(),
        }
    }

    fn projection(&self) -> Option<(Instant, f32, f32)> {
        let passes = self.pass_times.len();
        if passes >= self.iterations as usize {
            return None;
        }
        let (bytes, block_started) = if let Some(block) = self.block.filter(|_| !self.writing) {
            let (start, end) = self.block_bounds(block);
            (end - start, self.block_started)
        } else {
            // Between master blocks: the next one's first sub-block is being split and
            // passed over, which logs nothing until its first pass ends.
            let len = self
                .total
                .saturating_sub(self.next_master_start())
                .min(MASTER_BLOCK);
            let first_split = self
                .pending_split
                .as_ref()
                .and_then(|points| points.iter().copied().find(|&p| p > 0 && p < len));
            (first_split.unwrap_or(len), self.idle_since)
        };
        if bytes == 0 {
            return None;
        }
        let bytes = bytes as f64;
        let timing = self.timing();
        // Expected length of the pass under way, and when it started.
        let (since, seconds, units) = match self.pass_times.as_slice() {
            [] => (
                block_started,
                timing?.first_pass_per_byte * bytes,
                self.first_pass_weight(),
            ),
            [only] => {
                let first = only
                    .saturating_duration_since(self.block_started)
                    .as_secs_f64();
                let later = timing.map_or(first / self.first_pass_weight(), |t| {
                    t.later_pass_per_byte * bytes
                });
                (*only, later, 1.0)
            }
            [first, .., last] => {
                let average =
                    last.saturating_duration_since(*first).as_secs_f64() / (passes - 1) as f64;
                (*last, average, 1.0)
            }
        };
        if seconds <= 0.0 {
            return None;
        }
        let gain = units / self.total_units() * bytes / self.total as f64;
        let until = (f64::from(self.reported) + gain).min(f64::from(super::MAX_FRACTION));
        #[expect(clippy::cast_possible_truncation, reason = "fractions between 0 and 1")]
        Some((since, (gain / seconds) as f32, until as f32))
    }

    /// e.g. `Zopfli · part 2 of 4 · block 3 of 9 · pass 3 of 15`, see [`Self::place`].
    pub(super) fn detail(&self) -> String {
        format!("Zopfli · {}", self.place())
    }

    /// e.g. `part 2 of 4 · block 3 of 9 · pass 3 of 15`: the master block ("part", when there
    /// is more than one), the sub-block within it (once its split points are logged), and the
    /// pass under way. Passes start over in every sub-block.
    pub(super) fn place(&self) -> String {
        let number = |n: usize| crate::format::grouped(n as u64);
        let mut place = Vec::new();
        let masters = self.total.div_ceil(MASTER_BLOCK);
        if masters > 1 {
            let master = (self.written / MASTER_BLOCK + 1).min(masters);
            place.push(format!("part {} of {}", number(master), number(masters)));
        }
        if let Some((block, blocks)) = self.block_position() {
            place.push(format!("block {} of {}", number(block), number(blocks)));
        }
        let pass = (self.pass_times.len() + 1).min(self.iterations as usize);
        place.push(format!(
            "pass {} of {}",
            number(pass),
            number(self.iterations as usize)
        ));
        place.join(" · ")
    }
}

/// Sub-blocks of a master block of `len` bytes split at `points`.
fn block_count(points: &[usize], len: usize) -> usize {
    1 + points.iter().filter(|&&p| p > 0 && p < len).count()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn iteration(index: u32) -> Signal {
        Signal::ZopfliIteration {
            index,
            bits: 1000,
            improved: true,
        }
    }

    #[test]
    fn weights_the_first_pass_and_learns_the_real_ratio() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        // 3 master blocks of 1 MB, 5 passes per block.
        let mut zopfli = ZopfliEstimate::new(3 * MASTER_BLOCK, 5, None, t0);
        assert_eq!(zopfli.detail(), "Zopfli · part 1 of 3 · pass 1 of 5");

        // Default weight 8: the first pass is 8 of 12 units of the first third.
        zopfli.apply(&Signal::ZopfliSplit { points: vec![] }, at(100));
        zopfli.apply(&iteration(0), at(3200));
        let expected = (8.0 / 12.0) / 3.0;
        assert!((zopfli.position().fraction - expected).abs() < 1e-4);
        assert_eq!(
            zopfli.detail(),
            "Zopfli · part 1 of 3 · block 1 of 1 · pass 2 of 5"
        );

        // Later passes take 100 ms each: the first pass (3.2 s) was 32 times longer.
        for (index, ms) in [(1, 3300), (2, 3400), (3, 3500), (4, 3600)] {
            zopfli.apply(&iteration(index), at(ms));
        }
        assert_eq!(
            zopfli.detail(),
            "Zopfli · part 1 of 3 · block 1 of 1 · pass 5 of 5"
        );
        zopfli.apply(
            &Signal::ZopfliBlockWritten {
                uncompressed: MASTER_BLOCK,
            },
            at(3610),
        );
        let learned = zopfli.learned().expect("calibrated on a 1 MB block");
        assert!(
            (learned.first_pass_weight - 32.0).abs() < 0.1,
            "{learned:?}"
        );

        // In the next part, the first pass now counts as 32 of 36 units.
        zopfli.apply(&Signal::ZopfliSplit { points: vec![] }, at(3700));
        zopfli.apply(&iteration(0), at(6810));
        let expected = (1.0 + 32.0 / 36.0) / 3.0;
        assert!(
            (zopfli.position().fraction - expected).abs() < 1e-3,
            "{:?}",
            zopfli.position()
        );
    }

    #[test]
    fn projects_the_pass_under_way_from_measured_times() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let timing = ZopfliTiming {
            first_pass_weight: 30.0,
            first_pass_per_byte: 3.0 / MASTER_BLOCK as f64,
            later_pass_per_byte: 0.1 / MASTER_BLOCK as f64,
        };
        let mut zopfli = ZopfliEstimate::new(MASTER_BLOCK, 11, Some(timing), t0);

        // Before the first pass is logged, the projection runs from the block's start and is
        // expected to take 3 s to reach 30 of 40 units.
        let position = zopfli.position();
        let (since, per_second, until) = position.projection.expect("a projection");
        assert_eq!(since, t0);
        assert!((until - 0.75).abs() < 1e-4, "{until}");
        assert!((per_second - 0.25).abs() < 1e-4, "{per_second}");

        // After two passes, the measured pass length (200 ms here) drives the projection.
        zopfli.apply(&iteration(0), at(3000));
        zopfli.apply(&iteration(1), at(3200));
        let (since, per_second, until) = zopfli.position().projection.expect("a projection");
        assert_eq!(since, at(3200));
        assert!((until - 32.0 / 40.0).abs() < 1e-4, "{until}");
        assert!(
            (per_second - (1.0 / 40.0) / 0.2).abs() < 1e-3,
            "{per_second}"
        );
    }

    #[test]
    fn numbers_blocks_within_each_part() {
        let t0 = Instant::now();
        let mut zopfli = ZopfliEstimate::new(MASTER_BLOCK + 1000, 3, None, t0);
        assert_eq!(zopfli.place(), "part 1 of 2 · pass 1 of 3");
        zopfli.apply(
            &Signal::ZopfliSplit {
                points: vec![100_000, 600_000],
            },
            t0,
        );
        assert_eq!(zopfli.place(), "part 1 of 2 · block 1 of 3 · pass 1 of 3");

        // Passes start over in every block, and the block number says so.
        for index in 0..3 {
            zopfli.apply(&iteration(index), t0);
        }
        assert_eq!(zopfli.place(), "part 1 of 2 · block 1 of 3 · pass 3 of 3");
        zopfli.apply(&iteration(0), t0);
        assert_eq!(zopfli.place(), "part 1 of 2 · block 2 of 3 · pass 2 of 3");
        for index in [1, 2, 0, 1, 2] {
            zopfli.apply(&iteration(index), t0);
        }
        assert_eq!(zopfli.place(), "part 1 of 2 · block 3 of 3 · pass 3 of 3");

        // A second split attempt after the passes describes the part just compressed, not the
        // next one.
        zopfli.apply(
            &Signal::ZopfliSplit {
                points: vec![300_000],
            },
            t0,
        );
        for uncompressed in [300_000, 700_000] {
            zopfli.apply(&Signal::ZopfliBlockWritten { uncompressed }, t0);
        }
        assert_eq!(zopfli.place(), "part 2 of 2 · pass 1 of 3");
        zopfli.apply(&Signal::ZopfliSplit { points: vec![] }, t0);
        assert_eq!(zopfli.place(), "part 2 of 2 · block 1 of 1 · pass 1 of 3");
        zopfli.apply(&iteration(0), t0);
        assert_eq!(zopfli.place(), "part 2 of 2 · block 1 of 1 · pass 2 of 3");
    }

    #[test]
    fn progress_never_drops_while_blocks_are_written() {
        let t0 = Instant::now();
        let mut zopfli = ZopfliEstimate::new(3 * MASTER_BLOCK, 2, None, t0);
        zopfli.apply(
            &Signal::ZopfliSplit {
                points: vec![6170, 16451],
            },
            t0,
        );
        for _ in 0..3 {
            zopfli.apply(&iteration(0), t0);
            zopfli.apply(&iteration(1), t0);
        }
        let master_done = 1.0 / 3.0;
        for unc in [6170, 10281, 983_549] {
            zopfli.apply(&Signal::ZopfliBlockWritten { uncompressed: unc }, t0);
            assert!((zopfli.position().fraction - master_done).abs() < 1e-4);
            assert_eq!(zopfli.position().projection, None);
        }
    }

    #[test]
    fn caps_below_one() {
        let t0 = Instant::now();
        let mut zopfli = ZopfliEstimate::new(310, 1, None, t0);
        zopfli.apply(
            &Signal::ZopfliBlockWritten {
                uncompressed: 5_000,
            },
            t0,
        );
        assert_eq!(zopfli.position().fraction, super::super::MAX_FRACTION);
    }
}
