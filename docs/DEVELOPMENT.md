# Development

## Building and running

`rust-toolchain.toml` pins the Rust release that CI and releases use, and rustup installs it on
the first build. (`rust-version` in `Cargo.toml` is the oldest release supported, 1.99.)

On Linux, first install the development packages CI uses (Debian and Ubuntu names):

```sh
sudo apt-get install libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libgtk-3-dev
```

```sh
cargo run --release                       # open the app
cargo run --release -- path/to/images     # open and add files or folders right away
```

Dependencies are built optimized even in debug builds, since oxipng, Zopfli, and libdeflate are
far too slow without it.

## Checks

CI runs these with warnings as errors, and runs clippy, the tests, and the docs on Windows,
Linux, and macOS:

```sh
cargo fmt --all
cargo clippy --all-targets      # pedantic lints are on
cargo test
cargo doc --no-deps --document-private-items
cargo deny check                # advisories, licenses, and sources (deny.toml); needs cargo-deny
```

`cargo deny check` also runs weekly, so new advisories against `Cargo.lock` show up without a
push. Install it with `cargo install --locked cargo-deny`.

Lints are silenced with `#[expect]` and a reason, so a suppression that's no longer needed is
reported. `pub` is kept for the library's API; items only the crate uses are `pub(crate)` or
narrower (`unreachable_pub`).

## Helpers

| Command                                                                              | Purpose                                                                                            |
|--------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------|
| `cargo run --release --example make_samples`                                         | Writes generated test images to `target/samples`                                                   |
| `cargo run --release --example log_probe -- [FILE] [--level N] [--zopfli N]`         | Prints the oxipng/zopfli log stream that progress inference parses. Run it after upgrading oxipng. |
| `cargo run --release --example make_stress_samples -- [--long]`                      | Writes large, long animations to `target/stress` for checking conversion time and memory           |
| `cargo run --release --example convert_probe -- FILE... [--optimize]`                | Converts GIF/WebP files like the app does (then runs oxipng with `--optimize`) and reports times   |
| `cargo run --release --example progress_accuracy -- FILE [--zopfli N] [--level N]`   | Compares the progress the UI would show with the true share of time, before and after calibration  |
| `OXIDROP_SCREENSHOT=shot.png OXIDROP_SCREENSHOT_DELAY=4 cargo run -- target/samples` | Runs with demo settings and throwaway storage, saves a screenshot, then exits (see below)          |

Screenshot mode selects the largest finished file, and also reads `OXIDROP_SCREENSHOT_THEME`
(`light` or `dark`) and `OXIDROP_SCREENSHOT_SIZE` (the window's size in points, such as
`1280x720`). `docs/screenshot.png` is 1280×720 and dark, taken after 24 seconds. The folder
held a few of the generated samples, whose large noisy images keep running, and a photo saved
as PNG. It's the selected file, since the samples are too synthetic for Zopfli's passes chart
to show much of a curve. Files are optimized in place, so use copies, and give a relative
path so the details panel's Output row stays short.

## Graphics

Frames wait for vsync, with at most one queued; the swapchain starts without vsync and switches
on the first frame (see `src/app/graphics.rs`). Help → About shows the GPU backend;
`WGPU_BACKEND=vulkan` (or `dx12`, `gl`) forces one. `OXIDROP_FRAME_STATS=1` renders
continuously and prints frame times once a second.

## Layout

| Path              | Contents                                                                       |
|-------------------|--------------------------------------------------------------------------------|
| `src/png_info.rs` | Reads PNG/APNG headers and chunks without decoding pixels; describes changes   |
| `src/settings.rs` | User settings and their mapping to `oxipng::Options`                           |
| `src/optimize.rs` | One file: read, optimize, write (atomic replace, optional mtime preservation)  |
| `src/convert/`    | GIF and WebP decoding, animation compositing, writing PNG/APNG for oxipng      |
| `src/progress/`   | Log-line parser, per-file progress tracker, and the routing `log::Log`         |
| `src/runner/`     | Parallel queue; one rayon pool per running file so log lines can be attributed |
| `src/app/`        | eframe UI: layout in `mod.rs`, a file per panel, file list, widgets, crashes   |
| `assets/fonts/`   | The UI font, Inter 4.001 Variable, and its license (SIL Open Font License)     |
| `vendor/zopfli/`  | zopfli 0.8.4, patched so Stop is quick during Zopfli (see `vendor/README.md`)  |
| `tests/`          | Integration tests against real oxipng, with fixtures generated in code         |

## Dependencies

- **oxipng** stays on 10.2.x: progress is worked out from its log output, so a newer release
  needs the log format checked first with `cargo run --release --example log_probe`.
- **zopfli** is vendored and patched; `vendor/README.md` has the why, the update steps, and
  when to remove it.
- Dependabot proposes updates to crates (except oxipng past 10.2.x, and zopfli) and to GitHub
  Actions every week.
  Actions are pinned to a commit, with the version in a comment. Rust itself is moved by hand,
  in `rust-toolchain.toml`.

## Releases

Push a version tag to build the release archives and publish them as a GitHub release:

```sh
git tag v0.2.0
git push origin v0.2.0
```

`.github/workflows/release.yml` builds Windows (x86-64), Linux (x86-64), and macOS (Apple
Silicon). Running the workflow by hand only builds the archives. Each archive holds the
executable, `README.md`, `LICENSE`, `THIRD-PARTY-LICENSES.txt`, and `Inter-LICENSE.txt`.

### Third-party licenses

`THIRD-PARTY-LICENSES.txt` is written at release time by cargo-about, from `about.toml` and the
template `about.hbs`. To write it locally:

```sh
cargo install --locked cargo-about --features cli
cargo about generate --locked about.hbs -o THIRD-PARTY-LICENSES.txt
```

It needs network access: `about.toml` points some crates to the license files in their GitHub
repositories, because their published crates don't include them (see the comments there). The
accepted licenses are listed in both `about.toml` and `deny.toml`.

### Build paths

Release builds leave the build machine's folders out of the binary: the workflow passes rustc
`--remap-path-prefix` for the checkout, Cargo's home, and the Rust toolchain, with
`--remap-path-scope=object`. That's what Cargo's `trim-paths` will do once it's stable.

To do the same for a local release build, from the project folder in PowerShell:

```powershell
$env:RUSTFLAGS = "--remap-path-scope=object --remap-path-prefix=$PWD= --remap-path-prefix=$env:USERPROFILE\.cargo= --remap-path-prefix=$(rustc --print sysroot)="
cargo build --release --target-dir target/dist
Remove-Item Env:RUSTFLAGS
```

The separate target folder keeps this build from replacing the usual one. Don't use these flags
for builds you debug: they also apply to debug info, so the debugger can't find the sources.
