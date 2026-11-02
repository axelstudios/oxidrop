//! Runs optimizations in parallel and reports what happens over a channel.
//!
//! Each running file gets its own short-lived rayon pool. The pool's threads attach that file's
//! progress tracker (see [`crate::progress::logger`]), and its size spreads the CPU cores
//! across the files running at once: a single large file gets every core, a big batch gets
//! one core per file.

mod job;

use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use job::run_job;

use crate::{
    optimize::Outcome,
    progress::{Calibration, Report, Snapshot, Threads, logger},
    settings::{Settings, available_cores},
};

/// The note on a file whose Zopfli compression the time limit cut short. It starts like
/// oxipng's own "Timed out after N second(s)", which the file list looks for.
pub const ZOPFLI_TIMED_OUT: &str = "Timed out while Zopfli was compressing; kept the smallest \
                                    result that finished in time";

/// Identifies a file across events. Chosen by the caller.
pub type JobId = u64;

/// Called after every event is sent, e.g. to wake up the UI thread.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// What happened to a submitted file.
#[derive(Debug)]
pub enum Event {
    /// A worker picked the file up, with this share of the CPU.
    Started { id: JobId, threads: Threads },
    /// The file's progress changed.
    Progress { id: JobId, snapshot: Snapshot },
    /// The file is done, successfully or not.
    Finished {
        id: JobId,
        result: Result<Box<Outcome>, String>,
        elapsed: Duration,
        report: Box<Report>,
    },
    /// The file was cancelled while it ran (see [`Runner::cancel`]); nothing was written.
    Stopped { id: JobId },
}

/// What [`Runner::cancel`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cancel {
    /// The file hadn't started; it left the queue and sends no events.
    Dequeued,
    /// The file is running; it stops at its next progress update and sends
    /// [`Event::Stopped`].
    Stopping,
    /// The runner doesn't have the file.
    NotFound,
}

/// Queue of files to optimize, with a limit on how many run at once.
pub struct Runner {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    events: mpsc::Sender<Event>,
    waker: Waker,
    cores: usize,
    /// Timings learned from finished files, per group of settings that affect speed.
    calibration: Mutex<HashMap<CalibrationKey, Calibration>>,
}

/// Compression level and whether Zopfli is used: the settings stage timings depend on.
type CalibrationKey = (u8, bool);

fn calibration_key(settings: &Settings) -> CalibrationKey {
    (settings.level, settings.zopfli)
}

struct State {
    queue: VecDeque<Job>,
    /// Running files with their cancel flags. A file run again right after being stopped can
    /// be listed twice for a moment, which is why this isn't a map.
    running: Vec<(JobId, Arc<AtomicBool>)>,
    limit: usize,
}

/// A file to optimize; see [`Runner::submit_all`].
pub struct Job {
    pub id: JobId,
    pub path: PathBuf,
    /// Relative path of the result inside the output folder (folder mode), so files with the
    /// same name don't overwrite each other; `None` uses the file name.
    pub output_name: Option<PathBuf>,
    /// The PNG an earlier run converted this file to, which this run replaces.
    pub replaces: Option<PathBuf>,
    pub settings: Arc<Settings>,
}

