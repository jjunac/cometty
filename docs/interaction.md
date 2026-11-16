# Interaction and view state

The pure models in [`search.rs`](../src/search.rs),
[`selection.rs`](../src/selection.rs), [`links.rs`](../src/links.rs), and
[`input.rs`](../src/input.rs) are separate from app dispatch and egui widgets.
Per-tab state belongs to `Tab`; system clipboard access and window focus are
shared app concerns.

## Keyboard and IME ownership

In [`app/keyboard.rs`](../src/app/keyboard.rs), a captured tab-strip gesture gets
Escape first (cancelling only that gesture). Otherwise dispatch is intentionally
in this order:

1. Settings and Logs shortcuts, including while settings is open.
2. Settings early return: all remaining keys belong to the panel; Esc closes it.
3. Search open/re-focus shortcut, after that settings guard.
4. Search Esc, Enter/Shift+Enter, F3/Shift+F3, before the egui-consumed check.
5. egui-consumed keys, then copy/paste/tab/local-scroll shortcuts, then PTY input.

Search open is not a toggle. Pending search focus swallows printable/edit keys
until egui grants focus, preventing the opening keystrokes from reaching the
shell. `TextEdit::return_key(None)` keeps focus during search navigation. Closing
the bar must surrender the field's focus; an invisible focused widget can consume
shell keys. Logs/search otherwise follow egui's consumed flag rather than
swallowing all terminal input just because a panel is visible.

`terminal_owns_keyboard` means settings closed, no pending find focus, and no
focused egui text edit. Two non-obvious consequences are required:

- **Tab bypasses egui under terminal ownership.** egui-winit consumes Tab for
  focus navigation even without a text field; sending it there can strand focus
  in chrome and starve later PTY keys. Tab-strip/scrollbar/chevrons use
  non-focusable `Sense::CLICK`/`DRAG`, not focusable `Sense::click()`. Grid presses
  surrender stale egui focus.
- **Dead keys arrive as `Ime::Commit`, not necessarily `KeyboardInput`.** Forward
  commits only under the same ownership rule. After egui platform output, the
  renderer reasserts terminal IME when text focus is released; egui otherwise
  disables it and macOS dead keys can vanish before any event reaches the app.
  Cache that handoff so steady frames do not interrupt an ongoing composition.

`map_key` is the pure encoding seam: application cursor/keypad modes, modifier
sequences, function keys, and platform shortcuts are testable without constructing
winit's privately populated `KeyEvent`. Releases do not produce PTY bytes.

## Search: separate matches, projection, and navigation

[`app/search.rs`](../src/app/search.rs) owns `SearchPanel`; the renderer paints a
[`SearchView`](../src/search.rs), not the full result list.

- A query with any uppercase scalar is case-sensitive. Otherwise query and row
  text use the same per-character lowercase mapping; ASCII gets a cheaper path.
- Match non-overlapping text within each visual row. Lead text maps bytes back
  to columns; an intersected wide cluster highlights both columns. Trailing
  blank tails are excluded. Alternate-screen search examines that screen only.
- Matches cache against `grid.content_version`. Query/open changes pick an
  initial match and may reveal it; background content refresh **must not scroll**.
- Streaming output throttles full rescans to 150 ms. A separate trailing-refresh
  deadline ensures the final rescan happens even when no cursor blink is due.
  This remains a synchronous full-history scan, **not incremental matching**.
- Visible projection binary-slices the sorted matches. Its per-tab key includes
  match generation, current match, viewport base, rows, and columns. An `Arc`
  shares the cached view with rendering without a deep per-frame clone.
- Search captures `(row ID, revision, projection revision)` plus
  `history_dropped`. While a rescan is throttled, projected highlights/reveal
  must not target unrelated recycled rows; line indices rebase over eviction.
- Navigation wraps and reveals with minimal scroll. Closing drops identities
  and projection, and releases result capacities larger than **1 MiB**; small
  vectors may be reused. Inactive tabs can still retain an open search.

