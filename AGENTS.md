# AGENTS.md

Single-binary Rust terminal emulator (`cometty`), edition 2024. Start with the
[documentation index](docs/README.md) and read the relevant subsystem page before
changing its invariants. Keep model state in `app/`/pure helpers and widgets in
`renderer/`; the window, renderer, clipboard, and active-view snapshot are shared,
while terminal/history/transport/search are per-tab.

## Commands and verification

```sh
cargo test --locked
cargo check --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
git diff --check
```

CI runs tests, Clippy (`-D warnings`), and rustfmt on Linux/macOS. Ordinary tests
are headless-safe; Unix PTY lifecycle tests spawn and clean up local children.
Four ignored probes need explicit opt-in; two require a working GPU. See
[development](docs/development.md) and [performance](docs/performance.md).

`cargo run` launches the GUI and requires a display/GPU—do not run it headless.
`cargo build`/`check`/ordinary tests do not require a live graphics device, though
platform development libraries are needed to compile. Do not equate mocked frame
tests or headless timings with native-memory, GUI-latency, or energy verification.

## Where to work

| Area | Entry points | Design reference |
| --- | --- | --- |
| Event loop / settings / logging / session | `src/main.rs`, `src/app/{mod,scheduling,persistence,session}.rs`, `src/{logbuf,logging,session}.rs` | [Architecture](docs/architecture.md) |
| Parser / grid / history | `src/term.rs`, `src/term/parser.rs`, `src/grid/` | [Terminal model](docs/terminal.md) |
| Transport / shutdown | `src/pty.rs`, `src/pty/`, `src/app/pty_io.rs` | [PTY lifecycle](docs/pty.md) |
| Text / GPU / chrome | `src/renderer/`, `src/theme.rs`, `src/scrollbar.rs`, `src/tabbar.rs` | [Rendering](docs/rendering.md) |
| Input / search / links / titles | `src/app/{keyboard,mouse,search,links,tab}.rs`, pure `src/{input,search,selection,links,procinfo}.rs` | [Interaction](docs/interaction.md) |
| Storage limits / diagnostics | History, input, parser, search and renderer ownership | [Memory](docs/memory.md), [profiling](docs/performance.md) |

## Critical invariants

- **Terminal:** preserve `feed`'s parser swap, pending-wrap Unicode/lead/continuation
  semantics, and BCE `erase_cell()` versus structural `blank_cell()` fills. OSC 8
  state is outside `Pen`; SGR reset must not clear it. Keep inspection/test helpers
  marked `#[allow(dead_code)]`, rather than removing them as cleanup.
- **OSC:** bound wire input before vte accumulation (2 MiB); overflow dispatches
  no prefix. Any ESC ends OSC and starts the next escape; `0x9c` is data there.
  Release exceptional >64 KiB buffers only at safe boundaries. Keep handler
  preflights, final-UTF-8 clipboard cap, redaction, and split/allocation regressions.
- **History:** never eagerly rewrite sealed payloads on resize or decode all history
  for a scan. `RowView` is the shared projection; shrink is destructive, padding
  keeps growth-time colors. Compaction preserves IDs; recycling changes them.
  Charge capacities/dead prefixes/pools, evict by row, release all on clear/zero
  caps, and rebase selection/search through `history_dropped`.
- **Scheduling/input:** keep fair order and one shared drain budget per turn;
  parsing/replies/exits progress independently of presentation. Input admission is
  nonblocking and capacity-accounted through in-flight writes. Preserve reply
  headroom/progress wakes, FIFO paste transactions, and visible rejection notices.
- **Shutdown:** tab close cancels unsent input; ordinary sender drop drains it.
  Unix poll cancellation cannot be replaced by a stop flag/cross-thread fd close.
  Never wait/kill synchronously/join live workers on the UI thread. The supervisor
  owns children until reaped; never signal a cached PID afterward or recursively
  hunt detached descendants. Preserve data-before-Exit and the shared exit deadline.
- **GPU:** all frame writes/submissions use `renderer::frame`'s returned-outcome
  boundary. Mark upload calls before partial-failure dependencies; flush pending
  uploads on a live-device skip/error and free textures after submission. Keep
  atlas pins on preparation reuse. Device loss stops work/retries until restart.
- **Geometry:** use `app::geometry::clamp_grid_size` for window/Grid/PTY limits;
  preserve device-pixel shared edges, linear-light colors, highlight ordering, and
  the deliberate macOS fullscreen one-pixel crop.
- **Focus/repaint:** terminal-owned Tab bypasses egui; chrome stays non-focusable.
  Route IME commits under the same ownership policy and reassert terminal IME after
  text fields release it. Forward both input repaint responses (not for
  `RedrawRequested`) and root repaint delays; idle must remain reactive.
- **Search/links:** search opens/re-focuses after the settings guard; navigation
  precedes egui consumption and closing surrenders focus. Cache by content/row
  identity, never scroll on background refresh. Modifier-only hints must repaint;
  open links before mouse reporting. Preserve URI allowlisting and clipboard-read
  opt-in. See interaction docs for tab-label literal/OSC 7 precedence rules.
- **Settings/logs:** `ConfigSaver` is the sole ordered writer; preserve debounce,
  generation checks, atomic replacement, shutdown flush, and GUI/CLI precedence.
  Unrelated chrome/input edits must not flush shaping caches. Keep record/display
  filters separate, target-scoped recording independent of `RUST_LOG`, ID-only
  incremental log indexing outside the lock, fixed-height rows, and no closed-panel
  ring lock.
- **Session:** exit-time work belongs in `exiting()` (macOS Cmd+Q never returns
  from `run_app`). Tabs open at exit save; none left after a spawn removes; only
  the lock holder touches the snapshot; writes/removals share one ordered queue.
  Restore shell cwds from the kernel, not OSC 7.

## Documentation contract

- `README.md` is **for users only**: installation, usage, shortcuts, preferences,
  safety, and visible limitations. No source maps, dependency-stack explanations,
  internal algorithms, test commands, or contributor/agent guidance there.
- Maintain architecture rationale and implementation details in `docs/`; keep
  AGENTS.md a short working map plus high-risk rules, not a duplicate manual.
- Update the relevant document when limits/cache keys/ownership change. Explicitly
  distinguish implemented behavior, open work, modeled tests, and measured results.
  Do not bring back chronological studies or stale before/after tables as current
  guarantees. [Memory](docs/memory.md) lists unresolved bounds; compact history is
  not an application RAM limit, and full-history search is still synchronous.
