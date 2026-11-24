# Development and verification

cometty is an edition-2024 Rust binary. Use a current stable toolchain with
Clippy and rustfmt. Dependencies/features are declared in
[`Cargo.toml`](../Cargo.toml) and resolved in [`Cargo.lock`](../Cargo.lock).

Start with the [architecture](architecture.md); consult
[AGENTS.md](../AGENTS.md) for the short repository working rules.

## Build requirements

macOS builds use the installed system frameworks and Rust toolchain. On
Debian/Ubuntu, install the GUI development libraries used by CI:

```sh
sudo apt-get install -y libx11-dev libxkbcommon-dev libgl1-mesa-dev
```

Compilation/tests need the relevant platform libraries, but **building does not
launch a window or require a working GPU**. Running the application does:

```sh
cargo build --locked
cargo run --release --locked -- --theme tokyo-night
```

Do not launch `cargo run` in a headless session. `--help`/`--list-themes` resolve
before GUI setup; ordinary app startup creates a window, GPU surface, and shell.
For automated headless verification prefer the commands below.

## Releasing

`make release` runs [`scripts/release.py`](../scripts/release.py) and turns the
current branch into a published release, in numbered steps:

- checks that HEAD is on the default branch (`main`, unless
  `RELEASE_ALLOW_BRANCH=1` / `--allow-branch` is passed) with no uncommitted
  changes, untracked files included,
- fetches the branch and tags from `RELEASE_REMOTE` (default `origin`) and
  refuses if the local branch is behind or has diverged; local commits that are
  only ahead are pushed with the release,
- builds a Conventional Commits changelog from commits since the last canonical
  `vX.Y.Z` tag (grouping `feat`, `fix`/`revert`, `perf`/`mem`, breaking changes,
  and a collapsed `<details>` Maintenance section for `refactor`/`docs`/`build`/
  `ci`/`chore`/`test`/`style`), refusing if there are no new commits,
- asks for the next version, auto-suggesting the bump from Conventional Commits
  (`feat` → minor, breaking → major on `>=1.0.0` or minor on `0.x`, else patch;
  `RELEASE_BUMP=patch|minor|major` or `RELEASE_VERSION=X.Y.Z` overrides it),
- prints the release summary and asks for explicit confirmation (`[y/N/e]`,
  where `e` opens the release notes in `$EDITOR` before re-confirming; non-TTY
  runs require both `RELEASE_VERSION=X.Y.Z` and `RELEASE_YES=1`),
- writes the version to `Cargo.toml` and refreshes `Cargo.lock` offline via
  `cargo update --workspace --offline`,
- runs `make verify` (`RELEASE_SKIP_VERIFY=1` skips it),
- commits `chore: release vX.Y.Z` and creates the annotated `vX.Y.Z` tag
  carrying the finalized release notes,
- pushes the branch and tag in one atomic push (and with `RELEASE_WATCH=1` /
  `--watch`, tails the GitHub Actions workflow via `gh`).

The prompt rejects versions that are not canonical `X.Y.Z` (no leading zeros,
strictly greater than the current version) or whose tag already exists locally
or on the remote. A failed push leaves the commit and tag local and prints the
retry and undo commands. `RELEASE_DRY_RUN=1` runs the checks and prints the
changelog and plan without changing, committing, or pushing anything; refused
checks become warnings. `make changelog` previews unreleased notes at any time.
Pushing the tag triggers [`release.yml`](../.github/workflows/release.yml),
which runs the shared [`.github/actions/verify`](../.github/actions/verify/action.yml)
gate (avoiding a duplicate `ci.yml` run for `chore: release v*` commits), builds
and verifies the universal macOS bundle, and **publishes the GitHub release
directly** with the annotated tag's notes (falling back to regenerating them
from git history if a tag was pushed manually). Output is colored on a terminal
unless `NO_COLOR` is set.

### Reaching the updater

The in-app [updater](update.md) reads only GitHub's latest published release,
and skips a release that is a draft or a prerelease. Because `release.yml`
publishes directly once all assets are uploaded (`gh release create` uploads
into a temporary draft before publishing), every installed copy is offered the
new version as soon as the release workflow finishes; prereleases are never
offered. The updater also relies on these names:

- the tag is exactly `vX.Y.Z` (no suffix, no leading zeros),
- the archive asset is `Cometty-<X.Y.Z>-macos-universal.zip`,
- the checksum asset is that name plus `.sha256`, as written by
  `scripts/bundle-macos.sh`.

Renaming either asset or changing the tag scheme hides the release from every
installed copy.

### Manual update checklist

Automated tests do not cover these. Record each outcome here as a measured
result, not a guarantee.

