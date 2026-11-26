#!/bin/sh
# Install or update Cometty.app on macOS from published GitHub releases.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/jjunac/cometty/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/jjunac/cometty/main/install.sh | sh -s -- [OPTIONS]
#
# Options:
#   --version X.Y.Z   Install a specific release (default: latest published)
#   --dir PATH        Install directory (default: /Applications, or ~/Applications if not writable)
#   --force           Install even if Cometty is currently running from the destination
#   -h, --help        Show this help message
#
# Environment:
#   COMETTY_VERSION       Default for --version
#   COMETTY_INSTALL_DIR   Default for --dir
set -eu

export LC_ALL=C

REPO="jjunac/cometty"
APP_NAME="Cometty.app"
BUNDLE_ID="com.github.jjunac.cometty"
MIN_MACOS_MAJOR=13

TMP_DIR=""
STAGED_NEW=""

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

info() {
    printf '==> %s\n' "$*"
}

warn() {
    printf 'warning: %s\n' "$*" >&2
}

usage() {
    cat <<'EOF'
Install or update Cometty.app on macOS from published GitHub releases.

Usage:
  curl -fsSL https://raw.githubusercontent.com/jjunac/cometty/main/install.sh | sh
  curl -fsSL https://raw.githubusercontent.com/jjunac/cometty/main/install.sh | sh -s -- [OPTIONS]

Options:
  --version X.Y.Z   Install a specific release (default: latest published)
  --dir PATH        Install directory (default: /Applications, or ~/Applications if not writable)
  --force           Install even if Cometty is currently running from the destination
  -h, --help        Show this help message

Environment:
  COMETTY_VERSION       Default for --version
  COMETTY_INSTALL_DIR   Default for --dir
EOF
}

cleanup() {
    if [ -n "$STAGED_NEW" ] && [ -e "$STAGED_NEW" ]; then
        rm -rf "$STAGED_NEW" 2>/dev/null || true
    fi
    if [ -n "$TMP_DIR" ] && [ -d "$TMP_DIR" ]; then
        rm -rf "$TMP_DIR" 2>/dev/null || true
    fi
}

is_canonical_segment() {
    case "$1" in
        0) return 0 ;;
        [1-9]) return 0 ;;
        [1-9]*[!0-9]*) return 1 ;;
        [1-9]*) return 0 ;;
        *) return 1 ;;
    esac
}

is_canonical_version() {
    _v="$1"
    case "$_v" in
        *.*.*.*) return 1 ;;
        *.*.*) ;;
        *) return 1 ;;
    esac
    _major="${_v%%.*}"
    _rest="${_v#*.}"
    _minor="${_rest%%.*}"
    _patch="${_rest#*.}"
    is_canonical_segment "$_major" \
        && is_canonical_segment "$_minor" \
        && is_canonical_segment "$_patch"
}

check_platform() {
    if [ "$(uname -s)" != "Darwin" ]; then
        die "prebuilt Cometty bundles are macOS-only (build from source with: cargo install --path . --locked)"
    fi

    _os_ver="$(sw_vers -productVersion)"
    _os_major="${_os_ver%%.*}"
    case "$_os_major" in
        ""|*[!0-9]*)
            die "could not determine macOS version from sw_vers: $_os_ver"
            ;;
    esac
    if [ "$_os_major" -lt "$MIN_MACOS_MAJOR" ]; then
        die "Cometty requires macOS $MIN_MACOS_MAJOR or later (found $_os_ver)"
    fi

    for _cmd in curl shasum ditto plutil codesign xattr mktemp ps awk tr wc; do
        if ! command -v "$_cmd" >/dev/null 2>&1; then
            die "required tool not found: $_cmd"
        fi
    done
}

resolve_latest_version() {
    _latest_url="https://github.com/$REPO/releases/latest"
    _effective_url="$(curl --fail --silent --show-error --location --head \
        --proto '=https' --proto-redir '=https' --tlsv1.2 \
        --max-redirs 5 --connect-timeout 10 --max-time 30 \
        -A "cometty-installer" \
        -o /dev/null -w '%{url_effective}' \
        "$_latest_url")" || die "failed to resolve latest release from $_latest_url"

    _tag_prefix="https://github.com/$REPO/releases/tag/v"
    case "$_effective_url" in
        "${_tag_prefix}"*)
            _resolved="${_effective_url#"$_tag_prefix"}"
            ;;
        *)
            die "no published Cometty release found at $_latest_url"
            ;;
    esac

    if ! is_canonical_version "$_resolved"; then
        die "latest release tag is not a canonical vX.Y.Z version: v$_resolved"
    fi
    printf '%s\n' "$_resolved"
}

