# Terminal model

Sources: [`term.rs`](../src/term.rs), [`term/parser.rs`](../src/term/parser.rs),
and [`grid/`](../src/grid).

## Parser ownership and interpretation

`Terminal` owns a `BoundedParser`, a `Grid`, pending-wrap state, title/cwd facts,
and queues of response bytes/clipboard requests. It implements `vte::Perform`;
the app drains those queues after feeding output. Keep `feed`'s `mem::replace`
parser swap: advancing the parser needs mutable access to the performer without
losing parser state between chunks.

The implemented protocol includes cursor motion/save/restore, erase and
character/line edits, scroll margins, origin/insert/auto-wrap modes, alternate
screens, application cursor/keypad modes, cursor shape/blink, mouse/focus
reporting, synchronized output, SGR attributes/indexed/truecolor, and device/status
queries. `DECRQM` reports supported modes and unknowns; it is not a complete
xterm implementation. The bell is ignored; unhandled controls do not become
terminal text. Keep exact supported sequences and reply behavior in parser tests.

OSC metadata is borrowed and routed once, rather than rejoining a payload for
every handler:

| OSC | Effect |
| --- | --- |
| 0 / 1 / 2 | Cleaned, character-limited title |
| 7 | Bounded percent-decoded `file://host/path` cwd label; never opens a local file |
| 8 | Start/end an interned hyperlink; cells carry an ID |
| 52 | Decode a bounded clipboard request; the app applies access policy |
| Other | Bounded debug summary marked unhandled; no application effect |

Summaries escape controls and preview only a bounded prefix. OSC 52 contents are
redacted. Title/cwd limits, URI sizes, and clipboard encoded/decoded lengths are
checked while streaming or **before** large copies; limiting only the final
stored value is insufficient.

## Decision: bound OSC before accumulation

With vte's enabled `std` feature, its apparent const-generic OSC size does not
bound the growable raw `Vec`. Handler-side rejection cannot protect an
unterminated or unsupported command. `BoundedParser` therefore guards entry/exit
transitions while leaving CSI/DCS/UTF-8 interpretation to vte and ordinary text
on its bulk feed path.

- Hard limit: **2 MiB of wire bytes after `ESC ]`**, counting number, separators,
  and ignored controls, excluding the terminator. Applies even with OSC 52 off.
- Overflow drops the underlying parser without dispatching a prefix, then
  discards until BEL, CAN, SUB, or ESC. Warn once per terminal, size only.
