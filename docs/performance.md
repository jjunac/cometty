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
| `search_history_*_missing`, `copy_history_*` | Full-history Unicode search and 60-row history snapshot copying for CJK, combining marks, and ZWJ clusters; no shaping/GPU |
| `paste_build_1m` | One-pass normalization/bracketed framing, not clipboard/PTY transfer latency |
| `log_index_slice`, `log_index_unchanged`, `log_index_full_scan` | One cooperative filter unit, metadata-only reuse, and explicit full filtering of a 100,000-entry ring |

ZWJ rows cover the same 200 columns with 100 wide clusters rather than 200 narrow
ones. Warm cluster advances do not eliminate final complex-text shaping cost.
Increased requested bytes for large completed OSCs reflect intentional buffer
release, not evidence of retained-heap growth.

### History and parser memory checks

The harness prints `history_memory` for sparse/dense ASCII at 80/200/400/1,024
columns and 60 live rows, and `history_unicode` for CJK, combining marks, and ZWJ
clusters at 200 columns/60 live rows, using default limits. Each case feeds
20,000 lines and then another 20,000 with reusable input, reports accounted
history and terminal-owned requested live/peak/later-growth bytes, and checks
allocation balance on drop. The input string is outside the counting window;
the unfinished dense batch and recycling pools are inside it. Batch phase affects
exact retained bytes. Unicode copy timings include any unsealed dense rows among
the last 60 history rows; allocation counts can reflect that dense copy path.

```sh
cargo test --locked grid::history::
cargo test --release --locked term::
```

History regressions cover source-batch bounds, adaptive dense fallback, narrow/
wide Unicode-record boundaries and format reuse, exact cluster/style/link round
trips, row-level eviction, byte accounting, destructive resize, stable IDs,
direct ASCII search, viewport copy, and zero-cap/pool release. OSC
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

[`src/perf.rs`](../src/perf.rs) reports to stderr every ten seconds from a
dedicated `perf-report` thread, and once more at exit. The thread costs one
process wakeup per report and never wakes the event loop, so a sleeping loop is
observable too. Counters add no event-loop timer or log-ring feedback and are
off by default. They aggregate:

- CPU-side elapsed phases: drain, parse, search/projection, snapshot, rebuild,
  background, overlay, acquire, glyph prepare, encode/submit, present, history
  sealing, title/process-fact refresh, and the complete redraw callback.
- Parsed bytes, refreshed snapshot cells, shaped rows, prepare reuse/advance
  misses, PTY wakes/coalescing, redraws, actual presents/skips, drain yields,
  output queue wait/high-water bytes, and upload-only cleanup submissions.
- At most 64 stable tab IDs per interval: backlogged service gaps,
  input-to-first-output, and oldest unpresented output-to-present.
- Wake and frame sources (see [idle CPU and energy](idle.md)): `loop_turn`
  (`about_to_wait` calls), `wake_timer`/`wake_event` (loop wakes by a deadline or
  by anything else), `window_event`/`pointer_event`, `focus_in`/`focus_out`
  (window focus changes; blinking depends on focus), `occluded` (the window
  became occluded and stopped rendering), redraw requests by reason
  (`request_blink`, `request_egui_input`, `request_egui_deadline`,
  `request_egui_immediate`, `request_output`, `request_output_deadline`,
  `request_chrome`, `request_pointer`), full egui passes (`egui_pass`),
  background vertices per frame (`bg_vertices`: sum and `max_value`), and timed
  PTY-supervisor waits (`supervisor_poll`).