resolve_install_dir() {
    _explicit="$1"
    _requested="$2"
    _default_dir="${COMETTY_DEFAULT_INSTALL_DIR:-/Applications}"

    if [ "$_explicit" -eq 1 ]; then
        [ -n "$_requested" ] || die "--dir must not be empty"
        if [ ! -d "$_requested" ]; then
            mkdir -p "$_requested" || die "could not create install directory: $_requested"
        fi
        if [ ! -w "$_requested" ]; then
            die "install directory is not writable: $_requested"
        fi
        LOGICAL_INSTALL_DIR="$(cd "$_requested" && pwd -L)"
        INSTALL_DIR="$(cd "$_requested" && pwd -P)"
        return 0
    fi

    if [ -d "$_default_dir" ] && [ -w "$_default_dir" ]; then
        LOGICAL_INSTALL_DIR="$(cd "$_default_dir" && pwd -L)"
        INSTALL_DIR="$(cd "$_default_dir" && pwd -P)"
        return 0
    fi

    if [ -z "${HOME:-}" ]; then
        die "$_default_dir is not writable and HOME is not set; pass --dir PATH"
    fi

    _fallback="$HOME/Applications"
    warn "$_default_dir is not writable; installing to $_fallback"
    if [ ! -d "$_fallback" ]; then
        mkdir -p "$_fallback" || die "could not create fallback install directory: $_fallback"
    fi
    if [ ! -w "$_fallback" ]; then
        die "fallback install directory is not writable: $_fallback"
    fi
    LOGICAL_INSTALL_DIR="$(cd "$_fallback" && pwd -L)"
    INSTALL_DIR="$(cd "$_fallback" && pwd -P)"
}

is_exec_running() {
    ps -axww -o comm= | awk -v t1="$1" -v t2="$2" '
        {
            sub(/^[[:space:]]+/, "")
            if ($0 == t1 || (t2 != "" && $0 == t2)) { found = 1; exit 0 }
        }
        END { exit(found ? 0 : 1) }
    '
}

parse_and_verify_checksum_file() {
    _sha_file="$1"
    _archive_name="$2"

    _sha_bytes="$(wc -c < "$_sha_file" | tr -d '[:space:]')"
    _expected_len=$((64 + 2 + ${#_archive_name}))
    if [ "$_sha_bytes" -ne "$_expected_len" ] && [ "$_sha_bytes" -ne $((_expected_len + 1)) ]; then
        die "invalid checksum file format for $_archive_name"
    fi

    _line_count="$(awk 'END { print NR }' "$_sha_file")"
    if [ "$_line_count" -ne 1 ]; then
        die "invalid checksum file: expected exactly one line"
    fi

    _line=""
    IFS= read -r _line < "$_sha_file" || [ -n "$_line" ]

    case "$_line" in
        *"  $_archive_name")
            _hex="${_line%"  $_archive_name"}"
            ;;
        *)
            die "checksum file does not match $_archive_name"
            ;;
    esac

    if [ "${#_hex}" -ne 64 ]; then
        die "invalid SHA-256 digest length in checksum file"
    fi
    case "$_hex" in
        *[!0-9a-fA-F]*)
            die "invalid SHA-256 digest characters in checksum file"
            ;;
    esac

    printf '%s\n' "$_hex" | tr 'A-F' 'a-f'
}

