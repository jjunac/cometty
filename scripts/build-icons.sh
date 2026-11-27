#!/usr/bin/env bash
# Regenerate the checked-in icons with Xcode's asset catalog compiler.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ASSETS="$ROOT/assets"
MIN_MACOS="$(plutil -extract LSMinimumSystemVersion raw "$ROOT/packaging/macos/Info.plist")"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/cometty-icons.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

xcrun actool "$ASSETS/AppIcon.icon" \
    --compile "$WORK" \
    --app-icon AppIcon \
    --output-partial-info-plist "$WORK/Info.plist" \
    --output-format human-readable-text --notices --warnings --errors \
    --platform macosx --target-device mac --minimum-deployment-target "$MIN_MACOS" \
    --standalone-icon-behavior all --optimization space

# Use the fallback's 1x rendition for standalone runs, including its padding.
iconutil -c iconset -o "$WORK/AppIcon.iconset" "$WORK/AppIcon.icns"
PNG="$WORK/AppIcon.iconset/icon_128x128.png"
if [[ ! -s "$WORK/Assets.car" || ! -s "$PNG" ]]; then
    printf 'error: expected Assets.car and a 128px fallback rendition\n' >&2
    exit 1
fi

cp "$WORK/Assets.car" "$WORK/AppIcon.icns" "$ASSETS/"
cp "$PNG" "$ASSETS/icon-128.png"
