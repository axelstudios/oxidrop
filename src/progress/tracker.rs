//! Per-file progress state machine fed by [`Signal`]s.

use std::{
    collections::HashMap,
    thread::{self, ThreadId},
    time::{Duration, Instant},
};

use super::{
    MAX_FRACTION, Signal,
    calibration::{Calibration, ZopfliTiming},
    report::{Chosen, Report, ZopfliPass},
    zopfli::ZopfliEstimate,
};
use crate::{format::grouped, png_info::PngInfo};

/// Estimates for stages without countable log lines stop here until the stage really ends.
const SILENT_STAGE_CAP: f32 = 0.95;

/// The phases a file goes through while it is optimized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Decoding a GIF or WebP file and writing it as PNG/APNG, before oxipng starts.
    Converting,
    Reading,
    Reducing,
    Filtering,
    Compressing,
    Frames,
    Writing,
}

impl Stage {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Converting => "conversion",
            Self::Reading => "reading",
            Self::Reducing => "reductions",
            Self::Filtering => "filters",
            Self::Compressing => "compression",
            Self::Frames => "frames",
            Self::Writing => "writing",
        }
    }
}

/// What the UI shows for a running file.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub stage: Stage,
    /// Fraction of the current stage done when the snapshot was taken, if it can be told.
    pub fraction: Option<f32>,
    /// The fraction is an estimate (shown with "~"), not a count of finished steps.
    pub estimated: bool,
    pub detail: String,
    /// How `fraction` is expected to grow until the next log line arrives.
    pub projection: Option<Projection>,
    /// When the snapshot was taken; the projection runs from here.
    pub taken: Instant,
}

/// Expected growth of a fraction between log lines.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Projection {
    pub per_second: f32,
    /// Where the projection stops: the fraction the next log line is expected to report.
    pub until: f32,
}

impl Snapshot {
    /// The fraction at `now`, following the projection from when the snapshot was taken.
    #[must_use]
    pub fn fraction_at(&self, now: Instant) -> Option<f32> {
        let fraction = self.fraction?;
        Some(self.projection.map_or(fraction, |p| {
            let elapsed = now.saturating_duration_since(self.taken).as_secs_f32();
            (fraction + p.per_second * elapsed).min(p.until.max(fraction))
        }))
    }

    /// True when both show the same thing, whenever they were taken.
    #[must_use]
    pub fn same_as(&self, other: &Self) -> bool {
        self.stage == other.stage
            && self.fraction == other.fraction
            && self.estimated == other.estimated
            && self.detail == other.detail
            && self.projection == other.projection
    }
}

/// Tracks the progress of one file.
#[derive(Debug)]
pub struct Tracker {
    zopfli_iterations: Option<u32>,
    calibration: Calibration,
    animation_frames: Option<u32>,
    width: u32,
    height: u32,
    bits_per_pixel: u32,
    interlaced: bool,
    /// Filtered size of the input image, before any reductions.
    initial_bytes: usize,
    stage: Stage,
    stage_started: Instant,
    last_update: Instant,
    report: Report,
    reductions_tried: usize,
    trials_done: usize,
    trials_total: usize,
    full_trials: bool,
    /// The final compression is a single, silent libdeflate run.
    libdeflate_final: bool,
    frames_done: u32,
    zopfli: Option<ZopfliEstimate>,
    /// Zopfli passes of the single final compression.
    final_passes: PassLog,
    /// Zopfli passes of full-strength filter trials still running, per thread: at the highest
    /// levels oxipng compresses every filter with zopfli at once, one trial per thread, and
    /// each thread logs its trial's result right after its passes.
    trial_passes: HashMap<ThreadId, PassLog>,
    /// Zopfli passes of finished full-strength trials, with the filter each one tried.
    trial_zopfli: Vec<(String, Vec<Vec<ZopfliPass>>)>,
    /// Progress of the full-strength trials compressing with zopfli, per thread: passes
    /// logged against the iteration count, as for the single final compression.
    trial_estimates: HashMap<ThreadId, ZopfliEstimate>,
    /// Pass timings measured by finished trials, for the trials that start after them.
    trial_timing: Option<ZopfliTiming>,
    /// The trial whose place the detail text shows: the one furthest behind when it was
    /// picked, kept until it finishes so the numbers don't jump between trials.
    followed: Option<ThreadId>,
}

/// Zopfli passes in the order they were logged, split into blocks.
#[derive(Debug, Default)]
struct PassLog {
    blocks: Vec<Vec<ZopfliPass>>,
    /// Index of the last pass; a lower or equal index starts a new block.
    last_index: Option<u32>,
}

