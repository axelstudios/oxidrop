//! Showing a file in the system file manager.

use std::{io, path::Path, process::Command};

/// Opens the folder containing `path`, selecting the file where the platform supports it.
///
/// # Errors
///
/// Fails when the file manager can't be started.
pub(super) fn show_in_folder(path: &Path) -> io::Result<()> {
    command(path).spawn().map(drop)
}

#[cfg(windows)]
fn command(path: &Path) -> Command {
    use std::os::windows::process::CommandExt;
    // Explorer parses its own command line: `/select,` must be followed directly by the quoted
    // path, which normal argument quoting would wrap as a whole.
    let mut command = Command::new("explorer");
    command.raw_arg(format!("/select,\"{}\"", path.display()));
    command
}

#[cfg(target_os = "macos")]
fn command(path: &Path) -> Command {
    let mut command = Command::new("open");
    command.arg("-R").arg(path);
    command
}

#[cfg(all(unix, not(target_os = "macos")))]
fn command(path: &Path) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(path.parent().unwrap_or(path));
    command
}
