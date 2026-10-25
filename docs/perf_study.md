# Cometty performance study

- **Date:** 2026-10-25
- **Source baseline:** `6a058d1` (clean working tree before this study)
- **Scope:** runtime performance, responsiveness, memory, and power consumption; recommendations only. No application code was changed.

## Executive summary

The highest-value work is to remove avoidable work, not replace the rendering stack or introduce more threads everywhere.

1. **Fix the scrollbar's perpetual repaint request.** A stationary, fully opaque scrollbar keeps requesting frames while viewing history or hovering. This defeats the otherwise reactive event loop.
2. **Make ordinary ASCII character handling allocation-free.** A 256 KiB ASCII feed made **1,052,509 allocations** and took **86 ms** in an optimized, headless probe. Grapheme-continuation checks account for approximately four short-lived allocations per character.
3. **Bound work within a parser callback, not just between PTY chunks.** An **8-byte** insert-lines sequence took **49 ms** and requested **600 MiB** of cumulative allocations. A roughly 8 KiB combining-mark sequence took **37 ms**. Both bypass the intended 4 ms responsiveness budget.
4. **Make search incremental and keep full-history scans off the rendering critical path.** Searches over 10,060 rows took **15–139 ms**, depending on match density. The existing 150 ms throttle reduces frequency, not individual stalls.
5. **Reduce shaping allocations and cache multi-character cluster measurements.** Building one 200-column decomposed-accent row took **7.8 ms**, before its final row layout, because every cluster is measured again.

There are also worthwhile opportunities in retained glyph preparation, row-level invalidation, fair PTY scheduling, background geometry, and resize handling. GPU speedups remain hypotheses: **no GUI, presentation-latency, GPU, or energy measurements were taken**.

## 1. Scope, method, and confidence

### Repository coverage

The repository contains 45 Rust source files and 21,009 lines, including tests. The analysis traced these runtime subsystems:

| Subsystem | Main files | Questions examined |
| --- | --- | --- |
| Event loop and input | `src/main.rs`, `src/app/{keyboard,mouse,redraw}.rs`, `src/input.rs` | What wakes the application? What blocks input? When is an unchanged terminal repainted? |
| PTY transport and scheduling | `src/pty.rs`, `src/app/pty_io.rs` | Backpressure, allocations, wake frequency, drain budgets, multi-tab fairness |
| Parsing and terminal model | `src/term.rs`, `src/grid/*` | Per-character cost, graphemes, scrolling, editing, invalidation, memory bounds |
| Rendering | `src/renderer/*`, `src/theme.rs` | Shaping, retained work, uploads, geometry, color conversion, overlay cost |
| Search, links, selection | `src/{search,links,selection}.rs`, corresponding `src/app/*` | Full-history work, per-frame scans, repeated allocations and compilation |
| Chrome, configuration, diagnostics | `src/{config,tabbar,procinfo,logging,logbuf}.rs`, settings/log UI | Syscalls, persistence, filtering, logging feedback, startup-only work |
| Existing validation/profiling | `Cargo.toml`, `Cargo.lock`, CI, `scripts/profile-macos.sh`, relevant tests | Optimized builds, profiling facilities, correctness constraints |

Locked versions relevant to the analysis: wgpu 30.0.1, glyphon 0.12.0, cosmic-text 0.19.0, egui/egui-wgpu/egui-winit 0.36.1, vte 0.15.0, portable-pty 0.9.0. Installed dependency source was inspected where application-level behavior depended on it.

### Evidence labels

- **Measured:** optimized headless execution of the current production functions, or a deterministic state reproduction.
- **Observed:** a specific cost or scheduling behavior follows directly from the code, but its end-to-end impact was not profiled.
- **Hypothesis:** a plausible optimization requiring comparative application/GPU/power measurements.

Confidence in a mechanism is not a claim about its share of total application time. For example, a redundant glyph upload is observable without knowing whether CPU preparation, GPU rendering, or presentation dominates a real frame.

### Measurement environment and limitations

- Intel MacBookPro16,2, quad-core 2 GHz Core i5, 16 GB RAM; macOS 26.7.1.
- Rust 1.96.1, `x86_64-apple-darwin`; standard Cargo release profile.
- A temporary external Cargo probe imported the production modules using `#[path]`. Its dependency names and versions matched the repository lockfile, excluding the probe's package name.
- Text measurements used the production `renderer/text.rs` and `blocks.rs` with a minimal headless renderer containing the fields those functions need. No window, surface, glyphon GPU preparation, or presentation was involved.
- Each timing case received one warm-up call, followed by **seven batches**. Tables report the median batch mean per operation. Allocation counts came from a separate call with a counting system allocator; counting was disabled during timing.
- Allocation totals count allocation/reallocation requests, not allocator size classes. **Cumulative requested bytes are allocation traffic, not peak RSS or retained memory.** Even with counting disabled, the allocator wrapper adds a small conditional check.
- No affinity pinning, thermal control, confidence intervals, or cross-platform replication. These are exploratory results, not a production benchmark suite.
- The global application logger was not installed. In particular, OSC measurements exclude the application's default debug recording and are not a complete measurement of that path.
- Existing ignored trace bundles were not used as evidence for the current revision.

### Baseline verification

| Check | Result |
| --- | --- |
| `cargo test` | 328 passed; test execution 16.63 s in the unoptimized test profile |
| `cargo clippy -- -D warnings` | Passed |
| `cargo fmt --check` | Passed |

Test-suite elapsed time is not a runtime throughput benchmark. The GUI binary was not launched.

## 2. Measurements

### Parsing, shaping, and storage

Unless specified otherwise, terminal/grid dimensions were 200 columns × 60 rows. Streaming feed cases used 8,192-byte chunks and pre-filled scrollback to exercise steady-state eviction.

| Operation | Median time | Allocation calls / reallocations | Interpretation |
| --- | ---: | ---: | --- |
| Feed 262,144 ASCII `a` bytes | 86.24 ms | 1,052,509 / 0 | About 2.9 MiB/s; parsing/grid mutation only |
| Feed 262,160 bytes of 78 `a`s + CRLF | 89.62 ms | 1,019,147 / 0 | Row allocation contributes more with frequent newlines |
| Feed 262,152 bytes of truecolor/reset sequences | 9.80 ms | 131,076 / 0 | Mostly control bytes; not directly comparable to printable-byte throughput |
| Feed 253,500 bytes of OSC 8 open/text/close | 10.05 ms | 136,500 / 26,000 | Logger disabled; repeated URI already interned after warm-up |
| Process `ESC [ 65535 L` (8 bytes) | 48.91 ms | 65,535 / 0 | 629,136,000 cumulative requested bytes, approximately 600 MiB |
| Process `ESC [ 16384 b` (8 bytes), repeating `a` | 4.70 ms | 65,782 / 0 | One callback alone can exceed the drain time target |
| Process an unsupported OSC with a 1 MiB payload | 3.11 ms | 8 / 0 | About 4 MiB copied/allocated after parser-buffer warm-up, even with logger off |
| Clone an ASCII 200 × 60 viewport | 60 µs | 61 / 0 | 577,440 requested bytes per snapshot |
| Clone the same viewport with an extra combining mark in every cell | 658 µs | 12,061 / 0 | `Cell::extra` is deep-cloned |
| Hash/diff an unchanged 200 × 60 viewport | 458 µs | 0 / 0 | Existing partial shaping still scans every cell |
| Build one 200-character ASCII `BufferLine`, warm advances | 17 µs | 212 / 11 | Excludes final row layout |
| Rebuild buffer with no changed rows | 479 µs | 0 / 0 | Predominantly row hashing |
| Rebuild buffer with one changed row | 679 µs | 670 / 23 | Hashing plus row construction/layout |
| Rebuild buffer with every row changed | 9.06 ms | 40,023 / 1,325 | Approximately 7 MB cumulative allocation traffic |
| Build one 200-cell decomposed-accent `BufferLine` | 7.78 ms | 11,812 / 813 | Repeated multi-character measurement; excludes final row layout |
| Resize full history from 200 → 201 → 200 columns | 18.63 ms | 122 / 0 | Capacity was warmed; scans remain expensive without history reallocations |

