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

Dense search still stores all matches; this tranche does not change result
counts or make scans incremental. Projection storage is restricted to visible
hits and reused via a per-tab shared handle. Closing search releases match
allocations larger than 1 MiB; small capacities may be reused.
