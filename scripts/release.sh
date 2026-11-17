#!/usr/bin/env bash
# Release the current branch: ask for the next version, verify, commit, tag, and
# push the commit and tag together.
#
# Usage:
#   ./scripts/release.sh [--bump patch|minor|major] [--version X.Y.Z]
#                        [--remote NAME] [--dry-run] [--no-verify]
#
# Defaults:
#   prompt           ask for the next version; Enter accepts the suggestion
#   --bump patch     suggest this increment of the current Cargo.toml version
#   --version X.Y.Z  release X.Y.Z without prompting (required when stdin is
#                    not a terminal)
#   --remote origin  remote the branch must match and the release is pushed to
#   verify           run `make verify` before committing (Cargo.lock is still
#                    refreshed; --no-verify only skips the test/lint gate)
#   --dry-run        run every check and print the plan without changing,
#                    committing, or pushing anything
#
# A release refuses a detached HEAD, uncommitted changes (untracked files
# included), and a branch that is behind or has diverged from the remote.
# Local commits not on the remote yet are pushed with the release. The pushed
# tag triggers .github/workflows/release.yml, which builds the macOS bundle and
# opens a draft GitHub release. NO_COLOR=1 disables colors.
set -euo pipefail

SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
ROOT="$(cd "$(dirname "$SELF")/.." && pwd)"
cd "$ROOT"

BUMP="patch"
BUMP_SET=0
VERSION=""
VERSION_SET=0
REMOTE="origin"
DRY_RUN=0
VERIFY=1

# Colors only when both output streams are terminals.
if [[ -t 1 && -t 2 && -z "${NO_COLOR:-}" && "${TERM:-}" != dumb ]]; then
    BOLD=$'\033[1m' DIM=$'\033[2m' RED=$'\033[31m' GREEN=$'\033[32m'
    YELLOW=$'\033[33m' BLUE=$'\033[34m' RESET=$'\033[0m'
else
    BOLD="" DIM="" RED="" GREEN="" YELLOW="" BLUE="" RESET=""
fi

STEP=0
STEPS=7
# Messages inside a step are indented under its header.
indent() { if (( STEP )); then printf '  '; fi; }
error() { indent >&2; printf '%serror:%s %s\n' "$RED$BOLD" "$RESET" "$*" >&2; }
die() { error "$@"; exit 1; }
warn() { indent >&2; printf '%swarning:%s %s\n' "$YELLOW$BOLD" "$RESET" "$*" >&2; }
ok() { indent; printf '%s✓%s %s\n' "$GREEN$BOLD" "$RESET" "$*"; }
step() {
    STEP=$((STEP + 1))
    printf '\n%s[%d/%d]%s %s%s%s\n' "$BLUE$BOLD" "$STEP" "$STEPS" "$RESET" "$BOLD" "$*" "$RESET"
}
# A failed precondition stops a real release; a dry run reports it and goes on.
refuse() {
    if (( DRY_RUN )); then warn "a real release would stop here: $*"; else die "$*"; fi
}

usage() {
    awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$SELF"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --bump) BUMP="${2:?--bump needs a value}"; BUMP_SET=1; shift 2 ;;
        --bump=*) BUMP="${1#*=}"; BUMP_SET=1; shift ;;
        --version) VERSION="${2:?--version needs a value}"; VERSION_SET=1; shift 2 ;;
        --version=*) VERSION="${1#*=}"; VERSION_SET=1; shift ;;
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