Every match is still stored, and rescanning constructs a replacement before
dropping old results. Visible-only highlighting is not a search-memory bound.
See [memory](memory.md#remaining-limits-and-follow-up-work).

The painted find-bar rect is used for pointer/wheel hit testing, preventing
chrome interaction from starting terminal selection. Navigation chevrons are
painted shapes because bundled egui fonts do not supply the desired arrow glyphs.
Tab switches invalidate search state as needed; equal tab-local versions do not
prove equal terminal contents.

## Selection and mouse routing

Selections use global row/column coordinates, projected to the viewport for
paint. `history_dropped` rebases both endpoints and drag anchor; cancel when an
endpoint was evicted instead of selecting recycled storage. Resize/alternate
buffer transitions clear selection. Output outside a drag clears that tab's
selection; background output must not clear the active tab's selection.

Double-click word selection uses the configured match-words regex, triple-click
selects a line, and wide continuations resolve to their lead. Compilation and
validation share a one-pattern-per-thread cache, including invalid edits and
default fallback. This is bounded by entries, not arbitrary user pattern length.
Copy trims row tails and skips continuation placeholders; existing helpers may
materialize selected rows, but must use the same projected history semantics.

Application mouse modes coexist with local operations: Shift forces local
selection/scrolling, link activation precedes mouse reporting, and the matching
release after a link-open press is swallowed. Motion reports deduplicate encoded
cell/button/modifier state, not just pixel motion; preserve transitions.

## Hyperlinks and clipboard security

OSC 8 runs take priority over plain-text URL detection. Detection recognizes
allowlisted schemes and `www.`, with boundaries/trailing punctuation handling
and byte-to-column spans. `www.` opens as `https://`. Detection is **per visual
row**: the model records no soft-wrap flag, so wrapped URLs are not joined.

Link hints cache per tab by grid view version and `detect_urls`; modifier changes
still repaint and update cursor feedback with unchanged text. At open, strip
controls and allow only `http`, `https`, `ftp`, and `mailto`. Refuse `file` so
untrusted output cannot launch arbitrary local paths. macOS/Linux openers use
direct argv (`open`/`xdg-open`), not shell interpolation. Windows' `cmd /C start`
requires rejecting cmd metacharacters even inside a quoted URI.

OSC 52 parsing emits requests, not clipboard API calls. The app applies
`off` / `write` / `read-write`, with reads opt-in because they can exfiltrate
clipboard contents. Decode and final-text caps are separate from the raw OSC
bound. Handle operations before tab-index removal so replies stay with the
requesting tab. Log sizes/policy decisions, never clipboard payload contents.

## Tab ordering and pointer ownership

Tab bodies support primary-button drag-to-reorder within the existing window.
The app owns the toolkit-free [`TabDrag`](../src/tabbar.rs) gesture; native
press/motion/release events advance it even between painted frames. A horizontal
move of **6 logical points** starts the drag and activates its source. A click
below that threshold keeps the normal tab-switch behavior. Close `×`, park `↓`,
`+`, and empty titlebar chrome capture their release but never become reorder
sources; empty chrome retains macOS native window drag/zoom handling.

The default 38-point side margins (`tabbar.inset_x`) leave empty chrome before
the tabs (after the macOS traffic-light reserve) and after `+` for window dragging.
Painting and window-drag hit testing share the same inset geometry.

On macOS, transparency and a full-size content view alone do **not** disable
AppKit's implicit titlebar window drag: it can steal a tab's press before winit
dispatches it. [`platform/titlebar.rs`](../src/platform/titlebar.rs) installs an
ivar-free subclass on this window's content view before renderer/IME setup. Its
`_opaqueRectForWindowMoveWhenInTitlebar` override returns the current view bounds,
marking the titlebar content as app-owned. This is an isolated, undocumented
AppKit hook: recheck it when upgrading winit or supported macOS. It does not
modify the global winit view class, replace event/GPU/IME methods, or turn off
`NSWindow.isMovable` (which would also affect system tiling/display management).
The app's explicit window-drag/zoom path accepts only empty chrome; painted
tab/close/park/plus hit rects take precedence over geometric fallback, and a
latched tab press cannot fall through to a native window move before its threshold.

[`renderer/tabbar_ui.rs`](../src/renderer/tabbar_ui.rs) owns the widgets, clipped
hit rectangles, horizontal scroll position, and title-width cache. Tab, close,
and park-widget IDs use the stable `Tab::id`, not an index or title. The grabbed
tab follows the pointer horizontally and paints above neighbours, which shift to
preview its destination. This is only a projection: the model order changes once
on a valid release inside the tab-row viewport, excluding traffic lights and
`+`. The destination uses the grabbed fraction of the tab width and the current
scrolled layout, so a partially visible tab or a resized strip keeps its source.

Escape, focus loss, a vanished source, dropping outside the viewport, or having
fewer than two foreground tabs cancels the reorder, not the terminal's
selection/search.
Cancellation suppresses a trailing click and retains the matching release guard.
Focus loss releases motion capture immediately so an outside button release
cannot leave the window stuck. A new primary press retires that guard. During
capture, motion, additional presses, wheel input and the primary release never
reach terminal selection or mouse reporting; Escape precedes panel/search/PTY
handling and is withheld from egui too. Strip controls remain non-focusable.

Overflow still scrolls with the wheel/trackpad. A held drag in the viewport's
24-point edge bands scrolls at up to 600 points/second; only a changing offset
requests another frame. Release/cancellation, leaving the edge, or reaching the
scroll limit stops that source of wakeups. Pending strip presses also repaint on
motion before egui has processed the press, so threshold crossing cannot be
stranded by hover-motion coalescing.

The returned drop action carries a stable source ID. [`app/tab.rs`](../src/app/tab.rs)
moves the entire existing `Tab`, remaps the active index and PTY round-robin cursor,
and marks the session dirty. It does not respawn/resize terminals, invalidate
search or shaping caches, reset the shared drain allowance, or change the active
terminal identity. `$tab` labels and direct-tab shortcuts follow the committed
order; the session snapshot and quit-time dumps use that order too. Headless
gesture/widget/state tests cover these decisions, not native drag feel or latency.
The macOS-only windowless Objective-C test checks subclass installation, ABI,
inherited state and per-view isolation, not Window Server drag arbitration.

## Background tabs and in-place peek

Parked tabs stay in the same `App::tabs` vector (PTY scheduling, resize,
snapshot, search, and session all remain uniform) but occupy a prefix/suffix
split: foreground tabs fill `0..foreground_count`, parked tabs follow. That
keeps strip indices, `Cmd+1..9`, `Ctrl+Tab`, reorder, and foreground `$tab`
labels index-aligned; only the strip's label projection filters the suffix out
(a parked tab keeps its combined position for a unique `$tab`).

- **Park/unpark** moves the whole tab and never respawns or resizes it.
  Parking the only foreground tab first spawns a replacement prompt in the
  tab's `shell_cwd`, so a window always keeps one foreground tab; if that
  replacement cannot spawn, the original tab stays foreground.
- **Strip button:** hovering a foreground tab reveals a painted `↓` with
  “Send to background” help. Its fixed **24-point** left-hand hit box immediately
  follows `×`; a lone foreground tab, which has no close button, puts `↓` in its
  leftmost slot. Titles always reserve both controls' space and stay centered
  in the remaining title region, so hovering never reflows them. Shortcut hints
  stay on the right even during hover; when both left controls leave less title
  width than the measured ellipsis, the hint is omitted to keep the label visible.
  The button is present whenever the strip is visible.
  Its stable-ID action and the keyboard shortcut share
  `App::park_tab_for_window`; clicking an inactive tab's button preserves the
  displayed terminal, including a background peek. The button uses non-focusable
  `Sense::CLICK`, is `StripTarget::Park`, and never starts a tab/window drag.
- **Peek** sets `App::active` to the parked tab without moving it. The strip
  paints `active: None` (no inner pill) and the tray badge is highlighted;
  keyboard/IME, search, selection, and mouse reporting then act on the peeked
  tab. Leaving a peek returns to `last_foreground_tab`; `Ctrl+Tab` and
  `Cmd+1..9` always target the foreground prefix and therefore exit the peek.
  Clicking any foreground tab exits the peek without unparking its background
  tab. `App::can_drag_tab` is shared by native press admission and gesture
  reconciliation: a lone foreground tab captures the chrome release with no
  reorder source, so its click is not suppressed just because parked tabs exist.
- **Closing** the last foreground tab promotes a parked tab instead of
  quitting; closing the peeked tab returns to the foreground tab.
- **State dot:** `Tab::refresh_title` derives running/idle from
  `tcgetpgrp` versus the shell pid (no timer). A running→idle transition on a
  parked tab that is not displayed sets `bg_attention`, cleared on peek or
  unpark. The badge prefers the attention color over the running color.
- **Geometry:** `background_tray_width`/`tray_rect` reserve one `plus_gap` plus
  a pill to the right of `+`; the tab container loses exactly that width.
  Painted tray rects are `StripTarget::BackgroundTray`, so they beat the macOS
  empty-chrome window drag, like tabs/close/park/plus.
- **Popover:** an egui area anchored right-top under the tray, painted only
  while open. Rows, the painted unpark arrow, and `×` use non-focusable
  `Sense::CLICK`, so the terminal keeps keyboard/IME ownership. The painted
  rect is recorded for `ChromeHover::Popover` (pointer repaint),
  `press_on_chrome`, and wheel consumption. Escape, an outside press, tab
  switch, focus loss, or an empty parked list closes it; a press on the badge
  toggles it once (the outside-press close excludes the badge itself).
- **Session:** `TabState.background` restores the parked list. Restore
  partitions foreground tabs first, maps the saved active tab by identity
  (failed spawns do not shift it), and promotes the active tab if a hand-edited
  snapshot parks everything.

Headless widget/state tests cover park-button hit geometry, stable-ID actions,
title/hint hover layout, non-focusable controls, tooltip settling, foreground-only
drag admission, peek-to-foreground click/input routing, and replacement-unavailable
no-ops. They do not verify native titlebar interaction or a successful window-backed
replacement spawn; see [manual verification](development.md#manual-verification).

## Tab-label source policy

[`app/tab.rs`](../src/app/tab.rs) formats `$title`, `$command`, `$cwd`, and `$tab`
with braced variables, literal `$$`, and single-pass substitution. Missing values
expand empty; never strip literal separators to make a partial label look nicer.
An empty render or no filled referenced variable falls back to `Tab N`.

- `$command` is the foreground **process-group leader**, not an arbitrary child.
  Pipelines therefore report their first command; a prompt reports its shell.
- `$cwd` uses the **shell's** kernel cwd, not a walking foreground job's cwd.
  At the prompt, the kernel value beats stale OSC 7. While a job owns the TTY,
  a live OSC 7 from that job/integrated remote shell wins, else shell cwd.
- `osc7_is_live` compares report generation with the last prompt baseline;
  a finished job's leftover path must not resurface during the next job.
- Refresh kernel facts only after output, once per touched tab/burst. Cache
  command lookup until pgid changes, keep facts on failed queries, and keep all
  process syscalls off paint. Do not query immediately at spawn: before exec,
  the child can still look like cometty itself.

[`procinfo.rs`](../src/procinfo.rs) uses macOS process APIs or Linux `/proc`, with
best-effort empty/OSC fallbacks elsewhere. The OS window title follows the active
OSC title or configured app name, separately from the tab-label template. The
tab strip is always present on macOS to reserve traffic-light/titlebar space;
elsewhere it hides for a single tab.
