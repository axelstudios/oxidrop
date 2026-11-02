# Vendored crates

## `zopfli`, patched

`zopfli/` is [zopfli](https://crates.io/crates/zopfli) 0.8.4 (Apache-2.0, see
`zopfli/COPYING`) exactly as published, plus `zopfli.patch`. The root `Cargo.toml` uses it
through `[patch.crates-io]`. The patch is the only change: the `Drop` implementations of
`DeflateEncoder`, `ZlibEncoder`, and `GzipEncoder` do nothing while the thread is unwinding.

Why: Oxidrop stops a running file by unwinding out of oxipng from its log calls (see
`src/progress/logger.rs`). Zopfli's encoders finish compressing in `Drop`, so the unwind ran
the whole compression of the current chunk again, with every iteration, before the file
could stop: tens of seconds with high iteration counts. A second unwind from inside that
`Drop` isn't possible; it would abort the process.

### Updating to a new zopfli release

```bash
bash vendor/update-zopfli.sh 0.8.5
cargo update -p zopfli
cargo test --test runner stopping_a_running
```

The script has cargo download that release (`cargo vendor`, so cargo's cache, proxies, and
mirrors apply), replaces `zopfli/` with it, and applies `zopfli.patch`. Keep the version
within what oxipng accepts (`zopfli = "0.8"` today); otherwise cargo warns that the patch is
unused and builds the registry copy.

If the patch no longer applies, `zopfli/` is left holding the new release unpatched. Make the
same change by hand (return early from each encoder's `Drop` while
`std::thread::panicking()`), then record it:

```bash
bash vendor/update-zopfli.sh --save-patch
```

That rewrites `zopfli.patch` as the difference between `zopfli/` and the published release of
the same version, so it is also how to change the patch.

### Removing it

When a zopfli release skips finishing while panicking (or no longer finishes in `Drop`), delete
`zopfli/`, `zopfli.patch`, `update-zopfli.sh`, and the `[patch.crates-io]` entry, then run
`cargo update -p zopfli` and the `stopping_a_running` test above.