impl PassLog {
    fn record(&mut self, index: u32, pass: ZopfliPass) {
        let new_block = self.last_index.is_none_or(|last| index <= last);
        self.last_index = Some(index);
        match self.blocks.last_mut() {
            Some(block) if !new_block => block.push(pass),
            _ => self.blocks.push(vec![pass]),
        }
    }
}

impl Tracker {
    /// Starts tracking a file whose header is `info`. `zopfli_iterations` is the configured
    /// iteration count when Zopfli is enabled; `calibration` holds timings from earlier files
    /// optimized with the same settings.
    #[must_use]
    pub fn new(
        info: &PngInfo,
        zopfli_iterations: Option<u32>,
        calibration: Calibration,
        now: Instant,
    ) -> Self {
        Self {
            zopfli_iterations,
            calibration,
            animation_frames: info.frame_count,
            width: info.width,
            height: info.height,
            bits_per_pixel: info.bits_per_pixel(),
            interlaced: info.interlaced,
            initial_bytes: filtered_len(
                info.width,
                info.height,
                info.bits_per_pixel(),
                info.interlaced,
            ),
            stage: Stage::Reading,
            stage_started: now,
            last_update: now,
            report: Report::default(),
            reductions_tried: 0,
            trials_done: 0,
            trials_total: 0,
            full_trials: false,
            libdeflate_final: false,
            frames_done: 0,
            zopfli: None,
            final_passes: PassLog::default(),
            trial_passes: HashMap::new(),
            trial_zopfli: Vec::new(),
            trial_estimates: HashMap::new(),
            trial_timing: None,
            followed: None,
        }
    }

    /// Records time spent before tracking started, e.g. converting the file to PNG.
    pub fn record(&mut self, stage: Stage, spent: Duration) {
        self.report.stages.push((stage, spent));
    }

    #[must_use]
    pub const fn stage(&self) -> Stage {
        self.stage
    }

    /// Feeds one signal, logged on the current thread. Returns true when the snapshot may
    /// have changed.
    pub fn apply(&mut self, signal: Signal, now: Instant) -> bool {
        self.apply_from(thread::current().id(), signal, now)
    }

    /// Feeds one signal logged on `thread`. Returns true when the snapshot may have changed.
    pub fn apply_from(&mut self, thread: ThreadId, signal: Signal, now: Instant) -> bool {
        self.last_update = now;
        match signal {
            Signal::ImageSize { width, height } => {
                (self.width, self.height) = (width, height);
                self.enter(Stage::Reducing, now);
            }
            Signal::Format {
                bits_per_pixel,
                interlaced,
                ..
            } => {
                self.bits_per_pixel = bits_per_pixel;
                self.interlaced = interlaced;
                return false;
            }
            Signal::TrialFinished(trial) => match self.stage {
                Stage::Reading | Stage::Reducing => {
                    self.enter(Stage::Reducing, now);
                    self.reductions_tried += 1;
                    self.report.reduction_trials.push(trial);
                }
                _ => {
                    self.trials_done = (self.trials_done + 1).min(self.trials_total);
                    if let Some(passes) = self.trial_passes.remove(&thread) {
                        self.trial_zopfli
                            .push((trial.filter.clone(), passes.blocks));
                    }
                    if let Some(estimate) = self.trial_estimates.remove(&thread) {
                        self.trial_timing = estimate.learned().or(self.trial_timing);
                    }
                    self.follow_a_trial();
                    self.report.filter_trials.push(trial);
                }
            },
            Signal::TrialsPlanned {
                count,
                final_quality,
            } => {
                self.enter(
                    if final_quality {
                        Stage::Compressing
                    } else {
                        Stage::Filtering
                    },
                    now,
                );
                (self.trials_done, self.trials_total, self.full_trials) = (0, count, final_quality);
                self.trial_estimates.clear();
            }
            Signal::FinalCompression { zopfli } => {
                self.enter(Stage::Compressing, now);
                self.full_trials = false;
                self.trial_estimates.clear();
                self.libdeflate_final = !zopfli;
                self.zopfli = zopfli.then(|| {
                    ZopfliEstimate::new(
                        self.final_bytes(),
                        self.zopfli_iterations.unwrap_or(15),
                        self.calibration.zopfli,
                        now,
                    )
                });
            }
            Signal::Chosen { deflater, filter } => {
                self.report.chosen = Some(Chosen { deflater, filter });
                return false;
            }
            signal @ (Signal::ZopfliSplit { .. }
            | Signal::ZopfliIteration { .. }
            | Signal::ZopfliBlockWritten { .. }) => {
                if self.stage != Stage::Compressing {
                    return false;
                }
                let pass = match signal {
                    Signal::ZopfliIteration {
                        index,
                        bits,
                        improved,
                    } => Some((index, ZopfliPass { bits, improved })),
                    _ => None,
                };
                if self.full_trials {
                    // Trials run side by side, so their output interleaves: follow each
                    // thread's trial on its own, and keep its passes until its trial result
                    // names the filter.
                    let (total, iterations) =
                        (self.final_bytes(), self.zopfli_iterations.unwrap_or(15));
                    let prior = self.trial_timing.or(self.calibration.zopfli);
                    self.trial_estimates
                        .entry(thread)
                        .or_insert_with(|| ZopfliEstimate::new(total, iterations, prior, now))
                        .apply(&signal, now);
                    self.follow_a_trial();
                    if let Some((index, pass)) = pass {
                        self.trial_passes
                            .entry(thread)
                            .or_default()
                            .record(index, pass);
                    }
                    return true;
                }
                let Some(estimate) = self.zopfli.as_mut() else {
                    return false;
                };
                estimate.apply(&signal, now);
                if let Some((index, pass)) = pass {
                    self.final_passes.record(index, pass);
                }
            }
            Signal::FrameRecompressed => {
                self.enter(Stage::Frames, now);
                self.frames_done += 1;
            }
            Signal::Warning(message) | Signal::Note(message) => {
                if !self.report.notes.contains(&message) {
                    self.report.notes.push(message);
                }
                return false;
            }
        }
        true
    }