GUI pass (`cargo run` and a bundled build, on a machine with a display):

- [ ] The notice sits at the bottom right, clear of the scrollbar.
- [ ] Its buttons never take keyboard focus or IME from the terminal, and
  typing continues in every tab while it is shown.
- [ ] "Later" hides the notice until relaunch.
- [ ] The Updates checkbox persists across a relaunch without visible reflow or
  re-shaping.
- [ ] A failed download shows the Available notice with its reason, a failed
  feed check shows no notice, and both appear in the log panel under
  `cometty::update`.
- [ ] `make bench-idle` idle wakeups and the binary-size delta are recorded.

Real update (needs a published test release, which changes the shared GitHub
release; macOS only):

- [ ] A copy in `/Applications` downloads, verifies, and stages the update.
- [ ] "Restart to update" swaps, quits, relaunches the new version, and restores
  the session tabs; the previous bundle is removed after the relaunch.
- [ ] Whether macOS privacy (TCC) permissions are requested again.
- [ ] App Management `EPERM` on macOS 13+ returns to the Ready notice with the
  error and nothing changed.
- [ ] A translocated or read-only copy shows the move hint in notify-only mode.
- [ ] A second Dock-launched instance blocks the install.

## Packaging a macOS release

[`scripts/bundle-macos.sh`](../scripts/bundle-macos.sh) builds a universal
(Apple Silicon + Intel) binary, assembles `dist/Cometty.app` from
[`packaging/macos/Info.plist`](../packaging/macos/Info.plist), signs it ad-hoc
by default, and writes `Cometty-<version>-macos-universal.zip` plus a SHA-256
file. Pass `--identity "Developer ID Application: ..."` and
`--notary-profile NAME` for a hardened, notarized build; create the notary
profile once with `xcrun notarytool store-credentials NAME --apple-id ... --team-id ...`.
The script reads the version from `Cargo.toml`, so run `make release` before
building a bundle for a tag. [`release.yml`](../.github/workflows/release.yml)
runs the same script on `v*` tags and publishes the GitHub release directly.

The bundle icon is [`assets/AppIcon.icns`](../assets/AppIcon.icns), generated
once from `logo.svg` via `assets/icon-1024.png`; the embedded 128px icon is
used only for unbundled runs. Keep `LSMinimumSystemVersion` in the plist and
the macOS floor in the README in sync.

## Routine checks

`make verify` runs the local gate in order:

```sh
cargo test --locked
cargo check --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
git diff --check
```

`make fmt` rewrites formatting; `verify` only checks it. `make help` lists the
remaining targets: build/run, release tagging, the headless benchmark, the
cross-terminal comparison, the idle probe, opt-in probes, profiling, bundling,
and locked dependency inventories. Those preserve
the commands and limitations documented here and in [performance](performance.md).

[CI](../.github/workflows/ci.yml) runs the shared
[`.github/actions/verify`](../.github/actions/verify/action.yml) action
(`cargo test --locked`, `cargo clippy --locked --all-targets -- -D warnings`,
and `cargo fmt --check`) on macOS for branch pushes and pull requests (skipping
`chore: release v*` branch pushes, which [`release.yml`](../.github/workflows/release.yml)
verifies in the release job itself). Linux/Windows/backend branches exist, but
CI does not establish non-macOS build/runtime coverage. `--offline` is optional
only when dependencies are already cached.

Ordinary tests need no display, GPU, or external service. Unix lifecycle tests
do spawn/clean up short-lived local children and use real PTYs. Process-wide
thread/descriptor assertions execute in an isolated subprocess. Other model,
egui layout, and frame-boundary tests run headlessly; they are not evidence of
windowed/GPU correctness.

Update tests never touch the network: release feeds, checksums, and archives
come from fixtures and in-memory fakes. On macOS only, extraction and swap
integration tests run the real `/usr/bin/ditto` and `renamex_np` on temporary
directories; elsewhere those paths are checked to report unsupported.

Six probes are opt-in: release headless benchmarks, exact-corpus parser and
layout diagnostics, scheduling under load, retained-glyph pixel equivalence,
and failed-upload native-resource recovery.
See [performance](performance.md) for their exact names/commands and limitations.
Do not run all `--ignored` tests indiscriminately without a working GPU.

## Match tests to the change

