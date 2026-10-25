# Cometty memory-consumption and leak study

- **Date:** 2026-10-25
- **Audited revision:** `e06f6211caaad67a08c5476b3af523d52151ecc6`
- **Scope:** retained memory, allocation peaks, ownership/lifecycle defects, and plausible explanations for multi-gigabyte sessions.
- **Changes:** this report only; no application behavior or dependency changes.

> **Follow-up status:** The audit and its measurements below describe the historical
> `e06f621` baseline, not the current implementation. Batch 1 repaired PTY lifecycle
> ownership in `9c097cd`. Batch 2 addresses Finding B with bounded OSC parsing and
> handler-side allocation limits; see [the follow-up results](#14-follow-up-batch-2-osc-bounds).
> GPU staging, whole-application budgets, search, cluster tails, and renderer
> high-water storage are deliberately not changed by batch 2.

## Executive summary

**There are reproducible memory-retention defects, but this investigation does not identify which one caused the reported long-session RAM usage.** It also establishes that gigabyte-scale usage can result from the aggregate of individually bounded tabs, without a conventional unreachable-allocation leak.

The most important findings are:

1. **Closed tabs can leave PTY workers, descriptors, children, and input buffers alive.** Six dropped sessions with blocked 4 MiB writes retained **24 MiB**, six extra threads, and six extra descriptors. They remained after a two-second wait and disappeared when the probe terminated its children. Even without queued input, six raw-mode idle children left six reader threads/descriptors behind. This is a genuine lifecycle defect, not normal scrollback retention.
2. **The OSC parser has no raw-string memory limit.** A 32 MiB unsupported OSC, fed in ordinary 8 KiB chunks, caused a **64 MiB parser allocation**. Terminating the sequence, sending a small title update, clearing history, and issuing a terminal reset did not release it. Dropping the terminal did. An unterminated OSC can keep growing across arbitrarily many bounded PTY reads.
3. **Failed rendering can accumulate unsubmitted GPU uploads.** Background uploads and egui texture uploads happen before surface acquisition; several surface-failure paths return without submitting them. wgpu retains each staging allocation until submission/completion. This is a source-confirmed conditional growth path, but **not a reproduced GPU/RSS result**: both normal and downlevel offscreen device creation failed in this environment.
4. **Normal per-tab storage is expensive and there is no application-wide budget.** At 200 columns, a full default history plus a 60-row screen retained about **93 MiB per terminal**. A dense search added **48 MiB**, with another result vector temporarily alive during rescan. Wider tabs reached the 128 MiB history budget and searches added **96 MiB**. Multiple tabs multiply those amounts.
5. **Live grapheme-cluster tails are unbounded.** They also produce quadratic allocation traffic. A 32 KiB combining tail caused **1.31 GiB of cumulative allocation requests**, but only about **138 KiB peak tracked live allocation** in the small parser-only probe. Allocation traffic must not be mistaken for retained RAM.

Conversely, the ordinary scrolling and CPU text-layout probes did **not** show continuing retained-heap growth: scrolling plateaued through 100,000 lines at each tested width; 1,200 full text-layout replacements stayed near 4.27 MiB of live Rust allocations. These tests exclude the GUI/GPU rendering lifecycle.

**Recommended order:** repair PTY shutdown and raw OSC limits; fault-test and repair the pre-submit rendering exits; then add whole-application memory accounting and reduce dense-search/rendering high-water storage.

## 1. Method, evidence, and limitations

### What was examined

The audit followed ownership from PTY reads through parsing, live/alternate grids, history, selection/search/link projections, the shared viewport snapshot, text shaping, background geometry, egui, and GPU submission. It also checked logging, configuration persistence, global caches, tab removal, and subprocess ownership.

Relevant locked dependencies were inspected locally, not inferred from their names:

| Component | Locked version | Relevant detail |
| --- | --- | --- |
| vte | 0.15.0 | Enabled `std` feature uses a growable OSC `Vec`, not the apparent 1,024-byte fixed buffer |
| cosmic-text | 0.19.0 | Optional `shape-run-cache` is **not enabled** |
| glyphon | 0.12.0 | Uses uncached Swash rasterization and its own atlas cache |
| wgpu / wgpu-core / wgpu-hal | 30.0.1 | Queue writes allocate staging resources before submission |
| egui / egui-winit / egui-wgpu / epaint | 0.36.1 | Frame-aged galley cache; explicit texture-delta handling |
| portable-pty | 0.9.0 | Unix reader/writer handles clone the master descriptor; child is a `std::process::Child` |

Feature checks used `cargo tree -e features -i vte` and `cargo tree -e features -i cosmic-text`. The lockfile SHA-256 was:

```text
3aa2ddec3e8f3fbbd7372b09aeb5778dfbb45ee22ea7701bd3f8154a73f7c048
```

### Measurement approach

- Intel MacBookPro16,2, quad-core 2 GHz Core i5, 16 GB RAM, macOS 26.7.1.
- Rust 1.96.1, `x86_64-apple-darwin`, standard optimized Cargo release profile.
- A temporary external Cargo binary imported the production modules. PTY source files were copied unchanged into a module directory to preserve Rust's nested-module resolution. Dependencies and lockfile matched the application.
- An allocator wrapper delegated to `std::alloc::System`, counting successful Rust allocation sizes and subtracting deallocations. Reallocation adjusted the live count by the size difference and added the new requested size to allocation traffic. The wrapper used allocation-free atomics and covered worker threads too.
- **Live** means outstanding requested Rust allocation bytes; **peak** means the maximum of that counter; **traffic** means cumulative requested allocation/reallocation bytes. These exclude allocator size-class slack, internal `realloc` copying peaks, direct native allocations, mmap-backed fonts, thread stacks, and GPU allocations.
- RSS was sampled from the probe's own `MACH_TASK_BASIC_INFO`. It is not macOS Activity Monitor's complete physical-footprint accounting and is not a heap-size measurement.
- Live-heap tables subtract the small pre-case Rust baseline. Counts may include small driver/configuration strings. In the search probe, the driving row string remains alive through the final terminal-drop sample; that explains its small residual allocation.
- Different probe modes ran in separate processes. Widths within the history/search modes ran sequentially, so their later RSS readings include allocator history from earlier widths. These are deterministic mechanism demonstrations, not controlled production RSS benchmarks.
- No application logger was installed in the parser/layout probes. The log-ring probe exercised the production ring directly. This isolates ownership and does not measure full default-logging overhead.

Evidence terminology below:

- **Measured:** executed against production functions in the probe.
- **Observed:** follows from application and locked dependency source.
- **Conditional/unverified:** the triggering production condition or end-to-end RAM impact still needs reproduction.

### Important limits

No GUI window was launched and no existing long-running cometty process was attached. Process enumeration was unavailable in the harness (`sysmond service not found`). An offscreen attempt found the Intel Iris Plus Metal adapter, but both device requests failed with `Parent device is lost`; consequently there are **no GPU staging/atlas measurements** here.

The report therefore cannot quantify a native Metal/AppKit/driver leak, reproduce an hours-long interactive workload, or certify that all native allocations are freed. A plateau in a finite CPU probe is evidence against a particular growth mechanism, not proof of absence of all leaks.

Existing checks passed:

| Check | Result |
| --- | --- |
| `cargo test --locked` | 381 passed, 3 intentionally ignored, 0 failures |
| `cargo clippy --locked -- -D warnings` | Passed |
| `cargo fmt --check` | Passed |

## 2. Memory ownership and existing limits

### Ownership map

```text
App
├── tabs[]                                      no application-wide tab/memory ceiling
│   ├── Terminal
│   │   ├── Grid: live rows + optional saved main rows
│   │   ├── bounded history + width projections + bounded OSC 8 table
│   │   ├── live Cell::extra cluster tails       no byte ceiling
│   │   └── vte parser OSC storage               no byte ceiling; retains capacity
│   ├── PtySession
│   │   ├── bounded output channel
│   │   ├── bounded input queue + writer-owned in-flight message
│   │   └── detached reader/writer threads       can outlive tab removal
│   └── search matches + row identities + visible link/search caches
├── one shared viewport snapshot
├── one shared Renderer
│   ├── FontSystem + Buffer + bounded advance caches
│   ├── CPU background scratch + GPU buffers/atlases
│   ├── egui state/font resources
│   └── queue staging allocations               await submission/completion
└── bounded log ring + incremental log-ID index + bounded config-save worker
```

The renderer and viewport snapshot are shared, **not duplicated for every tab**. Each tab does have its own terminal, history, transport queues, and search state.

### Budget inventory

| Storage | Current policy | What the policy does not bound |
| --- | --- | --- |
| History, per tab | Default 10,000 rows **and** 128 MiB accounted storage; zero disables history | Other tabs, live cells, parser storage, search, renderer, allocator overhead |
| Live grid, per buffer | Default 1,024 per axis; hard 1,048,576 logical cells and PTY dimension clamp | Vector high-water capacities and variable-length cluster tails |
| Alternate screen | At most one saved main screen; active alternate screen does not add history | Main history remains resident; both live buffers can contain cluster tails |
| PTY output, per tab | 128 × 8,192-byte chunks, plus one blocked reader send | The current UI chunk, kernel buffering, parsed state accumulated across chunks |
| PTY input, per session | 16 MiB plus 64 KiB reserved headroom; allocation capacity/message cost charged through in-flight writes | Queue spare capacity, external clipboard copies, orphaned sessions accumulating after tab close |
| Search | All non-overlapping matches; 24 bytes per `Match` before vector slack | No result-byte budget; old and new vectors coexist while recomputing |
| Search close | Releases result allocations above 1 MiB; drops row IDs/projection | Open search panels in inactive tabs still retain their results |
| OSC 8 links | 1,024 URIs, 4,096 bytes each, 256 KiB unique URI bytes | URI bytes are owned twice, in the vector and index; raw parser input is separate |
| In-app log ring | Default 1,000 entries/~0.98 MiB accounted budget; derived maximum 16 MiB; messages truncated near 8 KiB | Arc/deque/allocator overhead, transient formatting/copying, the oversized-single-entry exception |
| Advance cache | 4,096 single-character entries; 1,024 cluster entries/128 KiB cluster-key bytes; oversized keys not retained | Font-system/native resources and shaped viewport storage |
| Background geometry | GPU buffer grows and shrinks with hysteresis | CPU scratch never explicitly shrinks; no independent geometry byte budget |
| Glyph atlas | Texture dimension limited by device; allocation-pressure eviction | No application byte budget or texture shrink; `trim()` only unpins entries |
| Configuration saves | One in-flight write, one replaceable pending snapshot, one result | User-supplied configuration strings themselves are not generally byte-capped |

The history budget includes row cell **capacities**, cluster-tail lengths, deque capacity, and projection accounting. Projection/B-tree overhead is estimated, and neither history nor log accounting is an exact measurement of malloc's physical footprint. Their configured ceilings are not process-RAM ceilings.

## 3. How much memory is normal?

### Cell density matters more than the amount of visible text

Measured 64-bit type sizes:

| Type | Bytes | Interpretation |
| --- | ---: | --- |
| `Cell` | 48 | Inline record even for an ASCII space; optional cluster tail is additional |
| `Row` | 40 | Identity/revision plus vector header; excludes cell allocation |
| `search::Match` | 24 | Three `usize` values |
| `Grid` | 632 | Header/state only, not rows/history |
| `Terminal` | 1,184 | Header/state only, not owned heap allocations |

An 80-column row therefore needs 3,840 bytes of cell storage, and a 200-column row needs 9,600 bytes, **even if it contains only one character**. There is no sparse or text-compressed representation of blank history tails.

For ordinary cells without extra cluster data:

```text
live cell payload          ≈ 48 × columns × rows
history cell payload       ≈ 48 × stored columns × history rows
shared snapshot payload    ≈ 48 × active columns × active rows
```

Use vector capacities, not just lengths, when estimating actual retention. History rows keep their original buffers after a width change; lazy projections change what can be viewed, not the stored allocation size.

### Measured scrollback plateau

Each case used 60 live rows, default limits, and 100,000 repetitions of `x\r\n`, sampled every 20,000 lines. Thus the result also represents mostly blank rows, not just dense text.

| Columns | Retained history rows | Accounted history bytes | Terminal's live Rust heap | Growth between 20,000 and 100,000 emitted lines |
| ---: | ---: | ---: | ---: | --- |
| 80 | 10,000 | 39,317,672 | 37.72 MiB | None |
| 200 | 10,000 | 96,917,672 | 92.98 MiB | None |
| 400 | 6,966 | 134,206,120 | 129.09 MiB | None |
| 1,024 | 2,725 | 134,168,744 | 130.77 MiB | None |

After the first sample, cumulative requested allocation bytes also stopped increasing: the row-recycling path did not allocate on each additional line. Dropping each terminal returned tracked live allocations to its pre-case baseline.

**Conclusion:** default history is large but its current implementation did not leak under this sustained-output test. Clearing history releases its owned allocations, although RSS need not immediately fall.

Relevant code: [`src/config.rs:40–54`](../src/config.rs#L40-L54), [`src/grid/row.rs:37–55`](../src/grid/row.rs#L37-L55), [`src/grid/history.rs:176–272`](../src/grid/history.rs#L176-L272).

### Aggregate tabs can legitimately reach gigabytes

These are arithmetic projections from the measured terminal heaps, **not measured multi-tab process RSS**:

- Twelve full 200-column terminals: about **1.09 GiB**, before rendering, queues, search, and allocator overhead.
- Sixteen history-budget-limited wide tabs: about **2 GiB of history alone**.
- Eight full 200-column tabs retaining dense searches: about **1.10 GiB** of terminal/search allocations, before shared renderer/native resources.
- The settings UI permits a **1 GiB history budget per tab**. The file loader does not impose that UI range as a hard ceiling. With enough permitted history rows and output, user configuration can exceed the defaults substantially.

History limits were added in the audited revision. Confirm that a reported installed/running binary actually includes this revision; version `0.1.0` alone does not identify its commit. Older findings from [`perf_study.md`](perf_study.md) must not be assumed to describe the current, subsequently bounded implementation.

## 4. Finding A — closed tabs retain PTY resources

**Priority: high. Evidence: measured lifecycle failure.**

### Mechanism

[`App::close_tab`](../src/app/tab.rs#L648-L669) removes the tab and drops its `PtySession`. But:

- [`PtySession`](../src/pty.rs#L122-L133) stores the child as `_child`, with no `Drop` implementation, kill operation, or wait/reap operation.
- [`spawn_with_size`](../src/pty.rs#L196-L230) starts two detached threads, each holding a cloned master-side handle. Dropping the UI's master does not close those copies.
- Dropping [`InputSender`](../src/pty/input.rs#L169-L177) only marks the queue closed and signals its condition variable. It does **not** interrupt an ongoing `write_all`.
- The writer's [`Message`](../src/pty/input.rs#L180-L199) retains its complete original allocation, including bytes already written, until the transaction finishes. [`InputReceiver::run`](../src/pty/input.rs#L202-L234) cannot observe closure while blocked in the OS write.
- An idle reader blocked in `read` likewise cannot observe the dropped output receiver until another read completes and it tries to send.

The per-session queue budget is therefore insufficient: memory and resources can accumulate with the number of **previously closed sessions**, not just currently open tabs.

portable-pty's Unix writer drop attempts newline/EOT; that is not a reliable cancellation mechanism for raw-mode programs or a currently blocked writer. A retained master handle can also prevent the normal last-master-close hangup semantics.

### Reproduction results

The probe created six sessions running `/bin/sleep 60`, queued one 4 MiB paste per session, then dropped every session. The child does not consume terminal input, so writing blocks after kernel buffering fills.

| State | Extra live Rust heap | Process threads | Open descriptors in probe |
| --- | ---: | ---: | ---: |
| Before sessions | Baseline | 1 | 4 |
| Six sessions dropped | 24.003 MiB | 7 | 10 |
| After another two seconds | Same 24.003 MiB | Still retained | Still retained |
| After terminating/reaping the six probe children | ~110 bytes of probe/config state | 1 | 4 |

The extra threads in this case are the blocked writers. There was no `Tab`, `Terminal`, or UI-owned `PtySession` left to account for those buffers.

A second case ran six children with raw mode and echo disabled, waited for `READY`, then dropped the sessions **without sending a paste**. All six children remained alive; threads/descriptors again rose from **1/4 to 7/10**. This case retains readers, small Rust state, and native thread resources rather than large input buffers. Killing/reaping the probe children restored **1/4**.

These measurements demonstrate retention beyond tab ownership, not that a finite `sleep 60` runs forever. A child that never exits or makes progress can keep the resources indefinitely. The probe cleaned up only its own children.

### Related process-reaping defect

Six naturally exiting `/usr/bin/true` sessions delivered `PtyEvent::Exit` and were dropped. **All six were still waitable by the probe's subsequent `waitpid(..., WNOHANG)`**. The application never reaps them itself. That leaves Unix zombie process entries, not gigabytes of cometty heap, but confirms incomplete child lifecycle management.

[`links::open`](../src/links.rs#L387-L398) similarly drops a spawned opener child without waiting; it warrants the same Unix reaping review. This opener path was inspected, not executed.

### Recommended repair and tests

- Give a session explicit shutdown ownership: child termination/hangup policy, cancellable I/O, and worker completion tracking.
- Cancel/release queued input on close, and make blocked reader/writer operations interruptible. An atomic stop flag alone cannot wake a blocking OS call.
- Ensure every spawned child is reaped, including natural exits and partially failed session creation.
- Do not join potentially blocked workers on the UI thread or silently change bracketed-paste FIFO behavior while a session is open.
- Define how closing a tab treats its own shell/session/foreground job and descendants; do not rely on dropping the child wrapper to signal them.
- Regression-test repeated close with an idle raw child and with saturated input: threads, descriptors, child count, and retained byte counters must return to baseline after bounded shutdown.

## 5. Finding B — unbounded raw OSC accumulation and persistent capacity

**Priority: high. Evidence: measured and dependency-source confirmed.**

### Why existing limits do not protect this allocation

[`Terminal::feed`](../src/term.rs#L516-L522) preserves its vte parser across chunks. In vte 0.15.0 with `std`, `Parser::osc_raw` is a `Vec<u8>`:

- `action_osc_put` pushes bytes without a size check.
- The apparent `MAX_OSC_RAW = 1024` applies to the non-`std` fixed-capacity implementation only.
- `osc_end` calls `osc_raw.clear()`, not a capacity-releasing operation.

The application limits title characters, interned hyperlink bytes, and decoded clipboard bytes **after parsing the complete OSC**. Unsupported OSCs also pass through the same raw buffer. Neither output-channel backpressure nor the drain byte budget bounds parser state accumulated over many drains.

### Measured example

The probe sent `ESC ] 999 ;`, then reusable 8,192-byte `x` chunks, then BEL. It did not allocate a giant input corpus in the parent.

| Accumulated OSC payload | Extra live Rust heap including the small terminal |
| ---: | ---: |
| 8 MiB | 16.09 MiB |
| 16 MiB | 32.09 MiB |
| 24 MiB | 32.09 MiB |
| 32 MiB | 64.09 MiB |
| After BEL terminator | 64.09 MiB |
| After small title OSC + clear-history + `ESC c` reset | 64.09 MiB |
| After dropping the terminal | Back to baseline |

The prefix consumes bytes too, so an exact power-of-two payload can cross the next vector-growth boundary. The 64 MiB figure is requested allocation capacity, **not 64 MiB of populated OSC data or measured resident pages**. RSS at sequence completion was about 37 MiB; unused vector capacity need not be resident.

Repeating the same-sized terminated OSC normally reuses this allocation rather than adding another one. The risks are an indefinitely unterminated sequence, increasing high-water payload sizes, and multiple tabs independently retaining their largest parser buffer.

### Transient amplification after dispatch

For supported OSCs, application handlers can add full-payload copies before rejecting/truncating them:

- [`title_payload`](../src/term.rs#L123-L130) constructs cleaned/trimmed strings before keeping the configured number of characters.
- [`cwd_payload`](../src/term.rs#L137-L170) decodes the complete path before truncation.
- [`hyperlink_payload`](../src/term.rs#L178-L186) allocates the URI before the grid's 4 KiB URI check.
- [`clipboard_payload`](../src/term.rs#L195-L254) bounds decoded input, but joins fragments and performs filtering/decoding first. Lossy UTF-8 conversion can enlarge invalid decoded text beyond the decoded-byte count.

Current ignored-OSC routing and debug previews are much better: they no longer copy the entire ignored payload into each handler or log. That optimization does **not** cap vte's raw buffer.

### Recommended repair and tests

Apply an explicit raw control-string bound **before accumulation exceeds it**, via a parser change or a carefully parser-aware bounded integration. On overflow, discard through the appropriate terminator/cancellation and resume normal parsing correctly. Preserve chunk-split and UTF-8 behavior, and allow a deliberate policy for legitimate OSC 52 sizes.

Also release unusually large completed raw buffers, and avoid whole-payload transformations before per-handler validation/truncation. Merely resetting the parser after dispatch would release high-water capacity but would **not** stop an unterminated sequence from exhausting memory.

Test oversized supported/unsupported/unterminated OSCs at every important chunk boundary; assert bounded capacity and correct processing of the text following the terminator. Turning `[clipboard] osc52` off is not a workaround for raw OSC accumulation: policy is enforced later.

## 6. Finding C — failed frames can retain unsubmitted staging allocations

**Priority: high to verify/fix. Evidence: observed growth mechanism; production trigger and RAM impact unmeasured.**

The relevant ordering in [`Renderer::render`](../src/renderer/mod.rs#L719-L778) is:

```text
paint_bg -> queue.write_buffer(...)
paint_overlay -> egui texture updates -> queue.write_texture(...)
surface.get_current_texture()
    Timeout / Occluded / Lost / Outdated / Validation -> early return
...
text preparation / encode -> queue.submit(...) -> present
```

[`paint_bg`](../src/renderer/background.rs#L339-L357) uploads nonempty geometry on every attempted frame, including text-identical frames. [`paint_overlay`](../src/renderer/overlay.rs#L570-L580) applies texture changes before acquisition. Several failures therefore return after scheduling writes but before the submission at [`src/renderer/mod.rs:884`](../src/renderer/mod.rs#L884).

wgpu 30.0.1's `Queue::write_buffer` documentation explicitly states that data is immediately copied into newly allocated native staging memory and released after the next submission finishes. In wgpu-core, `PendingWrites::temp_resources` owns these staging buffers until submission; subsequent writes to the same destination do not replace earlier staging allocations. Buffer mapping can also flush pending writes internally, but this application path does not map its background buffer.

**Conditional consequence:** repeatedly attempting a colored frame while surface acquisition keeps failing can grow memory with each failed attempt, even though the live grid and destination GPU buffer are unchanged. Preparation/encoding errors after uploads deserve the same audit. Successful later submission/completion can release the backlog, so this is a missing-progress/unbounded-backlog defect rather than necessarily permanently unreachable memory.

Scale estimate, not a measurement: one colored background quad uses six 20-byte vertices, or **120 bytes per cell**. A 200 × 60 colored screen uploads about **1.37 MiB per attempt**. One unsuccessful attempt per second for an hour represents roughly **4.83 GiB of staging payload** if nothing submits it. A plain default-background screen uploads far less, often just cursor geometry. Actual native allocation, padding, command storage, and resident footprint are backend-dependent.

The application already skips drawing when its own occluded/zero-sized state says visuals are suspended. That reduces exposure, but does not eliminate backend timeouts, device/surface errors, or disagreement with the OS visibility state. Retry backoff limits the rate, not the accumulated bytes.

### Recommended repair and validation

- Keep UI state/repaint handling independent, but defer GPU uploads until a frame has been acquired successfully where possible.
- Ensure any remaining early exit after queue writes has an explicit bounded submission/cleanup policy. On a live device, an empty queue submission can flush writes; simply polling completion cannot execute never-submitted work. Device-loss paths need separate handling.
- Inject thousands of consecutive acquisition failures on a working backend, with a colored screen and egui texture updates. Track staging bytes/resource counts, native allocations, and physical footprint. Verify a plateau and recovery on the next successful frame.
- Test preparation failure as well as surface acquisition failure; preserve atlas pinning and retained-text correctness.

An offscreen write-only probe was prepared, but could not create a device, even with the application's downlevel fallback. **No measured GPU growth rate or driver-leak claim is made.**

## 7. Finding D — search is bounded by retained text, not by a memory budget

**Priority: medium for normal RAM consumption. Evidence: measured.**

[`find_matches`](../src/search.rs#L59-L161) appends every match to a geometrically growing `Vec<Match>`. [`SearchPanel::refresh`](../src/app/search.rs#L148-L155) evaluates a complete replacement before dropping its old vector. Dense one-character queries can therefore require much more storage than the compact visible highlight projection suggests.

The probe filled history with rows of `columns - 1` copies of `a`, opened search for `a`, then explicitly invalidated and rescanned it.

| Columns × live rows | Matches | Result capacity bytes | Heap before search | Heap with search | Peak through one rescan |
| --- | ---: | ---: | ---: | ---: | ---: |
| 200 × 60 | 2,001,741 | 50,331,648 (48 MiB) | 92.98 MiB | 141.21 MiB | 189.22 MiB |
| 1,024 × 60 | 2,848,032 | 100,663,296 (96 MiB) | 130.77 MiB | 226.83 MiB | 322.87 MiB |

Additional retention includes the row-identity array and visible projection. Result capacities above are inferred exactly from the normal `Vec` growth steps and measured allocation increase; the public panel interface exposes the match count, not its private vector capacity.

Closing search returned live allocations essentially to pre-search levels. The current code already releases large result capacities on close; the older closed-search retention issue is **not still present**. However:

- Switching away from a tab does not close its search panel or discard its result list.
- Background refreshes allocate another full result list before replacing the previous one.
- History's byte limit does not include search results.
- Multiple hits inside one grapheme tail can map to the same cell range. Once unbounded cluster text is considered, match count is not bounded by the number of cells alone.

**Recommendations:** introduce an explicit result-memory policy, compact/group matches by row, and reuse or incrementally replace storage without keeping two maximal lists. Any result cap must be visible to users and preserve defined navigation/count semantics. Do not solve this by silently dropping arbitrary matches or cloning the entire grid into an unbounded worker backlog.

## 8. Finding E — unbounded cluster tails and allocation amplification

**Priority: high for input robustness; medium as an explanation for ordinary long sessions. Evidence: measured.**

[`Cell::extra`](../src/grid/cell.rs#L28-L50) is an optional owned `Box<str>`. [`append_to_cell` / `append_to_prev`](../src/grid/edit.rs#L71-L138) repeatedly rebuild the whole cluster and its tail. There is no maximum byte/codepoint count for a live cluster. History accounting only constrains it after its row enters history.

The parser-only probe used a 2 × 1 terminal containing `a` followed by 16,384 combining acute accents, supplied in four batches of 4,096. Results:

| Quantity | Measured value |
| --- | ---: |
| Final cluster tail | 32,768 bytes |
| History bytes | 0 |
| Final extra live Rust heap | ~32 KiB plus small row state |
| Peak total tracked live allocation | 141,151 bytes (~138 KiB) |
| Cumulative allocation requests | 1,409,376,662 bytes (~1.31 GiB) |
| After erasing the cell / dropping terminal | Tail released / baseline restored |

This is **not a 1.31 GiB retained-memory leak**. It is quadratic allocation/copy work plus an uncapped live-text representation. A much larger tail would increase live storage, viewport-snapshot copies, search haystacks, and shaping work; rendering was deliberately excluded from this probe.

**Recommendations:** specify and enforce a per-cluster byte/scalar limit and a predictable overflow policy. Consider an append-efficient representation, but retain a hard bound even if appends become cheap. Preserve combining/ZWJ/wide-cell semantics for valid ordinary clusters and test overflow while pending wrap is active.

## 9. Rendering and resize high-water storage

### Live row capacities survive shrinking

[`resize_live_rows`](../src/grid/mod.rs#L319-L325) calls `Vec::resize`, which truncates elements but does not shrink capacity. In a measured 800 × 200 → 80 × 200 resize:

- Every surviving row changed from length 800 to length 80 while retaining **capacity 800**.
- The live heap stayed at **7.33 MiB**, not the roughly 0.74 MiB suggested by the new dimensions.
- Entering an alternate screen added about 0.74 MiB; exiting released that alternate allocation rather than accumulating another screen on each transition.

Rows archived after shrinking are charged by their actual capacities. Recycling drops an exceptionally wide buffer when it exceeds twice the new column count, but rows still live or retained in history can remain wide. Lazy history projections deliberately preserve stored buffers; this is not a per-resize history-copy leak.

The shared snapshot in [`src/app/redraw.rs:82–109`](../src/app/redraw.rs#L82-L109) similarly truncates/reuses vectors without generally shrinking their column capacity. Its cell tails are owned copies. This can retain a previous large viewport's capacity across tabs, but it is one shared snapshot, not an accumulating snapshot per frame.

**Recommendation:** use hysteretic capacity reduction for substantially oversized live/snapshot rows, while preserving the history projection/recycling design. Account retained capacities when applying `max_cells`; that limit currently bounds logical cells, not all retained bytes.

### Background geometry can be larger than grid storage

`BgVertex` is 20 bytes and every rectangle emits six vertices. Consequently:

- One rectangle per cell at the maximum 1,048,576-cell grid is **120 MiB** of vertex payload.
- A cell can emit seven rectangles: background, two block-element rectangles, double underline, strike, and overline. That is **840 MiB** of payload at the logical grid ceiling, before vector rounding, GPU capacity, staging, and the grid itself.
- These are extreme arithmetic cases, **not measured ordinary workloads**. The default font/window does not produce a million-cell viewport.

[`next_bg_capacity`](../src/renderer/background.rs#L49-L59) shrinks the GPU destination below quarter utilization, but `bg_scratch.clear()` keeps the CPU vector's maximum capacity. Larger geometry can therefore leave a long-lived CPU high-water allocation after the screen changes back. glyphon's instance vector/GPU instance buffer are also high-water storage.

**Recommendations:** give geometry its own byte ceiling or stream/batch it; shrink CPU scratch hysteretically too. The logical cell cap alone is not a sufficient renderer-memory budget. Retest block/decoration output and queue writes after any change.

### Glyph atlas: eviction is not shrinking

Locked glyphon starts with separate 256 × 256 mask/color atlases, uses allocation-pressure LRU eviction, and grows dimensions when it cannot allocate around glyphs in use. **`TextAtlas::trim()` only clears the in-use set.** It does not shrink textures or immediately purge all cached metadata.

Raw texture payload estimates for equal atlas dimensions:

| Atlas dimensions | Mask, one byte/pixel | Color, four bytes/pixel | Combined |
| --- | ---: | ---: | ---: |
| 2,048² | 4 MiB | 16 MiB | 20 MiB |
| 4,096² | 16 MiB | 64 MiB | 80 MiB |
| 8,192² | 64 MiB | 256 MiB | 320 MiB |

The actual maximum comes from the requested/granted device limits; normal content need not grow either atlas this far. These figures exclude alignment and temporary old/new resources, and are not measured GPU residency. On integrated/unified-memory hardware, native graphics memory contributes to RAM pressure; avoid double-counting it when interpreting OS totals.

The atlas LRU is constructed without an entry-count ceiling, although sized entries are constrained/evicted by texture allocation pressure. Zero-size/skip-rasterization entries need allocation pressure to be purged, so a dimension cap is not a complete metadata-byte cap either. This is a secondary observed issue, not a reproduced gigabyte cause.

**Recommendation:** instrument atlas dimensions/entry counts and resource bytes. Consider budgeted atlas recreation after exceptional peaks, coordinating it with prepared-text invalidation and current glyph pins. Do not call `trim()` on retained-preparation reuse frames as an attempted memory fix; that can invalidate live glyph placement assumptions.

## 10. Paths checked that did not establish an unbounded leak

### CPU text layout and font caches

A headless probe repeatedly replaced and laid out all 60 rows of a 200-column ASCII viewport using production line construction and cosmic-text layout. Font size was 14, scale 1, with varying printable ASCII content.

| Completed full replacements | Live Rust heap above baseline |
| ---: | ---: |
| 300 | 4.267 MiB |
| 600 | 4.267 MiB |
| 900 | 4.268 MiB |
| 1,200 | 4.269 MiB |

It requested **8.68 GiB cumulatively** across 1,200 replacements while peak tracked live allocation stayed about **4.39 MiB**. Dropping the buffer/cache/font system returned live Rust allocation to baseline. This demonstrates heavy allocation turnover, not a line-layout cache retaining all previous frames. It excludes glyphon preparation/rasterization/GPU submission and does not exhaust Unicode/font diversity.

Specific dependency checks prevent two tempting but incorrect diagnoses:

1. **cosmic-text's shaped-run cache is not enabled in this build.** It would require an eviction policy if enabled later, but is not the current explanation for retaining every unique terminal line. The separate shape-plan cache holds only six plans; font-match cache has a 256-entry reset policy. Loaded fonts and shaping scratch can still establish content-dependent high-water storage.
2. **glyphon calls `SwashCache::get_image_uncached`.** Its normal preparation and atlas-growth paths do not populate Swash's public `image_cache`/outline maps. Those maps are unbounded in isolation, but blaming them here would ignore the actual call path. Swash's internal scaler context and font/native resources are separate considerations.

The application's natural-advance cache is explicitly bounded. Font/metrics changes clear it. Repeated font/DPI changes can still affect atlas/native high-water storage, which needs a display-backed test rather than inference from the cache type alone.

### Logs and egui

The log probe configured 100,000 entries and pushed four batches of 100,000 records with ~200-byte payloads. The ring plateaued at **54,827 entries / 17.34 MiB actual tracked Rust heap**, despite continued allocation traffic. This exceeds its 16 MiB *accounted entry* budget because actual ownership also includes Arc/deque storage and other overhead; it did not keep growing with total records.

Clearing released entries but retained about **0.5 MiB** of deque capacity. Dropping the ring returned live allocation to baseline. The log view caches IDs, not owning Arcs to every old message; explicit Copy briefly takes an owning snapshot and builds a text string. UI virtualization prevents every retained record from becoming a widget/layout every frame.

epaint's galley cache ages out layouts not used in the current generation, and its font atlas is recreated when nearly full or its text options change. Cometty processes both texture updates and frees. The title-width cache is periodically cleared after 512 entries; its check is per pass rather than a strict insertion-time ceiling. Neither normal path establishes a lifetime-long cache of every title/log message.

Default debug logging still causes formatting/allocation work. `RUST_LOG=off` disables stderr, **not** the independent in-app ring. For diagnostic A/B tests, use `[log] filter = "off"` too; do not interpret a log-filter improvement as proof that the ring was unbounded.

### Other ownership checks

- **Output queue:** a blocked reader send applies backpressure; no unbounded queue of historical PTY chunks was found. Notifications are coalesced, not one queued wake per output byte.
- **Input queue while open:** the byte budget includes in-flight capacity, which prevents the usual “dequeue frees quota while writer still owns the paste” accounting bug. The problem is shutdown/cancellation, not missing open-session accounting.
- **OSC 8 table:** stores two owned URI copies, so 256 KiB unique bytes can mean roughly 512 KiB string payload plus containers. IDs are not reclaimed with history, but the count/byte caps prevent normal unlimited growth.
- **Titles/cwd:** bounded final strings by default, but their raw OSC and pre-truncation work are covered in Finding B. File-configured limits can be much larger than defaults.
- **Config saver:** at most one pending snapshot and one result replace earlier ones; no save-job queue growing with every slider edit.
- **Selection/copy/paste:** whole selected text/system clipboard text can cause transient copies; paste admission preflights its additional input buffer but cannot prevent the clipboard API from first materializing an oversized source string. Those copies are not a lifetime-long application text history.
- **Globals:** the logger intentionally lives for the process lifetime and owns one bounded ring. Profiling metrics use fixed histograms and at most 64 tab IDs per reporting interval; the selection-regex cache holds one pattern per calling thread. No obvious application-owned `Arc`/`Rc` ownership cycle, `mem::forget`, or repeated explicit heap leak was found in these paths. That is not a native-library leak proof.

## 11. Allocator retention is visible and can resemble a leak

Freeing Rust allocations does not require macOS malloc to immediately unmap/reclaim their resident pages. Several probes demonstrated this distinction:

- In the 80-column history case, dropping the terminal restored the live counter to baseline while sampled RSS remained around **43.5 MiB**.
- In the layout case, all measured objects were dropped but RSS remained around **25 MiB**.
- Closing the 200-column dense search released ~48 MiB of result storage while RSS remained near its **189.7 MiB** sampled high-water value at that point.

Allocator arenas, size classes, cached free pages, native resources, and OS reclamation policy can all contribute. RSS can also rise while a reused large allocation touches previously unpopulated pages. Conversely, a large reserved vector can have relatively low RSS.

These are reasons to distinguish three different questions:

1. **Is application-owned live memory still increasing?** Inspect capacities, heap snapshots, and ownership.
2. **Is an exceptional high-water allocation still intentionally reachable?** Parser capacity, CPU scratch, atlas textures, and thread-owned transactions are examples.
3. **Has freed memory been returned to the OS yet?** A flat live heap with high RSS is not, by itself, proof of a leak.

Do not add indiscriminate per-frame cache clears or `shrink_to_fit` calls merely to force an attractive RSS number: that can create allocation churn and latency without fixing the missing bounds/lifecycles above.

## 12. Recommended action plan

| Order | Work | Success criterion |
| ---: | --- | --- |
| 1 | Explicit PTY shutdown, interruptible I/O, child reaping | Repeated close of idle/raw/blocked-input tabs returns live bytes, threads, descriptors, and children to baseline |
| 2 | Parser-level raw OSC limit, oversize discard/recovery, high-water release | Oversized/unterminated input cannot grow memory beyond a declared bound; following text still parses correctly |
| 3 | Remove/flush uploads on failed-frame paths; fault-inject surface and prepare failures | Staging/native memory plateaus across thousands of failures and recovers cleanly |
| 4 | Memory telemetry and an application-wide budget | Report accounts for all tabs and shared resources; configured budget has a documented enforcement policy |
| 5 | Budget/compact dense search and avoid double maximal vectors | Dense search has a predictable retained/peak byte bound without undefined result truncation |
| 6 | Bound cluster tails; reduce append amplification | Adversarial combining runs have bounded storage and work, preserving ordinary Unicode behavior |
| 7 | Hysteretic live/snapshot/scratch/atlas reduction; compact history representation | Exceptional large windows/colored output do not pin disproportionate storage for the rest of the session |

Useful telemetry, updated at existing work boundaries rather than every cell/frame:

- Per tab: live/saved-main cell capacities and cluster-tail bytes; history accounted bytes/rows; raw OSC current/high-water bytes; search count/capacity/identity bytes; queued and in-flight input/output bytes.
- Lifecycle: live sessions versus visible tabs; reader/writer counts; pending shutdowns; unreaped children.
- Shared renderer: snapshot capacities, background CPU/GPU capacities, glyph instance capacities, atlas dimensions/entries, uploads awaiting submission, failed acquisitions and last successful submit/completion.
- Process: RSS and platform physical footprint, clearly distinguished from the owned-byte subtotal.

The settings panel already displays the active tab's history usage, and `COMETTY_PERF=1` reports output queue peaks/phase activity. Neither currently explains the complete memory total. A global budget should preserve active-session input correctness and distinguish evictable history/caches from non-evictable in-flight operations; do not “enforce” it by silently dropping terminal input.

## 13. Reproducing and diagnosing a real long session

### Immediate, non-code mitigations

- Verify the exact binary/source revision; the audited revision contains recent history/input/log bounds.
- Inspect **all tabs**, not only the visible tab, and close unused search bars. Large searches are released on close.
- Reduce the history budget if its aggregate is the main contributor. For example:

  ```toml
  [terminal]
  scrollback_lines = 3000
  scrollback_bytes = 33554432 # 32 MiB per tab, not for the whole application
  max_cells = 65536          # optional smaller live-grid ceiling
  ```

  Lowering these limits can discard old history or constrain grid dimensions; choose values appropriate to the workload. This does not fix OSC storage, orphaned workers, or GPU staging.
- Until shutdown is repaired, prefer letting a job finish and exiting its shell before closing a tab, rather than closing a nonreading/raw job with a blocked paste. Natural exit still needs proper reaping in code.
- Restarting releases process-owned parser/scratch/native high-water state, but is only a workaround and may not terminate orphaned children correctly.

### A controlled display-backed experiment

Record the actual binary hash/commit, config, OS, GPU, dimensions, font/DPI, number of tabs/search bars, and whether the workload emits OSCs or uses raw/fullscreen TUIs. Track the **cometty parent PID separately from its shells/jobs**. Child-process RAM must not be attributed to the parent; retained worker threads, by contrast, are part of cometty.

Use macOS Activity Monitor plus `vmmap -summary PID`, `footprint PID` where available, and Instruments Allocations/VM Tracker/Metal resource tools. Capture snapshots before workload, after warm-up, after repeated fixed-size batches, after returning to idle, and after releasing the relevant state. A `leaks PID` scan can find unreachable allocations, but may report nothing for these bugs: a parser, blocked thread, or pending GPU queue still makes the allocation reachable.

Suggested isolation matrix:

| Workload | Expected diagnostic distinction |
| --- | --- |
| One fixed-width tab, repeated ASCII lines well past history saturation, search closed | Plateau tests history/row recycling; continued growth needs another explanation |
| Same text with many tabs | Predictable per-tab multiplication versus unbounded single-tab growth |
| Dense `a` search, rescan, close; repeat across tabs | Search peaks and inactive-tab result retention |
| Repeated terminated same-size OSCs, then one larger OSC | One high-water step versus one new retained allocation per sequence |
| Repeated raw-mode tab close, idle and with blocked paste | Worker/descriptor/child growth after visible tab count returns to baseline |
| Colored TUI during injected acquire failures, then recovery | Pending-upload growth versus stable grid/atlas sizes |
| Large-to-small resize; font/DPI changes; return to plain prompt | Row/snapshot/geometry/atlas high-water storage versus continuing growth |
| Default logging versus both sinks disabled | Formatting/native/UI overhead versus bounded log storage |

For a small **terminated** unsupported-OSC manual check inside a disposable cometty tab:

```sh
python3 -c 'import sys; sys.stdout.buffer.write(b"\x1b]999;" + b"x" * (8 * 1024 * 1024) + b"\x07"); sys.stdout.flush()'
```

That deliberately increases parser capacity; it is not a harmless visual-only test. The producing Python process has its own temporary allocation, so observe cometty after Python exits. Do not run unbounded/unterminated payload generators in an important session.

Heap traces, terminal-output recordings, and clipboard captures can contain credentials/private terminal contents. Redact them before sharing.

### Probe protocol and artifacts

The external probe and full raw logs from this study are retained locally under **`target/memory-study/e06f621/`** (ignored build artifacts, not a required tracked dependency of this report). Its Cargo manifest/lockfile, unchanged PTY module copies, and production source/assets snapshot are included. Production imports point at this checkout; repoint them at the saved source snapshot if the checkout moves or changes.

On this checkout it can be rebuilt with:

```sh
cargo build --manifest-path target/memory-study/e06f621/Cargo.toml --release --locked --offline --target-dir target
target/release/cometty-memory-study history
target/release/cometty-memory-study search
target/release/cometty-memory-study osc
target/release/cometty-memory-study cluster
target/release/cometty-memory-study resize
target/release/cometty-memory-study logs
target/release/cometty-memory-study layout
target/release/cometty-memory-study pty
```

`pty` spawns and explicitly cleans up its own short-lived children; `gpu` is an optional offscreen mode and failed to obtain a working device here. The source in the ignored artifact documents exact workloads and allocator bookkeeping. The result tables, environment, source references, and method above are self-contained if these disposable artifacts are later cleaned.

### Locked dependency source references

The mechanisms above were checked against these files in Cargo's local registry sources:

| Dependency source | Relevant implementation |
| --- | --- |
| `vte-0.15.0/src/lib.rs:55–69,543–559` | `std` OSC vector, unchecked push, capacity-preserving clear |
| `portable-pty-0.9.0/src/unix.rs:352–404` and `src/lib.rs:271–280` | Cloned master descriptors, writer EOT behavior, child wait delegation |
| `wgpu-30.0.1/src/api/queue.rs:137–175` and `wgpu-core-30.0.1/src/device/queue.rs:358–447,631–685` | Staging allocation lifetime and `PendingWrites` ownership |
| `glyphon-0.12.0/src/text_atlas.rs:30–221,334–337` | Atlas allocation/eviction/growth and unpin-only trimming |
| `glyphon-0.12.0/src/text_render.rs:284–305,329–343,462–563` | Uncached rasterization, instance capacity, cached glyph metadata |
| `cosmic-text-0.19.0/Cargo.toml:33–60` and `src/font/system.rs:139–164,183,359–364` | Feature defaults and font/cache ownership/limits |
| `cosmic-text-0.19.0/src/shape.rs:84–111,173–191` and `src/swash.rs:131–172` | Six shape plans, scratch storage, cached versus uncached image paths |
| `epaint-0.36.1/src/text/fonts.rs:734–748,1290–1296` | Font-atlas recreation and frame-generation galley eviction |

## 14. Follow-up: batch 2 (OSC bounds)

Implemented on 2026-10-25, separately from the baseline audit above.

### Policy and implementation

- [`src/term/parser.rs`](../src/term/parser.rs) wraps the existing vte 0.15 parser
  with a small streaming guard. The hard limit is **2 MiB of wire bytes after
  `ESC ]`**, including the OSC number, separators, and ignored C0s, excluding its
  terminator. It applies to supported/unsupported OSCs and regardless of the
  clipboard policy. A normal base64-encoded 1 MiB OSC 52 transfer fits, including
  usual line wrapping; arbitrarily whitespace-inflated transfers do not.
- Overflow drops the underlying parser immediately, without dispatching any
  truncated title/cwd/link/clipboard update. The rest is discarded through
  BEL/CAN/SUB/ESC. Recovery matches locked vte: **any ESC** ends OSC and starts
  the next escape; raw or UTF-8 `0x9c` inside OSC is data, not a terminator.
  One size-only raw-overflow warning is emitted per terminal, with no payload.
- After a completed/cancelled OSC longer than **64 KiB**, the underlying parser
  is replaced at that boundary, preserving an ending ESC's state. Smaller OSCs
  reuse capacity. No parser fork, `std` feature change, per-frame shrinking, or
  bytewise normal-text feed was introduced. `memchr` is now a direct dependency
  for the normal-text scan; its already-locked version did not change.
- [`src/term.rs`](../src/term.rs) streams title cleaning and bounded cwd percent
  decoding instead of constructing whole-payload intermediate strings. URI
  sizes are checked before allocation, preserving oversized-link clearing and
  the grid's one-shot warning. OSC 52 validates target/encoded length before
  filtering/decoding, reuses valid decoded UTF-8 storage, and rejects lossy
  UTF-8 expansion beyond the **1 MiB final-text** cap before copying it.

### Headless regression evidence

The test-only allocator now records successful allocation/reallocation requests,
deallocation deltas, a relative live-byte peak, and the largest request on the
test's thread. These are **requested Rust bytes**, not RSS/physical footprint,
allocator slack/internal realloc-copy peaks, or native/GPU allocations. Parser
tests use a nonallocating callback sink; they do not measure the grid/log ring or
clipboard together with the raw buffer.

| Regression | Result in debug and release tests |
| --- | --- |
| Unsupported unterminated OSC, streamed in reusable 8 KiB chunks through 32 MiB | Largest allocation and live-byte peak **2 MiB**; **zero** retained raw bytes after overflow, with the parser still alive |
| Another 32 MiB before termination of that same OSC | **Zero** allocation/reallocation requests; following valid OSC still dispatches |
| Repeated terminated/cancelled OSCs at 64 KiB + 1 and the exact 2 MiB limit | Raw allocation released at each boundary; exact-limit commands accepted; ending ESC still introduces the next CSI |
| OSC at the 64 KiB reuse threshold, followed by small OSCs | Allocation reused; no new allocations for subsequent small commands |
| Supported and unknown oversized commands | No partial title, cwd, hyperlink, or clipboard update; following text/OSC parses after BEL/ST/CAN/SUB |
| Full 1 MiB clipboard text, base64 with newline/space wrapping and split ST | Accepted unchanged |
| 1 MiB of decoded invalid UTF-8 requiring 3 MiB of replacement text | Rejected without allocating the expanded string |
| Megabyte-scale title/path with a 16-character result limit | No individual handler allocation reaches 4 KiB |

Additional tests compare guarded callbacks with locked vte under identical
chunking, including every split of mixed CSI/DCS/SOS/PM/APC/OSC/UTF-8/control
corpora, bytewise feeds, and deterministic mixed-byte inputs. Small test limits
exhaust every split immediately below/at/above overflow. Label transformations
are compared with the previous full-transform semantics, including trimming,
fragment separators, percent escapes, and invalid UTF-8.

Validation:

- `cargo test --locked --offline`: **403 passed, 3 opt-in probes ignored**.
- `cargo test --release --locked --offline term::`: **76 passed**.
- `cargo clippy --locked --offline --all-targets -- -D warnings`: passed.
- `cargo fmt --check`: passed.
- Existing release headless benchmark: passed with both quiet/default in-app
  logging. ASCII/CRLF still request zero allocations; the quiet OSC 8 case fell
  from 19,500 allocations/325,000 requested bytes to 6,500/156,000. Repeating the
  accepted 1 MiB ignored OSC now reallocates instead of retaining its exceptional
  buffer between sequences: this is an intentional retention/traffic tradeoff,
  not a claim that allocation traffic is retained RAM. Timings remain noisy and
  do not establish GUI latency or an RSS improvement.

Reproduce the committed headless checks with `cargo test --locked term::` (or
add `--release`). No disposable external probe or display/GPU is required.
Finding C (failed-frame uploads) remains the next graphics-side repair to verify;
this batch does **not** impose a process-wide RAM ceiling.

## Bottom line (audited revision)

The current history/log/input caps are valuable and work in the measured steady-state paths, but **they do not establish a bound on total application RAM**. The strongest reproduced defects are PTY resources outliving closed tabs and an uncapped, high-water-retaining OSC parser. Failed-frame staging is a particularly important graphics-side mechanism to reproduce next. Dense search, many tabs, large geometry, retained capacities, and allocator behavior can add substantial memory without an unreachable-allocation leak. Attributing the original gigabyte-scale sessions requires an affected-process trace that separates those categories.