    /// Filtered size of the image as it will be compressed, after any reductions.
    fn final_bytes(&self) -> usize {
        filtered_len(
            self.width,
            self.height,
            self.bits_per_pixel,
            self.interlaced,
        )
    }

    /// Moves to `stage`, recording how long the previous stage took.
    pub fn enter(&mut self, stage: Stage, now: Instant) {
        self.last_update = now;
        if stage != self.stage {
            self.close_stage(now);
            self.stage = stage;
            self.stage_started = now;
        }
    }

    fn close_stage(&mut self, now: Instant) {
        let spent = now.saturating_duration_since(self.stage_started);
        match self
            .report
            .stages
            .iter_mut()
            .find(|(s, _)| *s == self.stage)
        {
            Some((_, total)) => *total += spent,
            None => self.report.stages.push((self.stage, spent)),
        }
    }

    /// The current state for display. Counters show the step under way: "1 of 4" while the
    /// first runs, "4 of 4" while the last one does.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let counted = |done: usize, total: usize, prefix: &str| {
            let current = grouped((done + 1).min(total) as u64);
            let total_text = grouped(total as u64);
            Shown::exact(
                ratio(done, total),
                format!("{prefix}{current} of {total_text}"),
            )
        };
        let shown = match self.stage {
            Stage::Converting => Shown::text("Converting"),
            Stage::Reading => Shown::text("Reading"),
            Stage::Reducing => {
                let detail = match self.reductions_tried {
                    0 => "Trying reductions".to_owned(),
                    1 => "Trying reductions · 1 candidate".to_owned(),
                    n => format!("Trying reductions · {} candidates", grouped(n as u64)),
                };
                self.silent(self.calibration.reducing, self.initial_bytes, detail)
            }
            Stage::Filtering => counted(self.trials_done, self.trials_total, "Trying filters · "),
            Stage::Compressing if self.full_trials => self.zopfli_trials().unwrap_or_else(|| {
                counted(self.trials_done, self.trials_total, "Compressing · filter ")
            }),
            Stage::Compressing => match &self.zopfli {
                Some(estimate) => {
                    let position = estimate.position();
                    let mut shown = Shown::text(estimate.detail());
                    shown.estimated = true;
                    shown.fraction = Some(position.fraction);
                    if let Some((since, per_second, until)) = position.projection {
                        shown.project(self.last_update, since, per_second, until);
                    }
                    shown
                }
                None => self.silent(
                    self.calibration.compressing,
                    self.final_bytes(),
                    "Compressing".to_owned(),
                ),
            },
            Stage::Frames => match self.animation_frames {
                // The first frame is usually the IDAT image, which is not an fdAT chunk.
                Some(frames) if frames > 1 => counted(
                    self.frames_done as usize,
                    frames as usize - 1,
                    "Recompressing frames · ",
                ),
                _ => Shown::text("Recompressing frames"),
            },
            Stage::Writing => Shown::text("Writing"),
        };
        Snapshot {
            stage: self.stage,
            fraction: shown.fraction,
            estimated: shown.estimated,
            detail: shown.detail,
            projection: shown.projection,
            taken: self.last_update,
        }
    }

    /// Picks the trial the detail text follows when the previous one has finished: the one
    /// furthest behind.
    fn follow_a_trial(&mut self) {
        if self
            .followed
            .is_some_and(|thread| self.trial_estimates.contains_key(&thread))
        {
            return;
        }
        self.followed = self
            .trial_estimates
            .iter()
            .map(|(&thread, estimate)| (thread, estimate.position().fraction))
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(thread, _)| thread);
    }

    /// Full-strength trials compressing with zopfli side by side: finished trials count
    /// fully and running ones by their passes, e.g. `Zopfli · filters 3–8 of 10 · part 2 of 4 ·
    /// block 3 of 9 · pass 5 of 15` (the place of the trial followed). None until a trial
    /// logs zopfli's progress.
    fn zopfli_trials(&self) -> Option<Shown> {
        let followed = self
            .followed
            .and_then(|thread| self.trial_estimates.get(&thread))
            .or_else(|| self.trial_estimates.values().next())?;
        let mut done = self.trials_done as f32;
        let mut per_second = 0.0;
        // Seconds until the first running projection stops: the sum of the projections grows
        // linearly only until then.
        let mut runway = f32::INFINITY;
        for estimate in self.trial_estimates.values() {
            let position = estimate.position();
            let mut fraction = position.fraction;
            if let Some((since, rate, until)) = position.projection {
                let elapsed = self.last_update.saturating_duration_since(since);
                fraction = (fraction + rate * elapsed.as_secs_f32()).min(until.max(fraction));
                if rate > 0.0 && fraction < until {
                    per_second += rate;
                    runway = runway.min((until - fraction) / rate);
                }
            }
            done += fraction;
        }
        let total = self.trials_total.max(1) as f32;
        let first = self.trials_done + 1;
        let last = (self.trials_done + self.trial_estimates.len()).min(self.trials_total);
        let filters = if last > first {
            format!(
                "filters {}–{} of {}",
                grouped(first as u64),
                grouped(last as u64),
                grouped(self.trials_total as u64)
            )
        } else {
            format!(
                "filter {} of {}",
                grouped(first.min(last) as u64),
                grouped(self.trials_total as u64)
            )
        };
        let mut shown = Shown::text(format!("Zopfli · {filters} · {}", followed.place()));
        shown.estimated = true;
        shown.fraction = Some((done / total).min(MAX_FRACTION));
        if per_second > 0.0 && runway.is_finite() {
            shown.projection = Some(Projection {
                per_second: per_second / total,
                until: ((done + per_second * runway) / total).min(MAX_FRACTION),
            });
        }
        Some(shown)
    }

    /// A stage that logs nothing to count: estimate it from how long it took per byte in
    /// earlier files, if any were measured.
    fn silent(&self, seconds_per_byte: Option<f64>, bytes: usize, detail: String) -> Shown {
        let mut shown = Shown::text(detail);
        let Some(seconds) = seconds_per_byte.map(|rate| rate * bytes as f64) else {
            return shown;
        };
        if seconds > 0.0 {
            shown.estimated = true;
            shown.fraction = Some(0.0);
            #[expect(clippy::cast_possible_truncation, reason = "a rate between 0 and 1000")]
            let per_second = (1.0 / seconds).min(1000.0) as f32;
            shown.project(
                self.last_update,
                self.stage_started,
                per_second,
                SILENT_STAGE_CAP,
            );
        }
        shown
    }

    /// Closes the current stage and returns everything collected about the file, including
    /// the timings later files can be estimated with.
    #[must_use]
    pub fn finish(mut self, now: Instant) -> Report {
        self.close_stage(now);
        // The chart shows the passes that produced the file: the single final compression,
        // or the full-strength trial of the filter oxipng chose.
        self.report.zopfli_blocks = if self.final_passes.blocks.is_empty() {
            let chosen = self.report.chosen.as_ref().map(|c| c.filter.as_str());
            std::mem::take(&mut self.trial_zopfli)
                .into_iter()
                .find(|(filter, _)| Some(filter.as_str()) == chosen)
                .map(|(_, blocks)| blocks)
                .unwrap_or_default()
        } else {
            std::mem::take(&mut self.final_passes.blocks)
        };
        let spent = |stage: Stage| {
            self.report
                .stages
                .iter()
                .find(|(s, _)| *s == stage)
                .map(|(_, d)| d.as_secs_f64())
        };
        let per_byte = |seconds: Option<f64>, bytes: usize| {
            seconds
                .filter(|&s| s > 0.001 && bytes > 0)
                .map(|s| s / bytes as f64)
        };
        let calibration = Calibration {
            reducing: per_byte(spent(Stage::Reducing), self.initial_bytes),
            compressing: if self.libdeflate_final {
                per_byte(spent(Stage::Compressing), self.final_bytes())
            } else {
                None
            },
            zopfli: self
                .zopfli
                .as_ref()
                .and_then(ZopfliEstimate::learned)
                .or(self.trial_timing),
        };
        self.report.calibration = calibration;
        self.report
    }
}

