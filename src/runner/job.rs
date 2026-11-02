//! Running one file: optimizing it (converting GIF and WebP first) in a thread pool of its
//! own, with a probe that turns oxipng's log into progress events.

use std::{
    any::Any,
    panic::{self, AssertUnwindSafe},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use super::{Event, Job, JobId, Shared, ZOPFLI_TIMED_OUT};
use crate::{
    convert, format,
    optimize::{self, OptimizeError, Outcome},
    progress::{
        Report, Signal, Snapshot, Stage, Threads, Tracker,
        logger::{self, SignalSink},
    },
};

/// Least time between progress events while a file is being converted.
const CONVERSION_UPDATE_INTERVAL: Duration = Duration::from_millis(50);

pub(super) fn run_job(shared: &Arc<Shared>, job: &Job, threads: Threads, cancel: &Arc<AtomicBool>) {
    let started = Instant::now();
    shared.send(Event::Started {
        id: job.id,
        threads,
    });
    // A panic inside oxipng (or here) must not take the worker down with it: the running
    // count would never drop and the queue would stall. Report it as this file's error.
    // Stopping a file unwinds the same way.
    let finished = panic::catch_unwind(AssertUnwindSafe(|| {
        optimize_job(shared, job, threads, started, cancel)
    }));
    let (result, report) = match finished {
        Ok((result, report)) => (result.map(Box::new).map_err(|err| err.to_string()), report),
        // While a stopped file unwinds, oxipng's other threads can trip over what it left
        // behind (a poisoned lock), so any panic after cancelling counts as stopped.
        Err(payload)
            if logger::is_cancellation(payload.as_ref()) || cancel.load(Ordering::Relaxed) =>
        {
            shared.send(Event::Stopped { id: job.id });
            return;
        }
        Err(payload) => (
            Err(format!(
                "oxipng crashed: {}",
                panic_message(payload.as_ref())
            )),
            Report::default(),
        ),
    };
    shared.send(Event::Finished {
        id: job.id,
        result,
        elapsed: started.elapsed(),
        report: Box::new(report),
    });
}

/// The message of a panic payload, which is a `&str` or a `String` for `panic!` with text.
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown error")
}

