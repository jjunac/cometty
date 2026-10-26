#!/usr/bin/env bash
# Bump the package version, commit it, and create the matching release tag.
#
# Usage:
#   ./scripts/release.sh [--bump patch|minor|major] [--version X.Y.Z]
#                        [--push] [--remote NAME] [--dry-run] [--no-verify]
#
# Defaults:
#   --bump patch     increment the current Cargo.toml version
#   --version X.Y.Z  set the next version instead of incrementing it
#   --remote origin  remote used by --push
#   no push          print the push commands instead
#   verify           run `make verify` before committing (Cargo.lock is still
#                    refreshed; --no-verify only skips the test/lint gate)
#
# The tag push triggers .github/workflows/release.yml, which builds the macOS
# bundle and opens a draft GitHub release.
set -euo pipefail

SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
ROOT="$(cd "$(dirname "$SELF")/.." && pwd)"
cd "$ROOT"

BUMP="patch"
BUMP_SET=0
VERSION=""
VERSION_SET=0
PUSH=0
REMOTE="origin"
DRY_RUN=0
VERIFY=1

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
info() { printf '==> %s\n' "$*"; }

usage() {
    awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$SELF"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --bump) BUMP="${2:?--bump needs a value}"; BUMP_SET=1; shift 2 ;;
        --bump=*) BUMP="${1#*=}"; BUMP_SET=1; shift ;;
        --version) VERSION="${2:?--version needs a value}"; VERSION_SET=1; shift 2 ;;
        --version=*) VERSION="${1#*=}"; VERSION_SET=1; shift ;;
        --push) PUSH=1; shift ;;
        --remote) REMOTE="${2:?--remote needs a value}"; shift 2 ;;
        --remote=*) REMOTE="${1#*=}"; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        --no-verify) VERIFY=0; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown option: $1 (try --help)" ;;
    esac
done

(( BUMP_SET && VERSION_SET )) && die "--bump and --version are mutually exclusive"

case "$BUMP" in
    patch|minor|major) ;;
    *) die "unknown bump: $BUMP (expected patch, minor, or major)" ;;
esac

command -v git >/dev/null 2>&1 || die "git not found"
command -v cargo >/dev/null 2>&1 || die "cargo not found"
command -v perl >/dev/null 2>&1 || die "perl not found"