- `process`: the whole process's CPU time over the interval (all threads,
  including Metal/AppKit queues and PTY workers) and, on macOS, its
  package-idle and interrupt wakeups and retired instructions
  (`proc_pid_rusage`). Elsewhere it reports voluntary/involuntary context
  switches; macOS does not maintain those. Compare revisions by instructions:
  CPU time and energy swing severalfold with performance/efficiency-core
  placement (see [idle](idle.md#measuring-reliably)).

Interpretation rules:

- Phases **nest**; do not sum them and call the result frame time. Elapsed time
  includes waits/preemption; acquire/present can wait on the platform. These are
  neither process CPU time nor GPU execution timings.
- `interval_us` gives the actual reporting interval and `at_ms` its end since
  launch. `history_seal` nests inside parsing; `title_refresh` nests inside
  draining; `redraw_total` includes any drain performed by that callback as well
  as rendering. Do not add these to their parents. The first report includes
  startup and the exit report includes shutdown; exclude both from idle rates.
- Redraw requests coalesce, so request counts can exceed `redraw`. Requests
  made by keyboard, mouse-button, resize, focus, search and settings paths are
  not attributed; `redraw` minus the attributed reasons bounds them.
  `wake_event` also counts run-loop wakes that deliver no winit event.
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
  not speed. Also compares bounded cached advanced shaping against fresh uncached
  layouts with named-font CJK, combining, emoji/ZWJ, Arabic and Indic text.
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

### Diagnosing throughput, rather than ranking terminals

DA1 establishes a **parsing completion point**, not parser-only execution.
Cometty's UI thread also shapes text and acquires/presents surfaces while the
producer is writing. Those operations delay subsequent parsing even though the
final DA1 reply does not wait for final presentation. See the
[measured throughput diagnosis](throughput.md) for causal controls and findings.

Two headless ignored tests use the exact generators from `compare/driver.py`:

```sh
python3 scripts/bench-throughput.py target/perf/replay-example
COMETTY_BENCH_CASE=ascii,unicode COMETTY_BENCH_SAMPLES=15 python3 scripts/bench-throughput.py --layout target/perf/layout-example
```

The runner refuses an existing output directory and preserves source, lockfile,
environment, corpus sizes/checksums, raw samples, and separate allocation counts.
`--mib` controls corpus/sample size (default 2); `--generate-only` skips execution.
Both tests run release/locked, with counters off and no installed logger:

- **Replay:** `vte` with counted callbacks, the OSC `guard` with the same callbacks,
  and real terminals with `history`, `no_history`, or an initial `alt` screen.
  History warms through the line cap before timing. `ascii_overwrite`, `cjk`,
  `combining`, and `zwj` are controlled diagnostic variants, not comparison scores.
  Redraw already enters/exits alternate storage, so its `alt` lane is not a
  continuously alternate-screen variant.
- **Layout:** capture the generated final viewport (before redraw's final alt
  exit), then force every visible line through production construction/layout.
  Reports build/layout milliseconds, allocation traffic per viewport, installed
  face count, generic-family availability, and actual glyph fonts. `default`,
  `resolved_mono`, and `named` compare font-selection policies without disabling
  advanced shaping or changing cell metrics. `production` uses the shipping
  generic-to-named resolution and bounded final-shaping cache, reporting its
  entry count and accounted retained bytes. Reference lanes disable that cache.
  This deliberately excludes row-hash
  reuse, rendering/GPU/PTY work, and font discovery/startup time.

Filters/settings: `COMETTY_BENCH_CASE` and `COMETTY_BENCH_LANE` take comma-separated
names; `COMETTY_BENCH_SAMPLES`, `COLS`, `ROWS`, and `CHUNK` have the same prefix.
Layout defaults to physical font size 28 (Menlo 14 at 2×); use
`COMETTY_BENCH_FONT_SIZE` and `COMETTY_BENCH_FONT` (default `Menlo`) on other setups.
Missing override fonts are explicitly skipped. Both tests use the test allocator
wrapper described above, not the application's allocator. Replay's
`COMETTY_BENCH_PROFILE_SECONDS` repeats a selected case/lane and prints its PID
for a native sampler. Allocation counting is off during timed/profiled work.

For a real GUI/PTY control on macOS, without changing any other terminal's
preferences or posting keys:

```sh
cargo build --release --locked
python3 scripts/compare/diagnose-throughput.py target/perf/gui-visible
python3 scripts/compare/diagnose-throughput.py --minimize target/perf/gui-minimized
python3 scripts/compare/diagnose-throughput.py --perf --sample-seconds 8 --workloads ascii,unicode target/perf/gui-profile
```

Each case starts its own isolated Cometty and agent, uses Menlo-resolution
`monospace` 14 / 1000×700 / 10k history, and cleans up only its own process.
All runs need a display/GPU; `--minimize` additionally needs Accessibility.
Default: one warm-up, three 8 MiB samples, phase counters and native sampling off.
`--binary`, `--workloads`, `--mib`, `--samples`, and `--timeout` are configurable.
Artifacts include exact source/config/ops (including local dependency patches),
binary/lockfile hashes, per-sample
process CPU, DA1 success, and optional stderr/native samples. Instrumented runs
are diagnostic evidence, not headline comparison scores. None of these commands
publishes or overwrites `BENCHMARK.md`.

### Idle probe (macOS)

[`scripts/compare/idle-probe.py`](../scripts/compare/idle-probe.py) measures what
an idle, or nearly idle, window costs:

```sh
cargo build --release --locked
python3 scripts/compare/idle-probe.py --perf                    # default scenarios
python3 scripts/compare/idle-probe.py --perf --scenarios blink,steady --seconds 60
python3 scripts/compare/idle-probe.py --perf --scenarios hover  # manual: move the pointer
```

Each scenario starts a fresh Cometty with isolated `HOME`/XDG directories and a
private config. Several tabs are opened by restoring a written session, so no
keystrokes are posted. Every tab runs the benchmark agent instead of a shell.
The agent blocks on a FIFO, so it adds no wakeups. Optional agent ops prepare the
screen: `fill` writes a screen of text, and `tick` rewrites a 15-cell status at a
fixed rate, like a TUI clock or spinner. After `--settle` seconds the probe diffs
`proc_pid_rusage` over `--seconds`. It reports retired instructions, process CPU,
interrupt and package-idle wakeups, and CPU energy (`ri_energy_nj`, which
excludes GPU and WindowServer work). Use instructions to compare revisions: CPU
time and energy depend on core placement. With `--perf` it also aggregates the
periodic counter reports that lie wholly inside the window: frames, wake sources,
supervisor polls, and per-frame phases. `--sample-seconds N` records a native call
graph *after* the window, so sampling does not perturb the counters. Artifacts go
to `target/perf/idle-<UTC>/`.

Blinking depends on window focus and `cursor.blink_timeout_ms` (default 10 s).
The default-config `blink*` scenarios therefore stop blinking 10 s after launch;
settle longer than that to measure the inactive state. `blink-forever*` sets the
timeout to `0`, so a focused window keeps blinking, which measures what blink
frames cost. `blink-forever-unfocused` launches a small second Cometty beside
the measured window, which takes focus. The summary reports each run's focus
changes and occlusions, launch included, next to pointer events and skipped
frames.

The manual `hover` scenario opens its window pinned at the top-left of the main
display. macOS sends pointer motion only to the focused window, and a window
launched just after the operator clicked elsewhere does not get focus. Click
the window once during the settle time, then keep the pointer moving over the
text. Normalize hover runs per pointer event, or compare runs whose
`pointer_event` rates match.

The probe only cleans up its own processes. Keep the probe windows uncovered,
and keep the pointer away from them unless the scenario asks for it. An occluded
window suspends rendering, and pointer motion adds frames; the counters show both.
Results are host-specific diagnostics. See [idle CPU and energy](idle.md) for
measured results and their interpretation.

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
For the steady idle cost (wakeups, frames per second, per-frame CPU), use the
[idle probe](#idle-probe-macos) instead.