Selected timing ranges, to expose variability: ASCII feed 77.67–90.48 ms; all-row rebuild 8.02–9.87 ms; insert-lines sequence 48.06–51.77 ms. Do not add independently measured phase medians and call the result a measured frame.

### Search and secondary paths

| Operation | Median time | Important detail |
| --- | ---: | --- |
| Search 10,060 rows × 200 columns, absent lowercase query | 14.99 ms | Full history conversion/scan, even without results |
| Same history, query `a` over repeated `abcdefghij` | 32.65 ms | 201,180 matches |
| Same-sized history, query `a` over 199 `a`s per populated row | 139.37 ms | 2,001,741 matches; result capacity occupies 48 MiB |
| Build visible `SearchView` from 201,180 matches | 427 µs | Scans all matches, not just visible ones |
| Build visible `SearchView` from 2,001,741 matches | 2.85 ms | Paid again on unchanged rendered frames while search is open |
| Hit-test all visible cells with dense matches | 1.01 ms | Per-cell linear range checks |
| Filter a 100,000-entry log ring, empty text filter | 4.34 ms | Includes returning/dropping an owned vector of `Arc`s |
| Filter that ring with an absent text query | 8.69 ms | Text scan occurs while holding the ring mutex in the UI |
| Expand a word on a 200-column row | 36 µs | Includes compiling the default regex again; 131 allocations and 26 reallocations |
| Detect URLs on one 200-column row | 2–3 µs | Small in isolation; repeated on pointer motion |
| Query this process's cwd | 2.5 µs | Local macOS result; not evidence that title queries dominate frames |
| Synthetic loop: two sRGB-to-linear conversions × 12,000 colors | 1.25 ms | Conversion-only microbenchmark, **not** a measurement of `paint_bg` |

### Deterministic scheduling and pathological-input checks

- With history available and `offset = 10`, **600 of 600** scrollbar updates requested another frame; opacity had already reached `1.0`. A settled hovered scrollbar also returned `true`.
- A cursor-left CSI changed `grid.content_version`, despite changing no cell text.
- Feeding a single cluster consisting of `a` followed by combining acute accents produced:

| Combining marks | Median construction + feed + drop | Cumulative requested bytes |
| ---: | ---: | ---: |
| 1,000 | 3.48 ms | 7.3 MB |
| 2,000 | 9.22 ms | 29.1 MB |
| 4,000 | 37.34 ms | 116.1 MB |
| 8,000 | 132.45 ms | 464.1 MB |

The nonlinear allocation growth agrees with repeated whole-cluster copying. The 4,000-mark payload is 8,001 bytes: it fits in one normal PTY read. These timings include creation/destruction of an 80 × 24 terminal, unlike the steady-state feed cases.

## 3. Critical paths and existing strengths

### Output → frame

```text
PTY reader: read → allocate chunk → bounded channel → wake event loop
UI: drain_pty → Terminal::feed → vte callbacks → Grid mutations
    → refresh process/title metadata → request redraw
redraw: drain again → refresh search → link hints → clone changed viewport
    → build highlight views/titles → hash changed grid → shape changed rows
    → rebuild background geometry → run egui → acquire surface
    → prepare all text glyph instances → upload/encode/submit/present
```

Parsing, process queries, searching, snapshots, shaping, UI building, and surface acquisition all occur on the UI thread. Reducing average work helps throughput; bounding individual bursts is essential for tail latency.

### Input → echo

```text
winit/egui dispatch → key mapping → owned input bytes → writer channel
    → write_all/flush → child → PTY reader → output path above
```

Normal key encoding is small. A slow output drain or frame can dominate latency even if the writer is fast.

### Good optimizations already present — preserve them

- PTY output is bounded to 128 × 8 KiB, approximately 1 MiB per tab, plus in-flight/kernel buffers. Backpressure is correct; an unbounded queue is not a throughput optimization.
- Drains have byte/time budgets, even though those budgets need stronger scheduling and callback bounds.
- Viewport snapshots are version-gated; cursor-blink frames do **not** currently clone the entire grid every time.
- Text shaping already uses per-row hashes and relayouts only changed rows. The remaining problems are all-row hashing, scroll reuse, construction allocations, and unconditional glyph preparation—not absence of any text cache.
- Single-character natural advances are cached; background scratch storage and GPU vertex capacity are reused.
- Search uses borrowed rows, scratch reuse, ASCII folding, binary-search byte/column mapping, content-version caching, and a 150 ms background throttle.
- Link underline detection is cached per tab; log UI rows are virtualized, and a closed log panel does not lock the ring.
- Configuration snapshots are gated on the settings panel. Window-title syscalls are gated on title changes; command lookup is gated on foreground pgid changes.
- The loop sleeps between work using `WaitUntil`; egui repaint deadlines and deferred resize are already integrated.

## 4. Ranked recommendations

Ranking balances frequency in normal use, stall/power severity, measured evidence, and implementation risk. It is not a fabricated numerical score or a forecast of percentage speedups. **S** ≈ a localized change plus tests; **M** ≈ a cross-module change; **L** ≈ a substantial representation/scheduling change. These are relative engineering sizes, not delivery estimates.

