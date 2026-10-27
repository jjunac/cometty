#!/usr/bin/env bash
# Benchmark cometty against other macOS terminals (see docs/performance.md).
#
# Usage:
#   scripts/compare/compare.sh [--out DIR] [--no-build] [--cometty-app PATH] [compare.py options...]
#   scripts/compare/compare.sh --report DIR        re-render DIR/summary.md only
#
# The summary is also copied to BENCHMARK.md at the repository root; pass
# --no-publish (e.g. for smoke runs) to leave BENCHMARK.md untouched.
#
# compare.py options (defaults in brackets):
#   --terminals cometty,terminal,iterm2,ghostty,kitty
#   --suites startup,idle,throughput,memory,latency
#   --workloads ascii,sgr,unicode,scroll_region,redraw
#   --runs 3  --bytes 32 (MiB/sample)  --samples 5  --idle-seconds 60
#   --tabs 10  --latency-samples 200  --cooldown 10  --quit-running
#
# Run it from a terminal you are not measuring, with Accessibility (and, for
# latency, Screen Recording) granted to that terminal. Measured apps are launched
# and quit; running instances are refused unless --quit-running (asks first).
set -euo pipefail
cd "$(dirname "$0")/../.."

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
[[ "$(uname -s)" == "Darwin" ]] || die "macOS only"

OUTPUT="target/compare/$(date -u +%Y%m%dT%H%M%SZ)"
BUILD=1
APP=""
PASS=()
REPORT=""
PUBLISH=1
publish() {
  /usr/bin/python3 -I scripts/compare/report.py "$1"
  if [[ "$PUBLISH" == 1 ]]; then
    cp "$1/summary.md" BENCHMARK.md
    printf 'Results: %s/summary.md (copied to BENCHMARK.md)\n' "$1"
  else
    printf 'Results: %s/summary.md\n' "$1"
  fi
}
while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUTPUT="${2:?}"; shift 2 ;;
    --no-build) BUILD=0; shift ;;
    --cometty-app) APP="${2:?}"; shift 2 ;;
    --report) REPORT="${2:?}"; shift 2 ;;
    --no-publish) PUBLISH=0; shift ;;
    -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
    *) PASS+=("$1"); shift ;;
  esac
done
if [[ -n "$REPORT" ]]; then
  publish "$REPORT"
  exit 0
fi
[[ -e "$OUTPUT" ]] && die "refusing to overwrite existing output: $OUTPUT"

BIN=target/compare/bin
mkdir -p "$BIN"
if [[ ! -x "$BIN/termlat" || scripts/compare/termlat.swift -nt "$BIN/termlat" ]]; then
  printf 'building termlat...\n'
  swiftc -O -parse-as-library scripts/compare/termlat.swift -o "$BIN/termlat"
fi

if [[ -z "$APP" ]]; then
  if [[ "$BUILD" == 1 ]]; then
    bash scripts/bundle-macos.sh --arch "$(uname -m)" >/dev/null
  fi
  APP=dist/Cometty.app
fi
[[ -d "$APP" ]] || die "no cometty bundle at $APP (run without --no-build or pass --cometty-app)"
# Private copy: a host cometty at any other path is never matched or quit.
rm -rf target/compare/Cometty.app
ditto "$APP" target/compare/Cometty.app

mkdir -p "$OUTPUT"
{
  printf 'utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'revision=%s\n' "$(git rev-parse HEAD)"
  printf 'cometty_app=%s\n' "$APP"
  uname -a
  sw_vers
  sysctl -n machdep.cpu.brand_string hw.memsize
  pmset -g batt | head -1
  pmset -g | grep -E 'lowpowermode|powermode' || true
  system_profiler SPDisplaysDataType 2>/dev/null | grep -E 'Chipset|Resolution|Refresh|Display Type' || true
  git status --short
} > "$OUTPUT/environment.txt"
git diff --binary > "$OUTPUT/source.patch"
cp scripts/compare/*.py scripts/compare/*.sh scripts/compare/*.swift "$OUTPUT/" 2>/dev/null || true

/usr/bin/python3 -I scripts/compare/compare.py --out "$OUTPUT" --termlat "$BIN/termlat" \
  --cometty-app "$PWD/target/compare/Cometty.app" --revision "$(git rev-parse --short HEAD)" ${PASS[@]+"${PASS[@]}"}
publish "$OUTPUT"