- **Any ESC** ends OSC and starts the next escape in the locked parser. Preserve
  that transition across overflow and buffer release; do not swallow a following
  CSI while looking only for `ESC \`.
- Raw/UTF-8 `0x9c` is data in vte's OSC state, not a discard terminator.
- A completed/cancelled command longer than **64 KiB** releases parser storage
  at that safe boundary. Smaller commands reuse capacity. This trades exceptional
  allocation traffic for lower retention without per-frame shrinking.
- Normal base64 transfer of 1 MiB clipboard text fits the raw bound, including
  usual wrapping. OSC 52 separately caps decoded bytes **and final UTF-8 text**
  at 1 MiB; invalid UTF-8 replacement expansion must not bypass the latter.

When changing vte, preserve callback-equivalence tests under identical chunking,
every-split overflow/recovery tests, and allocation bounds. Do not replace this
guard with substring filtering or feed normal output byte-by-byte.

## Cells, Unicode, and erase semantics

[`grid/cell.rs`](../src/grid/cell.rs) stores a lead scalar, optional owned cluster
tail, width, resolved colors, attributes, and optional hyperlink ID. Width 1 is
narrow, width 2 is a wide lead, and width 0 is its continuation placeholder.
Continuation cells carry no independent searchable/rendered text.

[`grid/unicode.rs`](../src/grid/unicode.rs) and
[`grid/edit.rs`](../src/grid/edit.rs) handle combining marks, ZWJ, regional
indicators, variation selectors, and skin tones. Ordinary nonjoining ASCII pairs
avoid allocation; non-ASCII boundary cases keep full grapheme logic. Terminal
writes resolve continuation once, then use `put_fresh_char`; direct `put_char`
remains append-aware.

Pending wrap is deliberate: a fresh write can leave `Grid`'s cursor at `cols`.
`Terminal::print` holds a visual cursor at the margin and wraps only for the next
advancing character. A joining mark still attaches to the margin lead without
wrapping. Appending keeps the established width, avoiding retroactive placeholder
growth. Overwrites, edits, erases, and resize must repair split wide halves.

Two kinds of blank must remain distinct:

- **BCE erase cells:** current pen background, default foreground/attributes, no
  hyperlink. Used for ED/EL, inserted/deleted lines, scroll fills, and recycling.
- **Structural blanks:** theme defaults for resize growth and fresh alternate
  storage. Existing cells keep resolved colors when the theme changes.

The active OSC 8 hyperlink lives **outside `Pen`**. `SGR 0` does not end it; an
empty OSC 8 URI or full terminal reset does. The intern table is capped by count,
URI bytes, and total unique bytes; do not reuse IDs while retained cells reference
them.

Bounded-region operations rotate/fill storage in bulk and cap counts to affected
rows. Full-screen main-buffer upward scrolling still produces history for every
requested line; clamping it to viewport height would change behavior. REP expands
up to 16,384 `print` calls. Neither this nor long cluster appends is preempted by
the drain timer; see [remaining limits](memory.md#remaining-limits-and-follow-up-work).

## Versions and row identity

| Token | Meaning / consumer |
| --- | --- |
| `Grid::version` | Visual invalidation, including viewport/cursor state |
| `Grid::content_version` | Content-oriented invalidation for search; presentation-only paths use `bump_view` |
| `Row::{id, revision}` | Logical row and conservative mutation revision, independent of position |
| `RowView::identity()` | `(id, revision, projection revision)` for projected content |
| `history_dropped` | Cumulative rows leaving the global-front sequence; rebases selection/search |

Moving a row into history or compacting it preserves identity. Recycling storage
or cloning a `Row` creates a new ID. A retained allocation address is not a row
identity. Search captures identities; the shared snapshot uses projected keys to
avoid recopying unchanged rows. Grid counters are tab-local, so shared renderer
caches also need tab/session identity or content comparison.

## Decision: immutable compact history with lazy width projection

History is not another mutable live grid. Keeping every blank as a dense `Cell`
costs far more than its displayed text, and rewriting every history cell during
resize makes work scale with scrollback area. The current design separates
immutable stored payloads from visible width semantics.

### Bounded storage batches

[`grid/history/storage.rs`](../src/grid/history/storage.rs) accumulates a dense
tail of at most **32 rows or 256 KiB** of source cell capacities/cluster tails per
ordinary batch. It seals at row/byte thresholds, cohort boundaries, or byte
pressure. A stopped output stream retains only an accounted bounded tail.
An individually oversized row stays dense without copying it into a packer.

[`grid/history/packed.rs`](../src/grid/history/packed.rs) preflights bounded
batches and packs only when savings reach **one eighth** of dense storage.
Otherwise it keeps the original rows. Packed chunks contain:

- Shared UTF-8 text plus indexed row identity/offset records.
- Sparse metadata preserving exact nontrivial cluster boundaries and widths;
  decoding never resegments text or guesses widths.
- Consecutive style runs that can coalesce across rows, including hyperlink IDs.
- Implicit narrow-space text suffixes, while retaining their full styles.

Indexed lookup uses admission order. Eviction expires **individual rows**, not
whole chunks for the line cap. One partially expired oldest compact chunk remains
fully charged until its last row leaves.

### Width cohorts and projection

[`grid/history.rs`](../src/grid/history.rs) keeps one compact `Projection` for
rows admitted in a width cohort. Resize seals the current batch and updates
cohort metadata, never rewrites sealed row payloads. The projection records the
surviving prefix, growth-time structural-padding colors, and wide-edge repair.

Shrink is **destructive**: clipped text must never reappear after regrowth.
Later theme changes must not recolor existing cells or earlier padding. Preserve
this behavior across packed and dense history rather than treating lazy storage
as permission to change terminal semantics.

### One read abstraction and complete accounting

Render, search, links, and copy consume `RowView`/borrowed `CellRef`. Compact ASCII
rows expose projected text directly for search; general scans borrow cluster
tails. Only visible rows decode into the existing shared snapshot. Some selection
helpers still materialize selected rows; never expand the whole history to scan it.

The line cap defaults to 10,000 and the per-tab byte budget to 128 MiB. Accounting
includes chunk/container capacities, tails, dead prefixes, cohort projections,
and bounded recycling storage: dense buffers, one empty row-header deque, and
one evicted packed chunk. Release reusable storage before evicting history under
byte pressure. Clearing history or setting **either cap to zero** releases chunks
and pools. `history_dropped` advances even when a row is immediately discarded.

The live/alternate grid remains dense. `app::geometry::clamp_grid_size` is shared
by window, Grid, and PTY paths: per-axis limits, PTY `u16`, and a hard 1,048,576
logical cells per buffer all apply. These are not process-memory ceilings.

## Regression entry points

```sh
cargo test --locked term::
cargo test --locked grid::
```

Keep byte-split equivalence, pending-wrap Unicode, BCE, alt/margin isolation,
bulk-operation equivalence, row-ID recycling, eager-reference resize/theme
equivalence, exact eviction, zero-cap release, and accounting tests. See
[performance](performance.md) for release/memory probes and
[memory](memory.md) for what their byte counters exclude.
