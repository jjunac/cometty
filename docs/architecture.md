# Architecture

cometty is a single Rust binary (edition 2024). The UI thread owns terminal
state and rendering; background workers handle PTY I/O, child lifecycle, and
configuration writes. Model helpers remain usable without a display or GPU.

## Ownership and module boundaries

```text
App
├── shared Window, Renderer, clipboard, Config, settings and log panels
├── shared viewport snapshot (only the active view)
├── ConfigSaver → ordered background writer
├── SessionState → session lock + SessionSaver (ordered background writer)
└── tabs: Vec<Tab>
    ├── stable session ID (independent of its vector index)
    ├── Terminal → bounded parser + Grid → live/alternate cells + history
    ├── PtySession → output channel + input queue + lifecycle control
    └── selection, search, link hints, scrollbar, title facts, sync state

Process-wide services: logger/log ring and PTY child/worker supervisor
```

The renderer, font system, glyph atlas, and snapshot are **not per-tab**.
Terminal contents, transport queues, history, and search results are per-tab.
This distinction matters when diagnosing memory or invalidating shared caches.

| Source | Responsibility |
| --- | --- |
| [`main.rs`](../src/main.rs) | CLI, startup, winit `ApplicationHandler`, event-loop deadlines, final shutdown |
| [`app/mod.rs`](../src/app/mod.rs) | App state, cross-subsystem settings application, save/result servicing |
| [`app/tab.rs`](../src/app/tab.rs) | Tab ownership, spawn/switch/close, title facts, selection rebasing |
| [`session.rs`](../src/session.rs), [`app/session.rs`](../src/app/session.rs) | Session snapshot format/validation; snapshot, restore, and exit decision |
| [`app/keyboard.rs`](../src/app/keyboard.rs), [`app/mouse.rs`](../src/app/mouse.rs) | Input dispatch, local shortcuts/selection, terminal reporting |
| [`app/pty_io.rs`](../src/app/pty_io.rs), [`app/scheduling.rs`](../src/app/scheduling.rs) | Output drain, fair shared budget, resize, visual wake decisions |
| [`app/redraw.rs`](../src/app/redraw.rs) | Snapshot/highlight gathering, render call, returned UI actions |
| [`term.rs`](../src/term.rs), [`grid/`](../src/grid) | Escape interpretation and terminal storage; see [terminal model](terminal.md) |
| [`pty.rs`](../src/pty.rs), [`pty/`](../src/pty) | Transport and child/worker lifecycle; see [PTY design](pty.md) |
| [`renderer/`](../src/renderer) | wgpu submission, glyphon/cosmic-text, egui chrome; see [rendering](rendering.md) |
| [`search.rs`](../src/search.rs), [`selection.rs`](../src/selection.rs), [`links.rs`](../src/links.rs) | Matching, coordinate mapping, selection extraction, URI validation |
| [`config.rs`](../src/config.rs), [`theme.rs`](../src/theme.rs) | Typed settings/defaults and resolved colors |
| [`logbuf.rs`](../src/logbuf.rs), [`logging.rs`](../src/logging.rs), [`perf.rs`](../src/perf.rs) | Bounded logs and opt-in aggregate diagnostics |
| [`tabbar.rs`](../src/tabbar.rs), [`scrollbar.rs`](../src/scrollbar.rs) | Testable chrome geometry and animation decisions |
| [`procinfo.rs`](../src/procinfo.rs), [`menu.rs`](../src/menu.rs), [`icon.rs`](../src/icon.rs) | Best-effort OS integration |

Panel state belongs in `app/`; egui widgets belong in `renderer/`. Platform
clipboard access stays in the app, not in the terminal parser.

## Output and input flow

```text
child → PTY reader → bounded/coalesced output channel
  → fair UI drain → Terminal::feed → Grid mutation
  → parser replies / clipboard requests / title-fact refresh
  → redraw if active contents or displayed chrome changed

visible redraw → apply pending resize → bounded drain → sync check
  → search and link views → row-keyed shared snapshot
  → text diff/layout → egui → surface acquisition → GPU work → present

key / IME / paste → ownership + shortcut policy → encoded bytes
  → nonblocking input admission → ordered writer → child
```

Parsing, searching, snapshot copying, text layout, and painting still run on the
UI thread. Worker I/O does not make those operations asynchronous. The drain
budget is cooperative, not an end-to-end latency guarantee.

## Decisions that shape the event loop

