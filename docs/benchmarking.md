# Headless performance checks

The ignored `perf_bench::headless_release` test exercises the production parser,
search, line-construction and row-hash helpers. It needs no window, PTY, GPU or
services. Font discovery uses local fonts; record the host/font environment when
comparing shaping results.

Run each logging mode in a separate process (the real application logger is
installed, with no event-loop proxy). `RUST_LOG` controls stderr independently:

```sh
bash scripts/bench-headless.sh
```

The script saves both logs, compiler/OS/revision/lockfile metadata, and an exact
source snapshot under a new `target/perf/<UTC timestamp>/` directory. It refuses
to overwrite an existing run. Add host CPU/font details when comparing machines.
To run one mode directly:

```sh
RUST_LOG=off COMETTY_BENCH_LOG=default cargo test --release perf_bench::headless_release -- --ignored --exact --nocapture --test-threads=1
RUST_LOG=off COMETTY_BENCH_LOG=quiet cargo test --release perf_bench::headless_release -- --ignored --exact --nocapture --test-threads=1
```

Keep complete output, `git rev-parse HEAD`, `git diff`, the Cargo.lock checksum,
`rustc -Vv`, OS/CPU and font information with results (for example under ignored
`target/perf/`). Do not compare development builds with these release runs.
Each case warms once, then reports seven batch means and their median. A separate
call counts allocation/reallocation requests and cumulative requested bytes,
**not peak RSS**. The test-only system-allocator wrapper uses thread-local
accounting; it is inactive during timings but still adds a TLS check. No
allocator wrapper is installed in the application binary.

`shape_*` measures line construction only; `layout_*` includes construction and
final cosmic-text row layout. `projection_cached_*` measures the production
per-tab projection cache, not just the standalone projector.
`shape_*_cold_advances` clears the advance/measurement cache for each operation;
the regular shape/layout cases warm it once. Font discovery/font-system caches
remain warm, so these are not cold-start font measurements. ZWJ cases fill the
same 200 columns with 100 wide clusters rather than 200 narrow ones.

Use same-machine A/B runs, not fixed wall-clock CI thresholds. Deterministic
tests cover work bounds, state equivalence, repaint decisions and invalidation.
GUI frame rate, GPU activity, presentation latency and energy still require
manual display-backed profiling; these headless measurements do not establish
those outcomes.

## Application phase counters

Set `COMETTY_PERF=1` for a display-backed run. Approximately every ten seconds
of **existing event-loop activity**, stderr receives aggregate invocation counts,
total and maximum CPU-side elapsed time for drain, parser, search/projection,
snapshot, text rebuild, background, overlay, surface acquire, glyph prepare,
encode/submit and present. Byte/cell/row counters, PTY wakes, redraws, actual
present calls and skipped frames are separate. Counters add no wake timer, use
no per-character instrumentation and bypass the log ring to avoid feedback.
They are disabled by default. Phases nest: do not sum their totals as frame time;
these are wall-clock phase measurements, not process CPU time. They can include
preemption, and acquire/present can include platform waits; neither measures GPU
execution.