| Area | Useful test filters / invariants |
| --- | --- |
| Terminal parsing / Unicode | `term::`, `grid::`, `input::`: every-byte split equivalence, pending wrap, wide repair, BCE, modes/replies |
| History / resize | `grid::history::`, `app::geometry::`: eager-reference equivalence, IDs, capacities/pools, row eviction, zero caps, PTY-size agreement |
| Transport / lifecycle | `pty::`, `app::scheduling::`, `app::pty_io::`: notification races, fairness, shared budget, FIFO/reply reserve, cancellation/reaping |
| Text / frames | `renderer::`: named-font resolution, cached/uncached complex shaping fingerprints, run/advance capacity bounds, prepared-key invalidation, pins, upload/submit/free order on every returned outcome |
| Search / selection / links | `search::`, `selection::`, `links::`, `app::search::`, `app::tab::`: projected columns, eviction/recycling, security gates, no background reveal |
| Tab reordering | `tabbar::`, `renderer::tabbar_ui::`, `app::tab::`, `app::mouse::`, `app::scheduling::`, `app::session::`: gesture/preview/drop, stable identity, capture/cancellation, overflow/idle, preserved caches/transport/drain cursor, saved order |
| Updates | `update::`, `app::update::`, `renderer::update_ui::`: version/asset rules, checksum-before-archive order, staged-digest reuse, staging/lock, swap fallback, instance counting, stale job ids, focus-free notice |
| Panels / settings / logs | `app::settings::`, `app::persistence::`, `logbuf::`, `logging::`, `renderer::log_ui::`: generation ordering, bounded indexing, independent filters, focus and fixed-height rows |

For memory/parser or optimization changes, also run the relevant tests with
`--release`. CI should assert state equivalence, work counts, and resource bounds,
not noisy microsecond speed thresholds. Report a blocked probe as blocked, never
as passed based on a nearby mocked test.

## Manual verification

Changes to event dispatch/rendering need a display-backed pass in addition to
headless tests:

- Keyboard: terminal Tab/arrows/Enter; dead-key/IME commits before/after search,
  log filters, and settings; no invisible text field retaining focus.
- Search: open/re-focus, navigation without losing query focus, smart case,
  Unicode-wide highlights, eviction while streaming, close and resume typing.
- Mouse/chrome: selection/word/line dragging, scrollbar fade and stationary idle,
  link modifier/click before mouse reporting, Shift override, tooltips/animations.
- Tabs: drag first/middle/last in both directions, activate an inactive source,
  crowded-strip edge/trackpad scrolling, click/×/+, Esc/outside/focus-loss
  cancellation, source/other shell exit during a drag, resize/DPI mid-drag,
  hover `↓` on active/inactive/lone foreground tabs, replacement prompt cwd,
  tray peek/↑/×, clicking back to the only foreground tab and resuming input,
  macOS empty-titlebar dragging/zoom (tab bodies, ×, ↓ and + must never move the
  window), system tiling/fullscreen/display changes, search/terminal Tab and IME
  afterward, and saved order after quit/restore. Headless widget tests do not
  replace this native pass.
- Window: resize/DPI, one↔many tabs (macOS always reserves the titlebar), fullscreen
  one-pixel workaround, minimize/occlude/restore, surface recovery and sync timeout.
- Settings: live edits/resets, CLI theme precedence, custom path, save failure,
  close/shutdown flush. Logs: display versus record filters and explicit Copy.
- PTY: earlier/background floods with active input, shell exit/tab removal,
  nonreading paste admission, and repeated close without lingering children/workers.
- Program status/attention: opencode's `OC | <session>` label while it runs and
  the template again after quitting; a permission prompt in a background strip
  tab, a parked tab (tray), and the active tab with the window unfocused (pulse,
  then steady glow; cleared on activate/peek/unpark/refocus); badge counts with
  two sessions; `printf '\a'` from a background tab; `static`/`off` modes; a
  prompt-titling shell (oh-my-zsh) and `ssh` titles; the log panel showing
  `7501 (program status)` handled; and no frames once the glow settles.

Use [performance profiling](performance.md#display-backed-profiling) for frame,
latency, power, or native-memory claims. Historical headless measurements did not
verify those outcomes.

## Dependency-sensitive decisions

Inspect the **locked** dependency source/features before changing parser or GPU
lifetime assumptions. Useful feature inventories:

```sh
cargo tree --locked -e features -i vte
cargo tree --locked -e features -i cosmic-text
```

In this configuration vte's `std` raw-OSC vector needs the application guard,
cosmic-text's optional shape-run cache is not enabled, and glyphon preparation
uses uncached Swash rasterization with its own atlas. Do not diagnose unbounded
caches merely from a dependency type that the actual call path does not populate.
Revalidate these assumptions on upgrades, especially atlas `trim`/pinning,
wgpu staging submission, and egui texture destruction semantics.

Keep documentation by subsystem: update the relevant reference when behavior or
limits change. Keep the root README for installation and user-visible behavior;
put source maps, sequence internals, profiling, tests, and agent guidance here or
in AGENTS.md, not in the README.