impl Runner {
    /// Creates a runner and installs the progress logger.
    #[must_use]
    pub fn new(parallel_files: usize, events: mpsc::Sender<Event>, waker: Waker) -> Self {
        logger::install();
        let state = State {
            queue: VecDeque::new(),
            running: Vec::new(),
            limit: parallel_files.max(1),
        };
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(state),
                events,
                waker,
                cores: available_cores(),
                calibration: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Queues a file. It starts right away if fewer than the limit are running.
    pub fn submit(&self, id: JobId, path: PathBuf, settings: Arc<Settings>) {
        self.submit_all([Job {
            id,
            path,
            output_name: None,
            replaces: None,
            settings,
        }]);
    }

    /// Queues several files at once. Each file's share of the CPU is set when it starts, from
    /// the files running and waiting then, so files queued together split it evenly; queued
    /// one by one, the first would start alone and take every thread.
    pub fn submit_all(&self, jobs: impl IntoIterator<Item = Job>) {
        self.shared.lock().queue.extend(jobs);
        pump(&self.shared);
    }

    /// Changes how many files may run at once. Running files are not interrupted.
    pub fn set_parallel_files(&self, parallel_files: usize) {
        self.shared.lock().limit = parallel_files.max(1);
        pump(&self.shared);
    }

    /// Removes every file that has not started yet and returns their ids. Running files are
    /// left alone; see [`Self::cancel_running`].
    #[must_use = "cancelled files never send events, so the caller has to mark them"]
    pub fn cancel_queued(&self) -> Vec<JobId> {
        self.shared
            .lock()
            .queue
            .drain(..)
            .map(|job| job.id)
            .collect()
    }

    /// Cancels every running file and returns their ids. Each stops at its next progress
    /// update, usually within a fraction of a second, and sends [`Event::Stopped`].
    #[must_use = "the caller marks these files as stopping"]
    pub fn cancel_running(&self) -> Vec<JobId> {
        self.shared
            .lock()
            .running
            .iter()
            .map(|(id, cancel)| {
                cancel.store(true, Ordering::Relaxed);
                *id
            })
            .collect()
    }

    /// Stops one file, whether it is queued or running.
    #[must_use = "a dequeued file sends no events, so the caller has to mark it"]
    pub fn cancel(&self, id: JobId) -> Cancel {
        let mut state = self.shared.lock();
        if let Some(index) = state.queue.iter().position(|job| job.id == id) {
            state.queue.remove(index);
            return Cancel::Dequeued;
        }
        let mut found = Cancel::NotFound;
        for (_, cancel) in state.running.iter().filter(|(running, _)| *running == id) {
            cancel.store(true, Ordering::Relaxed);
            found = Cancel::Stopping;
        }
        found
    }

    /// Number of files being optimized right now.
    #[must_use]
    pub fn running(&self) -> usize {
        self.shared.lock().running.len()
    }

    /// Number of files waiting for a free slot.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.shared.lock().queue.len()
    }
}

impl Shared {
    /// What is known about how long files take with these settings.
    fn calibration(&self, settings: &Settings) -> Calibration {
        self.calibration
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&calibration_key(settings))
            .copied()
            .unwrap_or_default()
    }

    /// Folds one finished file's timings into what is known.
    fn learn(&self, settings: &Settings, measured: Calibration) {
        let mut known = self
            .calibration
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let entry = known.entry(calibration_key(settings)).or_default();
        *entry = entry.merge(measured);
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn send(&self, event: Event) {
        // The receiver only disappears when the app is closing; nothing is left to tell.
        let _ = self.events.send(event);
        (self.waker)();
    }
}

/// Starts queued jobs until the limit is reached.
fn pump(shared: &Arc<Shared>) {
    let mut state = shared.lock();
    while state.running.len() < state.limit {
        let Some(job) = state.queue.pop_front() else {
            break;
        };
        let id = job.id;
        let cancel = Arc::new(AtomicBool::new(false));
        state.running.push((id, Arc::clone(&cancel)));
        let sharing = (state.running.len() + state.queue.len()).min(state.limit);
        let threads = Threads {
            used: (shared.cores / sharing).max(1),
            available: shared.cores,
            sharing,
        };

        let worker = Arc::clone(shared);
        let flag = Arc::clone(&cancel);
        let spawned = thread::Builder::new()
            .name(format!("oxidrop-job-{id}"))
            .spawn(move || {
                run_job(&worker, &job, threads, &flag);
                worker
                    .lock()
                    .running
                    .retain(|(_, other)| !Arc::ptr_eq(other, &flag));
                pump(&worker);
            });
        if let Err(err) = spawned {
            state
                .running
                .retain(|(_, other)| !Arc::ptr_eq(other, &cancel));
            shared.send(Event::Finished {
                id,
                result: Err(format!("could not start a worker thread: {err}")),
                elapsed: Duration::ZERO,
                report: Box::default(),
            });
        }
    }
}
