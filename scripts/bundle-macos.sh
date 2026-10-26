#!/usr/bin/env bash
# Build a macOS .app bundle and a GitHub-release-ready zip for cometty.
#
# Usage:
#   ./scripts/bundle-macos.sh [--arch universal|arm64|x86_64]
#                             [--identity "Developer ID Application: ..."]
#                             [--notary-profile NAME]
#
# Defaults:
#   --arch universal     build both aarch64 and x86_64, then lipo them
#   --identity -         ad-hoc signature (Gatekeeper warns on downloaded copies)
#   no notarization
#
# Environment:
#   BUNDLE_VERSION       CFBundleVersion override (default: git commit count)
#   CARGO_TARGET_DIR     forwarded to Cargo
#   CODESIGN_IDENTITY    default --identity
#   NOTARY_PROFILE       default --notary-profile
#
# Examples:
#   ./scripts/bundle-macos.sh
#   ./scripts/bundle-macos.sh --arch x86_64
#   ./scripts/bundle-macos.sh \
#     --identity "Developer ID Application: Jane Doe (TEAMID)" \
#     --notary-profile cometty-notary
set -euo pipefail

SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
ROOT="$(cd "$(dirname "$SELF")/.." && pwd)"
cd "$ROOT"

APP_NAME="Cometty"
DIST="$ROOT/dist"
APP="$DIST/$APP_NAME.app"
ICON="$ROOT/assets/AppIcon.icns"
PLIST_TEMPLATE="$ROOT/packaging/macos/Info.plist"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

ARCH="universal"
IDENTITY="${CODESIGN_IDENTITY:--}"
NOTARY_PROFILE="${NOTARY_PROFILE:-}"

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
info() { printf '==> %s\n' "$*"; }

usage() {
    awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$SELF"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch)
            [[ $# -ge 2 ]] || die "--arch needs a value"
            ARCH="$2"
            shift 2
            ;;
        --identity)
            [[ $# -ge 2 ]] || die "--identity needs a value"
            IDENTITY="$2"
            shift 2
            ;;
        --notary-profile)
            [[ $# -ge 2 ]] || die "--notary-profile needs a value"
            NOTARY_PROFILE="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown option: $1 (try --help)"
            ;;
    esac
done

case "$ARCH" in
    universal) TARGETS=(aarch64-apple-darwin x86_64-apple-darwin) ;;
    arm64) TARGETS=(aarch64-apple-darwin) ;;
    x86_64) TARGETS=(x86_64-apple-darwin) ;;
    *) die "unknown arch: $ARCH (expected universal, arm64, or x86_64)" ;;
esac

[[ "$(uname -s)" == "Darwin" ]] || die "this script only builds macOS bundles"
command -v cargo >/dev/null 2>&1 || die "cargo not found"
command -v rustup >/dev/null 2>&1 || die "rustup not found"
command -v lipo >/dev/null 2>&1 || die "lipo not found"
command -v plutil >/dev/null 2>&1 || die "plutil not found"
command -v ditto >/dev/null 2>&1 || die "ditto not found"
command -v codesign >/dev/null 2>&1 || die "codesign not found"

[[ -f "$PLIST_TEMPLATE" ]] || die "missing $PLIST_TEMPLATE"
[[ -f "$ICON" ]] || die "missing $ICON (see docs/development.md#packaging-a-macos-release)"

VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
[[ -n "$VERSION" ]] || die "could not read the package version from Cargo.toml"
BUILD="${BUNDLE_VERSION:-$(git rev-list --count HEAD 2>/dev/null || printf '1')}"
[[ "$BUILD" =~ ^[0-9]+([.][0-9]+)*$ ]] || die "BUNDLE_VERSION must be numeric: $BUILD"

if [[ -n "$NOTARY_PROFILE" && "$IDENTITY" == "-" ]]; then
    die "notarization needs a Developer ID via --identity (ad-hoc builds cannot be notarized)"
fi
if [[ -n "$NOTARY_PROFILE" ]]; then
    command -v xcrun >/dev/null 2>&1 || die "xcrun not found (install the Xcode command line tools)"
fi

for target in "${TARGETS[@]}"; do
    if ! rustup target list --installed | grep -qx "$target"; then
        info "installing Rust target $target"
        rustup target add "$target"
    fi
    info "building $target"
    cargo build --release --locked --target "$target"
done

info "assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

if [[ ${#TARGETS[@]} -eq 1 ]]; then
    cp "$TARGET_DIR/${TARGETS[0]}/release/cometty" "$APP/Contents/MacOS/cometty"
else
    lipo -create \
        "$TARGET_DIR/aarch64-apple-darwin/release/cometty" \
        "$TARGET_DIR/x86_64-apple-darwin/release/cometty" \
        -output "$APP/Contents/MacOS/cometty"
fi
chmod 755 "$APP/Contents/MacOS/cometty"

cp "$ICON" "$APP/Contents/Resources/AppIcon.icns"
sed -e "s/__VERSION__/$VERSION/g" -e "s/__BUILD__/$BUILD/g" \
    "$PLIST_TEMPLATE" > "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"

plutil -lint "$APP/Contents/Info.plist"
file "$APP/Contents/MacOS/cometty"
lipo -archs "$APP/Contents/MacOS/cometty"

# Extended attributes from copied resources make codesign refuse the bundle.
xattr -cr "$APP" 2>/dev/null || true

if [[ "$IDENTITY" == "-" ]]; then
    info "signing ad-hoc (downloaded builds require Gatekeeper approval)"
    codesign --force --sign - "$APP"
else
    info "signing with $IDENTITY"
    codesign --force --options runtime --timestamp --sign "$IDENTITY" "$APP"
fi
codesign --verify --deep --strict "$APP"

# --help resolves before GUI setup, so this stays headless-safe. A single-arch
# cross-build cannot execute on this host; the default universal bundle can.
HOST_ARCH="$(uname -m)"
if [[ "$ARCH" == "universal" || "$ARCH" == "$HOST_ARCH" ]]; then
    "$APP/Contents/MacOS/cometty" --help >/dev/null
else
    info "skipping smoke test: $ARCH build on $HOST_ARCH host"
fi

ZIP_NAME="$APP_NAME-$VERSION-macos-$ARCH.zip"
ZIP="$DIST/$ZIP_NAME"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"

if [[ -n "$NOTARY_PROFILE" ]]; then
    info "notarizing with keychain profile $NOTARY_PROFILE"
    xcrun notarytool submit "$ZIP" --keychain-profile "$NOTARY_PROFILE" --wait
    xcrun stapler staple "$APP"
    xcrun stapler validate "$APP"
    spctl -a -vvv "$APP"

    # Staple the ticket before producing the artifact users download.
    rm -f "$ZIP"
    ditto -c -k --keepParent "$APP" "$ZIP"
fi

( cd "$DIST" && shasum -a 256 "$ZIP_NAME" > "$ZIP_NAME.sha256" )

info "done"
printf '    app:  %s\n' "$APP"
printf '    zip:  %s\n' "$ZIP"
printf '    sha:  %s\n' "$DIST/$ZIP_NAME.sha256"
