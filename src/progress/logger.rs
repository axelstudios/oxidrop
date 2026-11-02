//! A [`log::Log`] implementation that routes oxipng/zopfli records to the file being
//! optimized on the current thread.
//!
//! Every file runs in its own rayon pool whose threads call [`attach`] when they start, so
//! records from different files never mix, even when several are optimized at once.
//!
//! The same records are where a file is stopped: oxipng has no way to cancel a running
//! optimization, but it and zopfli log many times a second, so when the file's sink says it
//! was cancelled, the record unwinds that thread's work with a [`Cancelled`] payload.

use std::{
    cell::RefCell,
    panic,
    sync::{Arc, Once},
};

use super::{Signal, signal};

/// Receives the progress signals of one file.
pub trait SignalSink: Send + Sync {
    fn signal(&self, signal: Signal);

    /// True once the file should stop; this record (from the module `target`) unwinds.
    fn cancelled(&self, target: &str) -> bool {
        let _ = target;
        false
    }
}

/// The unwind payload of a stopped file, told apart from a real panic with
/// [`is_cancellation`].
#[derive(Debug)]
pub struct Cancelled;

/// Unwinds the current thread's work for a stopped file. Unlike a panic, it doesn't run the
/// panic hook, so nothing is printed.
pub fn unwind_cancelled() -> ! {
    panic::resume_unwind(Box::new(Cancelled))
}

/// True when an unwind payload comes from [`unwind_cancelled`].
#[must_use]
pub fn is_cancellation(payload: &(dyn std::any::Any + Send)) -> bool {
    payload.is::<Cancelled>()
}

thread_local! {
    static SINK: RefCell<Option<Arc<dyn SignalSink>>> = const { RefCell::new(None) };
}

/// Sends this thread's oxipng/zopfli log records to `sink` until [`detach`] is called.
pub fn attach(sink: Arc<dyn SignalSink>) {
    SINK.with(|current| *current.borrow_mut() = Some(sink));
}

/// Stops routing this thread's records.
pub fn detach() {
    SINK.with(|current| current.borrow_mut().take());
}

/// Installs the routing logger and enables trace-level records. Calling it again is a no-op.
///
/// Records from other crates at warning level or above are printed to stderr.
pub fn install() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        if log::set_logger(&RoutingLogger).is_ok() {
            log::set_max_level(log::LevelFilter::Trace);
        }
    });
}

struct RoutingLogger;

fn is_optimizer(target: &str) -> bool {
    target.starts_with("oxipng") || target.starts_with("zopfli")
}

impl log::Log for RoutingLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        if is_optimizer(metadata.target()) {
            SINK.with(|current| current.borrow().is_some())
        } else {
            metadata.level() <= log::Level::Warn
        }
    }

    fn log(&self, record: &log::Record<'_>) {
        let target = record.target();
        if !is_optimizer(target) {
            if record.level() <= log::Level::Warn {
                eprintln!("[{}] {target}: {}", record.level(), record.args());
            }
            return;
        }
        let cancelled = SINK.with(|current| {
            let current = current.borrow();
            let Some(sink) = current.as_ref() else {
                return false;
            };
            if sink.cancelled(target) {
                return true;
            }
            let parsed = match record.args().as_str() {
                Some(message) => signal::parse(target, record.level(), message),
                None => signal::parse(target, record.level(), &record.args().to_string()),
            };
            if let Some(signal) = parsed {
                sink.signal(signal);
            }
            false
        });
        // Never while already unwinding (a record logged by a destructor): a second unwind
        // would abort the process.
        if cancelled && !std::thread::panicking() {
            unwind_cancelled();
        }
    }

    fn flush(&self) {}
}
