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

## Cross-terminal comparison (macOS)

[`scripts/compare/`](../scripts/compare/) benchmarks the real GUI against
Terminal.app, iTerm2, Ghostty, and kitty on one Mac. The procedure is fully scripted:

```sh
make bench-compare                                   # all terminals and suites, 3 runs
make bench-compare COMPARE_TERMINALS=cometty,kitty COMPARE_SUITES=throughput COMPARE_RUNS=1
bash scripts/compare/compare.sh --help               # every option
bash scripts/compare/compare.sh --report DIR         # re-render (and publish) a summary
make bench-compare COMPARE_RUNS=1 COMPARE_ARGS=--no-publish   # smoke run, keep BENCHMARK.md
```

Run it from a terminal that is **not** being measured. A separate cometty works,
because the benchmark runs a private copy of the bundle with an isolated `HOME`.
Grant Accessibility to that host terminal, and Screen Recording too for the latency
suite. The harness refuses targets that are already running or that host the run.
`--quit-running` asks before it quits them. Keep the Mac on AC power and leave it
alone during a run: the harness moves focus and posts key events.

**Control model.** Every window and tab runs `driver.py agent` instead of a shell.
There are no rc files and no typed commands. The agent blocks on a FIFO, so it adds
no idle wakeups. It executes JSON ops (`throughput`, `fill`, `flood`, `latency`)
and writes their results to files. `termlat` (Swift, built into `target/compare/bin/`)
waits for windows and places them through AX. It also measures key-to-pixel latency.
`procstat.py` reads `proc_pid_rusage` for every terminal-owned process: the app and
its helpers, such as iTermServer or kitten, but never the agents.

**Normalization.** All terminals use Menlo 14, which is what cometty's generic
`monospace` resolves to on macOS. They also share the same 1000×700-point window,
10,000 history lines, no cursor blink, no bells or ligatures, and no session
restore or user config. The resulting grid differs slightly per terminal, and the
report lists it. Known non-equivalences:

- Ghostty limits history by bytes, set to 10 MB.
- Line height and padding follow each terminal's defaults.
- iTerm2 and Terminal.app windows come from AppleScript or a profile.

Terminal.app and iTerm2 preferences are exported before the run. After the run,
added keys are deleted and the backup is imported again. The temporary iTerm2
Dynamic Profile is removed too.

| Suite | Measures | Does not include |
| --- | --- | --- |
| `startup` | Launch command → first on-screen window, and → child start (wall clock, includes LaunchServices/AppleScript) | First painted prompt |
| `idle` | CPU %, interrupt + package-idle wakeups/s, and `ri_energy_nj` over 60 s with one focused window | Display/GPU power outside the process |
| `throughput` | MiB/s and terminal CPU s/MiB for ASCII, dense 256-color SGR, CJK/ZWJ/combining, DECSTBM scrolling, alternate-screen redraws. Timing runs from the first write to the DA1 reply, so the terminal has **parsed** everything | Presentation of those bytes |
| `memory` | `phys_footprint` (includes attributed GPU/IOSurface memory) at fresh → 10k ASCII → 10k SGR → N filled tabs → back to 1 tab, plus lifetime peak; `footprint` breakdown per checkpoint | Memory of other processes (WindowServer) |
| `latency` | Synthetic key post → display time of the first ScreenCaptureKit frame that shows a toggled block. Measured idle and while another tab floods output | A hardware photodiode measurement: capture quantization (≈8 ms at 120 Hz) and the compositor path are common to all terminals, so compare the numbers, don't read them as absolute |

Results go to `target/compare/<UTC>/`:

- `summary.md` and `summary.csv`: medians and min–max ranges.
- `raw/*.jsonl`: every sample.
- `footprint/`: per-checkpoint breakdowns.
- `config/`: the exact configs used.
- `environment.txt`, `source.patch`, and copies of the scripts.

The summary is also copied to [`BENCHMARK.md`](../BENCHMARK.md) at the repository root.
Its header records the date, host, power source and cometty revision. A run on
battery power gets a warning. `--no-publish` leaves the copy untouched, so use it
for smoke or partial runs.

These are host-specific measurements. They are not CI thresholds. Re-run on the same
host when you compare revisions.

Validation status: every adapter has run every suite in short smoke runs. A full
multi-run comparison has not been recorded in this document.

## Long-session stress results (measured)

These numbers come from one display-backed run on 2026-10-26. They are **measurements
on one host**, not guarantees or CI thresholds. Re-measure before you rely on them for
a different revision, host, font, or DPI.

- **Setup:** Apple M3 Pro, macOS 26 (Darwin 25.6.0), release build. The CPU and
  restore rows were measured at `fdbf3cd`. The other rows were measured at `06a3e0a`
  with the idle-spin fix applied.
- **Driver:** the real GUI, driven by `osascript` keystrokes. `HOME` and
  `XDG_STATE_HOME` were isolated, and `/bin/zsh` ran with a minimal rc.
- **Metrics:** only the cometty **parent** PID was sampled, using `footprint`,
  `vmmap`, `ps` (RSS, threads, CPU time), `lsof` fd counts, child counts, and
  `COMETTY_PERF=1` stderr.
- **Harness:** the scripts lived under the ignored `target/stress/` directory and
  are not committed. Keystroke injection mistypes `~`, so avoid special characters
  in injected commands.

| Check | Result |
| --- | --- |
| Idle, 1 tab | ~1% of a core (0.22 s CPU per 20 s, cursor blink only); startup ≈ 0.33 s CPU |
| Idle, 10 full tabs | ~1.5% CPU, ~2 presents/s (blink), memory stable for 3 min |
| Tab churn (450 + 180 + 270 open/close) | ~0.9 s CPU per 9-tab batch; threads return to 11 and fds to 57 (+2 after the first batch, then flat); no leftover children |
| Tab churn, footprint | Flat at 216 MB over 30 batches with both log sinks off. Default logging creeps ~25 KB per batch while the bounded log ring fills |
| Resize churn (2400×1300 ↔ 700×450) | ~200 ms per round trip, including AppleScript overhead |
| Quit/restore, 10 full tabs | 15/15 cycles restored all tabs and 9/9 non-home cwds; quit 0.63–0.89 s with ~84 MB of dumps; all 10 shells up in ~1.3 s; dumps pruned after commit |

Single-tab throughput, timed from the producer's completion. This includes up to one
PTY queue of lag but not presentation:

| Workload | Rate |
| --- | --- |
| ASCII, 80 columns | ~130k lines/s (~10.6 MB/s) |
| SGR 256-color fg+bg on every cell | ~18k lines/s (~30 MB/s) |
| CJK + ZWJ emoji + combining marks | ~23k lines/s |
| OSC 8, unique link per line | ~70k lines/s |

[Memory](memory.md#measured-footprint-one-host) has the memory results from the same
run.

Gaps in this run:

- It used one host and took no Instruments traces, so GPU memory is not attributed.
- Search was not tested at the multi-million-match scale. The synchronous
  full-history scan is still unmeasured under load.
- Accessibility-driven focus forces runs to be serial.

Regression guard: an idle instance that uses a whole core, or tab-churn CPU that
grows with the number of tabs ever opened, means something is cloning
`EventLoopProxy` on a per-turn or per-tab path again. See the
[event-loop decisions](architecture.md#decisions-that-shape-the-event-loop). Use
`sample PID` to confirm. The spinning main thread sits in `CFRunLoopAddSource`.
