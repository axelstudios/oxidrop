#!/usr/bin/env bash
# Replaces vendor/zopfli with a zopfli release from crates.io and applies Oxidrop's patch
# (vendor/zopfli.patch). See vendor/README.md.
#
#   vendor/update-zopfli.sh 0.8.5        # vendor zopfli 0.8.5 and patch it
#   vendor/update-zopfli.sh --save-patch # rewrite the patch from vendor/zopfli's changes
#
# On Windows, run it with Git Bash: `bash vendor/update-zopfli.sh 0.8.5`.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
patch="$root/vendor/zopfli.patch"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Files a published crate carries that aren't part of its source.
clean() {
  rm -rf "$1/.cargo" "$1/.cargo-ok" "$1/.cargo_vcs_info.json" "$1/.cargo-checksum.json" \
    "$1/Cargo.lock"
}

# Puts zopfli $1, exactly as published, in $work/pristine. Cargo downloads it, so its cache,
# proxies, and mirrors apply.
fetch() {
  mkdir -p "$work/probe/src"
  printf '[package]\nname = "probe"\nversion = "0.0.0"\nedition = "2021"\n\n[dependencies]\nzopfli = "=%s"\n' \
    "$1" >"$work/probe/Cargo.toml"
  : >"$work/probe/src/lib.rs"
  cargo vendor --quiet --manifest-path "$work/probe/Cargo.toml" "$work/crates" >/dev/null
  mv "$work/crates/zopfli" "$work/pristine"
  clean "$work/pristine"
}

if [[ "${1:-}" == "--save-patch" ]]; then
  version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/vendor/zopfli/Cargo.toml" | head -n 1)"
  fetch "$version"
  cp -r "$root/vendor/zopfli" "$work/patched"
  clean "$work/patched"
  # Paths relative to the crate (pristine/src/…), applied with `--directory=vendor/zopfli`.
  status=0
  (cd "$work" && git -c core.autocrlf=false diff --no-index --no-prefix pristine patched)     >"$patch" || status=$?
  if [[ $status -ne 1 ]]; then
    echo "vendor/zopfli doesn't differ from zopfli $version, or the diff failed." >&2
    exit 1
  fi
  echo "Wrote vendor/zopfli.patch: vendor/zopfli's changes to zopfli $version."
  exit 0
fi

version="${1:?usage: vendor/update-zopfli.sh VERSION | --save-patch}"
fetch "$version"
rm -rf "$root/vendor/zopfli"
mv "$work/pristine" "$root/vendor/zopfli"
if ! git -C "$root" apply --directory=vendor/zopfli "$patch"; then
  echo >&2
  echo "The patch doesn't apply to zopfli $version. vendor/zopfli now holds it unpatched:" >&2
  echo "make the change by hand (see vendor/README.md), then run" >&2
  echo "vendor/update-zopfli.sh --save-patch to record it." >&2
  exit 1
fi
cat <<EOF
vendor/zopfli is now zopfli $version with Oxidrop's patch. Next:
  cargo update -p zopfli
  cargo test --test runner stopping_a_running
EOF