validate_extracted_bundle() {
    _extract_dir="$1"
    _expected_version="$2"

    _count=0
    _only_entry=""
    for _entry in "$_extract_dir"/* "$_extract_dir"/.[!.]* "$_extract_dir"/..?*; do
        if [ ! -e "$_entry" ] && [ ! -L "$_entry" ]; then
            continue
        fi
        _count=$((_count + 1))
        _only_entry="$_entry"
    done

    if [ "$_count" -ne 1 ] || [ "$_only_entry" != "$_extract_dir/$APP_NAME" ]; then
        die "archive must contain only $APP_NAME at top level"
    fi
    if [ -L "$_only_entry" ] || [ ! -d "$_only_entry" ]; then
        die "extracted $APP_NAME is not a directory"
    fi

    if [ -L "$_only_entry/Contents" ] || [ ! -d "$_only_entry/Contents" ]; then
        die "extracted bundle has invalid Contents directory"
    fi
    if [ -L "$_only_entry/Contents/MacOS" ] || [ ! -d "$_only_entry/Contents/MacOS" ]; then
        die "extracted bundle has invalid Contents/MacOS directory"
    fi

    _exec="$_only_entry/Contents/MacOS/cometty"
    if [ -L "$_exec" ] || [ ! -f "$_exec" ] || [ ! -x "$_exec" ]; then
        die "extracted bundle is missing an executable Contents/MacOS/cometty"
    fi

    _plist="$_only_entry/Contents/Info.plist"
    if [ -L "$_plist" ] || [ ! -f "$_plist" ]; then
        die "extracted bundle is missing Contents/Info.plist"
    fi
    if ! plutil -lint "$_plist" >/dev/null 2>&1; then
        die "extracted Info.plist failed plutil validation"
    fi

    _found_id="$(plutil -extract CFBundleIdentifier raw "$_plist" 2>/dev/null || true)"
    if [ "$_found_id" != "$BUNDLE_ID" ]; then
        die "unexpected CFBundleIdentifier: ${_found_id:-<missing>} (expected $BUNDLE_ID)"
    fi

    _found_ver="$(plutil -extract CFBundleShortVersionString raw "$_plist" 2>/dev/null || true)"
    if [ "$_found_ver" != "$_expected_version" ]; then
        die "unexpected CFBundleShortVersionString: ${_found_ver:-<missing>} (expected $_expected_version)"
    fi

    if ! codesign --verify --deep --strict "$_only_entry" >/dev/null 2>&1; then
        die "extracted $APP_NAME failed codesign verification"
    fi
}

main() {
    VERSION="${COMETTY_VERSION:-}"
    EXPLICIT_VERSION=0
    if [ -n "$VERSION" ]; then
        EXPLICIT_VERSION=1
    fi
    REQUESTED_DIR="${COMETTY_INSTALL_DIR:-}"
    EXPLICIT_DIR=0
    if [ -n "$REQUESTED_DIR" ]; then
        EXPLICIT_DIR=1
    fi
    FORCE=0

    while [ $# -gt 0 ]; do
        case "$1" in
            --version)
                [ $# -ge 2 ] || die "--version needs a value"
                VERSION="$2"
                EXPLICIT_VERSION=1
                shift 2
                ;;
            --dir)
                [ $# -ge 2 ] || die "--dir needs a value"
                REQUESTED_DIR="$2"
                EXPLICIT_DIR=1
                shift 2
                ;;
            --force)
                FORCE=1
                shift
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

    check_platform

    if [ "$EXPLICIT_VERSION" -eq 1 ]; then
        VERSION="${VERSION#v}"
        if ! is_canonical_version "$VERSION"; then
            die "version must be canonical X.Y.Z (got: $VERSION)"
        fi
    else
        info "resolving latest Cometty release"
        VERSION="$(resolve_latest_version)"
    fi

    LOGICAL_INSTALL_DIR=""
    INSTALL_DIR=""
    resolve_install_dir "$EXPLICIT_DIR" "$REQUESTED_DIR"
    DEST_APP="${INSTALL_DIR%/}/$APP_NAME"
    DEST_EXEC="$DEST_APP/Contents/MacOS/cometty"
    LOGICAL_EXEC="${LOGICAL_INSTALL_DIR%/}/$APP_NAME/Contents/MacOS/cometty"

    if [ "$FORCE" -ne 1 ] && [ -e "$DEST_APP" ]; then
        if is_exec_running "$DEST_EXEC" "$LOGICAL_EXEC"; then
            die "Cometty is currently running from $DEST_APP; quit Cometty first and re-run (or pass --force)"
        fi
    fi

    trap cleanup EXIT INT TERM
    TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/cometty-install.XXXXXX")"
    chmod 700 "$TMP_DIR"

    ARCHIVE_NAME="Cometty-$VERSION-macos-universal.zip"
    SHA_NAME="$ARCHIVE_NAME.sha256"
    BASE_URL="https://github.com/$REPO/releases/download/v$VERSION"
    ZIP_URL="$BASE_URL/$ARCHIVE_NAME"
    SHA_URL="$BASE_URL/$SHA_NAME"

    SHA_FILE="$TMP_DIR/$SHA_NAME"
    ZIP_FILE="$TMP_DIR/$ARCHIVE_NAME"
    EXTRACT_DIR="$TMP_DIR/extract"
    mkdir -p "$EXTRACT_DIR"

    info "fetching checksum for Cometty $VERSION"
    curl --fail --silent --show-error --location \
        --proto '=https' --proto-redir '=https' --tlsv1.2 \
        --max-redirs 5 --connect-timeout 10 --max-time 30 \
        -A "cometty-installer" \
        -o "$SHA_FILE" "$SHA_URL" \
        || die "failed to download $SHA_URL"

    EXPECTED_SHA="$(parse_and_verify_checksum_file "$SHA_FILE" "$ARCHIVE_NAME")"

    info "downloading $ARCHIVE_NAME"
    if [ -t 2 ]; then
        curl --fail --location --progress-bar \
            --proto '=https' --proto-redir '=https' --tlsv1.2 \
            --max-redirs 5 --connect-timeout 10 --max-time 600 \
            -A "cometty-installer" \
            -o "$ZIP_FILE" "$ZIP_URL" \
            || die "failed to download $ZIP_URL"
    else
        curl --fail --silent --show-error --location \
            --proto '=https' --proto-redir '=https' --tlsv1.2 \
            --max-redirs 5 --connect-timeout 10 --max-time 600 \
            -A "cometty-installer" \
            -o "$ZIP_FILE" "$ZIP_URL" \
            || die "failed to download $ZIP_URL"
    fi

    info "verifying SHA-256 checksum"
    ACTUAL_SHA="$(shasum -a 256 "$ZIP_FILE" | awk '{ print $1 }' | tr 'A-F' 'a-f')"
    if [ "$ACTUAL_SHA" != "$EXPECTED_SHA" ]; then
        die "SHA-256 mismatch for $ARCHIVE_NAME (expected $EXPECTED_SHA, got $ACTUAL_SHA)"
    fi

    info "extracting and validating $APP_NAME"
    ditto -x -k "$ZIP_FILE" "$EXTRACT_DIR" \
        || die "failed to extract $ARCHIVE_NAME"
    validate_extracted_bundle "$EXTRACT_DIR" "$VERSION"

    EXTRACTED_APP="$EXTRACT_DIR/$APP_NAME"
    xattr -dr com.apple.quarantine "$EXTRACTED_APP" 2>/dev/null || true

    PREV_VERSION=""
    if [ -f "$DEST_APP/Contents/Info.plist" ]; then
        PREV_VERSION="$(plutil -extract CFBundleShortVersionString raw "$DEST_APP/Contents/Info.plist" 2>/dev/null || true)"
    fi

    if [ -n "$PREV_VERSION" ] && [ "$PREV_VERSION" != "$VERSION" ]; then
        info "upgrading Cometty $PREV_VERSION -> $VERSION in $INSTALL_DIR"
    elif [ -n "$PREV_VERSION" ]; then
        info "reinstalling Cometty $VERSION in $INSTALL_DIR"
    else
        info "installing Cometty $VERSION to $DEST_APP"
    fi

    STAGED_NEW="${INSTALL_DIR%/}/.${APP_NAME}.install-new.$$"
    BACKUP_OLD="${INSTALL_DIR%/}/.${APP_NAME}.install-old.$$"
    rm -rf "$STAGED_NEW" "$BACKUP_OLD"

    ditto "$EXTRACTED_APP" "$STAGED_NEW" \
        || die "failed to stage $APP_NAME into $INSTALL_DIR"

    if [ -e "$DEST_APP" ] || [ -L "$DEST_APP" ]; then
        mv "$DEST_APP" "$BACKUP_OLD" \
            || die "failed to move existing $DEST_APP aside"
        if ! mv "$STAGED_NEW" "$DEST_APP"; then
            mv "$BACKUP_OLD" "$DEST_APP" 2>/dev/null || true
            die "failed to install $DEST_APP (previous version restored)"
        fi
        STAGED_NEW=""
        if ! rm -rf "$BACKUP_OLD" 2>/dev/null; then
            warn "could not remove previous bundle backup at $BACKUP_OLD"
        fi
    else
        mv "$STAGED_NEW" "$DEST_APP" \
            || die "failed to move $APP_NAME into $DEST_APP"
        STAGED_NEW=""
    fi

    info "installed Cometty $VERSION at $DEST_APP"
    printf '    launch with: open "%s"\n' "$DEST_APP"
}

main "$@"
