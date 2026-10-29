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

`make release` turns the current tree into a release:

- bumps the `Cargo.toml` version (`RELEASE_BUMP=patch|minor|major`, default
  patch, or set `RELEASE_VERSION=X.Y.Z`),
- refreshes `Cargo.lock`,
- runs `make verify`,
- commits `chore: release vX.Y.Z`,
- creates the annotated `vX.Y.Z` tag.

It refuses tracked worktree changes, a detached HEAD, or an existing tag.
`RELEASE_DRY_RUN=1` prints the plan without touching the tree;
`RELEASE_SKIP_VERIFY=1` skips the gate. Nothing is pushed unless
`RELEASE_PUSH=1` (remote `RELEASE_REMOTE`, default `origin`); otherwise the
target prints the two `git push` commands. Pushing the tag triggers
[`release.yml`](../.github/workflows/release.yml), which tests, builds the
macOS bundle, and opens a draft release.

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
runs the same script on `v*` tags and opens a draft GitHub release.

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

[CI](../.github/workflows/ci.yml) runs `cargo test`,
`cargo clippy -- -D warnings`, and `cargo fmt --check` on Linux and macOS.
All-target Clippy also checks test-only code locally. Windows/backend branches
exist, but this CI matrix does not establish Windows build/runtime coverage.
`--offline` is optional only when dependencies are already cached.

Ordinary tests need no display, GPU, or external service. Unix lifecycle tests
do spawn/clean up short-lived local children and use real PTYs. Process-wide
thread/descriptor assertions execute in an isolated subprocess. Other model,
egui layout, and frame-boundary tests run headlessly; they are not evidence of
windowed/GPU correctness.

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
- Window: resize/DPI, one↔many tabs (macOS always reserves the titlebar), fullscreen
  one-pixel workaround, minimize/occlude/restore, surface recovery and sync timeout.
- Settings: live edits/resets, CLI theme precedence, custom path, save failure,
  close/shutdown flush. Logs: display versus record filters and explicit Copy.
- PTY: earlier/background floods with active input, shell exit/tab removal,
  nonreading paste admission, and repeated close without lingering children/workers.

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
