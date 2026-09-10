#!/usr/bin/env bash
# Profile cometty on macOS with Instruments / xctrace.
# Requires the full Xcode app (App Store). The Command Line Tools alone
# are NOT enough: xctrace fails with "requires Xcode" on a CLT instance.
#
# Usage:
#   ./scripts/profile-macos.sh [--template NAME] [--output PATH] [--time-limit 60s] [--no-build] [-- <cometty args...>]
#   ./scripts/profile-macos.sh --list-templates
#
# Examples:
#   ./scripts/profile-macos.sh
#   ./scripts/profile-macos.sh --time-limit 30s -- --theme tokyo-night
#
# Output is an Instruments .trace bundle. Open it in Instruments.app to
# inspect per-type / per-callsite heap usage (see footer message + MEMORY_STUDY.md).
set -euo pipefail

TEMPLATE="Allocations"
OUTPUT=""
TIME_LIMIT=""
BUILD=1

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
info() { printf '%s\n' "$*"; }

need_xcode() {
  command -v xctrace >/dev/null 2>&1 || die "xctrace not found. Download Xcode from the App Store, then: sudo xcode-select -s /Applications/Xcode.app/Contents/Developer"
  if ! xctrace version >/dev/null 2>&1; then
    die "tool 'xctrace' requires Xcode, but active developer directory '$(xcode-select -p 2>/dev/null || echo unknown)' is a command line tools instance. Download Xcode from the App Store, then run: sudo xcode-select -s /Applications/Xcode.app/Contents/Developer"
  fi
}

list_templates() {
  xcrun xctrace list templates 2>/dev/null || xctrace list templates
}

# --- arg parsing ---
ARGS=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --list-templates) need_xcode; list_templates; exit 0 ;;
    --template) TEMPLATE="${2:?}"; shift 2 ;;
    --template=*) TEMPLATE="${1#*=}"; shift ;;
    --output) OUTPUT="${2:?}"; shift 2 ;;
    --output=*) OUTPUT="${1#*=}"; shift ;;
    --time-limit) TIME_LIMIT="${2:?}"; shift 2 ;;
    --time-limit=*) TIME_LIMIT="${1#*=}"; shift ;;
    --no-build) BUILD=0; shift ;;
    --build) BUILD=1; shift ;;
    -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
    --) shift; while [[ $# -gt 0 ]]; do ARGS+=("$1"); shift; done ;;
    *) ARGS+=("$1"); shift ;;
  esac
done

[[ "$(uname -s)" == "Darwin" ]] || die "macOS only (uname=$(uname -s)). On Linux use heaptrack/massif, see MEMORY_STUDY.md."
need_xcode

if [[ "$BUILD" == "1" ]]; then
  info "building release binary..."
  cargo build --release
fi

BIN="target/release/cometty"
[[ -x "$BIN" ]] || die "binary not found at $BIN"

if [[ -z "$OUTPUT" ]]; then
  OUTPUT="target/profile/cometty-$(date +%Y%m%d-%H%M%S).trace"
fi
mkdir -p "$(dirname "$OUTPUT")"

info "template : $TEMPLATE  (see --list-templates; try 'Allocations', 'Leaks', 'VM Tracker')"
info "output   : $OUTPUT"
info "binary   : $BIN ${ARGS[*]:-}"
info ""
info "Interact with cometty (idle, cat a big file, open a 2nd tab), then quit"
info "cometty (closing the last tab exits) or press Ctrl-C to stop recording."
info ""

XCTRACE_ARGS=(record --template "$TEMPLATE" --output "$OUTPUT")
[[ -n "$TIME_LIMIT" ]] && XCTRACE_ARGS+=(--time-limit "$TIME_LIMIT")
XCTRACE_ARGS+=(--launch -- "$BIN")
[[ "${#ARGS[@]}" -gt 0 ]] && XCTRACE_ARGS+=("${ARGS[@]}")

set -x
xctrace "${XCTRACE_ARGS[@]}"
set +x

info ""
info "Trace saved to: $OUTPUT"
info "Visualize:"
info "  open \"$OUTPUT\"                                  # Instruments.app GUI"
info "  xctrace export --input \"$OUTPUT\" --toc            # list instruments inside"
