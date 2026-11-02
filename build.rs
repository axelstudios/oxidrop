//! Build script:
//!
//! - Passes the version of oxipng in `Cargo.lock` to the app as `OXIPNG_VERSION`, for the
//!   About window. Cargo updates the lock file before it runs build scripts, so this is the
//!   version being built.
//! - On Windows, embeds the app icon and version information in the `.exe`, so Explorer, the
//!   taskbar, and file properties show them. The icon is drawn by `src/icon.rs`, shared with
//!   the app. The copyright year is the year the build script last ran (UTC): on every clean
//!   build, and whenever this file, `src/icon.rs`, or `Cargo.lock` changes.
//! - Fully strips Windows release builds. `strip = "symbols"` keeps symbols out of the executable,
//!   but rustc still passes `/DEBUG` to the MSVC linker, which then writes a PDB of public
//!   symbols next to the exe and records its name in it. `/DEBUG:NONE` comes later on the
//!   linker command line and wins. Debug builds keep their PDB for debugging.

// Only Windows hosts embed the icon (see `embed_windows_resources`).
#[cfg(windows)]
#[path = "src/icon.rs"]
#[expect(
    unreachable_pub,
    reason = "the library, which shares this file, makes it public"
)]
mod icon;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rustc-env=OXIPNG_VERSION={}", oxipng_version());
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let release = std::env::var("PROFILE").is_ok_and(|profile| profile == "release");

    if release && target_env == "msvc" {
        println!("cargo:rustc-link-arg-bins=/DEBUG:NONE");
    }
    if target_os == "windows" {
        embed_windows_resources();
    }
}

/// The version of oxipng in `Cargo.lock`, where each package's name line is followed by its
/// version line.
fn oxipng_version() -> String {
    let lock = std::fs::read_to_string("Cargo.lock").unwrap_or_default();
    let version = lock
        .lines()
        .skip_while(|line| *line != "name = \"oxipng\"")
        .find_map(|line| line.strip_prefix("version = \"")?.strip_suffix('"'));
    let Some(version) = version else {
        println!("cargo:warning=oxipng isn't in Cargo.lock, so the About window says \"unknown\"");
        return "unknown".to_owned();
    };
    version.to_owned()
}

#[cfg(windows)]
fn embed_windows_resources() {
    let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default());
    let ico = out_dir.join("oxidrop.ico");
    if let Err(err) = std::fs::write(&ico, icon::ico()) {
        println!("cargo:warning=could not write the app icon: {err}");
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon(&ico.to_string_lossy())
        .set("ProductName", "Oxidrop")
        .set("FileDescription", "Oxidrop PNG optimizer")
        .set("OriginalFilename", "oxidrop.exe")
        .set(
            "LegalCopyright",
            &format!("Copyright © {} Alex Swindler", build_year()),
        );
    // A missing resource compiler only costs the icon, so don't fail the build over it.
    if let Err(err) = resource.compile() {
        println!("cargo:warning=could not embed the app icon and version info: {err}");
    }
}

/// The current year (UTC). The standard library has no calendar, so this counts whole years
/// from 1970.
#[cfg(windows)]
fn build_year() -> u64 {
    let since_1970 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let mut days = since_1970.as_secs() / 86_400;
    let mut year = 1970;
    loop {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let length = if leap { 366 } else { 365 };
        if days < length {
            return year;
        }
        days -= length;
        year += 1;
    }
}

/// Cross-compiling from another OS: `winresource` is only available on Windows hosts.
#[cfg(not(windows))]
fn embed_windows_resources() {
    println!("cargo:warning=building for Windows on another OS: no icon or version info embedded");
}