| Rank | Recommendation | Most relevant workload | Evidence/confidence | Size |
| ---: | --- | --- | --- | --- |
| 1 | Stop perpetual scrollbar repaints; suppress unnecessary blink/occluded work | Idle history viewing, hover, battery use | Measured state reproduction; high | S–M |
| 2 | Allocation-free ASCII grapheme fast path; avoid duplicate continuation checks | Every shell, logs, large text output | Measured; high | M |
| 3 | Bound parser work amplification and variable-length terminal data | Bursts, malformed/hostile output | Measured + dependency source; high | M–L |
| 4 | Incremental/budgeted search with viewport-only result projection | Find bar over large history, streaming output | Measured; high | L, with S quick wins |
| 5 | Allocation-light line construction and multi-character advance caching | TUI redraws, decomposed Unicode, scrolling | Measured; high | M |
| 6 | Retain glyphon preparation across text-identical frames | Blink, selection, chrome animations | Observed; GPU/application benefit unmeasured | M |
| 7 | Precise row damage, reusable snapshots, and scroll-aware shaped-line reuse | Typing, cursor-only updates, scrolling | Measured primitives + observed invalidation; high | L |
| 8 | Fair PTY draining, coalesced wakes, active-view-aware redraws | Multiple busy tabs, background jobs | Observed; high mechanism confidence | M–L |
| 9 | Cheaper background painting: lazy/LUT colors, merged runs, retained geometry | Colored TUIs, selection, block graphics | Observed + synthetic measurement; medium impact confidence | S–M |
| 10 | Avoid eager full-history resize work | Live resize with large history/many tabs | Measured; high | M–L |
| 11 | Explicit memory budgets and a more compact cell/history layout | Wide windows, many tabs, long sessions | Measured sizes; high | M–L |
| 12 | Parse/route OSC once instead of copying every payload for every handler | Hyperlink-heavy output, large ignored OSCs | Measured; high | S–M |
| 13 | Cache log-filter results and shorten lock-held work | Open log panel, large configured ring | Measured; high | M |
| 14 | Reuse scrolled-out rows; move rather than clone shifted cells | Sustained scrolling, alt-screen TUIs, editing | Observed; medium impact confidence | M |
| 15 | Cache selection regex; borrow rows for word selection/copy | Word dragging, copying large histories | Measured subpath; high | S–M |
| 16 | Cache composed titles and gate/throttle metadata queries | Many tabs, frequent output/title updates | Observed; local syscall cost measured small | S–M |
| 17 | Debounce settings writes; invalidate only affected renderer state | Dragging font/chrome settings, slow config storage | Observed; impact unmeasured | S–M |
| 18 | Bound/stream large input and batch only queued writes | Large/repeated pastes, mouse-reporting floods | Observed; impact unmeasured | M |

Workload overrides: #5 rises for decomposed Unicode; #10–11 rise for wide, multi-tab use; #3 is urgent if untrusted terminal output is in scope. For a small ASCII prompt, neither a complex search index nor a new GPU pipeline should precede #1–2.

## 5. Findings in detail

### 1. Stop requesting frames when the scrollbar has stopped changing