| Decision | Rationale and required behavior |
| --- | --- |
| Sleep when idle (`Wait`/`WaitUntil`) | Do not introduce permanent polling to improve throughput or animation. Wake only for input, queued work, or an actual deadline. |
| One shared event-loop proxy | `App` holds one `Arc<EventLoopProxy>`; wakers clone the `Arc`. On macOS (winit 0.30) every proxy clone wakes the loop and registers a run-loop source that is never removed, so per-turn paths must not create or clone a proxy (a per-turn clone once kept idle at 100% CPU). |
| Separate parsing progress from presentation | Occlusion, selection dragging, synchronized output, and hidden cursors must not strand bytes, replies, or exits. |
| One drain allowance per event-loop turn | `new_events` resets it; user-event, redraw, and pre-wait drains share the allowance. Fair order survives exhaustion and tab removal. |
| Defer resize until redraw | Coalesce window events; update renderer, terminal, then PTY before feeding that visible frame. All tabs use the same geometry clamp. |
| Use explicit visual deadlines | Cursor blink, scrollbar fade, egui animations, trailing search refresh, and transient surface recovery do not depend on each other. |
| Separate visual and content invalidation | Cursor/view changes may repaint without rescanning history or reshaping identical text. Cache keys also need session/row identity and layout inputs. |

Known occluded, minimized, and zero-sized windows skip visual preparation while
transport continues. A blinking cursor needs a timer only when enabled, visible
at the live bottom, and not presentation-suspended. A settled scrollbar requests
no immediate frame; a pending fade schedules its deadline, and a changing opacity
requests animation frames based on elapsed time.

egui repaint requests have two paths, both required:

- Forward `EventResponse::repaint` for input events, **except**
  `RedrawRequested` (which would create a loop).
- Forward the root viewport's `repaint_delay` from the overlay through
  `RenderOutput` to `App::egui_repaint_at`; `Duration::MAX` means no wake.
  Update the scrollbar **before** `end_pass` so its request belongs to this pass.

