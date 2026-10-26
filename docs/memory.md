# Memory ownership, budgets, and diagnostics

cometty has bounded history, transport queues, OSC storage, and logs, but **no
application-wide RAM ceiling**. Several tabs and open searches multiply per-tab
storage; graphics/native resources and allocator retention are separate. Most of
this page describes current ownership. The one exception is
[measured footprint](#measured-footprint-one-host), which records measurements
from a single host.

## Owned storage and enforced bounds

| Storage | Current bound / release policy | Outside that bound |
| --- | --- | --- |
| History, per tab | Default 10,000 rows **and** 128 MiB accounted storage; chunks/tails/pools/projections charged; either zero cap releases everything | Live cells, other tabs, search, renderer, allocator overhead |
| Live screen / saved main | Hard 1,048,576 logical cells **per buffer**, shared geometry clamp; at most one saved main screen | Vector high-water capacities, unbounded cluster tails |
| Raw OSC, per terminal | 2 MiB wire limit before accumulation; >64 KiB completion/cancellation releases raw storage | Handler requests, logs, clipboard API/native copies |
| OSC 52 | 1 MiB decoded data and final UTF-8 text | Original clipboard read, encoded reply/transient copies |
| OSC 8 table | 1,024 URIs, 4,096 bytes per URI, 256 KiB unique URI bytes | Index/vector own two URI copies plus container overhead; IDs are not reclaimed on row eviction |
| Output, per PTY | 128 × 8 KiB chunks plus one blocked reader send | Current UI chunk, kernel buffering, parsed state accumulated across chunks |
| Input, per PTY | 16 MiB + 64 KiB reserve; capacity/message charge includes in-flight writes; message count also bounded | Queue container overhead and source clipboard copies |
| Search, per tab | Every non-overlapping match; close releases result capacity >1 MiB plus identities/projection | No open-search result-byte limit; old/new results coexist during refresh |
| Log ring, shared | Count cap; `min(lines × 1 KiB, 16 MiB)` entry-byte budget; messages truncated around 8 KiB | Arc/deque/allocator overhead, pre-truncation formatting, single-oversized-entry exception |
| Advance cache, shared | 4,096 single / 1,024 cluster entries, 128 KiB cluster keys, 512-byte keys | Font-system/native resources, final viewport shaping |
| Config saver, shared | One write + one replaceable pending snapshot + one result | User-configured string sizes, shutdown filesystem wait |

Sources: [`grid/history.rs`](../src/grid/history.rs),
[`grid/history/storage.rs`](../src/grid/history/storage.rs),
[`grid/link.rs`](../src/grid/link.rs),
[`term/parser.rs`](../src/term/parser.rs), [`pty/input.rs`](../src/pty/input.rs),
[`logbuf.rs`](../src/logbuf.rs), and
[`renderer/text.rs`](../src/renderer/text.rs).

History accounting uses retained **capacities**, not just populated lengths.
A partially expired compact chunk stays fully charged; reusable pools and
projection overhead also count. Some container overhead is estimated, so this
budget is not malloc physical footprint. Compact history saves space for ordinary
text but adds bounded packing work and can fall back to dense storage for
high-entropy text/styles. See [terminal model](terminal.md#decision-immutable-compact-history-with-lazy-width-projection).

The renderer and active-view snapshot are shared, not replicated per tab. Active
alternate-screen use retains main history and a saved main screen, but does not
archive alternate output. Closing a tab requests cancellation/reaping, not merely
removal of visible state; see [PTY lifecycle](pty.md).

## Retained capacity is not necessarily a leak

Distinguish these quantities in every result:

- **Allocation traffic:** sum of requested allocation/reallocation bytes over
  time. Quadratic copying can generate gigabytes of traffic while live storage
  stays small.
- **Live requested Rust bytes:** outstanding requested sizes, with peak relative
  to the measured baseline. Excludes allocator rounding/slack and internal
  realloc-copy peaks, thread stacks, native allocations, mmap fonts, and GPU memory.
- **RSS / physical footprint:** OS process metrics, not a heap-size counter.
  Freed heap pages need not immediately return to the OS. Reusing an allocation
  can fault in previously untouched pages without new allocation requests.
- **Native/GPU attributed bytes:** resource accounting, not necessarily resident
  bytes or complete process RAM. Unified/integrated memory must not be counted
  twice when interpreting OS totals.

Reachable high-water storage includes live/snapshot row capacities after shrink,
CPU background scratch, glyph instance buffers, font resources, and glyph atlas
textures. Background destination buffers shrink hysteretically; CPU scratch does
not currently shrink explicitly. `TextAtlas::trim()` unpins entries, not frees
texture storage. Do not clear/shrink everything every frame to improve an RSS
number: allocation churn and latency can worsen without fixing a lifecycle bug.

On a 64-bit layout where a `Cell` is 48 bytes, dense storage alone is roughly
`48 × rows × columns`, excluding cluster tails and vector slack. Verify type
sizes on the target before treating this as a byte contract; compact history
does not use one `Cell` per stored column. Similarly, each background rectangle
currently uses 120 vertex bytes, and decorations can add several rectangles per
cell. Logical cell limits are not renderer-byte limits.

## Measured footprint (one host)

These results come from the display-backed stress run described in
[performance](performance.md#long-session-stress-results-measured) (Apple M3 Pro,
macOS, release build, 2026-10-26). Values are macOS `footprint` for the parent PID.
They describe that host and revision, not a bound.

- **Fixed GPU cost dominates idle.** A fresh 1-tab instance has a 207 MB footprint,
  broken down as:
  - 156 MB "owned physical footprint (graphics)";
  - 22 MB IOSurface (drawables);
  - 14 MB malloc.

  The graphics share does not scale with tabs (161 MB at 10 tabs).
  `MemoryHints::MemoryUsage` made no measurable difference in an A/B run. For
  comparison, iTerm2 on the same host showed 98 MB graphics plus 173 MB IOSurface.
- **Per-tab history is the main multi-tab cost.** These are the deltas for one
  saturated 10,000-row tab:

  | Content | Footprint delta |
  | --- | --- |
  | ASCII | +7 MB |
  | SGR on every cell (dense fallback) | +27 MB |
  | CJK/emoji | +18 MB |
  | OSC 8 | +6 MB |

  Ten tabs of mixed ASCII+SGR reached 455 MB (≈22 MB per tab). After
  saturation, every content type plateaued: repeated batches added 0–3 MB.
- **Retention, not leaks:**
  - **`ESC[3J`:** RSS is not returned to the OS after clearing, but a refill reuses
    it without growth.
  - **Resize churn:** large↔small resizing left IOSurface at 30 MB (from 22 MB)
    after the window returned to small, then plateaued on repeats.
  - **Search:** open/close cycles and a search left open on an inactive tab showed
    no measurable retention. That workload did not reach dense full-history scale.
- **Restore:** replay streams each dump in 64 KiB chunks (see
  [session persistence](architecture.md#session-persistence)), so restore leaves no dump-sized
  allocation behind. With 10 full tabs, the footprint was 368 MB before quit, 361 MB
  after the first restore, and 347 MB after the fifth. A single SGR-heavy tab went
  from 240 MB to 238 MB with no `MALLOC_LARGE` region. Reading a whole dump at once
  instead leaves a dirty, freed `MALLOC_LARGE (empty)` region about the size of
  the dump, and `footprint` counts it.

## Remaining limits and follow-up work

These are **not implemented policies** or promises of future performance:

| Gap | Consequence | Requirement for a future change |
| --- | --- | --- |
| No cross-tab/shared memory budget | Individually bounded tabs/caches can still produce large totals | Account evictable history/caches separately from active/in-flight state; never silently drop input |
| Synchronous, unbudgeted search results | Dense queries allocate millions of matches; rescan temporarily retains two lists; inactive tabs retain open searches | Define count/navigation UI semantics before capping results; consider compact per-row storage and cancellable/budgeted scans |
| Unbounded live cluster tails | Repeated whole-cluster appends cause quadratic copying; tails also amplify snapshot/search/shaping costs | Specify a hard per-cluster byte/scalar and overflow policy while preserving normal combining/ZWJ/wide/pending-wrap behavior |
| Cooperative parser scheduling | REP and history-producing scroll can expand short sequences into long callbacks | Bound or resume expanded work without dropping required terminal semantics |
| Live/snapshot/geometry/atlas high-water storage | Exceptional windows, Unicode, styles, or font/DPI changes can keep large reachable allocations | Add accounting and measured hysteresis/batching; coordinate atlas recreation with prepared-key invalidation/pins |
| Opener child ownership | `links::open` drops the spawned child without an explicit wait | Review Unix reaping separately; PTY supervisor ownership does not cover opener children |
| Unattributed fixed GPU footprint | ~156 MB of graphics memory (≈75% of an idle 1-tab footprint on one Apple-silicon host), independent of tab count | Attribute it with an Instruments Metal/VM Tracker trace before changing atlas, surface, or buffer policy |
| Incomplete whole-process telemetry | History usage and phase counters do not explain the entire process total | Add per-tab capacities/tails/results/queues and shared snapshot/geometry/atlas/lifecycle counters at existing work boundaries |

Compaction can retain **more history rows** within the same byte budget than dense
storage, so a wide dense search can now have more results, not fewer. Smaller
history storage is not a search-memory policy.

The former raw-OSC high-water and closed-PTY ownership defects are addressed by
the current parser guard and supervised cancellation. The failed-frame boundary
addresses never-submitted uploads on a live device. None proves an unrelated
native/driver leak absent; submission does not bound an indefinitely stalled GPU.
Do not reuse old dense-history tables or repaired findings as current behavior.

## Reproducing a long-session problem

1. Record exact binary/source revision, lockfile, config, OS/GPU, font/DPI,
   viewport dimensions, tab/search counts, and terminal-output workload. A package
   version alone does not identify the code. Inspect all tabs, not just the active one.
2. Track the cometty **parent PID separately** from shells/jobs; their RAM is not
   parent heap. Worker threads and their retained messages do belong to cometty.
3. Sample before work, after warm-up, after repeated fixed-size batches, at idle,
   and after releasing the relevant state. Look for plateaus versus continuing
   live growth and exceptional high-water steps.
4. Compare default logging with **both sinks off**: `RUST_LOG=off` alone leaves
   in-app recording enabled; use `[log] filter = "off"` too.
5. On macOS use Instruments Allocations/VM Tracker/Metal tools plus Activity
   Monitor, `vmmap -summary PID`, and `footprint PID` where available. On Linux
   use heaptrack/Massif and process/resource metrics. An unreachable-allocation
   leak scan can miss a reachable parser, thread, cache, or pending queue.

| Controlled workload | Distinction to investigate |
| --- | --- |
| Fixed-width ASCII far past history saturation, search closed | Steady-state storage/recycling plateau |
| Same work in multiple tabs | Expected per-tab multiplication versus single-tab growth |
| Dense search, refresh, close, switch away | Result capacity, double-vector peak, inactive-tab retention |
| Repeated terminated OSC, overflow/recovery | Small-buffer reuse, exceptional-buffer release, bounded discard |
| Repeated raw/nonreading tab close | Child/thread/descriptor cleanup after visible count returns to baseline |
| Colored output with injected surface/prepare failures | Upload submission/recovery versus stable grid/atlas sizes |
| Large-to-small resize, font/DPI churn, plain prompt | Reachable row/snapshot/geometry/atlas peaks |

Use committed [headless probes](performance.md) before inventing external test
programs. Never run unbounded/unterminated payload generators in an important
session. Heap traces, output recordings, paths, and clipboard captures can contain
secrets; redact before sharing. Headless graphics attempts could not create a
working device, so they established **no native/RSS plateau or display-backed
latency result**. The single-host plateaus above come from a display-backed run.
Record GPU-probe skips/failures explicitly.
