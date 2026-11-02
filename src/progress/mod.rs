//! Progress inference from oxipng's log output.
//!
//! oxipng has no progress callback. At trace verbosity, though, it (and the zopfli crate)
//! logs each filter trial, reduction candidate, and zopfli iteration. [`signal`] parses those
//! lines, [`Tracker`] turns them into a stage and fraction per file and collects a [`Report`]
//! of what was tried and chosen, and [`logger`] routes each record to the file that produced it.
//! Stages that log nothing countable are estimated from [`Calibration`] timings of earlier files.

mod calibration;
pub mod logger;
mod report;
pub mod signal;
mod tracker;
mod zopfli;

pub use calibration::{Calibration, ZopfliTiming};
pub use report::{Candidate, Chosen, Report, Threads, ZopfliPass, ZopfliSummary};
pub use signal::{Signal, Trial, TrialSize};
pub use tracker::{Projection, Snapshot, Stage, Tracker, filtered_len};

/// Progress is never reported as complete before the file is actually finished.
const MAX_FRACTION: f32 = 0.99;
