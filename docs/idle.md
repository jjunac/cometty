# Idle CPU and energy

This page covers what cometty costs while nothing happens on screen, why, how to
measure it, the plan to reduce it, and the fixes shipped so far. Each statement
below is labeled as implemented behavior, a measurement (one host, not a
guarantee), a modeled estimate, or a proposal. The investigation ran on
2026-10-29 at `28d9a69`, plus the counters described in
[performance](performance.md#application-phase-counters) (committed as
`c0b0f78`).

## Shipped fixes (2026-10-29)

The investigation below is preserved as the **pre-fix baseline**. Its
code-path descriptions refer to that revision, not to current behavior. Each
[plan](#plan) step is marked shipped or proposed. Implemented so far:

1. **Event-driven PTY supervision (step 1).** Workers start through
   `Startup::spawn_worker`. Its drop guard, which also runs on panic, counts the
   worker as finished and wakes the supervisor; close requests publish the same
   way, under the queue lock. The 25 ms timed wait remains only while a job is
   closing, has a finished worker not yet joined, or has a finished worker while
   its child is unreaped. A shell that exits while a background job holds the
   PTY now stays a zombie until its tab closes; see
   [PTY lifecycle](pty.md#decision-cancellation-aware-io-supervised-child-cleanup).
2. **Cursor blink policy (step 2).** Blinking stops while the window is
   unfocused, and `cursor.blink_timeout_ms` (default 10 s, `0` = forever) after
   the last keyboard, IME, or paste input. Input or regained focus restarts it,
   visible phase first. The timeout adds no timer: it is checked when the loop
   next runs, normally at the next blink tick. `blink_step` now requests the one
   frame needed when blinking stops in the hidden phase. An unfocused window
   draws a steady cursor, hollow for the block shape.
3. **Pointer motion repaints only when chrome changes (step 3).** egui's
   `repaint` for `CursorMoved`/`CursorEntered`/`CursorLeft` is no longer
   forwarded. Motion requests a frame only while egui uses the pointer, on every
   move over the find bar or an open panel, and when the hovered tab, close `×`,
   `+`, or visible scrollbar track changes, including one frame on leaving
   chrome. Tab-strip rects come from the last egui pass. Unpainted motion is
   coalesced in egui's pending input. See
   [architecture](architecture.md#decisions-that-shape-the-event-loop).

### Same-host results

Apple M3 Pro, macOS 26.7.1, AC power, release builds, and the baseline's probe
settings: a fresh process per scenario, 6 s to settle, a 40 s window, and in-app
counters from the periodic reports inside it. `before` is `c0b0f78`; each step's
binary adds that step to the previous ones. Every run had zero pointer events
and zero skipped frames. These are measurements on one host, not guarantees.

| Scenario | Binary | M instr/s | Interrupt wakeups/s | Frames/s | Supervisor polls/s |
| --- | --- | ---: | ---: | ---: | ---: |
| `steady` | before | 0.76 | 35.5 | 0 | 35.0 |
| `steady` | step 1 | 0.05 | 0.4 | 0 | 0 |
| `steady-5tabs` | before | 1.63 | 35.4 | 0 | 35.1 |
| `steady-5tabs` | step 1 | 0.05 | 0.4 | 0 | 0 |
| `blink` | before | 10.17 | 38.0 | 1.87 | 35.1 |
| `blink` | step 1 | 9.37 | 2.9 | 1.87 | 0 |

Step 1 removes the supervisor's wakeups whatever the tab count: five idle shells
cost the same as one. Blink frames are untouched by it.

Step 2 runs settled 12 s instead of 6 s, so the measured window starts after the
10 s blink timeout. Probe windows get focus at launch on this host (one
`focus_in` per run). `blink-forever` sets `blink_timeout_ms = 0`, and
`blink-forever-unfocused` adds a small second Cometty beside the measured window
that takes its focus; no run recorded an occlusion. `before` ignores the new
key and blinks in every case.

| Scenario | Binary | M instr/s | Interrupt wakeups/s | Frames/s | Supervisor polls/s |
| --- | --- | ---: | ---: | ---: | ---: |
| `blink` (inactive after 10 s) | before | 9.96 | 38.0 | 1.87 | 35.3 |
| `blink` (inactive after 10 s) | step 2 | 0.04 | 0.4 | 0 | 0 |
| `blink-forever-unfocused` | before | 9.88 | 34.7 | 1.87 | 31.8 |
| `blink-forever-unfocused` | step 2 | 0.07 | 0.4 | 0 | 0 |
| `blink-forever` (focused) | before | 9.96 | 35.7 | 1.87 | 32.4 |
| `blink-forever` (focused) | step 2 | 9.34 | 2.8 | 1.87 | 0 |
| `steady` | before | 0.68 | 32.2 | 0 | 31.7 |
| `steady` | step 2 | 0.04 | 0.4 | 0 | 0 |

An inactive or unfocused window now costs what a steady cursor costs. A focused
window still blinks for 10 s after each input, at the per-frame cost that step 4
addresses.

Step 3 used the manual `hover` scenario: the operator clicks the pinned window
once, then moves the pointer in continuous circles over the text area. The rates
come from the 10 s counter reports that contain motion. In both runs the
operator's motion produced about 116 pointer events per second.

| `hover`, continuous motion | Pointer events/s | Frames/s | M instr/s | M instr/event | Loop wakes/s |
| --- | ---: | ---: | ---: | ---: | ---: |
| before | 115.8 | 117.9 | 634.7 | 5.48 | 117.9 |
| step 3 | 117.2 | 0.30 | 93.8 | 0.80 | 232.7 |

The remaining 0.3 frames/s are the entry and exit frames when the pointer
crossed the tab strip. The remaining ~0.8 M instructions per event is platform
event delivery, not rendering. A native sample taken during motion shows the
process almost entirely blocked: the main thread is active in ~2% of samples,
and AppKit's event thread spends its active time pulling and decoding events
from the WindowServer. Each unpainted event also costs two run-loop turns
instead of one. Not accepting mouse-moved events while nothing needs them
(no chrome hover, mouse reporting, or link hints) would remove that cost; that
is not implemented. An earlier, slower run of the old binary (22.5 events/s)
cost 7.2 M instructions and 1.31 frames per event, close to the investigation's
estimate.

## Investigation baseline (before fixes)

Everything from here to the [plan](#plan) describes the code at `c0b0f78` and
was measured there.

## Summary

A quiet window with the default configuration (one tab, blinking cursor) wakes
the process about **38 times per second**. It retires about **10 M instructions
per second**, which is 0.3–0.4% of one core on an Apple M3 Pro (measured). Two
behaviors cause nearly all of it, and two more dominate "almost idle" screens:

1. **The PTY supervisor polls every 25 ms** while any shell exists, even when the
   window is hidden or minimized. It causes ~35 of the ~38 wakeups per second.
2. **The default blinking cursor renders 1.87 frames per second, forever.** Focus
   and inactivity do not stop it; only hiding the window does. Blink frames
   account for ~92% of the idle instructions.
3. **Every frame redraws the whole window.** A blink changes one cell, but the
   frame still runs the full egui pass and rebuilds every cell's background
   geometry, converting two colors per cell through `powf`. It then encodes all
   glyphs and presents the full surface. That is about **5 M instructions per
   frame**.
4. **Pointer motion and small periodic output also redraw everything.** In the
   focused window, each pointer event renders about 1.35 frames, roughly
   **8 M instructions**. On a full screen of text, a 15-cell status update, such
   as a TUI clock or spinner, costs **8–11 M instructions**. A 10 Hz spinner
   therefore costs 85 M instructions/s, 2.2–5.5% of a core.

A prototype of the supervisor fix (step 1 below) removed 99% of the
steady-cursor wakeups (measured). Stopping the blink when unfocused or inactive
(step 2) would remove the frames. Together they would bring a quiet window to
roughly zero work (modeled from the steady-cursor measurements). Steps 3–4 then
make the remaining frames, pointer motion, and TUI updates cheaper.

## Measuring reliably

Use the [idle probe](performance.md#idle-probe-macos). It launches isolated
instances whose tabs run a FIFO-blocked agent rather than a shell, so only the
terminal's own work remains. The probe then diffs `proc_pid_rusage` and the
periodic `COMETTY_PERF=1` reports over a fixed window. Lessons from this
investigation:

- **Compare retired instructions, not CPU time or energy.** Two identical 10 Hz
  ticker runs retired 84.82 and 84.89 M instructions/s. Their CPU time differed
  2.5× (5.5% vs 2.2% of a core: 0.65 vs 0.26 ns per instruction), and CPU energy
  moved the other way (4.8 vs 16.8 mW). macOS ran the work on efficiency cores
  in one run and on performance cores in the other. Small, periodic loads land on
  either.
- `ri_energy_nj` covers CPU energy only. GPU rendering and WindowServer
  composition of each presented frame are real costs that these numbers miss, so
  frames cost more energy than the CPU counters suggest.
- **Use wakeups to measure timer cost.** A timer's work can be tiny while its
  wakeups still keep cores out of idle states. On macOS, the useful count is
  interrupt wakeups; package-idle wakeups stay near zero on a busy desktop.
- `sample` over-weights timer-driven threads. In a 20 s profile, the supervisor
  had 7× as many active samples as the main thread, yet kernel per-thread
  accounting (`ps -M`) puts it at ~0.085% of a core. Use `ps -M` or the
  counters for per-thread CPU, and `sample` only for stack shapes.
- Keep probe windows uncovered and the pointer away from them. Occlusion
  suspends rendering, and pointer motion adds frames. The counters show both
  (`skipped`, `pointer_event`), so check them before trusting a run.

## Measured baseline

Apple M3 Pro, macOS 26.7.1, AC power, release build, Menlo 14 at 2×, a
1000×700-point window (118×37 cells), and in-app logging off. Each scenario had a
fresh process, 6 s to settle, and a 40 s window. Counter rates come from the
30 s of periodic reports inside that window. "Full" scenarios first fill the
screen with plain text. "Tick" scenarios hide the cursor and rewrite a 15-cell
status line at the given rate.

| Scenario | M instr/s | CPU % | Interrupt wakeups/s | Frames/s | Loop wakes/s (timer + other) | Supervisor polls/s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `blink` (default config) | 9.99 | 0.38 | 38.0 | 1.87 | 1.9 + 3.8 | 35.1 |
| `blink-full` | 10.15 | 0.36 | 38.1 | 1.87 | 1.9 + 3.8 | 35.2 |
| `blink-5tabs` | 11.15 | 0.41 | 38.0 | 1.87 | 1.9 + 3.8 | 35.1 |
| `steady` (`cursor.default_blinking = false`) | 0.76 | 0.09 | 35.6 | 0 | 0 + 0.1 | 35.2 |
| `steady-full` | 0.77 | 0.09 | 35.6 | 0 | 0 + 0.1 | 35.2 |
| `tick-1hz-full` (TUI clock) | 11.54 | 0.80 | 36.3 | 1.00 | 0 + 2.1 | 35.1 |
| `tick-10hz-full` (spinner) | 84.82 | 2.2–5.5 | 32.6 | 10.0 | 0 + 20.1 | 31.3 |

Per presented frame, with the idle floor (~0.76 M instr/s) removed, and
main-thread elapsed milliseconds. The phases nest inside `redraw_total`. The tick
runs here most likely executed on efficiency cores: 0.65–0.70 ns per
instruction, against 0.26 ns in a repeat. Their milliseconds are therefore
roughly 2.5× what a performance core takes. Their instruction counts are not
affected.

| Scenario | M instr/frame | `redraw_total` | overlay | background | encode/submit | acquire | present | rebuild | prepare |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `blink` | 4.9 | 0.76 | 0.13 | 0.26 | 0.27 | 0.06 | 0.02 | – | – |
| `blink-full` | 5.0 | 0.71 | 0.12 | 0.24 | 0.25 | 0.05 | 0.02 | – | – |
| `blink-5tabs` | 5.5 | 0.77 | 0.14 | 0.26 | 0.27 | 0.06 | 0.02 | – | – |
| `tick-1hz-full` | 10.8 | 3.62 | 0.24 | 1.24 | 0.48 | 0.15 | 0.05 | 0.61 | 0.81 |
| `tick-10hz-full` | 8.4 | 3.64 | 0.24 | 1.20 | 0.49 | 0.15 | 0.04 | 0.61 | 0.87 |

On a performance core, a blink frame spends ~0.75 ms in the redraw callback.
Kernel per-thread accounting and native samples add roughly 0.3 ms of AppKit and
CoreAnimation work on the main thread and 0.3 ms on Metal/CoreAnimation queues.
That is an approximate split, not an exact one.

## Where the cost comes from

### 1. The PTY supervisor's fixed poll

[`pty/lifecycle.rs`](../src/pty/lifecycle.rs) sleeps without a timer only when
it owns no job. While any shell exists, it waits on its condition variable with
a **25 ms timeout**. Each tick it takes the queue lock, calls `waitpid(WNOHANG)`
for every child, and checks every worker's `is_finished()`. That is 40 ticks/s
nominally; timer coalescing gives ~35/s. It does not depend on the window, the
number of tabs, or the cursor. It runs the same while the window is minimized
or hidden.

It polls because child exit has no notification here. The reader sees EOF only
once every slave descriptor closes. Hangup grace and kill escalation also need
timed steps. The work per tick is tiny, about 22k instructions. The wakeups
matter: ~35/s, which is **92% of all wakeups** with blink on and 99% with a
steady cursor.

### 2. The cursor blink timer

`cursor.default_blinking = true` and `cursor.blink_ms = 530`. `about_to_wait`
keeps a blink deadline while `scheduling::cursor_blinks` holds. That function
checks only five conditions: the window is not suspended (occluded, minimized,
or zero-sized), no synchronized output is pending, the cursor is enabled, its
DECSCUSR style blinks, and the view is at the live bottom. **Focus and
inactivity are not inputs.** A visible but unfocused window, such as a terminal
beside an editor, renders 1.87 frames/s for as long as it stays open.

Each blink takes three run-loop turns. The timer wakes the loop, and
`about_to_wait` requests a redraw. winit then wakes the loop again to deliver
`RedrawRequested`. A third wake per frame carries no winit event; its platform
source is not attributed. Blink frames are 9.99 − 0.76 = 9.2 M of the 10 M idle
instructions per second.

### 3. Every frame redraws the whole window

Two caches already hold across blink frames: the version-gated snapshot copies
nothing, and glyph preparation is reused (`prepare_reuse`). The rest of each
frame is recomputed in full:

- **egui pass (`overlay`).** `should_skip_overlay` never skips on macOS, because
  the tab strip always lives in the titlebar. Every frame runs
  `begin_pass`…`tessellate` and re-uploads egui's vertex and index buffers
  inside encode/submit. This grows with the tab count.
- **Background geometry (`background`).** `paint_bg` walks every visible cell
  (4,366 here) to rebuild quads that have not changed. For every cell it
  converts the foreground and decoration colors with `as_linear_f32_array`.
  That is six `powf` calls, even when no block element or decoration needs
  them. `powf` is the top main-thread leaf in native samples.
- **GPU work and presentation.** The surface is cleared, and every background
  quad and glyph instance is encoded and drawn. The full drawable is then
  presented, so WindowServer recomposites the whole window. GPU and compositor
  energy are not included in the numbers above.
- **Per-frame bookkeeping.** `tab_titles()` re-formats every tab label on every
  frame. That parses the template into a `Vec<char>` and reads `HOME` for each
  tab with a known directory. The window title string is also rebuilt each
  frame.

### 4. Pointer motion over the focused window

egui-winit returns `repaint: true` for every `CursorMoved`, `CursorEntered`, and
`CursorLeft`. `window_event` forwards that as a redraw request, and
`on_cursor_moved` adds one more whenever the pointer is over the always-visible
tab strip. Moving the mouse across a focused window therefore renders one full
frame per pointer event, up to the display rate. egui follow-up frames
(`request_egui_immediate`) come on top when its hover state changes. AppKit
delivers `mouseMoved:` only to the key window, and winit installs no
always-active tracking area, so unfocused windows see at most enter and leave.

In two unplanned observations, someone moved the pointer over a probe window.
In one 10 s interval, 49 pointer events produced 66 frames, 61 of them egui
follow-ups. They cost 406 M instructions: about **8.3 M per pointer event, or
1.35 frames each**. In the other interval, 14 events produced 22 extra frames.
Modeled from these measurements, continuous motion at 60–120 events/s costs
0.5–1 G instructions/s, about 50–100× a blinking idle window. To measure it
deliberately, run the manual `hover` probe scenario.

### 5. Small periodic output on a full screen

TUI clocks, spinners, progress bars, `top`-like tools, and agent UIs change a
few cells at a time. Each update pays for a PTY wake, a proxy wake, and a drain.
Two loop turns per update are measured. The update then pays the full-frame
costs above, plus:

- **`rebuild`** runs `diff_rows`, which hashes every visible cell with SipHash
  to find the changed rows. The snapshot step has already compared per-row
  identity keys to decide which rows to copy.
- **`prepare`**: any reshaped row bumps the shared text revision, so glyphon
  re-prepares every visible glyph and re-uploads all instances.
- **`background`**: the same full walk as on blink frames.

On a full screen of text, the main thread spends about 2.7 of the 3.6 ms per
update in work proportional to the whole screen (measured on efficiency cores).

### What does not cost anything at idle (checked)

- PTY reader and writer threads, the config and session writers, and log-panel
  wakes all block without timeouts. With a steady cursor, the main loop wakes
  about 0.1 times/s; in quiet intervals, not at all.
- No event-loop spin, and egui schedules no repaint deadline at idle
  (`request_egui_deadline` = 0).
- Blink frames reshape nothing, re-prepare nothing, and copy no snapshot cells.
- Five tabs add ~12% per blink frame (the tab strip and labels). The
  supervisor's rate does not depend on the tab count.

### Side finding: a one-second stall while the window is hidden

In one run, `get_current_texture` blocked the main thread for **1.01 s** while
the window was being covered. wgpu disables `allowsNextDrawableTimeout`, and its
occlusion pre-check (the wgpu#8309 workaround) can race a window that is being
hidden. This is not an idle-CPU cost, but parsing and input stall with it. It
needs its own investigation.

## Plan

The steps are ordered by impact per unit of risk. Each preserves the
[critical invariants](../AGENTS.md#critical-invariants). Verify each one with
unit tests and an idle-probe A/B on instructions, wakeups, and frames, run on
the same host. Shipped steps are marked; their results are in
[shipped fixes](#shipped-fixes-2026-10-29).

### Step 1 — Event-driven child supervision (shipped)

Replace the fixed poll with notifications. The timed poll then runs only while a
job actually needs time-based steps.

- Supervised workers announce their return through a drop guard, which also
  covers a panic. The reader returning at EOF is the natural-exit signal. The
  guard increments a per-job count, then **locks and unlocks the queue mutex
  before notifying**. Close requests publish the same way. A supervisor sitting
  between its wait predicate and its wait therefore cannot miss either one.
- A job needs polling only while one of these holds: it is closing (hangup grace
  and escalation), it has a finished worker not yet joined, or a worker has
  finished and the child is not yet reaped. Otherwise the supervisor blocks with
  `wait_while`, with no timeout.
- Unchanged: the supervisor owns children until they are reaped, never signals a
  cached PID, never runs a UI-thread wait, and keeps the shared exit deadline.
- Remaining gap: a shell that exits while a descendant keeps the slave open
  produces no EOF. It stays a zombie until its tab closes; before step 1 it was
  reaped within 25 ms. Its tab already stays open in that case. If that matters,
  add exact exit events: kqueue `EVFILT_PROC`/`NOTE_EXIT` on macOS and BSD, or a
  pidfd on Linux, with today's poll as the fallback elsewhere.
- **Prototype (measured before merging).** The diff was +100/−50 lines, mostly
  re-indented worker spawning. It passed every PTY test, and the lifecycle tests
  eight times in a row. Those include natural exit reaped before tab drop,
  hangup escalation, and the isolated descriptor and thread baseline.

  | Scenario | Baseline | Prototype |
  | --- | --- | --- |
  | `steady` | 0.76 M instr/s, 35.6 wakeups/s, 0.093% CPU | 0.051 M instr/s, 0.45 wakeups/s, 0.004% CPU |
  | `blink` | 9.99 M instr/s, 38.0 wakeups/s, 0.38% CPU | 9.36 M instr/s, 2.9 wakeups/s, 0.29% CPU |

  About 0.1 of the remaining wakeups per second comes from the counter
  reporter itself. A five-tab run was disturbed by pointer activity in one
  interval; its quiet 10 s intervals had no supervisor polls and at most three
  loop turns.
- **Shipped** with the guard created inside the worker thread (a failed spawn
  cannot count a worker that never ran) and these tests: the isolated resource
  probe asserts that one idle child leaves the supervisor in its timer-free wait
  with no timed waits, and fake-child stress tests race close requests and
  returning or panicking workers against the supervisor's wait. A test-only hook
  holds the lock between the idle predicate and the wait, so a wakeup published
  without the lock hangs those tests instead of needing a nanosecond-wide
  coincidence.

### Step 2 — Cursor blink policy (shipped)

- Stop blinking while the window is unfocused, and draw the cursor steady.
  Optionally use a hollow block, the usual unfocused-terminal cue.
- Stop blinking after a period without keyboard, IME, or paste input:
  `cursor.blink_timeout_ms`, default ~15 s, where 0 means blink forever. Restart
  the blink, visible phase first, on input or focus. Comparable settings
  elsewhere: kitty's `cursor_stop_blinking_after` (15 s) and Alacritty's
  `cursor.blink_timeout` (5 s).
- `cursor_blinks` gains focus and inactivity inputs. `blink_step` must request
  **one** repaint when it stops blinking during the hidden phase. Before step 2
  it only set `visible = true`. That was harmless, because every stop condition
  then coincided with another frame, but a timeout or focus change would leave
  the cursor invisible. No new timer: the timeout is checked at the next blink
  tick, so it costs at most one extra frame.
- Expected: the `steady` rows above, which are 0.05 M instructions/s and
  ~0.5 wakeups/s with step 1, for every unfocused or inactive window.
- **Shipped** with a 10 s default timeout and the hollow block. Every key press,
  IME commit or preedit, and paste restarts the blink, so a cursor stays solid
  while you type. Pure scheduling tests cover focus loss, timeout expiry in
  both phases, restart on input and focus, a zero timeout, and unchanged
  DECSCUSR, scroll-offset, and suspension behavior. The probe gained
  `blink-forever*` scenarios (no timeout) to keep measuring blink frames, a
  focus-thief scenario, and `focus_in`/`focus_out`/`occluded` counters.

### Step 3 — Repaint pointer motion only when chrome changes (shipped)

- Forward egui's `repaint` for pointer events only while egui is using the
  pointer, or when the pointer is over egui chrome or has just left it. Chrome
  means the tab strip, scrollbar strip, find bar, and open panels:
  `Context::is_pointer_over_area()` plus a "was over" flag for the exit frame.
- Over the tab strip, repaint only when the hovered element changes: tab, close
  `×`, or `+`. Selection drags, link-hint cursors, and mouse reporting keep
  their own repaint and report paths.
- Expected: no frames while the pointer moves over the terminal area. Measure
  with the `hover` probe before and after. Check hover highlights, scrollbar
  fade-in, and tooltips by hand.
- **Shipped** with explicit hit tests instead of
  `Context::is_pointer_over_area()`. egui only knows the previous frame's
  pointer position, and the scrollbar area spans the whole window while the
  scrollbar is shown, so egui's area rects would count the grid as chrome. The
  tab strip records each tab's, close box's, and `+` button's interactive rect
  during the egui pass. The scrollbar strip counts only while its track is
  visible or dragged, since a faded track ignores hover. Open panels count as
  chrome everywhere. Unpainted motion is collapsed to the newest pending egui
  pointer event, so egui's input cannot grow while no frames run. Tests cover
  the repaint rule, the strip hit test (close boxes above tabs), the scrollbar's
  interactive state, and the coalescing.

### Step 4 — Make the remaining frames cheaper

These steps need measurement one at a time. The phase table above gives each
sub-step's ceiling.

1. **Colors:** build `fg_col`/`deco_col` only when a block element or decoration
   uses them, and replace `powf` with a 256-entry sRGB→linear table. This is a
   small, local change inside `background`, which costs 0.24–0.26 ms per blink
   frame in total.
2. **Background geometry cache:** rebuild the cell quads only when the snapshot,
   highlights (selection, search, link hints), metrics, or tab-bar offset
   change. Draw the cursor as a separate small draw. Blink frames then upload a
   few vertices instead of walking every cell.
3. **egui reuse:** when the frame has no new egui input, no pending egui repaint,
   and unchanged chrome inputs, skip `begin_pass`…`update_buffers` and redraw
   the last paint jobs. Chrome inputs are titles, active tab, scrollbar state,
   notices, panels, theme, size, and scale. On macOS this pass runs on every
   frame (0.12–0.14 ms plus its uploads).
4. **Changed rows from identity:** pass the snapshot's changed-row set to the
   renderer instead of hashing every cell in `diff_rows` (`rebuild`, 0.6 ms per
   update measured on efficiency cores). Keep the hash for metric, theme, and
   size invalidation.
5. **Incremental glyph preparation:** split the text into row bands with their
   own buffers or prepared instance ranges. A one-row change then re-prepares
   one band, not the screen (`prepare`, 0.8–0.9 ms per update). This is the most
   invasive item, because atlas pinning and `trim` rules apply. See
   [rendering](rendering.md#decision-retain-preparation-while-keeping-atlas-pins).
6. **Bookkeeping:** cache tab labels and the window title, and recompute them
   when title facts change.

Combined ceiling (modeled from the phase table): blink frames would drop from
~0.75 ms to ~0.35–0.4 ms of callback time, because the egui pass and background
work go away. A one-row update could drop ~70% of its main-thread time, since
2.7 of its 3.6 ms are proportional to the screen. Per-present platform overhead
remains, so avoiding frames (steps 2–3) matters more than making them cheaper.

### Step 5 — Optional: lower the output frame cap for background windows

Output-driven frames are capped at 60 Hz whether or not the window has focus. A
lower cap for unfocused windows, such as 20–30 Hz, would bound the cost of
spinners and agent UIs left running beside other work. That is a smoothness
trade-off, so it needs a setting and a product decision. Visible occluded
windows already render nothing.

### Step 6 — Power preference on dual-GPU Macs

The renderer requests `PowerPreference::HighPerformance`. On Intel Macs with a
discrete GPU, that keeps the discrete GPU powered for as long as cometty runs,
whatever the frame rate. That is a large battery cost for a text renderer. The
proposal is `LowPower`, or a setting. Apple Silicon has a single GPU, so nothing
changes there. This is not measured here, because the test host has a single
GPU.

## Expected end state

| Quiet window | Baseline (measured) | Step 1 (measured) | Steps 1–2 (measured) |
| --- | --- | --- | --- |
| Instructions | 10 M/s | 9.4 M/s | 0.04–0.07 M/s once unfocused or inactive |
| Wakeups | 38/s | 2.9/s | 0.4/s |
| Frames | 1.87/s, forever | 1.87/s | 0 when unfocused or inactive |

Steps 3–4 address the costs that remain while a person or a program is actually
changing the screen: pointer motion, TUI updates, and the first seconds of blink
after input.
