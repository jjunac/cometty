# Performance measurement

Use release builds, reproducible workloads, and deterministic correctness/work
bounds before optimizing. Headless throughput, allocation traffic, retained Rust
bytes, UI latency, GPU execution, and energy are different measurements. None
can stand in for the others.

## Headless release benchmark

The ignored `perf_bench::headless_release` test exercises production parser,
search/projection, line-construction/layout, row-hash, history, paste, and log-index
helpers without a window, PTY, GPU, or service. Font discovery uses local fonts;
record font/host information when comparing layout results.

```sh
bash scripts/bench-headless.sh
```

[`scripts/bench-headless.sh`](../scripts/bench-headless.sh) runs default and quiet
in-app logging in **separate processes**, with stderr logging and phase counters
off. It creates `target/perf/<UTC timestamp>/` (or the directory passed as its
first argument), refuses to overwrite an existing run, and saves:

- Complete `default.log` and `quiet.log`.
- Compiler/OS/revision/lockfile checksum and build-environment metadata.
- `git status`, a binary source diff, exact `src/`/assets/manifest/lockfile copies,
  and the runner itself, including uncommitted/new Rust files.

Add CPU/GPU/font details yourself. Preserve artifacts outside the source history
when comparing changes; a commit hash alone cannot identify a dirty measured tree.
Do not require old ignored `target/` artifacts to reproduce a current check.

For a single mode:

```sh
RUST_LOG=off COMETTY_PERF=0 COMETTY_BENCH_LOG=default cargo test --release --locked perf_bench::headless_release -- --ignored --exact --nocapture --test-threads=1
RUST_LOG=off COMETTY_PERF=0 COMETTY_BENCH_LOG=quiet cargo test --release --locked perf_bench::headless_release -- --ignored --exact --nocapture --test-threads=1
```

The real global logger is installed, without an event-loop proxy. `default`
records `cometty=debug,warn`; `quiet` records nothing. `RUST_LOG=off` alone is
**not** quiet in-app logging.

### Reading the cases

Each timed case warms once, then reports seven batch means and their median.
Allocation accounting is a separate call. The test-only system-allocator wrapper
counts successful requests/reallocations and requested bytes; it is inactive
during timing but still adds a thread-local check. It is not installed in the
application binary. Thread-local measurements do not capture worker/native/GPU
allocations or prove a process-memory bound.

| Cases | What they establish |
| --- | --- |
| `ascii`, `crlf`, `sgr`, `osc8` | Parser/grid throughput and allocation traffic with warmed history; 8 KiB feeds |
| `ignored_osc` | A **terminated 1 MiB** unsupported payload, below the raw cap but above the reuse threshold; deliberate allocation/release each iteration |
| `il_65535`, `dl_65535` | Bounded-region expanded work and allocation behavior |
| `search_*`, `projection_*` | Full matching versus visible-only projection; dense result capacity is printed separately |
| `projection_cached_*` | The production per-tab cached view, not just the standalone projector |
| `shape_*` | Line construction only, not final row layout |
| `layout_*` | Construction plus final cosmic-text layout |
| `shape_*_cold_advances` | Clear the advance/measurement cache per operation; font discovery/system caches remain warm |
| `hash_unchanged`, `expand_word` | Viewport hashing and warmed selection-pattern path |
| `resize_lazy_200_201`, `scroll_recycle_1000` | Width projection and saturated history recycling; compare CPU cost alongside compact storage savings |
| `paste_build_1m` | One-pass normalization/bracketed framing, not clipboard/PTY transfer latency |
| `log_index_slice`, `log_index_unchanged`, `log_index_full_scan` | One cooperative filter unit, metadata-only reuse, and explicit full filtering of a 100,000-entry ring |

ZWJ rows cover the same 200 columns with 100 wide clusters rather than 200 narrow
ones. Warm cluster advances do not eliminate final complex-text shaping cost.
Increased requested bytes for large completed OSCs reflect intentional buffer
release, not evidence of retained-heap growth.

### History and parser memory checks

The harness prints `history_memory` for sparse/dense ASCII at 80/200/400/1,024
columns and 60 live rows, using default limits. It feeds 20,000 lines and then
another 20,000 with reusable input, reports accounted history and terminal-owned
requested live/peak/later-growth bytes, and checks allocation balance on drop.
The input string is outside the counting window; the unfinished dense batch and
recycling pools are inside it. Batch phase affects exact retained bytes.

```sh
cargo test --locked grid::history::
cargo test --release --locked term::
```

History regressions cover source-batch bounds, adaptive dense fallback, Unicode/
style/link round trips, row-level eviction, byte accounting, destructive resize,
stable IDs, direct ASCII search, viewport copy, and zero-cap/pool release. OSC
tests cover 2 MiB overflow/discard/recovery, >64 KiB release, small-buffer reuse,
callback equivalence, and handler allocation preflights.

The allocator also exposes relative requested live/peak deltas and largest
requests for relevant tests. These are **not RSS**, allocator-internal realloc
copying peaks, or native/GPU memory. See [memory diagnostics](memory.md).

## Application phase counters

For a display-backed run:

```sh
COMETTY_PERF=1 cargo run --release --locked
```

[`src/perf.rs`](../src/perf.rs) reports to stderr approximately every ten seconds
of **existing event-loop activity**. Counters introduce no wake timer or log-ring
feedback and are off by default. They aggregate:

- CPU-side elapsed phases: drain, parse, search/projection, snapshot, rebuild,
  background, overlay, acquire, glyph prepare, encode/submit, present.
- Parsed bytes, refreshed snapshot cells, shaped rows, prepare reuse/advance
  misses, PTY wakes/coalescing, redraws, actual presents/skips, drain yields,
  output queue wait/high-water bytes, and upload-only cleanup submissions.
- At most 64 stable tab IDs per interval: backlogged service gaps,
  input-to-first-output, and oldest unpresented output-to-present.

Interpretation rules:

- Phases **nest**; do not sum them and call the result frame time. Elapsed time
  includes waits/preemption; acquire/present can wait on the platform. These are
  neither process CPU time nor GPU execution timings.
- `p99_us_le` is an approximate percentile **upper bound** from fixed log2
  microsecond buckets, not an exact p99. Untimed counters have zero timing fields;
  `max_value` records high-water values.
- Queue wait starts before bounded send, including reader backpressure. Queued
  bytes include that queue's one in-flight send, not every tab's combined memory.
- `snapshot_cells` counts refreshed cells, not viewport area × redraws.
- Empty queues reset service-gap clocks; tab switches discard pending presentation
  stamps. Unrelated output in the **same tab** can satisfy input-to-first-output.
  It is not verified echo, correlation of every repeated key, or scanned-out pixels.

## Explicit scheduling and GPU probes

Run these individually; do not enable all ignored tests on a headless host.

```sh
COMETTY_PERF=0 cargo test --release --locked pty::tests::fair_drain_under_load -- --ignored --exact --nocapture --test-threads=1
cargo test --release --locked renderer::gpu_tests::retained_glyphs_match_fresh_preparation_after_atlas_churn -- --ignored --exact --nocapture --test-threads=1
cargo test --release --locked renderer::gpu_tests::failed_frame_uploads_are_reclaimed_and_recover -- --ignored --exact --nocapture --test-threads=1
```

- **Scheduling:** production bounded/coalesced queues, fair order/budgets, and
  terminal feeds with an earlier saturated queue, identified request/response in
  active slot 2, and exit in slot 3. After eight warm-ups it prints sample
  p50/p95/p99 request-to-parse timings. Excludes OS PTYs, metadata queries,
  event-loop dispatch, rendering, and display; not GUI echo latency or a CI timer gate.
- **Retained glyphs:** offscreen pixel comparison against fresh preparation while
  changing glyphs/styles and growing/reusing the atlas. Checks lifetime correctness,
  not speed.
- **Failed uploads:** positive control for unsubmitted staging, then thousands of
  skipped/error outcomes through the production boundary with real texture/geometry
  writes. Test-only completion waits inspect native counters against a warmed
  baseline; recovery validates pixels and uploaded bytes. No real window/surface
  failures are induced, and attributed counters are not complete physical footprint.

GPU probes need a working adapter, but no window. Device creation failure means
their assertions were **not executed**, not that they passed. Native counters
are enabled by the dev-only wgpu dependency; production cleanup has no GPU waits.
Headless fake-backend frame tests cannot substitute for native verification.

## Display-backed profiling

[`scripts/profile-macos.sh`](../scripts/profile-macos.sh) needs full Xcode, not
only Command Line Tools, and must be run from the repository root on a machine
with a display/GPU. Use separate traces and unique output paths:

```sh
CARGO_PROFILE_RELEASE_DEBUG=1 COMETTY_PERF=1 bash scripts/profile-macos.sh --template 'Time Profiler' --time-limit 60s --output target/profile/cpu.trace
CARGO_PROFILE_RELEASE_DEBUG=1 COMETTY_PERF=1 bash scripts/profile-macos.sh --template Allocations --time-limit 60s --output target/profile/allocations.trace
```

Use a separate config with `[log] filter = "off"` plus `RUST_LOG=off` for quiet
application comparisons; pass it after the script's `--` as `--config PATH`.
On Linux collect equivalent CPU/allocation/scheduler traces. Record dimensions,
tab count, DPI, refresh rate, font, OS/GPU, source/lockfile, and both log filters.

| Workload | Primary evidence to collect |
| --- | --- |
| Idle bottom/history/hover; steady/hidden cursor; minimize/restore | Frames/wakes, CPU/GPU activity, energy, legitimate fades/tooltips |
| One-row versus full-screen TUI updates; blocks; decomposed/ZWJ text | Snapshot/shaped rows, preparation calls, geometry/upload volume, layout stalls |
| Later active tab under earlier-tab flood; third-tab exit | Service gaps, queue occupancy, identified input-to-visible distributions |
| Missing/dense/smart-case search; edits during output | Largest synchronous scan, result/refresh memory, projection work |
| Resize with 0/1k/10k history; many tabs; font/DPI churn | Tail latency and retained capacity after returning to small/plain state |
| Large log filtering and settings drags | Lock/scan slices, save coalescing, UI/Tab/IME ownership |
| Large paste to a nonreading child; repeated tab close | Admission notices, FIFO transactions, worker/descriptor/child cleanup |

Prefer same-machine A/B runs, not fixed wall-clock CI thresholds or development
versus release comparisons. Choose deeper damage tracking, retained geometry,
incremental search, or atlas policy from traces rather than stacking caches
speculatively. Preserve invariants in [rendering](rendering.md),
[terminal model](terminal.md), and [interaction](interaction.md). Do not trade them
for unbounded queues, forced polling, key-batching sleeps, or unexplained cache clears.