**Evidence:** [`ScrollbarUi::update`](../src/scrollbar.rs#L110-L172) returns:

```rust
animating || (target > 0.0 && self.opacity > 0.0)
```

At a fixed scrollback offset, `target == opacity == 1`, but the second clause stays true. [`paint_overlay`](../src/renderer/overlay.rs#L520-L544) calls `ctx.request_repaint()` on that result, and [`on_redraw`](../src/app/redraw.rs#L233-L246) immediately requests another frame for a zero delay. Hovering has the same settled-state behavior. This is an application-created repaint loop, not merely the expected cursor timer.

**Proposal:** return an explicit animation/deadline decision: repaint while opacity changes; schedule one wake at the fade deadline when waiting to hide; do nothing while fully visible and stationary. Use elapsed time rather than the fixed `1/60` step so animation speed is independent of actual frame rate.

Also gate the application's blink redraw on whether a visible, blinking cursor can change pixels. [`about_to_wait`](../src/main.rs#L359-L382) currently requests a frame every 530 ms even for a steady/hidden cursor or a cursor hidden by scrollback. Track occlusion/minimization and skip visual work when appropriate: currently an occluded acquisition is discovered only after text/background/overlay work ([`render`](../src/renderer/mod.rs#L664-L719)). Continue draining/responding to PTYs while not rendering.

**Expected benefit:** removes whole unnecessary frames, potentially the largest idle/power win. Actual frame rate and energy savings depend on presentation mode/platform; they were not measured.

**Validation:** test settled hover/scrollback returning no immediate repaint, fade-deadline wake-up, disappearance after returning to bottom, and elapsed-time animation. Measure frames/s and CPU/GPU activity for 60 seconds with stationary history, then repeat with steady cursors and occlusion. Preserve egui's legitimate animation/tooltip deadlines.

### 2. Remove allocations from ordinary printable ASCII handling

**Evidence:** [`Terminal::print`](../src/term.rs#L615-L663) asks `is_append_continuation`, then calls `put_char`, which checks `append_to_prev` again. Both reach [`append_target`](../src/grid/edit.rs#L11-L77). That helper constructs `lead.cluster()` before deciding whether the next character joins. [`continues_cluster`](../src/grid/unicode.rs#L53-L61) then allocates `prev + next` and segments it. A normal nonjoining ASCII character after another nonblank ASCII character therefore commonly pays for two copies of this two-allocation check.

The 256 KiB feed's allocation count is approximately four allocations per printable byte plus row/storage overhead. The measured cost is already present without rendering or logging.

**Proposal:** add a proven nonjoining fast path when both the incoming printable character and previous lead are ASCII and the previous lead has no `extra`. Only invoke full grapheme logic for possible joining cases. Resolve a continuation target once and pass/reuse that result instead of rediscovering it in both terminal and grid paths. Avoid allocating `prev_text` until it is actually needed; explicit joining modifiers often do not need that string either.

Do **not** simply declare every incoming ASCII character a new cluster: non-ASCII preceding text can affect grapheme boundaries. Preserve the Unicode slow path, pending-wrap behavior, blanks, wide continuations, RI pairs, ZWJ, and combining-at-margin semantics. Keep the parser swap pattern in `feed`.

**Expected benefit:** a large reduction in allocator and segmentation work in the most universal hot path. No optimized implementation was benchmarked, so no speedup multiplier is claimed.

**Validation:** allocation counts for steady ASCII writes should approach row-management overhead rather than scale four-per-character. Compare terminal state across every split point for mixed ASCII/Unicode/control corpora, and retain the existing grapheme and wrap regressions.

### 3. Make the PTY time budget meaningful inside expensive operations

**Evidence:** [`drain_pty`](../src/app/pty_io.rs#L41-L64) checks elapsed time only after `Terminal::feed` finishes a chunk. One callback can do much more work than its byte count suggests:

- [`insert_lines`/`delete_lines`](../src/grid/edit.rs#L478-L535) repeat allocation and vector manipulation `n` times without capping `n` to the affected region. The 65,535-line example is visually saturated after far fewer iterations.
- [`scroll_region_*`/`scroll_up`/`scroll_down`](../src/grid/scroll.rs#L200-L301) have similar count-driven loops.
- [`REP`](../src/term.rs#L822-L835) permits 16,384 full `print` callbacks from one short escape sequence.
- Cluster appends repeatedly reconstruct the full cluster ([`append_to_cell`/`append_to_prev`](../src/grid/edit.rs#L82-L147)); `extra` has no length limit.
- With its enabled default `std` feature, vte 0.15.0 stores OSC raw bytes in a growable `Vec`. Its apparent 1,024-byte generic limit applies to the **non-std** implementation, not this build. Application title/link/clipboard limits are applied only after parsing an OSC.

**Proposal:** cap IL/DL and bounded-region operations to the number of rows they can affect, use bulk rotation/fill, and give REP/other expanded work a defensible operation budget or resumable representation. Check full-screen scroll/history semantics before clamping SU: dropping intentional history is not automatically a valid optimization.

Introduce explicit cluster byte/codepoint bounds with a documented overflow policy. Apply raw control-string bounds before complete OSC accumulation, with a parser-aware discard-until-terminator state; rejection in `osc_dispatch` is too late. Do not truncate arbitrary byte chunks in ways that desynchronize UTF-8 or escape parsing. A smaller no-std parser buffer is not a drop-in solution because legitimate OSC 52 payloads can be much larger.

**Expected benefit:** prevents input-driven UI stalls and allocation spikes. Smaller PTY chunks alone cannot fix amplified work in an 8-byte sequence or repeated copying of a growing cluster.

**Validation:** adversarial tests for large counts on tiny grids, long combining runs, split/unterminated OSCs, correct recovery at terminators, bounded retained memory, and correct processing of the following printable/control input. Compare optimized bulk operations against existing behavior for small counts before deciding any intentional large-count policy changes.

### 4. Separate search scanning, match storage, and visible highlights

**Evidence:** [`SearchPanel::refresh`](../src/app/search.rs#L104-L137) synchronously calls [`find_matches`](../src/search.rs#L59-L142) during redraw. Any new query scans all history; background updates can repeat that scan about every 150 ms. The throttle does not divide a scan into short work units.

Even after matching is cached, [`SearchView::build`](../src/search.rs#L163-L208) walks the **entire match list on every rendered frame**, then `hit` linearly searches row ranges for each painted cell. The list is already ordered by line and column. Dense one-character queries can also allocate millions of 24-byte matches. Closing the bar clears the vector but retains its capacity ([`close`](../src/app/search.rs#L81-L87)).

**Proposal, in increasing scope:**

1. Use `partition_point` to select only matches between the first and last visible global line. Cache the projection until matches, current match, viewport, or dimensions change.
2. Classify a row with a monotonic range cursor, binary search, or a compact visible-cell mask rather than scanning every range per cell. Handle the active range separately.
3. Give rows stable IDs and content revisions. Cache searchable row text/column maps selectively, then rescan only changed/appended rows for an unchanged query. Retire evicted rows by ID instead of rebasing every match on every scroll.
4. For query changes, use time-sliced scanning or a cancellable worker. Publish results by query generation and terminal identity/version, not just a numeric grid version. Avoid deep-copying the whole `Grid` to move work to another thread.
5. Bound match storage or use compact per-line offsets, with explicit UI semantics if results are capped. Release exceptionally large retained capacities at sensible lifecycle points, not after every query.

**Expected benefit:** removes 15–139 ms synchronous scans from common find-bar interactions and makes unchanged-frame highlighting proportional to visible results rather than all history.

**Validation:** preserve smart-case mapping, Unicode expansion/byte-to-column spans, nonoverlap, wide cells, trailing-blank behavior, alt-screen scope, navigation wrap, and the rule that background refresh never scrolls. If scans are deferred, schedule their next deadline explicitly; do not depend on the next blink to finish a trailing refresh. Test cancellation and eviction during scanning.

### 5. Build text lines without one heap string per cell

**Evidence:** [`build_buffer_line`](../src/renderer/text.rs#L116-L239) materializes `Vec<(usize, String)>`, including trailing blank cells that are subsequently removed; builds a combined string; clones that string into `BufferLine`; and creates another spacing vector and attribute structures. A warm 200-character ASCII row still made 212 allocations before layout.

[`cached_advance`](../src/renderer/text.rs#L55-L97) caches only single-character clusters. Every decomposed accent or ZWJ cluster constructs a temporary cosmic-text buffer and shapes/layouts it again. The measured repeated `e` + combining-acute row was roughly 7.8 ms just to construct a line.

**Proposal:** determine the last relevant cell first; append `ch`/borrowed `extra` directly into one preallocated string and store byte/column descriptors rather than per-cell strings. Move the string into the line instead of cloning it. Reuse bounded descriptor/spacing scratch storage and reduce redundant `AttrsList`/feature cloning where ownership permits.

Extend the natural-advance cache to multi-character clusters with style and a font/metrics generation in its key. Bound both entry count and key bytes; the pathological clusters in #3 must not become permanent cache entries. Consider reusing a measurement buffer or batching cache misses before attempting a different shaping algorithm.

**Expected benefit:** fewer allocations on every changed row and a substantial reduction in repeated decomposed/ZWJ measurement. Cold font fallback and final row shaping still cost work.

**Validation:** compare layout fingerprints and exact cell advances for ASCII, mixed weights/colors, fallback scripts, CJK, ligature-like sequences, emoji and decomposed accents. Keep advanced shaping/fallback for complex text; globally changing to basic shaping is not a safe shortcut.

### 6. Reuse prepared glyph instances on text-identical frames

**Evidence:** [`render`](../src/renderer/mod.rs#L728-L753) calls `TextRenderer::prepare` on every successful surface acquisition, whether or not row text/layout changed. Locked glyphon 0.12.0 clears its instance vector, walks layout runs, and uploads glyph-instance data during preparation; `render` itself draws the previously prepared layouts.

**Proposal:** track prepared-text dirtiness independently of frame dirtiness. Reuse the prepared text for cursor blinking, selection changes, link underlines, and chrome-only frames when glyph data and text placement are unchanged. Invalidate on actual shaped-line changes, viewport/bounds/tab-bar offset, DPI/metrics/font changes, color/default-color changes, and any atlas/resource lifecycle event that makes cached instances invalid.

Do not key this solely to `grid.version`: that includes cursor/view/mode changes. Have the text rebuild report whether it actually replaced a line or changed layout inputs.

**Atlas caveat:** the current `atlas.trim()` clears glyph-in-use tracking; later allocation can evict glyph slots. Skipping preparation needs an explicit atlas-lifetime policy. In this single terminal renderer, an unchanged frame that does no atlas allocation is a plausible reuse case, but future shared atlases or multiple prepared buffers need stronger pinning/invalidation. Do not simply delete `prepare` calls without testing eviction and growth.

**Expected benefit:** avoids repeat CPU glyph traversal and uploads. It does not eliminate the need to redraw text into a newly acquired surface when another element changes. The size of the application-level win requires GPU/CPU traces.

### 7. Carry precise row damage through the pipeline

**Evidence:** [`Grid::bump`](../src/grid/mod.rs#L237-L247) advances both render and content versions. Cursor movement, carriage return, backspace, tab, SGR pen changes, and several mode flags use it even without cell changes. That invalidates search content caches and snapshots unnecessarily.

On any grid-version change, [`on_redraw`](../src/app/redraw.rs#L55-L75) deep-clones the whole viewport. [`diff_rows`](../src/renderer/text.rs#L248-L286) then hashes every cell, even when only the cursor or one character changed. Hashes are tied to row positions: scrolling one line usually invalidates all shifted rows rather than reusing their existing shaped layouts.

**Proposal:** distinguish presentation state, viewport mapping, cell content, and text-shaping inputs. Track dirty rows or stable row IDs/revisions as mutations happen. Reuse snapshot allocations with `clone_from` as an intermediate improvement; longer term, borrow immutable visible rows through disjoint app-field borrows or update only dirty snapshot rows. A small iterator/view abstraction can replace the temporary `Vec<&Vec<Cell>>` without making pointer-allocation removal a separate project.

Reuse shaped lines when rows move through the viewport, subject to a bounded cache of recent/visible rows. Do not retain a cosmic-text layout for all 10,000 history rows just to optimize a one-line scroll. Prefer explicit revisions over switching to a faster hash and leaving all-row scans intact.

**Expected benefit:** sparse updates approach changed-row work instead of whole-viewport cloning/hashing; cursor-only sequences stop triggering full-history search refreshes. The measured 458 µs hash scan is notably larger than the 60 µs ASCII clone on this machine.

**Correctness risks:** erase/split-wide repair can touch neighboring columns; scroll and alt-buffer swaps change row identity; decoration-only changes must repaint without reshaping. Tab identities must survive vector-index changes/removal, and cached renderer data cannot rely on two tabs having different counters. Existing content-version and row-hash tests are a useful starting point, not complete damage-tracking coverage.

### 8. Make PTY scheduling fair and wakeups proportional to pending work

**Evidence:** [`drain_pty`](../src/app/pty_io.rs#L34-L83) starts at tab zero each time and stops the entire tab loop on a shared budget hit. A constantly busy earlier tab can repeatedly prevent a later active tab or exit event from being serviced.

The reader wakes the event loop for every chunk ([`pty.rs`](../src/pty.rs#L100-L119)). Each wake invokes a drain; redraw and `about_to_wait` also drain ([`main.rs`](../src/main.rs#L192-L198), [`main.rs`](../src/main.rs#L351-L357), [`redraw.rs`](../src/app/redraw.rs#L29-L32)). Consequently, the comment “per-frame budget” describes a per-invocation limit, not an enforced total budget before the next paint/input opportunity.

Output from any tab can also invalidate the renderer and request an active-view frame, even when active cells and rendered tab labels did not change ([`pty_io.rs`](../src/app/pty_io.rs#L105-L143)). `refresh_title` is once per drain burst, not necessarily once per rendered frame.

**Proposal:** persist a round-robin drain cursor with an active-tab latency preference and minimum service for other tabs. Apply a global work deadline across a scheduling turn, plus per-tab quotas. Coalesce PTY notifications with a pending-work flag and re-arm race-safely. Always schedule continuation when a budget is exhausted, independently of selection-drag and synchronized-output suppression.

Separate “more bytes need parsing” from “a visible frame needs presenting.” Only invalidate/repaint the active terminal for active visual changes, and tab chrome for actual label changes. Honor synchronized-output batching in the presentation decision, with a timeout/recovery policy, rather than assuming suppressed PTY redraw requests suppress all other redraw sources.

**Expected benefit:** better active-tab latency, fewer empty drains/wakes, and less background-induced rendering. Keep the existing bounded channel and parser reply/exit ordering.

**Validation:** saturate tab zero, type in a later tab, exit a third tab, and measure per-tab service gaps, queue occupancy and p95/p99 echo latency. Test notification races, pending-work continuation, selection drags, and synchronization markers split across chunks. More aggressive batching must not add an arbitrary sleep to isolated keystrokes.

### 9. Reduce background-painter work and geometry traffic

**Evidence:** [`paint_bg`](../src/renderer/background.rs#L186-L358) traverses every visible lead cell each frame, checks highlight ranges, computes colors, and emits six 20-byte vertices per rectangle. Equal-color neighboring backgrounds/underlines are not merged. Geometry is rebuilt/uploaded even on text-identical frames.

The source computes foreground and decoration linear colors before knowing whether those colors will be used. [`Rgb::as_linear_f32_array`](../src/theme.rs#L28-L44) performs three transfer-function calculations, usually involving `powf`. Whether an optimized compiler sinks unused conversions in this particular loop should be checked; the 1.25 ms conversion probe forces the calls and is not proof of the painter's actual cost.

**Proposal:** first make color conversion lazy and use a 256-entry per-channel sRGB lookup table, preserving the transfer function. Hoist theme/selection/cursor constants. Merge adjacent same-color background and decoration runs. Keep base cell geometry separate from transient cursor/selection/search/link overlays so one cursor toggle does not regenerate a colored screen.

If geometry upload remains significant, use indexed quads or rectangle instances instead of six duplicated color/position vertices. Preserve draw order: active search beats selection, which beats other search matches; blocks/decorations/cursor have their own layering. Preserve pixel snapping/shared edges and whole-wide-cell highlighting.

**Scale illustration:** one rectangle per cell on a 200 × 60 colored viewport is 72,000 vertices, **1.44 MB/frame**, or **86.4 MB/s at 60 frames/s**, before blocks/decorations. This is a calculated upload volume, not a measured GPU bottleneck. Eliminating the frame loop in #1 comes first.

### 10. Avoid touching all history on every width increment

**Evidence:** [`Grid::resize`](../src/grid/mod.rs#L260-L321) allocates/copies live cells and a saved main buffer, then resizes and repairs every scrollback row when columns change. `fix_row_wide` scans each row twice. All tabs are resized together ([`apply_resize`](../src/app/pty_io.rs#L174-L193)). Deferred resize coalesces events before a frame, but each applied width step still does history-wide work.

The warmed 200 → 201 → 200 experiment took 18.6 ms even without scrollback reallocation. A first growth can additionally reallocate thousands of row buffers.

**Proposal:** retain history rows at their stored width and project/clip/pad them to the current viewport, or normalize history lazily with a bounded migration budget. Keep live PTY dimensions correct immediately. Use in-place resize/moves for live rows where possible. Avoid gratuitous PTY resize syscalls when dimensions remain equal—the code already checks this.

**Tradeoff:** lazy history storage changes assumptions about row widths in search, copy, selection and wide-cell repair; it is not just replacing one `resize` call. Decide whether shrinking irreversibly discards history tails, as current eager resizing does, or whether preserving them is an intentional product change. Maintain tested visible behavior unless that change is explicitly approved.

### 11. Make memory policy explicit before pursuing an exotic representation

**Evidence:** `size_of::<Cell>()` is **48 bytes** on this target; a row `Vec` header is 24 bytes. Scrollback is count-bounded but not byte-bounded, and cells own optional heap extras ([`Cell`](../src/grid/cell.rs#L28-L50), [`Grid`](../src/grid/mod.rs#L16-L48)). The existing [worst-case test](../src/grid/mod.rs#L1037-L1055) deliberately checks inline storage below 1 GiB, not a real total-memory ceiling.

| Width | Inline cells in 10,000 history rows, per tab |
| ---: | ---: |
| 94 columns, approximately default window width | 43.0 MiB |
| 200 columns | 91.6 MiB |
| 1,024 columns | 468.8 MiB |

These exclude row capacity, live/saved-main buffers, snapshot clones, `extra` allocations, search results, fonts/atlas, PTY queues and allocator overhead. Several wide tabs can become a memory-pressure problem well before any per-tab count limit is exceeded. User-configured dimensions/history can exceed defaults.

**Proposal:** define per-tab and possibly global history byte budgets, with explicit UX if fewer than the configured number of lines can be retained. Track capacity/extra bytes, not merely `len × size_of<Cell>`. Bound dimensions by a total-cell/byte budget as well as per-axis limits and the PTY's `u16` range; validate configuration arithmetic.

Then evaluate packed style flags and a smaller hyperlink option encoding; measure actual layout before assuming bit-packing reduces the struct after alignment. Larger options include shared/interned cluster tails and attributes, or a compact immutable history-row representation separate from mutable live cells. These trade allocation/copy savings against refcounting, lookup, hashing and fragmentation costs. Preserve direct truecolor support and safe link-ID lifetimes.

**Expected benefit:** lower retained memory and improved cache locality/multi-tab scalability. Reducing a hypothetical 48-byte cell to 32 bytes would reduce inline-cell storage by one third, but **that layout is an illustration, not an implemented or validated design**.

### 12. Decode the OSC number once and route only the applicable handler

**Evidence:** [`osc_dispatch`](../src/term.rs#L908-L931) calls the title, cwd, hyperlink and clipboard helpers for every OSC. Each helper calls [`osc_parts`](../src/term.rs#L64-L88), which rejoins/copies the payload **before** checking its number. Debug summary generation does the split/copy again when evaluated.

The 1 MiB ignored OSC experiment made four payload copies, despite no handler accepting it and logging being disabled. OSC 8-heavy output pays this overhead on both open and close sequences, independently of URI interning.

**Proposal:** inspect/parse the OSC number without allocating, dispatch once, and let only that handler assemble any needed payload. Reuse one decoded view for logging/application where practical; summaries can stream only the first bounded number of characters rather than rejoin a whole payload. Enforce handler lengths before copies/decoding where possible, while also adding the parser-level bound from #3.

**Validation:** preserve split `;` payloads, OSC 8 parameter/URI separation, malformed/bare title handling, OSC 7 decoding, bounded OSC 52 whitespace/base64 processing, and redaction. Count bytes/allocations with logging both off and at the real default. Do not optimize by removing useful diagnostics or clipboard privacy gates.

### 13. Virtualize/filter the log model, not just the painted rows

**Evidence:** [`show_logs`](../src/renderer/log_ui.rs#L71-L82) locks the ring and calls [`LogBuffer::filtered`](../src/logbuf.rs#L87-L98) every frame. It scans every entry and clones every matching `Arc`; text filtering uses sliding-window case-insensitive comparisons. `show_rows` virtualizes widget creation, not filtering or the snapshot vector.

The 100,000-entry experiment took 4.3–8.7 ms per call. Default capacity is only 1,000 entries, so this ranks below the universal hot paths, but the configured maximum is supported and the work blocks concurrent logging under the mutex.

**Proposal:** give the ring a generation and monotonic entry IDs. Reuse filtered results while generation/filter/level are unchanged, update them incrementally on append/eviction, and acquire only the visible entries for paint. Keep an explicit complete snapshot for Copy when requested. Move expensive filtering outside the lock with a safe snapshot or use a bounded indexing strategy; merely copying all `Arc`s and filtering afterward improves contention but not total work.

Coalesce log notifications while the panel is open. Rate-limit/aggregate [`slow pty drain`](../src/app/pty_io.rs#L115-L119) records, which currently can log on every over-budget drain, so instrumentation does not amplify a slowdown. Keep the separate stderr and in-app directive filters.

### 14. Reuse row storage and move shifted cells rather than deep-cloning

**Evidence:** normal scrolling removes a `Vec` row and allocates a fresh erase row ([`newline`](../src/grid/edit.rs#L274-L305), [`scroll_up`](../src/grid/scroll.rs#L263-L284)). At the history cap, the evicted oldest row is dropped even though its allocation could become the new blank bottom row. Alt-screen/region scrolling also drops rows that could be reset and reused.

The outer `Vec<Vec<Cell>>` shifts **row headers**, not all cell payloads; describing each scroll as a full-grid cell copy would overstate the current problem. The remaining costs are header movement, allocation/fill, dropping extras, and repeated operations for larger counts. ICH/DCH/insert mode clone individual cells and therefore heap extras ([`edit.rs`](../src/grid/edit.rs#L207-L225), [`edit.rs`](../src/grid/edit.rs#L538-L601)).

**Proposal:** recycle an evicted row by filling it with BCE erase cells, rotate region slices in bulk, and use move/rotation-based cell shifts with correct overflow drops. A ring layout for live rows may help tall grids, but should follow profiling because indexed screen access becomes more complicated. Before history reaches its cap, retaining old rows genuinely requires additional storage; row reuse cannot eliminate that growth.

**Validation:** all BCE, scroll-margin, alt-buffer, insert/delete, wide-half repair and link tests. Do not replace BCE fills with structural `blank_cell` fills. Use a bounded free list if recycling cannot be direct; never build an unbounded row pool.

### 15. Compile selection regex once; avoid copy-only cell intermediates

**Evidence:** [`expand_word`](../src/selection.rs#L176-L240) calls [`word_regex_compiled`](../src/config.rs#L430-L437) for every expansion. Word dragging computes both anchor and current word bounds on every motion ([`mouse.rs`](../src/app/mouse.rs#L218-L221)), even if the pointer remains in the same cell. The selection settings page also compiles the pattern every frame ([`settings_ui.rs`](../src/renderer/settings_ui.rs#L734-L758)).

Copy/word helpers clone full grid cells, then allocate cluster strings for `SelCell` intermediates ([`sel_cells_for_global`](../src/app/mouse.rs#L62-L73), [`copy_selection`](../src/app/clipboard.rs#L112-L130)). This is excessive for reading a selected slice of history.

**Proposal:** cache the compiled effective regex and validation result by pattern, preserving fallback for invalid in-progress edits. Cache the pressed anchor's word bounds for the drag and skip repeated same-cell motion. Expose borrowed cluster iteration/shared byte-column mapping so copy appends selected `ch`/`extra` directly into its output without cloning every row or allocating a string for every cell.

**Expected benefit:** a small, low-risk interactive win plus much lower transient allocation traffic for large copies. This is not the first explanation for general typing latency.

### 16. Cache composed labels and query only metadata that is used

**Evidence:** [`tab_titles`](../src/app/tab.rs#L393-L429) formats every title on every frame, including HOME lookup/shortening, template character/token allocations, numeric strings and truncation. The width cache avoids some egui shaping, not this work. `fit_title` can construct/measure successively shorter candidates ([`tabbar.rs`](../src/tabbar.rs#L139-L159)); label length defaults to only 32 characters, limiting normal severity.

[`refresh_title`](../src/app/tab.rs#L116-L139) re-queries foreground pgid and shell cwd after each output burst even if a custom title template references neither. Cwd lookup was only about 2.5 µs locally, so the study does **not** identify it as a dominant bottleneck.

**Proposal:** parse the title template once, record which sources it needs, cache HOME and composed labels, and invalidate only when a referenced source/tab index/config changes. For sustained output, use a bounded metadata refresh interval with prompt/OSC/job-transition triggers and a guaranteed final refresh. Cache fitted text by title/font/available width if traces justify it.

**Correctness:** preserve literal separators, stale/live OSC 7 precedence, shell-vs-job cwd policy, failed-query fallback and post-spawn exec races. Do not reintroduce process syscalls on the render path.

### 17. Debounce persistence and narrow settings invalidation

**Evidence:** every config mutation during a rendered settings frame is applied and saved afterward ([`redraw.rs`](../src/app/redraw.rs#L296-L304)). [`save_to_path`](../src/config.rs#L665-L675) serializes TOML, creates directories, writes and renames synchronously on the UI thread. Dragging a setting can do this for every changed frame.

Also, broad “chrome” changes call [`Renderer::apply_config`](../src/renderer/mod.rs#L429-L442), which clears advance/hash caches even for input/clipboard/link settings that do not affect glyph measurements ([`diff_actions`](../src/app/settings.rs#L158-L182), [`apply_settings_changes`](../src/app/mod.rs#L161-L191)).

**Proposal:** live-apply only relevant state immediately; debounce/coalesce persistence until edits settle, with ordered flush on close/shutdown and visible error reporting. If using a writer thread, prevent an older generation from overwriting a newer config. Preserve atomic temp-file/rename behavior and GUI-wins semantics. Separate font/metrics changes from chrome/input settings so unrelated edits do not flush expensive shaping caches.

**Expected benefit:** removes avoidable filesystem spikes and cache cold starts while editing settings; not a steady-state terminal throughput improvement.

### 18. Keep normal input immediate, but bound bulk input and duplicate mouse work

**Evidence:** PTY input uses an unbounded `Sender<Vec<u8>>`; the writer does `write_all` and `flush` for every message ([`pty.rs`](../src/pty.rs#L124-L147)). Repeated large pastes can queue arbitrary bytes if a child stops reading. A message-count bound alone would not bound their size.

Paste normalization performs three whole-string replacements, then creates another owned byte buffer ([`clipboard.rs`](../src/app/clipboard.rs#L9-L13), [`clipboard.rs`](../src/app/clipboard.rs#L139-L150)). Mouse motion reports are sent for each pixel event even within the same terminal cell, and modifier-held link hover re-detects the row URL on each movement instead of using cached detection ([`mouse.rs`](../src/app/mouse.rs#L157-L188), [`links.rs`](../src/app/links.rs#L57-L75)).

**Proposal:** normalize/wrap bulk paste in one byte-building pass, send it in bounded ordered chunks, and implement a byte-accounted nonblocking input queue/progress policy. Never block the UI on a bounded channel and never silently drop keyboard bytes. Consider draining already queued small writer messages into one write, without adding a timer delay to isolated keystrokes; verify whether `flush` has any real effect on the concrete backend before treating its removal as a performance win.

Deduplicate mouse motion by encoded cell/button/modifier state, preserving transitions and coordinate changes. Reuse cached URL spans for hover and materialize the URI only on click. These are secondary wins; don't sacrifice modifier, IME, bracketed-paste or reporting semantics for fewer tiny allocations.

## 6. Instrumentation and experiments needed before implementation claims

### Improve the existing probes

The existing `key-to-echo`, `echo-to-paint`, slow-frame phase logs, and slow title-query logs are useful leads, but not a complete latency measurement:

- `last_key_write`/`last_echo` are application-global, so unrelated/background output can be attributed to an input event.
- “echo-to-paint” is a CPU-side application milestone, not proof of a scanned-out pixel. Some successful render returns are early exits for surface conditions and do not present a frame.
- [`submit_elapsed`](../src/renderer/mod.rs#L804-L814) excludes `present`, but includes earlier surface acquisition; acquisition is also reported separately. Do not interpret that total as pure CPU work without separating waits.
- Slow rebuild and slow submit share one cooldown, so one category can suppress the other. Threshold logs do not provide distributions or queue-depth information.

Add low-overhead phase counters/histograms and stable tab/input/output generation IDs. Record bytes, expanded operations, dirty rows, shaped rows, snapshot bytes, glyph prepares, uploaded bytes, wake count, frames presented/skipped, per-tab queue occupancy, and drain continuation/service gaps. Aggregate diagnostics periodically; do not log every cell/chunk/keystroke in timing runs.

Keep these measurements distinct: parser CPU, UI-thread active work, channel/OS waits, GPU execution, presentation scheduling, input-to-visible latency, retained memory, allocation traffic, and energy. A fast submission does not establish a fast display update.

### Reproducible workload matrix

Use fixed corpora/seeds, dimensions, fonts, scale, tab counts and logging filters. Repeat on macOS/Metal and Linux; include 1×/2× DPI and 60/120 Hz where available.

| Scenario | Variants | Primary measurements |
| --- | --- | --- |
| Idle | Live bottom; stationary scrollback; hover; steady/hidden cursor; occluded | Frames/wakes per second, CPU time, GPU activity, energy |
| Interactive echo | Isolated keys and key repeat; foreground and background floods | Input-to-visible p50/p95/p99, writer/reader timestamps, service gaps |
| Output throughput | Plain ASCII, CRLF logs, ANSI/truecolor, OSC 8, mixed Unicode | MiB/s, printed cells/s, allocations, UI stalls, queue occupancy |
| TUI updates | One-row spinner, sparse updates, full-screen updates, blocks | Dirty/shaped rows, prepare calls, geometry/upload bytes, CPU/GPU frame time |
| Search | Absent query, mixed case, dense one-char query, streaming edits | Query latency, largest UI work slice, result memory, per-frame projection |
| Scroll/resize | 0/1k/10k history; 94/200/1,024 columns; 1/10 tabs | p95/p99 frame/resize latency, reallocations, retained memory |
| Panels | Default/max log ring; empty/absent filters; settings drags | Lock time, scan time, save count/latency, incidental terminal work |
| Robustness | Large CSI counts, REP, growing clusters, split/unterminated OSC | Maximum callback duration, bounded memory, parser recovery |
| Bulk input | 1/10 MiB paste; repeated paste to nonreading child; mouse flood | Queued bytes, UI responsiveness, ordering, write syscall counts |

Measure optimized builds. The README's plain `cargo run` is an unoptimized development run; observations from it must not be compared directly to release performance.

The existing script can collect manual GUI traces on a machine with a display and full Xcode. For example, **proposed commands, not executed during this study**:

```sh
CARGO_PROFILE_RELEASE_DEBUG=1 ./scripts/profile-macos.sh --template 'Time Profiler' --time-limit 60s --output target/profile/perf-study-cpu.trace
CARGO_PROFILE_RELEASE_DEBUG=1 ./scripts/profile-macos.sh --template Allocations --time-limit 60s --output target/profile/perf-study-alloc.trace
```

Use separate CPU and allocation runs, unique output paths, and controlled interactions. On Linux, collect equivalent CPU, allocation and scheduler traces. Compare default logging with an explicitly quiet run, not just `RUST_LOG=off`: stderr and in-app filters are independent.

### Microbenchmark reconstruction notes

The temporary probe was not added as application code or a permanent benchmark suite. To reproduce its measurements, create a headless release harness with the lockfile's dependencies and import `config`, `theme`, `grid`, `term`, `search`, `links`, `selection`, `logbuf` and `procinfo` from `src/`. For text tests, include production `renderer/text.rs` and `blocks.rs` and provide the renderer fields used by `rebuild_buffer`, without creating a GPU device.

Use one warm-up, seven batches, `std::hint::black_box`, and separately enabled allocation counting. Corpus definitions:

```text
ASCII:       "a" × 262144
CRLF:        ("a" × 78 + "\r\n") × 3277
SGR:         "\x1b[38;2;100;150;200mx\x1b[0m" × 10923
OSC 8:       "\x1b]8;;https://example.com/path\x1b\\x\x1b]8;;\x1b\\" × 6500
Ignored OSC: "\x1b]999;" + "x" × 1048576 + "\x07"
Clusters:    "a" + "\u{0301}" × {1000, 2000, 4000, 8000}
```

Steady feed cases prefilled history with 10,100 CRLFs, then fed chunks of 8,192 bytes. Sparse search history was built with 10,060 rows of `abcdefghij` repeated 20 times, each followed by `Grid::newline`; dense history used 199 `a`s per row. The final row is blank, explaining the exact match counts. Absent query was `missing`. Search batches used 5 iterations, or 2 for maximum-density search; feed/large-CSI/cluster batches used 1; snapshots/hashes used 100–500. Text rows were 200 columns, with single-row edits alternating one character; all-row edits changed one character in each row. Repeated Unicode shaping used 200 cells whose `ch` was `e` and `extra` was a combining acute. Resize used 10,000 blank history rows and alternated widths 200/201, after warm-up. Log tests used 100,000 short debug records and queries `""`/`not-found`.

Add a permanent benchmark target or a small reusable library seam before using these numbers as regression gates. Preserve the raw distributions and source/lockfile revision with future results; CI timing thresholds need runner-noise tolerance. Deterministic tests for frame decisions, work counts, bounds, and invalidation are more reliable CI gates than sub-millisecond wall-clock assertions.

## 7. Correctness and architectural constraints

Every optimization should keep these invariants explicit:

- Pending-wrap behavior, combining/ZWJ/RI handling, wide-cell lead/continuation repair, and no cross-column ligature drift.
- BCE erasure; resize/fresh-buffer structural blanks are not interchangeable with pen-background erase cells.
- Alt/main buffer isolation, scroll-region behavior, bounded history, and correct search/copy line identities after eviction.
- Effective fg/bg after inverse and dim; color conversion must remain linear-light for the sRGB surface.
- Search/selection/link/cursor paint ordering, device-pixel shared edges, and modifier-driven link underlines even when grid text is unchanged.
- The deliberate macOS one-pixel fullscreen surface workaround.
- PTY backpressure, byte ordering, query replies, clipboard request ownership, shell exit ordering, and no UI-thread blocking on transport.
- Clipboard opt-in read policy, OSC 52 redaction, URI allowlisting, and no shell interpolation in openers.
- egui repaint forwarding, focus surrender, terminal-owned Tab routing and IME handoff. Skipping UI work must not resurrect the keyboard/dead-key bugs these paths address.
- Settings precedence/persistence behavior and independent stderr/in-app log filters.

Cache keys should state which inputs invalidate them. “Grid unchanged” alone is not sufficient for viewport, tab identity, metrics, theme, search-current selection, hint modifiers, or GPU-resource lifetimes.

## 8. Suggested implementation sequence and non-priorities

### First tranche: low-risk fixes plus trustworthy baselines

1. Add deterministic regressions for settled scrollbar repainting and large bounded-region counts; fix #1 and the clearly redundant count loops in #3.
2. Establish headless release parser/search/shaping benchmarks and phase counters. Record default-vs-quiet logging separately.
3. Implement #2 with Unicode equivalence coverage; implement #12's single OSC dispatch and #15's regex cache.
4. Apply the small search wins: visible-match slicing and cached projection. Test dense-query memory explicitly.

### Second tranche: reduce changed-frame cost

5. Refactor line construction/cluster measurement (#5), then add actual-text-change reporting and safe retained glyph preparation (#6).
6. Add fair/coalesced PTY scheduling (#8), measuring active-tab p99 under load rather than only total throughput.
7. Use a trace to choose between deeper damage/scroll reuse (#7), background painting (#9), and incremental search (#4). Avoid implementing three overlapping cache systems independently.

### Third tranche: scalability and lifecycle work

8. Coordinate lazy history width handling, byte budgets, and row reuse (#10–11, #14) around shared stable row identity.
9. Address max-size log filtering, settings persistence and input queue bounds (#13, #17–18), then title polish (#16) if measured work remains material.

The relevance ranking and implementation order differ deliberately: a small part of a high-ranked architectural issue can ship before its full redesign, and some lower-ranked fixes are cheap independent wins.

### Do not prioritize without new evidence

- **Replacing wgpu/glyphon/cosmic-text, adding a custom shaper, or parallelizing everything.** Current measurements already expose substantial avoidable application work; complex text and synchronization costs make replacement high-risk.
- **Unbounded PTY queues, larger drain budgets, forced polling, or sleeps to batch keys.** These can trade apparent throughput for memory growth or worse interactive latency.
- **Turning off vsync globally.** Mailbox preference and AutoVsync fallback already exist; fix unnecessary frames before changing presentation policy. Measure acquisition waits separately.
- **Caching the entire rendered terminal into another full-screen texture as the first renderer change.** It adds memory/bandwidth and compositing complexity; retained glyph/geometry work is a smaller first step.
- **Assuming high-performance GPU selection is free.** Startup requests `HighPerformance`; test a lower-power/adapter-choice policy on actual multi-GPU laptops only after repaint loops are fixed. This is an energy experiment, not an established throughput improvement.
- **Unconditionally enabling cosmic-text shape-run caching.** It may help scrolling, but retaining unique log lines can consume memory; evaluate a bounded policy against row-layout reuse rather than stacking caches blindly.
- **Faster hashes, allocator replacement, aggressive inlining, LTO/codegen tuning, or native-CPU flags before algorithmic fixes.** Use controlled release A/B builds later; do not ship host-specific CPU assumptions unintentionally.
- **Optimizing startup icon decoding or rarely used CLI/config parsing ahead of hot paths.** Font discovery, GPU setup and shell spawn should be timed separately if startup latency becomes a requirement; no startup breakdown was measured here.

**Bottom line:** eliminate the unintended frame loop, stop allocating for ordinary characters, and bound UI-thread work first. Then make the rendering/search pipeline truly incremental. These changes have stronger evidence and broader relevance than a graphics-stack rewrite or speculative compiler tuning.