CURRENT="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
[[ -n "$CURRENT" ]] || die "could not read the package version from Cargo.toml"
[[ "$CURRENT" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "Cargo.toml version is not X.Y.Z: $CURRENT"

version_gt() {
    local left="$1" right="$2" left_major left_minor left_patch
    local right_major right_minor right_patch
    IFS=. read -r left_major left_minor left_patch <<<"$left"
    IFS=. read -r right_major right_minor right_patch <<<"$right"
    left_major=$((10#$left_major))
    left_minor=$((10#$left_minor))
    left_patch=$((10#$left_patch))
    right_major=$((10#$right_major))
    right_minor=$((10#$right_minor))
    right_patch=$((10#$right_patch))
    if (( left_major != right_major )); then
        if (( left_major > right_major )); then return 0; else return 1; fi
    fi
    if (( left_minor != right_minor )); then
        if (( left_minor > right_minor )); then return 0; else return 1; fi
    fi
    if (( left_patch > right_patch )); then return 0; else return 1; fi
}

if [[ -n "$VERSION" ]]; then
    [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "version is not X.Y.Z: $VERSION"
    NEW="$VERSION"
    version_gt "$NEW" "$CURRENT" || die "new version $NEW must be greater than $CURRENT"
else
    IFS=. read -r current_major current_minor current_patch <<<"$CURRENT"
    current_major=$((10#$current_major))
    current_minor=$((10#$current_minor))
    current_patch=$((10#$current_patch))
    case "$BUMP" in
        patch) NEW="$current_major.$current_minor.$((current_patch + 1))" ;;
        minor) NEW="$current_major.$((current_minor + 1)).0" ;;
        major) NEW="$((current_major + 1)).0.0" ;;
    esac
fi
TAG="v$NEW"

BRANCH="$(git symbolic-ref --quiet --short HEAD || true)"
[[ -n "$BRANCH" ]] || die "HEAD is detached; check out a branch before releasing"

DIRTY="$(git status --porcelain --untracked-files=no)"
if [[ -n "$DIRTY" ]]; then
    if (( DRY_RUN )); then
        printf 'warning: a real release would refuse these tracked changes:\n%s\n' "$DIRTY" >&2
    else
        die "working tree has tracked changes; commit or stash them before releasing"
    fi
fi

if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    die "tag already exists: $TAG"
fi
if (( PUSH )); then
    if git ls-remote --exit-code --tags "$REMOTE" "refs/tags/$TAG" >/dev/null 2>&1; then
        die "remote $REMOTE already has tag $TAG"
    fi
fi

if (( VERIFY )); then VERIFY_LABEL="make verify"; else VERIFY_LABEL="skipped"; fi

if (( DRY_RUN )); then
    printf 'Release plan (dry run):\n'
    printf '  branch:  %s\n' "$BRANCH"
    printf '  current: %s\n' "$CURRENT"
    printf '  next:    %s\n' "$NEW"
    printf '  tag:     %s\n' "$TAG"
    printf '  verify:  %s\n' "$VERIFY_LABEL"
    printf '  commit:  chore: release %s\n' "$TAG"
    if (( PUSH )); then
        printf '  push:    %s and %s to %s\n' "$BRANCH" "$TAG" "$REMOTE"
    else
        printf '  push:    no\n'
    fi
    printf 'No files were changed.\n'
    exit 0
fi

MODIFIED=0
COMMITTED=0
cleanup() {
    local status=$?
    if (( status != 0 && MODIFIED == 1 && COMMITTED == 0 )); then
        printf 'restoring Cargo.toml and Cargo.lock\n' >&2
        git reset -q -- Cargo.toml Cargo.lock || true
        git checkout -- Cargo.toml Cargo.lock || true
    fi
}
trap cleanup EXIT

info "bumping $CURRENT -> $NEW"
MODIFIED=1
NEW="$NEW" perl -0pi -e 's/(\[package\][^\[]*?\nversion = ")[^"]*(")/$1$ENV{NEW}$2/s' Cargo.toml
ACTUAL="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
[[ "$ACTUAL" == "$NEW" ]] || die "failed to update Cargo.toml to $NEW"

info "refreshing Cargo.lock"
"${CARGO:-cargo}" check --quiet
LOCK_VERSION="$(awk '/^name = "cometty"$/ { getline; print }' Cargo.lock | head -n 1)"
[[ "$LOCK_VERSION" == "version = \"$NEW\"" ]] || \
    die "Cargo.lock did not update to $NEW (found: ${LOCK_VERSION:-none})"

if (( VERIFY )); then
    info "running make verify"
    "${MAKE:-make}" verify
fi

git add -- Cargo.toml Cargo.lock
git commit -m "chore: release $TAG"
COMMITTED=1
git tag -a "$TAG" -m "cometty $TAG"

if (( PUSH )); then
    info "pushing $BRANCH and $TAG to $REMOTE"
    git push "$REMOTE" "HEAD:$BRANCH" || \
        die "failed to push $BRANCH; local commit and $TAG remain"
    git push "$REMOTE" "$TAG" || \
        die "failed to push $TAG; local commit and tag remain"
fi

REV="$(git rev-parse --short HEAD)"
printf '\nReleased %s (%s).\n' "$TAG" "$REV"
if (( PUSH )); then
    printf 'Pushed %s and %s; the release workflow will build the draft release.\n' "$BRANCH" "$TAG"
else
    printf 'Publish with:\n  git push %s %s\n  git push %s %s\n' "$REMOTE" "$BRANCH" "$REMOTE" "$TAG"
fi