fn optimize_job(
    shared: &Arc<Shared>,
    job: &Job,
    threads: Threads,
    started: Instant,
    cancel: &Arc<AtomicBool>,
) -> (Result<Outcome, OptimizeError>, Report) {
    let check_cancelled = || {
        if cancel.load(Ordering::Relaxed) {
            logger::unwind_cancelled();
        }
    };
    let options = convert::Options {
        clean_transparent: job.settings.optimize_alpha,
    };
    let mut last_update: Option<Instant> = None;
    let mut converting = |step: convert::Step| {
        check_cancelled();
        let now = Instant::now();
        // Frames can decode much faster than the UI redraws.
        if last_update.is_some_and(|last| now - last < CONVERSION_UPDATE_INTERVAL) {
            return;
        }
        last_update = Some(now);
        shared.send(Event::Progress {
            id: job.id,
            snapshot: conversion_snapshot(step, now),
        });
    };
    let mut input = match optimize::read_input_with(&job.path, options, &mut converting) {
        Ok(input) => input,
        Err(err) => return (Err(err), Report::default()),
    };
    if let Some(name) = &job.output_name {
        input.output_name.clone_from(name);
    }
    input.replaces.clone_from(&job.replaces);
    let zopfli_iterations = job
        .settings
        .zopfli
        .then(|| job.settings.zopfli_iterations());
    let calibration = shared.calibration(&job.settings);
    let tracker = if input.source.is_some() {
        let now = Instant::now();
        let mut tracker = Tracker::new(&input.info, zopfli_iterations, calibration, now);
        tracker.record(Stage::Converting, now - started);
        tracker
    } else {
        Tracker::new(&input.info, zopfli_iterations, calibration, started)
    };
    // oxipng checks its time limit only before it starts a trial, so a Zopfli compression
    // under way runs on past it, for minutes with high iteration counts. Zopfli is stopped
    // at the limit instead (see `Probe::cancelled`).
    let zopfli_deadline = job
        .settings
        .zopfli
        .then(|| job.settings.time_limit())
        .flatten()
        .map(|limit| Instant::now() + limit);
    let probe = Arc::new(Probe {
        id: job.id,
        shared: Arc::clone(shared),
        cancel: Arc::clone(cancel),
        zopfli_deadline,
        timed_out: AtomicBool::new(false),
        crash: Mutex::new(None),
        state: Mutex::new(Some(ProbeState {
            tracker,
            last_sent: None,
        })),
    });

    let opts = job.settings.oxipng_options();
    check_cancelled();
    let first = panic::catch_unwind(AssertUnwindSafe(|| {
        run_in_pool(job.id, &probe, threads.used, || {
            optimize::optimize(&input, &opts)
        })
    }));
    let timed_out = probe.timed_out.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed);
    // With Zopfli cut short, the smallest result that finished in time stands. At levels 5 and
    // 6 each filter is a Zopfli trial in a spawned job, so oxipng carries on and picks from
    // the trials that finished. Below that, the unwind ends oxipng in its final Zopfli
    // compression, before anything smaller finished: the file stays as it was (`None`).
    let (optimized, used) = match first {
        Ok((optimized, used)) => (optimized.map(Some), used),
        Err(_) if timed_out => (Ok(None), threads.used),
        Err(payload) => panic::resume_unwind(payload),
    };
    if timed_out {
        probe.note(ZOPFLI_TIMED_OUT);
    }
    // Stopped after oxipng's last log line: still write nothing.
    check_cancelled();
    probe.rethrow_crash();
    probe.enter(Stage::Writing);
    let result = optimized.and_then(|bytes| {
        let best = bytes.as_deref().unwrap_or(&input.data);
        optimize::write_output(&input, best, &job.settings)
    });
    let mut report = probe.finish();
    report.threads = Some(Threads { used, ..threads });
    // Timings of a run the time limit cut short would mislead later estimates.
    if result.is_ok() && !timed_out {
        shared.learn(&job.settings, report.calibration);
    }
    (result, report)
}

/// What a row shows while its file is converted to PNG. Animations are decoded twice, so
/// reading the frames counts as the first half and writing them as the second.
fn conversion_snapshot(step: convert::Step, now: Instant) -> Snapshot {
    let share = |done: u32, total: u32| done as f32 / total.max(1) as f32 / 2.0;
    let (fraction, detail) = match step {
        convert::Step::Reading {
            done,
            total: Some(total),
        } if total > 1 => (
            Some(share(done, total)),
            format!(
                "Converting · reading frame {} of {}",
                format::grouped(u64::from((done + 1).min(total))),
                format::grouped(u64::from(total))
            ),
        ),
        convert::Step::Reading { done, total: None } if done > 0 => (
            None,
            format!(
                "Converting · {} read",
                format::count(done as usize, "frame", "frames")
            ),
        ),
        convert::Step::Writing { done, total } => (
            Some(0.5 + share(done, total)),
            format!(
                "Converting · writing frame {} of {}",
                format::grouped(u64::from((done + 1).min(total))),
                format::grouped(u64::from(total))
            ),
        ),
        convert::Step::Reading { .. } => (None, "Converting".to_owned()),
    };
    Snapshot {
        stage: Stage::Converting,
        fraction,
        estimated: false,
        detail,
        projection: None,
        taken: now,
    }
}

/// Runs `work` in a dedicated pool whose threads report to `probe`. Also returns how many
/// threads the pool had.
fn run_in_pool<T: Send>(
    id: JobId,
    probe: &Arc<Probe>,
    threads: usize,
    work: impl FnOnce() -> T + Send,
) -> (T, usize) {
    let sink: Arc<dyn SignalSink> = probe.clone();
    let attach_sink = Arc::clone(&sink);
    let crash_probe = Arc::clone(probe);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(move |i| format!("oxidrop-{id}-{i}"))
        .start_handler(move |_| logger::attach(Arc::clone(&attach_sink)))
        // oxipng runs its trials with `rayon::spawn`, whose panics (stopping a file unwinds
        // too) go here; without a handler rayon aborts the whole app.
        .panic_handler(move |payload| crash_probe.spawned_panic(payload.as_ref()))
        .build();
    match pool {
        Ok(pool) => (pool.install(work), pool.current_num_threads()),
        Err(err) => {
            // Without a dedicated pool, oxipng's parallel work lands on the global pool and
            // only the log lines from this thread are seen. Progress becomes coarser.
            log::warn!("could not create a thread pool, optimizing on one thread: {err}");
            logger::attach(sink);
            let output = work();
            logger::detach();
            (output, rayon::current_num_threads())
        }
    }
}