IFS=. read -r current_major current_minor current_patch <<<"$CURRENT"
current_major=$((10#$current_major))
current_minor=$((10#$current_minor))
current_patch=$((10#$current_patch))
case "$BUMP" in
    patch) SUGGESTED="$current_major.$current_minor.$((current_patch + 1))" ;;
    minor) SUGGESTED="$current_major.$((current_minor + 1)).0" ;;
    major) SUGGESTED="$((current_major + 1)).0.0" ;;
esac

# Tag refs on the remote, one per line; filled by check_remote.
REMOTE_TAGS=""

# Print why $1 cannot be the next release; print nothing when it can.
version_problem() {
    if [[ ! "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        printf 'version is not X.Y.Z: %s' "$1"
    elif ! version_gt "$1" "$CURRENT"; then
        printf 'new version %s must be greater than %s' "$1" "$CURRENT"
    elif git rev-parse -q --verify "refs/tags/v$1" >/dev/null; then
        printf 'tag already exists: v%s' "$1"
    elif grep -qxF "refs/tags/v$1" <<<"$REMOTE_TAGS"; then
        printf '%s already has tag v%s' "$REMOTE" "$1"
    fi
}

# Fetch the remote branch and require HEAD to contain it, so the final push is
# a fast-forward. Records the remote's tags for version_problem.
check_remote() {
    local refs remote_sha behind ahead
    if ! git remote get-url "$REMOTE" >/dev/null 2>&1; then
        refuse "no remote named $REMOTE"
        return
    fi
    if ! refs="$(git ls-remote --refs "$REMOTE" "refs/heads/$BRANCH" 'refs/tags/*')"; then
        refuse "could not reach $REMOTE"
        return
    fi
    REMOTE_TAGS="$(awk '$2 ~ /^refs\/tags\// { print $2 }' <<<"$refs")"
    remote_sha="$(awk -v ref="refs/heads/$BRANCH" '$2 == ref { print $1 }' <<<"$refs")"
    if [[ -z "$remote_sha" ]]; then
        warn "$REMOTE has no $BRANCH branch yet; the push will create it"
        return
    fi
    if ! git fetch --quiet "$REMOTE" "refs/heads/$BRANCH"; then
        refuse "could not fetch $BRANCH from $REMOTE"
        return
    fi
    read -r behind ahead <<<"$(git rev-list --left-right --count "$remote_sha...HEAD")"
    if (( behind && ahead )); then
        refuse "$BRANCH has diverged from $REMOTE/$BRANCH ($ahead local, $behind remote commits); rebase or merge first"
    elif (( behind )); then
        refuse "$BRANCH is $behind commit(s) behind $REMOTE/$BRANCH; pull first"
    elif (( ahead )); then
        warn "$ahead local commit(s) not on $REMOTE yet will be pushed with the release"
    else
        ok "$BRANCH is up to date with $REMOTE/$BRANCH"
    fi
}

step "Checking the working tree"
BRANCH="$(git symbolic-ref --quiet --short HEAD || true)"
[[ -n "$BRANCH" ]] || die "HEAD is detached; check out a branch before releasing"
DIRTY="$(git status --porcelain)"
if [[ -n "$DIRTY" ]]; then
    refuse "uncommitted changes; commit, stash, or remove them first:"$'\n'"$(sed 's/^/      /' <<<"$DIRTY")"
else
    ok "on $BRANCH at $(git rev-parse --short HEAD), nothing uncommitted"
fi

step "Comparing $BRANCH with $REMOTE"
check_remote

step "Choosing the version"
if [[ -n "$VERSION" ]]; then
    NEW="$VERSION"
    PROBLEM="$(version_problem "$NEW")"
    [[ -z "$PROBLEM" ]] || die "$PROBLEM"
else
    [[ -t 0 ]] || die "no terminal to ask for the version; pass --version X.Y.Z (make: RELEASE_VERSION=X.Y.Z)"
    # An empty answer takes the suggestion; an invalid one asks again.
    while :; do
        read -r -e -p "  Next version (current $CURRENT) [$SUGGESTED]: " ANSWER || {
            printf '\n' >&2
            die "aborted: no version entered"
        }
        NEW="${ANSWER:-$SUGGESTED}"
        PROBLEM="$(version_problem "$NEW")"
        [[ -n "$PROBLEM" ]] || break
        error "$PROBLEM"
    done
fi
TAG="v$NEW"
ok "releasing $TAG (from $CURRENT)"

if (( DRY_RUN )); then
    if (( VERIFY )); then VERIFY_PLAN="run make verify"; else VERIFY_PLAN="skip make verify"; fi
    printf '\n%sDry run:%s nothing was changed. A real release would:\n' "$YELLOW$BOLD" "$RESET"
    printf '  - set Cargo.toml and Cargo.lock to %s\n' "$NEW"
    printf '  - %s\n' "$VERIFY_PLAN"
    printf '  - commit "chore: release %s" on %s and tag it %s\n' "$TAG" "$BRANCH" "$TAG"
    printf '  - push %s and %s to %s\n' "$BRANCH" "$TAG" "$REMOTE"
    exit 0
fi

MODIFIED=0
COMMITTED=0
cleanup() {
    local status=$?
    if (( status != 0 && MODIFIED == 1 && COMMITTED == 0 )); then
        warn "restoring Cargo.toml and Cargo.lock"
        git reset -q -- Cargo.toml Cargo.lock || true
        git checkout -- Cargo.toml Cargo.lock || true
    fi
}
trap cleanup EXIT

step "Updating the version"
MODIFIED=1
NEW="$NEW" perl -0pi -e 's/(\[package\][^\[]*?\nversion = ")[^"]*(")/$1$ENV{NEW}$2/s' Cargo.toml
ACTUAL="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
[[ "$ACTUAL" == "$NEW" ]] || die "failed to update Cargo.toml to $NEW"
ok "Cargo.toml: $CURRENT -> $NEW"
"${CARGO:-cargo}" check --quiet
LOCK_VERSION="$(awk '/^name = "cometty"$/ { getline; print }' Cargo.lock | head -n 1)"
[[ "$LOCK_VERSION" == "version = \"$NEW\"" ]] || \
    die "Cargo.lock did not update to $NEW (found: ${LOCK_VERSION:-none})"
ok "Cargo.lock refreshed"

step "Verifying"
if (( VERIFY )); then
    "${MAKE:-make}" verify
    ok "make verify passed"
else
    warn "skipped (--no-verify)"
fi

step "Committing and tagging"
git add -- Cargo.toml Cargo.lock
git commit -q -m "chore: release $TAG"
COMMITTED=1
git tag -a "$TAG" -m "cometty $TAG"
ok "committed $(git rev-parse --short HEAD) \"chore: release $TAG\" and tagged $TAG"

step "Pushing to $REMOTE"
# Atomic: the remote gets both the branch update and the tag, or neither.
if ! git push --atomic "$REMOTE" "HEAD:refs/heads/$BRANCH" "refs/tags/$TAG"; then
    die "push failed; the release commit and $TAG are still local."$'\n'"    retry: git push --atomic $REMOTE HEAD:refs/heads/$BRANCH refs/tags/$TAG"$'\n'"    undo:  git tag -d $TAG && git reset --keep HEAD~1"
fi
ok "pushed $BRANCH and $TAG"

printf '\n%s✓ Released %s%s\n' "$GREEN$BOLD" "$TAG" "$RESET"
printf '  %sThe release workflow will build the draft GitHub release.%s\n' "$DIM" "$RESET"
