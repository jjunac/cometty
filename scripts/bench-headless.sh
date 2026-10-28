#!/usr/bin/env bash
# Repeatable headless release run; artifacts stay outside the source tree.
set -euo pipefail
cd "$(dirname "$0")/.."

OUTPUT="${1:-target/perf/$(date -u +%Y%m%dT%H%M%SZ)}"
if [[ -e "$OUTPUT" ]]; then
  printf 'Refusing to overwrite existing benchmark output: %s\n' "$OUTPUT" >&2
  exit 1
fi
mkdir -p "$OUTPUT/source"
# Include uncommitted/new Rust modules too: a revision alone cannot identify
# measurements taken while iterating. Never copy target/ or user config.
cp -R src "$OUTPUT/source/src"
cp -R assets "$OUTPUT/source/assets"
if [[ -d vendor ]]; then
  cp -R vendor "$OUTPUT/source/vendor"
fi
cp Cargo.toml Cargo.lock "$OUTPUT/source/"
cp scripts/bench-headless.sh "$OUTPUT/bench-headless.sh"
git diff --binary > "$OUTPUT/source.patch"
{
  printf 'utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'revision=%s\n' "$(git rev-parse HEAD)"
  printf 'lock_sha256=%s\n' "$(shasum -a 256 Cargo.lock)"
  printf 'RUSTFLAGS=%s\n' "${RUSTFLAGS-}"
  printf 'CARGO_ENCODED_RUSTFLAGS=%s\n' "${CARGO_ENCODED_RUSTFLAGS-}"
  printf 'CARGO_BUILD_TARGET=%s\n' "${CARGO_BUILD_TARGET-}"
  uname -a
  rustc -Vv
  git status --short
} > "$OUTPUT/environment.txt"

for mode in default quiet; do
  printf 'Headless release benchmarks (%s logging) -> %s/%s.log\n' "$mode" "$OUTPUT" "$mode"
  RUST_LOG=off COMETTY_PERF=0 COMETTY_BENCH_LOG="$mode" cargo test --release \
    perf_bench::headless_release -- --ignored --exact --nocapture --test-threads=1 \
    > "$OUTPUT/$mode.log" 2>&1
done
printf 'Results and exact source snapshot: %s\n' "$OUTPUT"
