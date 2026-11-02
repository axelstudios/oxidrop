//! Crash reports. A panic on the UI thread ends the app, and release builds on Windows have no
//! console to print it to, so the panic is also written to `crash.log` in the settings folder,
//! and a dialog says what happened and where the log is.
//!
//! A panic while optimizing a file doesn't end the app: the runner reports it on that file.

use std::{
    backtrace::Backtrace,
    env,
    fmt::Display,
    fs,
    panic::{self, AssertUnwindSafe},
    path::PathBuf,
    sync::{Mutex, PoisonError},
    thread,
};

use super::APP_ID;

/// What the panic hook wrote, kept for the dialog shown once the panic has left the event loop.
static CRASH: Mutex<Option<Crash>> = Mutex::new(None);

struct Crash {
    /// Where the panic happened, and its message.
    panic: String,
    /// The crash log, or why it couldn't be written.
    log: Result<PathBuf, String>,
}

/// Runs the event loop. A panic on this thread is written to the crash log as it happens, and
/// shown in a dialog once it has unwound out of `event_loop`, before it ends the process.
pub(super) fn guard<T>(event_loop: impl FnOnce() -> T) -> T {
    let ui_thread = thread::current().id();
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        default_hook(info);
        if thread::current().id() == ui_thread {
            let panic = info.to_string();
            let log = write_log(&panic);
            *CRASH.lock().unwrap_or_else(PoisonError::into_inner) = Some(Crash { panic, log });
        }
    }));
    // The event loop owns the app's state, which is gone by the time the panic gets here, so
    // nothing it left half-updated is used again.
    panic::catch_unwind(AssertUnwindSafe(event_loop)).unwrap_or_else(|payload| {
        if let Some(crash) = CRASH.lock().unwrap_or_else(PoisonError::into_inner).take() {
            show(&crash);
        }
        panic::resume_unwind(payload)
    })
}

/// Writes the crash log beside the settings, replacing the previous one.
fn write_log(panic: &str) -> Result<PathBuf, String> {
    let dir = eframe::storage_dir(APP_ID).ok_or("this system has no settings folder")?;
    let path = dir.join("crash.log");
    fs::create_dir_all(&dir)
        .and_then(|()| fs::write(&path, log_text(panic, &Backtrace::force_capture())))
        .map_err(|err| format!("{}: {err}", path.display()))?;
    Ok(path)
}

/// The crash log: the version and platform, the panic, and the backtrace.
fn log_text(panic: &str, backtrace: &impl Display) -> String {
    format!(
        "Oxidrop {} ({} {}) crashed.\n\n{panic}\n\nBacktrace:\n{backtrace}\n",
        env!("CARGO_PKG_VERSION"),
        env::consts::OS,
        env::consts::ARCH,
    )
}

fn show(crash: &Crash) {
    let details = match &crash.log {
        Ok(path) => format!("The details are in {}.", path.display()),
        Err(err) => format!("The details couldn't be saved to the crash log ({err})."),
    };
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("Oxidrop crashed")
        .set_description(format!(
            "Oxidrop ran into a bug and has to close.\n\n{}\n\n{details}",
            crash.panic
        ))
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_log_names_the_version_then_the_panic() {
        let text = log_text("panicked at src/app/mod.rs:1:1:\nboom", &"frames");
        let version = env!("CARGO_PKG_VERSION");
        assert!(text.starts_with(&format!("Oxidrop {version} (")), "{text}");
        assert!(
            text.ends_with(
                ") crashed.\n\npanicked at src/app/mod.rs:1:1:\nboom\n\nBacktrace:\nframes\n"
            ),
            "{text}"
        );
    }
}