/// Snapshot contents before the time it was taken is filled in.
struct Shown {
    fraction: Option<f32>,
    estimated: bool,
    detail: String,
    projection: Option<Projection>,
}

impl Shown {
    fn text(detail: impl Into<String>) -> Self {
        Self {
            fraction: None,
            estimated: false,
            detail: detail.into(),
            projection: None,
        }
    }

    fn exact(fraction: f32, detail: String) -> Self {
        Self {
            fraction: Some(fraction),
            ..Self::text(detail)
        }
    }

    /// Moves the fraction along a projection that started at `since`, as of `taken`.
    fn project(&mut self, taken: Instant, since: Instant, per_second: f32, until: f32) {
        let base = self.fraction.unwrap_or(0.0);
        let elapsed = taken.saturating_duration_since(since).as_secs_f32();
        self.fraction = Some((base + per_second * elapsed).min(until.max(base)));
        self.projection = Some(Projection { per_second, until });
    }
}

fn ratio(done: usize, total: usize) -> f32 {
    if total == 0 {
        0.0
    } else {
        (done as f32 / total as f32).min(MAX_FRACTION)
    }
}

/// Size of the filtered scanline data that gets compressed: one filter byte per row plus the
/// packed pixels, summed over the seven Adam7 passes for interlaced images.
#[must_use]
pub fn filtered_len(width: u32, height: u32, bits_per_pixel: u32, interlaced: bool) -> usize {
    const ADAM7: [(u32, u32, u32, u32); 7] = [
        (0, 0, 8, 8),
        (4, 0, 8, 8),
        (0, 4, 4, 8),
        (2, 0, 4, 4),
        (0, 2, 2, 4),
        (1, 0, 2, 2),
        (0, 1, 1, 2),
    ];
    let rows = |w: u32, h: u32| {
        if w == 0 || h == 0 {
            0
        } else {
            u64::from(h) * (1 + (u64::from(w) * u64::from(bits_per_pixel)).div_ceil(8))
        }
    };
    let total = if interlaced {
        ADAM7
            .iter()
            .map(|&(x0, y0, dx, dy)| {
                rows(
                    width.saturating_sub(x0).div_ceil(dx),
                    height.saturating_sub(y0).div_ceil(dy),
                )
            })
            .sum()
    } else {
        rows(width, height)
    };
    usize::try_from(total).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{
        png_info::{ColorType, PngInfo},
        progress::{Trial, TrialSize, calibration::ZopfliTiming},
    };

    fn info(width: u32, height: u32, color_type: ColorType) -> PngInfo {
        PngInfo {
            width,
            height,
            bit_depth: 8,
            color_type,
            interlaced: false,
            palette_len: None,
            has_trns: false,
            frame_count: None,
            chunks: Vec::new(),
            image_data: 0,
            file_size: 0,
        }
    }

    fn tracker(info: &PngInfo, zopfli: Option<u32>, now: Instant) -> Tracker {
        Tracker::new(info, zopfli, Calibration::default(), now)
    }

    fn trial() -> Signal {
        named_trial("8-bit RGB", "None")
    }

    fn named_trial(image: &str, filter: &str) -> Signal {
        Signal::TrialFinished(Trial {
            image: image.to_owned(),
            filter: filter.to_owned(),
            size: TrialSize::Exact(100),
        })
    }

    fn planned(count: usize, final_quality: bool) -> Signal {
        Signal::TrialsPlanned {
            count,
            final_quality,
        }
    }

    #[test]
    fn computes_filtered_sizes_seen_in_zopfli_logs() {
        // 1200x900 RGB: zopfli logged three 1,000,000-byte blocks plus 240,900.
        assert_eq!(filtered_len(1200, 900, 24, false), 3_240_900);
        // Interlaced 8-bit indexed: zopfli logged 1,000,000 + 81,688.
        assert_eq!(filtered_len(1200, 900, 8, true), 1_081_688);
        assert_eq!(filtered_len(3, 1, 1, false), 2);
        assert_eq!(filtered_len(1, 1, 32, true), 5);
        assert_eq!(filtered_len(0, 10, 8, false), 0);
    }

    #[test]
    fn walks_through_stages_and_counts_the_step_under_way() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut tracker = tracker(&info(64, 64, ColorType::Rgba), None, t0);
        assert_eq!(tracker.snapshot().detail, "Reading");

        tracker.apply(
            Signal::ImageSize {
                width: 64,
                height: 64,
            },
            ms(5),
        );
        tracker.apply(trial(), ms(6));
        tracker.apply(trial(), ms(7));
        let snap = tracker.snapshot();
        assert_eq!((snap.stage, snap.fraction), (Stage::Reducing, None));
        assert_eq!(snap.detail, "Trying reductions · 2 candidates");

        tracker.apply(planned(4, false), ms(10));
        let snap = tracker.snapshot();
        assert_eq!(snap.fraction, Some(0.0));
        assert_eq!(snap.detail, "Trying filters · 1 of 4");
        for n in 12..15 {
            tracker.apply(trial(), ms(n));
        }
        let snap = tracker.snapshot();
        assert_eq!((snap.stage, snap.fraction), (Stage::Filtering, Some(0.75)));
        assert_eq!(snap.detail, "Trying filters · 4 of 4");
        assert!(!snap.estimated);

        tracker.apply(Signal::FinalCompression { zopfli: false }, ms(20));
        let snap = tracker.snapshot();
        assert_eq!((snap.stage, snap.fraction), (Stage::Compressing, None));

        tracker.enter(Stage::Writing, ms(50));
        let report = tracker.finish(ms(52));
        assert_eq!(
            report.stages,
            vec![
                (Stage::Reading, Duration::from_millis(5)),
                (Stage::Reducing, Duration::from_millis(5)),
                (Stage::Filtering, Duration::from_millis(10)),
                (Stage::Compressing, Duration::from_millis(30)),
                (Stage::Writing, Duration::from_millis(2)),
            ]
        );
    }

    #[test]
    fn full_trials_count_as_compression_progress() {
        let t0 = Instant::now();
        let mut tracker = tracker(&info(10, 10, ColorType::Rgb), None, t0);
        tracker.apply(planned(10, true), t0);
        assert_eq!(tracker.snapshot().detail, "Compressing · filter 1 of 10");
        for _ in 0..5 {
            tracker.apply(trial(), t0);
        }
        let snap = tracker.snapshot();
        assert_eq!(snap.stage, Stage::Compressing);
        assert_eq!(snap.fraction, Some(0.5));
        assert_eq!(snap.detail, "Compressing · filter 6 of 10");
        for _ in 0..5 {
            tracker.apply(trial(), t0);
        }
        assert_eq!(tracker.snapshot().detail, "Compressing · filter 10 of 10");
    }

    #[test]
    fn parallel_zopfli_trials_count_their_passes() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        // Four filters with four zopfli passes each, two at a time. A pass counts as one
        // unit, and a block's first pass as 8 (the default weight): 11 units per trial.
        let mut tracker = tracker(&info(64, 64, ColorType::Rgb), Some(4), t0);
        tracker.apply(planned(4, true), t0);
        assert_eq!(tracker.snapshot().detail, "Compressing · filter 1 of 4");
        let [a, b] = [(); 2].map(|()| {
            thread::spawn(|| thread::current().id())
                .join()
                .expect("a thread id")
        });
        let split = || Signal::ZopfliSplit { points: vec![] };
        let pass = |index| Signal::ZopfliIteration {
            index,
            bits: 1000,
            improved: true,
        };
        let close = |a: f32, b: f32| (a - b).abs() < 1e-4;

        for thread in [a, b] {
            tracker.apply_from(thread, split(), ms(0));
        }
        for thread in [a, b] {
            tracker.apply_from(thread, pass(0), ms(100));
        }
        let snap = tracker.snapshot();
        assert_eq!(
            snap.detail,
            "Zopfli · filters 1–2 of 4 · block 1 of 1 · pass 2 of 4"
        );
        assert!(snap.estimated);
        let fraction = snap.fraction.expect("a fraction");
        assert!(close(fraction, 2.0 * (8.0 / 11.0) / 4.0), "{fraction}");
        // Each trial's next pass is expected to take 1/8 of the first (12.5 ms) and add one
        // unit; the projection stops where both have.
        let projection = snap.projection.expect("a projection");
        assert!(
            close(projection.until, 2.0 * (9.0 / 11.0) / 4.0),
            "{projection:?}"
        );
        assert!(close(
            projection.per_second,
            2.0 * (1.0 / 11.0) / 0.0125 / 4.0
        ));

        // The first trial finishes, and its thread starts the third filter.
        for (index, at) in [(1, 110), (2, 120), (3, 130)] {
            tracker.apply_from(a, pass(index), ms(at));
        }
        tracker.apply_from(a, trial(), ms(131));
        tracker.apply_from(a, split(), ms(132));
        let snap = tracker.snapshot();
        // The text moved on to the second trial when the first finished, and stays with it
        // although the new third trial is further behind.
        assert_eq!(
            snap.detail,
            "Zopfli · filters 2–3 of 4 · block 1 of 1 · pass 2 of 4"
        );
        // One trial done, the second as far as its projection goes (9 of 11 units), the
        // third not started.
        let fraction = snap.fraction.expect("a fraction");
        assert!(close(fraction, (1.0 + 9.0 / 11.0) / 4.0), "{fraction}");

        // Only the last trial is left.
        for index in 1..4 {
            tracker.apply_from(b, pass(index), ms(140));
        }
        tracker.apply_from(b, trial(), ms(141));
        for index in 0..4 {
            tracker.apply_from(a, pass(index), ms(150));
        }
        tracker.apply_from(a, trial(), ms(151));
        tracker.apply_from(b, split(), ms(152));
        let snap = tracker.snapshot();
        assert_eq!(
            snap.detail,
            "Zopfli · filter 4 of 4 · block 1 of 1 · pass 1 of 4"
        );
        assert!(close(snap.fraction.expect("a fraction"), 3.0 / 4.0));
    }

    #[test]
    fn learns_and_uses_timings_for_silent_stages() {
        let t0 = Instant::now();
        let s = |n| t0 + Duration::from_secs(n);
        let image = info(1000, 1000, ColorType::Rgb); // 3,001,000 filtered bytes

        // A first file: reductions take 3 s and libdeflate's compression 6 s.
        let mut first = tracker(&image, None, t0);
        first.apply(
            Signal::ImageSize {
                width: 1000,
                height: 1000,
            },
            t0,
        );
        first.apply(Signal::FinalCompression { zopfli: false }, s(3));
        first.enter(Stage::Writing, s(9));
        let learned = first.finish(s(9)).calibration;
        let reducing = learned.reducing.expect("reductions measured");
        assert!((reducing * 3_001_000.0 - 3.0).abs() < 1e-6);
        assert!(learned.compressing.is_some());

        // The next file of the same size gets estimated progress for both stages.
        let mut next = Tracker::new(&image, None, learned, t0);
        next.apply(
            Signal::ImageSize {
                width: 1000,
                height: 1000,
            },
            t0,
        );
        let snap = next.snapshot();
        assert!(snap.estimated);
        assert_eq!(snap.fraction, Some(0.0));
        let at_half = snap
            .fraction_at(t0 + Duration::from_millis(1500))
            .expect("estimate");
        assert!((at_half - 0.5).abs() < 1e-3, "{at_half}");
        let late = snap.fraction_at(s(60)).expect("estimate");
        assert!(
            (late - SILENT_STAGE_CAP).abs() < 1e-6,
            "estimates stop short of done"
        );

        next.apply(Signal::FinalCompression { zopfli: false }, s(3));
        let snap = next.snapshot();
        let at_third = snap.fraction_at(s(5)).expect("estimate");
        assert!((at_third - 1.0 / 3.0).abs() < 1e-3, "{at_third}");
    }

    #[test]
    fn projects_zopfli_progress_between_passes() {
        let t0 = Instant::now();
        let calibration = Calibration {
            zopfli: Some(ZopfliTiming {
                first_pass_weight: 30.0,
                first_pass_per_byte: 3.0 / 1_000_000.0,
                later_pass_per_byte: 0.1 / 1_000_000.0,
            }),
            ..Calibration::default()
        };
        // 1000x1000 grayscale: 1,001,000 bytes, so a single master block (plus 1,000 bytes).
        let image = info(1000, 999, ColorType::Grayscale); // 999 * 1001 = 999,999 bytes
        let mut tracker = Tracker::new(&image, Some(11), calibration, t0);
        tracker.apply(Signal::FinalCompression { zopfli: true }, t0);
        let snap = tracker.snapshot();
        assert!(snap.estimated);
        // The block layout is unknown until zopfli logs its split points.
        assert_eq!(snap.detail, "Zopfli · pass 1 of 11");
        let after_1500ms = snap
            .fraction_at(t0 + Duration::from_millis(1500))
            .expect("estimate");
        assert!((after_1500ms - 0.375).abs() < 1e-3, "{after_1500ms}");
        let much_later = snap
            .fraction_at(t0 + Duration::from_secs(60))
            .expect("estimate");
        assert!(
            (much_later - 0.75).abs() < 1e-3,
            "stops at the first pass: {much_later}"
        );
    }

    #[test]
    fn transformed_format_changes_the_zopfli_total() {
        let t0 = Instant::now();
        let mut tracker = tracker(&info(1200, 900, ColorType::Rgb), Some(1), t0);
        tracker.apply(
            Signal::Format {
                bits_per_pixel: 8,
                interlaced: true,
                transformed: true,
            },
            t0,
        );
        tracker.apply(Signal::FinalCompression { zopfli: true }, t0);
        tracker.apply(
            Signal::ZopfliBlockWritten {
                uncompressed: 1_000_000,
            },
            t0,
        );
        let fraction = tracker.snapshot().fraction.unwrap_or_default();
        assert!((fraction - 1_000_000.0 / 1_081_688.0).abs() < 1e-4);
    }

    #[test]
    fn counts_apng_frames_and_collects_notes_once() {
        let t0 = Instant::now();
        let mut apng = info(10, 10, ColorType::Rgba);
        apng.frame_count = Some(5);
        let mut tracker = tracker(&apng, None, t0);
        let warning = Signal::Warning("APNG detected, disabling all reductions".to_owned());
        assert!(!tracker.apply(warning.clone(), t0));
        tracker.apply(warning, t0);
        tracker.apply(Signal::FrameRecompressed, t0);
        tracker.apply(Signal::FrameRecompressed, t0);
        let snap = tracker.snapshot();
        assert_eq!(snap.fraction, Some(0.5));
        assert_eq!(snap.detail, "Recompressing frames · 3 of 4");
        assert_eq!(tracker.finish(t0).notes.len(), 1);
    }

    #[test]
    fn collects_trials_choice_zopfli_passes_and_notes() {
        let t0 = Instant::now();
        let mut tracker = tracker(&info(100, 100, ColorType::Rgb), Some(3), t0);
        tracker.apply(
            Signal::ImageSize {
                width: 100,
                height: 100,
            },
            t0,
        );
        tracker.apply(named_trial("8-bit Indexed (luma sort)", "None"), t0);
        tracker.apply(planned(1, false), t0);
        tracker.apply(named_trial("8-bit Indexed (12 colors)", "Sub"), t0);
        tracker.apply(Signal::FinalCompression { zopfli: true }, t0);
        for (index, improved) in [
            (0, true),
            (1, false),
            (2, true),
            (0, true),
            (1, false),
            (2, false),
        ] {
            tracker.apply(
                Signal::ZopfliIteration {
                    index,
                    bits: 100,
                    improved,
                },
                t0,
            );
        }
        tracker.apply(
            Signal::Chosen {
                deflater: "zopfli, zi = 3".to_owned(),
                filter: "Sub".to_owned(),
            },
            t0,
        );
        let note = "Replacing iCCP chunk with equivalent sRGB chunk";
        tracker.apply(Signal::Note(note.to_owned()), t0);

        let report = tracker.finish(t0);
        assert_eq!(report.reduction_trials.len(), 1);
        assert_eq!(report.filter_trials.len(), 1);
        let blocks: Vec<Vec<bool>> = report
            .zopfli_blocks
            .iter()
            .map(|b| b.iter().map(|p| p.improved).collect())
            .collect();
        assert_eq!(blocks, [[true, false, true], [true, false, false]]);
        assert_eq!(report.chosen.map(|c| c.filter).as_deref(), Some("Sub"));
        assert_eq!(report.notes, [note]);
    }

    #[test]
    fn snapshots_compare_by_content() {
        let t0 = Instant::now();
        let tracker = tracker(&info(10, 10, ColorType::Rgb), None, t0);
        let mut later = tracker.snapshot();
        later.taken = t0 + Duration::from_secs(1);
        assert!(tracker.snapshot().same_as(&later));
        assert_ne!(tracker.snapshot(), later);
    }
    #[test]
    fn keeps_the_zopfli_passes_of_the_chosen_trial() {
        let t0 = Instant::now();
        let mut tracker = tracker(&info(64, 64, ColorType::Rgb), Some(15), t0);
        tracker.apply(planned(2, true), t0);
        // Two full-strength trials on two threads, their output interleaved.
        let [a, b] = [(); 2].map(|()| {
            thread::spawn(|| thread::current().id())
                .join()
                .expect("a thread id")
        });
        let pass = |index, bits, improved| Signal::ZopfliIteration {
            index,
            bits,
            improved,
        };
        for (thread, signal) in [
            (a, pass(1, 1000, true)),
            (b, pass(1, 2000, true)),
            (a, pass(2, 990, true)),
            (b, pass(2, 2100, false)),
            (b, named_trial("8-bit RGB", "Sub")),
            // A second block of the first trial.
            (a, pass(1, 500, true)),
            (a, named_trial("8-bit RGB", "Bigrams")),
        ] {
            tracker.apply_from(thread, signal, t0);
        }
        tracker.apply(
            Signal::Chosen {
                deflater: "zopfli, zi = 15".to_owned(),
                filter: "Bigrams".to_owned(),
            },
            t0,
        );
        let report = tracker.finish(t0);
        let kept = |bits, improved| ZopfliPass { bits, improved };
        assert_eq!(
            report.zopfli_blocks,
            [
                vec![kept(1000, true), kept(990, true)],
                vec![kept(500, true)]
            ]
        );
    }
}
