# Interaction and view state

The pure models in [`search.rs`](../src/search.rs),
[`selection.rs`](../src/selection.rs), [`links.rs`](../src/links.rs), and
[`input.rs`](../src/input.rs) are separate from app dispatch and egui widgets.
Per-tab state belongs to `Tab`; system clipboard access and window focus are
shared app concerns.

## Keyboard and IME ownership

[`app/keyboard.rs`](../src/app/keyboard.rs) intentionally dispatches in this order:

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
