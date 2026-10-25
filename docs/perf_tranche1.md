# First performance tranche — implementation results

Date: 2026-10-25. The original [study](perf_study.md) is unchanged.

## Shipped scope

- Scrollbar repaint decisions now distinguish an actual opacity change, a
  future fade deadline, and idle. Fades use elapsed time; settled hover, drag
  and history viewing request no immediate frame. Cursor timers are disabled
  for steady/hidden/history cursors and suspended windows. Known occluded,
  minimized or zero-sized windows skip visual preparation, while PTY parsing,
  replies and exit handling continue independently. egui repaint requests and
  explicit trailing-search deadlines remain active when visible.
  Transient surface timeouts/loss have their own backed-off retry deadline,
  instead of relying on the next blink; occlusion adds no surface-retry timer.
- IL/DL, region scrolling, downward scrolling and alternate-screen SU rotate
  row storage once, cap counts to affected rows and use BCE fills. Full-screen
  main-buffer SU retains its history-producing semantics.
- Printable ASCII pairs bypass allocating grapheme checks. Terminal writes
  resolve continuation once; direct grid writes remain append-aware. Unicode
  slow paths and the parser-swap pattern are preserved.
- OSC number/payload metadata is borrowed and routed once. Only the selected
  handler joins payload fragments; summaries copy a bounded prefix, including
  for malformed long numbers. OSC 52 remains redacted. Clipboard encoded-length
  checks precede the whitespace-filtered allocation.
- Word regex compilation and validation share a bounded, one-entry-per-thread
  cache, including failed edits and the default fallback.
- Search projection binary-slices visible matches and is cached per tab by
  result generation, current match, viewport and dimensions. Rendering shares
  the cached view rather than deep-cloning it. Closing search releases match
  capacities above 1 MiB. Background refresh does not request scrolling.
- A permanent ignored release benchmark and `COMETTY_PERF=1` aggregate phase
  counters are documented in [benchmarking.md](benchmarking.md).

## Release measurements

Before: production source `6a058d1737991f028b19d20ad17de9242d500b0d` plus the
initial benchmark-only harness. After: this uncommitted tranche, with exact
source/assets/lockfile snapshots saved by the benchmark script. The measurement
snapshot precedes the final surface-retry-only hardening, which these headless
cases do not exercise. Same x86_64
macOS host, Rust 1.96.1, standard Cargo release profile, one warm-up and seven
batch means. Allocation counting is a separate call. These are exploratory
headless measurements, not frame-time, GPU, power or display-latency claims.

Quiet logging (`RUST_LOG=off`, in-app recording off):

| Case | Before | After | Allocation calls before → after |
| --- | ---: | ---: | ---: |
| Feed 256 KiB ASCII | 83.10 ms | 12.40 ms | 1,052,509 → 1,311 |
| Feed CRLF corpus | 83.58 ms | 18.05 ms | 1,019,147 → 3,277 |
| IL 65,535, 200×60 grid | 47.62 ms | 29.84 µs | 65,535 → 0 |
| DL 65,535, 200×60 grid | 46.43 ms | 29.81 µs | 65,535 → 0 |
| OSC 8 corpus | 10.77 ms | 3.37 ms | 136,500 → 19,500 |
| Ignored 1 MiB OSC | 2.89 ms | 2.74 ms | 8 → 0 |
| Project 2,001,741 matches | 3.59 ms | 80.53 µs | 60 → 60 |
| Expand word, warmed pattern | 19.79 µs | 0.55 µs | 131 → 3 |

ASCII's remaining allocations are scroll-row management, not character
continuation checks. IL/DL cumulative requested bytes fell from 629,136,000
to zero. The ignored OSC's parser buffer is warmed: zero allocation here does
**not** mean raw OSC accumulation is bounded. Standalone projection still
allocates visible-row ranges; the production projection cache hits allocate
nothing (about 0.014 µs per warmed handle lookup in this microbenchmark).

Default in-app recording (`cometty=debug,warn`), stderr off, measured separately:

| Case | Before | After |
| --- | ---: | ---: |
| ASCII | 85.33 ms | 13.26 ms |
| OSC 8 corpus | 23.21 ms | 14.53 ms |
| Ignored 1 MiB OSC | 3.75 ms | 2.79 ms |

The ignored OSC requests about 1.6 KiB with default logging now, versus about
5 MiB before. Useful OSC diagnostics are still recorded. Small timing
differences in unchanged paths are noise, not optimization claims.

Dense full-history search remains approximately 150 ms and its result vector
still has a 48 MiB capacity in this workload. Projection is now visible-only
and cached; scanning is **not** incremental. Decomposed-text line construction
also remains about 7 ms and is intentionally deferred to tranche two.

Local raw artifacts (ignored by Git):

- `target/perf-before-{quiet,default}.log`: pre-change samples and allocation counts.
- `target/perf/first-tranche-final/`: post-change samples, environment metadata,
  source patch, exact Rust/assets/Cargo snapshot and benchmark script.
- Cargo.lock SHA-256: `3aa2ddec3e8f3fbbd7372b09aeb5778dfbb45ee22ea7701bd3f8154a73f7c048`.

## Verification and remaining manual checks

- Debug and release tests: 349 passed, one opt-in benchmark ignored.
- `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.
- Search/cache tests also pass with phase counters enabled.
- Deterministic coverage includes settled repaint decisions, fade deadlines and
  elapsed-time behavior; huge bounded counts and row-storage reuse; allocation-
  free ASCII; mixed Unicode/control equivalence at every byte split; OSC
  fragmentation/redaction; regex cache invalidation; visible projection edges,
  cache reuse, dense memory and release-on-close.

No GUI was launched. Still manually verify stationary-history/hover frame
counts, fade appearance, steady/hidden cursors, minimize/restore and occlusion,
plus tooltip animations, terminal Tab/IME handoff and find-bar focus. Actual
CPU/GPU activity and energy gains require display-backed traces. Fair PTY
scheduling, parser-level control-string/cluster bounds, incremental scans,
shaping allocation changes and retained glyph preparation remain deferred.