Synchronized output defers **all** presentation sources, including chrome and
blink, until the end marker or a one-shot 150 ms recovery deadline. Parsing,
replies, and input continue. See [PTY scheduling](pty.md#fair-draining).

## Configuration application and persistence

Sources: [`config.rs`](../src/config.rs),
[`app/settings.rs`](../src/app/settings.rs),
[`app/persistence.rs`](../src/app/persistence.rs).

1. Startup loads defaults, then the selected TOML file. `--config` selects the
   read/save path; `--theme` owns the session theme over the file value.
2. Settings widgets edit the live `Config`. `diff_actions` routes only relevant
   changes: theme/terminal tuning to tabs, font/chrome to the renderer, window
   size to the window, and log settings to recording/storage. Shell/cwd/TERM
   changes apply to **new** tabs. Unrelated chrome/input edits must not clear
   shaping/advance caches; theme/font/metrics changes invalidate relevant state.
3. GUI edits win over later external file edits; there is no file-watch merge.
   With a CLI theme override, theme edits persist but do not replace the live
   session theme.
4. `ConfigSaver` debounces edits for 300 ms and is the sole runtime config writer.
   One worker owns at most one in-flight write, one replaceable pending snapshot,
   and one result. Generation checks prevent stale results from overriding newer
   status; sequential writes prevent an older save from landing after a newer one.
5. Saves create parent directories, serialize TOML, write a sibling temporary
   file, then rename. Panel close forces dispatch; application shutdown flushes
   and joins the writer in `exiting`. Failures remain visible in settings.

Do not move filesystem writes back into the render path or enqueue a save for
every slider frame. The atomic replacement path is not a claim of crash-durable
`fsync` semantics.

## Session persistence

Sources: [`session.rs`](../src/session.rs),
[`app/session.rs`](../src/app/session.rs),
[`app/persistence.rs`](../src/app/persistence.rs).

The snapshot is `${XDG_STATE_HOME:-$HOME/.local/state}/cometty/session.toml`
(owner-only, versioned TOML), independent of `--config`. It records tab order,
each shell's working directory, the active tab, the save time, and the last
windowed logical size and physical position, never commands. A quit snapshot
with `restore = "scrollback"` also names one dump per tab in the sibling
owner-only `scrollback/` directory.

- **Ownership:** startup takes a non-blocking exclusive lock on the sibling
  `session.lock`. Only the holder reads or writes the snapshot; a concurrent
  second instance starts fresh, so it cannot duplicate or clobber the first
  instance's tabs. Without `HOME` (and in tests) the session is inert.
- **Source of each cwd:** the kernel-reported cwd of the *shell* pid, cached by
  `Tab::refresh_title` (falling back to the restored spawn directory before the
  first query). OSC 7 is not used: it is label-truncated and may name a remote
  host.
- **Restore:** with `[session] restore = "tabs"`, the first window uses the saved
  size, and the saved position only if a current monitor contains its title bar.
  Shells spawn at the final tab count's grid size with `PtySession`'s cwd
  override. Vanished directories fall back to `[shell] cwd`; at most 64 tabs are
  restored; failed spawns are skipped, and none spawning opens a default tab.
  Unknown versions or unreadable files mean a fresh start. Each restored tab's
  terminal (not its shell) replays its dump, when enabled, and is then fed a bold ANSI-cyan
  `Session restored: <save time>` block (with the opt-out hint when contents were
  replayed), all before the shell's first output is drained.
- **Scrollback dumps:** `Grid::dump_primary` writes history plus the primary
  screen (the saved main screen while the alternate screen is active) as plain
  VT text: clusters, `SGR 0`-based style changes, OSC 8, CRLF per row. Theme
  default colors are emitted as `SGR 0`, other colors as truecolor, so default
  text follows the restoring theme. Trailing blank screen rows and default-blank
  cells are trimmed. The format is independent of compact history storage, and
  replay through `Terminal::feed` re-applies every history cap. Rows carry no
  soft-wrap flag, so a narrower restoring window wraps long rows.
  `Terminal::replay` drops any reply or OSC 52 request the dump provokes (a
  tampered file must not type into the shell) and closes open strings, styles,
  and links. Replay streams the file through `feed` in 64 KiB chunks (no
  whole-dump buffer, so restore holds no dump-sized allocation). Dumps are
  capped at 64 MiB per tab when written and, by file size, before replay; an
  oversized dump is skipped whole. Names are bare `*.vt` files (anything else
  in the snapshot is ignored). Encoding runs on the UI thread in `exiting`; its
  latency for many full tabs is unmeasured.
- **Dump commit order:** dumps get fresh names, are written first, then the
  snapshot is atomically renamed, then every unreferenced dump is deleted. A
  crash leaves either the old pair or the new one. Removal deletes the snapshot
  before the directory.
- **While running:** tab spawn/close/exit, active-tab switches, shell cwd changes,
  window moves/resizes, and restore-setting edits mark the snapshot dirty.
  `SessionSaver` writes at most once per second, anchored at the first change, so
  a crash restores recent state. These runtime writes are tabs-only (no dumps),
  so the previous quit's dumps are deleted by the first one after a restore.
- **Exit decision** (in `ApplicationHandler::exiting`): tabs still open (Cmd+Q,
  window close) → write (with dumps under `scrollback`); no tab left after one was spawned (every shell exited or
  was closed) → remove; restore `off` → remove; startup failed before any tab →
  leave the file. Writes and removals share one ordered queue, so a stale
  debounced write cannot resurrect a removed session.

On macOS, Cmd+Q terminates the process right after `exiting`, so `run_app` never
returns. Session finalization, PTY shutdown, and the config flush therefore run
in `exiting`; `main`'s tail repeats them idempotently. Fullscreen state and
running programs are not restored.

## Logging without UI feedback loops

The event loop is built before logging/config load so configuration warnings can
enter the in-app ring and later wake an open log panel. Logger installation is
nonfatal. There are two independent record filters:

| Sink | Filter |
| --- | --- |
| stderr | `RUST_LOG`, default `error` |
| In-app ring | `[log] filter`, or `cometty=<level>,warn` when empty |

The global maximum is the more verbose sink. A bare `debug` directive would
record dependency debug noise too, so keep the target-scoped default. Invalid
in-progress directives keep the last valid filter and expose the error in the
panel, rather than logging on every keystroke. Panel level/text controls are
display-only; closing it does not disable recording.

`LogBuffer` bounds entry count and accounted bytes, truncates large messages,
and assigns stable IDs/generations. `LogIndex` caches **IDs**, not owning copies
of every retained entry. It fetches bounded slices under the ring mutex, filters
outside it with a cooperative 2 ms target, drops evicted IDs, and fetches only
visible entries for paint. An unchanged frame reads metadata; explicit Copy
takes a complete filtered snapshot. In-progress filtering requests a short
follow-up repaint, not a permanent timer.

The closed viewer must not lock the ring. `ScrollArea::show_rows` relies on
fixed-height clipped rows; allowing multiline expansion breaks scrolling.
Records coalesce wakes only while the panel is open. Performance counters go
directly to stderr without using the ring; see [performance](performance.md).