/// Collects one file's signals and forwards changed snapshots as events.
struct Probe {
    id: JobId,
    shared: Arc<Shared>,
    /// Set when the file is stopped; oxipng's next log record then unwinds.
    cancel: Arc<AtomicBool>,
    /// The time limit, for Zopfli: its first log record after this unwinds.
    zopfli_deadline: Option<Instant>,
    /// The time limit cut Zopfli short.
    timed_out: AtomicBool,
    state: Mutex<Option<ProbeState>>,
    /// The message of a panic in one of oxipng's spawned trials, which ended that trial
    /// without reaching the code waiting for it.
    crash: Mutex<Option<String>>,
}

struct ProbeState {
    tracker: Tracker,
    last_sent: Option<Snapshot>,
}

impl Probe {
    fn lock(&self) -> MutexGuard<'_, Option<ProbeState>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds a note to the file's report.
    fn note(&self, note: &str) {
        self.update(|tracker| {
            tracker.apply(Signal::Note(note.to_owned()), Instant::now());
            false
        });
    }

    /// Records a panic in a spawned trial; stopping the file is not one.
    fn spawned_panic(&self, payload: &(dyn Any + Send)) {
        if logger::is_cancellation(payload) {
            return;
        }
        self.crash
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(|| panic_message(payload).to_owned());
    }

    /// Fails the file the way a panic on the main path would, if a spawned trial panicked:
    /// its result may be missing from what oxipng chose.
    fn rethrow_crash(&self) {
        let crash = self
            .crash
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(message) = crash {
            panic::resume_unwind(Box::new(message));
        }
    }

    fn update(&self, change: impl FnOnce(&mut Tracker) -> bool) {
        let snapshot = {
            let mut guard = self.lock();
            let Some(state) = guard.as_mut() else { return };
            if !change(&mut state.tracker) {
                return;
            }
            let snapshot = state.tracker.snapshot();
            if state
                .last_sent
                .as_ref()
                .is_some_and(|last| last.same_as(&snapshot))
            {
                return;
            }
            state.last_sent = Some(snapshot.clone());
            snapshot
        };
        self.shared.send(Event::Progress {
            id: self.id,
            snapshot,
        });
    }

    fn enter(&self, stage: Stage) {
        self.update(|tracker| {
            tracker.enter(stage, Instant::now());
            true
        });
    }

    fn finish(&self) -> Report {
        self.lock()
            .take()
            .map(|state| state.tracker.finish(Instant::now()))
            .unwrap_or_default()
    }
}

impl SignalSink for Probe {
    fn signal(&self, signal: Signal) {
        self.update(|tracker| tracker.apply(signal, Instant::now()));
    }

    fn cancelled(&self, target: &str) -> bool {
        if self.cancel.load(Ordering::Relaxed) {
            return true;
        }
        let late = target.starts_with("zopfli")
            && self
                .zopfli_deadline
                .is_some_and(|deadline| Instant::now() >= deadline);
        if late {
            self.timed_out.store(true, Ordering::Relaxed);
        }
        late
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_panic_messages() {
        let text = panic::catch_unwind(|| panic!("bad chunk")).expect_err("panics");
        assert_eq!(panic_message(text.as_ref()), "bad chunk");
        let formatted = panic::catch_unwind(|| panic!("bad chunk {}", 7)).expect_err("panics");
        assert_eq!(panic_message(formatted.as_ref()), "bad chunk 7");
        let other = panic::catch_unwind(|| panic::panic_any(42_u8)).expect_err("panics");
        assert_eq!(panic_message(other.as_ref()), "unknown error");
    }
}