Tranche two adds `advance_miss`, `prepare_reuse`, `coalesced_wake`, `drain_yield`,
`queue_wait` and `queued_bytes` (a per-queue high-water byte value including the
reader's one in-flight send). Queue wait starts before the bounded send, so it
includes reader backpressure as well as time in the channel.

Tranche three adds no steady-state wake: history width projection, row recycling,
config-save coalescing and the bounded input queue are event/test driven; only
an in-progress incremental log filter requests a bounded follow-up frame. These
paths are covered by deterministic tests plus the headless cases below.
`snapshot_cells` now counts cells actually refreshed in the per-row snapshot
cache (unchanged rows are reused), so compare it with `render`/`present` counts
rather than treating it as viewport area.

Timed phases report `p99_us_le`, an approximate percentile **upper bound** from
fixed log2-microsecond buckets, not an exact p99. Counters have zero timing fields;
`max_value` is used for high-water values. At most 64 stable tab IDs per reporting
interval also report backlogged service gaps, input-to-first-output and oldest
unpresented output-to-present. Empty queues reset their service-gap clock and
tab switches discard pending presentation stamps. The input probe is still a
heuristic: unrelated output in the **same tab** can arrive first. It does not
prove an echo, correlate every repeated key, or measure scanned-out pixels.
All diagnostics remain opt-in and add no timer/log-ring feedback.

## Explicit scheduling and GPU checks

```sh
COMETTY_PERF=0 cargo test --release pty::tests::fair_drain_under_load -- --ignored --exact --nocapture --test-threads=1
cargo test --release renderer::gpu_tests::retained_glyphs_match_fresh_preparation_after_atlas_churn -- --ignored --exact --nocapture --test-threads=1
```

The scheduling probe uses production bounded/coalesced queues, the production
fair order/budget and terminal feeds, with a saturated earlier queue, an identified
request/response in active slot 2, and an exit in slot 3. It reports exact sample
p50/p95/p99 request-to-parse timings after eight warm-ups. It excludes OS PTYs,
metadata queries, event-loop dispatch, rendering and presentation: it is **not**
an interactive GUI echo-latency result. It is intentionally not a wall-clock CI
gate. The ordinary tests assert fairness and shared budgets deterministically.

The GPU check is ignored by default and requires a working adapter, but creates
no window/surface. It compares offscreen pixels from retained instances against
fresh preparation while changing glyph sets/styles and exercising atlas growth/
reuse. It is a correctness check, not a GPU/application performance measurement.

Dense search still stores all matches; this tranche does not change result
counts or make scans incremental. Projection storage is restricted to visible
hits and reused via a per-tab shared handle. Closing search releases match
allocations larger than 1 MiB; small capacities may be reused.

## Third tranche: scalability and lifecycle probes

Use the same release harness for the added cases:

- `resize_lazy_200_201`, `scroll_recycle_1000` — 10,100-row history; resize
  sealed storage must not be rewritten, and steady scroll must reuse dense
  buffers and evicted compact chunks under the byte budget.
- `paste_build_1m` — one-pass CRLF normalization + bracketed wrapping, no
  intermediate `String` replacements.
- `log_index_slice`, `log_index_unchanged`, `log_index_full_scan` — 100,000-entry
  ring. A slice is the bounded incremental unit; an unchanged level/query
  generation only reads ring metadata. Full log filtering remains explicit
  (Copy, or the sum of progressive slices) and is intentionally not a per-frame
  timer.

Deterministic gates cover lazy-width equivalence across shrink/regrow and theme
changes, no resurrection of clipped text, row-ID changes on recycled storage,
history byte caps (including extras/capacity), search alignment after eviction,
selection rebase/cancel, ignored-versus-queued input admission, parser-reply
headroom, bracketed-paste transaction integrity, debounced/ordered config saves,
and log-ID eviction. Wall-clock thresholds remain manual, same-machine A/B
checks rather than CI gates.

## Compact history checks

`cargo test --locked --offline grid::history::` covers the bounded 32-row /
256 KiB batches, sparse cluster metadata and cross-row style runs, adaptive
dense fallback, row-level eviction within chunks, complete byte accounting,
unchanged resize projections/identities, Unicode/style/link round trips,
direct ASCII search, viewport copies, zero caps and zero-allocation steady
scrolling. Source sizes include cell capacities and cluster tails; exceptionally
large rows stay dense without copying their payload into a packer.

The release harness also prints `history_memory` for 80/200/400/1,024 columns,
60 live rows, default history limits and sparse/dense ASCII output. Each case
feeds 20,000 lines, then another 20,000 with reusable input. Counters show
accounted history bytes, terminal-owned requested Rust live/peak bytes and
later growth. Dropping each terminal must balance all counted allocations.
These are **not RSS, allocator overhead or native/GPU memory**. The prebuilt
input string is outside the counting window. A bounded unfinished dense batch
and recycling pools remain included; counts depend on the sampled batch phase.

Compare `scroll_recycle_1000` and output cases as well as memory: batching
trades additional CPU work for lower retention. ASCII compact history has a
direct projected-text search path; nontrivial clusters use borrowed cell views
without allocating owned tails per scanned cell. Search results still store
every match, and wider histories may now retain more rows within the same byte
cap. See the [memory-study follow-up](memory_study.md#16-follow-up-compact-history-chunks).

## OSC memory regression checks

`cargo test --locked term::` (also with `--release`) covers the 2 MiB raw OSC
limit, oversize discard/recovery, >64 KiB completion/cancellation release, small
buffer reuse, and supported-handler bounds. The test-only allocator additionally
tracks relative live/peak requested-byte deltas and the largest request; it is
not installed in the application binary and does not measure RSS/native/GPU
bytes or malloc's internal realloc copying peak. See the batch-2 follow-up in
[`memory_study.md`](memory_study.md#14-follow-up-batch-2-osc-bounds) for results.

The existing `ignored_osc` benchmark is a **terminated 1 MiB payload**, below
the raw limit but above the reuse threshold. It now deliberately allocates and
releases its raw buffer on each iteration instead of retaining the high-water
allocation. Increased requested-byte traffic in this case is expected, not
evidence of retained-heap growth. Normal small OSCs still reuse parser storage.
