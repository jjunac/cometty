# Second performance tranche — changed-frame cost

Date: 2026-10-25. Baseline: `bd562f5` (first tranche, clean tree).
The original [study](perf_study.md) remains unchanged.

## Implemented scope

### Line construction and cluster advances (#5)

- Find the final relevant lead before constructing text. Append `ch`/borrowed
  `extra` directly into one pre-sized row string; use reusable byte/column/spacing
  descriptors instead of a string allocation for every cell. Move the string and
  the finished attribute list into `BufferLine`, without cloning the row text.
- Keep advanced shaping, fallback, fixed-cell spacing, effective foreground,
  wide-continuation skipping, block-to-space substitution and whitespace rules.
  An independent pre-tranche construction reference checks exact text/attributes
  and layout fingerprints for cold and warm paths, including mixed styles,
  CJK, decomposed accents, ZWJ/RI emoji and fallback scripts.
- Cache multi-character advances with borrowed lookups in four weight/style maps.
  Font/metrics changes clear the cache generation. Limits: 1,024 cluster entries,
  128 KiB of key bytes, 512 bytes per key; single-character entries are bounded at
  4,096. At capacity a generation is retired, not an unbounded LRU maintained.
  Descriptor storage is retained only up to 4,096 slots. A reusable measurement
  buffer retains shaping/layout storage between misses; oversized cluster
  measurements and their temporary buffers are not retained.

### Retained glyph preparation (#6)

- Rebuilds report whether any shaped lines/layout inputs were replaced. A shared
  buffer revision, independent of tab-local grid counters, advances only then.
- Prepared instances are keyed by that revision, surface bounds, tab-bar offset
  and default text color. Cursor, selection, search-current, link-underlining and
  chrome-only changes do not force preparation. Resize, font/DPI/theme changes
  still invalidate the relevant shaping inputs; different tabs are always
  compared even when their grid counters match. Identical shaped text can reuse
  preparation across tabs if placement/color also match.
- The atlas is private to the single terminal `TextRenderer`. Keep its glyphs
  pinned during reuse; call `trim()` only immediately before replacing the entire
  preparation, not after every frame. No atlas allocation/write occurs on reuse.
  A failed preparation leaves no valid prepared key. Surface loss also clears
  the key; deferred preparation survives failed surface acquisition.
- Text is still drawn onto every successfully acquired surface. This avoids
  repeated preparation/uploads, not the draw itself or background/UI work.

### Fair, coalesced PTY scheduling (#8)

- Alternate an active-tab chunk with a chunk from a persistent round-robin
  cursor. Each visit reads at most one 8 KiB chunk. The order survives deadline
  exhaustion, tab switches and removals, so even an expensive active callback
  cannot restart priority at the active tab forever.
- `new_events` resets one 256 KiB / 4 ms drain allowance per event-loop turn.
  User-event, redraw and pre-wait drains share it; elapsed drain-call time is
  accumulated, rather than allowing each callback another 4 ms. This is still
  cooperative: a parser callback, metadata/clipboard work or resize can overshoot.
  Parser-level cluster/control-string/REP bounds remain separate unfinished work.
- Coalesce notifications per session until the queue is observed empty. Clear
  and recheck when re-arming so a racing send cannot be stranded. The existing
  bounded 128-chunk channel, reader backpressure, input ordering and reply/exit
  order are unchanged. No sleeps or unbounded output queues were introduced.
- Parsing continuation is independent of presentation, selection dragging,
  synchronization, blinking and occlusion. Background output neither dirties the
  active view nor clears its selection. Repaint chrome only when displayed labels
  change; metadata remains queried once per touched tab/burst, off the render path.
- All redraw sources respect synchronized output. A missing end marker releases
  presentation once after 150 ms; parsing/replies/input continue meanwhile. This
  briefly defers chrome presentation too, rather than painting intermediate grid
  state through a blink or UI redraw. The timeout does not become a frame loop.
- Stable session IDs key snapshots and diagnostics across index changes/removal.
  Exits are sorted before removal, and OSC 52 operations are routed before indices
  shift. Slow-drain logs are limited to one per two seconds.

## Release measurements

Same x86_64 macOS host, standard release profile, local monospace font at 16px,
200-column rows. The existing runner uses one warm-up and seven batch means;
allocation counts come from a separate call. Before/after runs save exact source,
assets, lockfile and environment metadata. Measurements are exploratory and
headless, not GUI frame times, GPU timings or display-latency claims.

Quiet logging (`RUST_LOG=off`, in-app recording off):

| Operation | Before | After | Allocation calls before → after |
| --- | ---: | ---: | ---: |
| Construct ASCII row, warm advances | 18.73 µs | 7.05 µs | 211 → 7 |
| Construct + lay out ASCII row | 123.90 µs | 118.42 µs | 666 → 462 |
| Construct decomposed-accent row, warm advances | 9,837.09 µs | 7.70 µs | 11,811 → 7 |
| Construct + lay out decomposed-accent row | 7,122.90 µs | 204.27 µs | 12,664 → 860 |

Warm construction requests 1,912 bytes for ASCII and 2,312 bytes for decomposed
accents, versus 19,712 and 1,538,848 bytes before. Both now request zero
reallocations. Final cosmic-text layout still allocates and costs more than
construction; the ASCII layout samples vary enough that no strong wall-clock
speedup claim is warranted there. Unchanged parser/search paths remain noise-level
comparisons, not new optimization wins.

Default in-app recording (`cometty=debug,warn`), stderr off, measured separately:

| Operation | Before | After |
| --- | ---: | ---: |
| Construct ASCII row | 27.43 µs | 7.01 µs |
| Construct decomposed-accent row | 7,468.23 µs | 7.58 µs |
| Construct + lay out decomposed-accent row | 7,823.83 µs | 211.59 µs |

New cases (quiet logging, no pre-tranche comparison): clearing the advance and
measurement cache per operation costs 38.22 µs for repeated ASCII and 46.67 µs
for repeated decomposed accents. A 200-column row of repeated family-ZWJ emoji
constructs in 5.86 µs warm / 122.20 µs with cold advances, but final row layout
still takes **11.45 ms** and makes **42,044 allocation calls** on this host.
The cluster cache does not solve all complex-text shaping cost.

The identified-request scheduling probe processed 128 samples after eight
warm-ups while an earlier queue was saturated and a later queue exited. Its saved
run reports p50 **0.581 ms**, p95 **0.934 ms**, p99 **10.733 ms**, maximum
**10.908 ms**, with 1,146,880 flood bytes parsed. This is one exploratory run,
with no equivalent pre-tranche marker baseline. It excludes OS PTYs, metadata,
event-loop dispatch, rendering and presentation; it is not an active-window
input-to-visible p99 or a 4ms end-to-end latency guarantee.

Local raw artifacts (ignored by Git):

- `target/perf/tranche2-before-20261025/`: baseline source/metadata and both modes.
- `target/perf/tranche2-after-20261025/`: post-change source/metadata, both logging
  modes and `scheduling.log`. The source snapshot includes the test helpers.

## Diagnostics and checks

[benchmarking.md](benchmarking.md) documents the existing release runner plus
cold/warm/ZWJ shaping cases, an identified-request scheduling probe and an explicit
offscreen GPU pixel-equivalence check. Opt-in phase diagnostics now include
preparation reuse, advance misses, coalescing, drain yields, queue high-water/wait
and bounded approximate per-tab percentile histograms. The old application-global
key/echo timestamps are replaced by same-tab probes with honest names:
input-to-first-output and output-to-present. They are not verified echo or display
latencies; unrelated output in the same tab can still arrive first.

Ordinary tests cover allocation scaling, cache bounds/style/generation behavior,
Unicode layout equivalence, shared budgets, deterministic service gaps, tab
removal, notification races/order, background selection isolation and continued
parsing/replies while dragging, occluded and synchronized. No wall-clock speed
threshold is a CI gate.

Verification in this session:

- Debug tests with `COMETTY_PERF=1`: **366 passed**, three opt-in checks ignored.
- Release tests: **366 passed**, three opt-in checks ignored.
- `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`,
  `git diff --check`: passed.
- Release headless benchmark in default and quiet recording modes: passed.
- Explicit release identified-request scheduling probe: passed.

The opt-in GPU check was attempted in this session. Both normal and downlevel
device creation fail with `RequestDeviceError: Device(Lost)` before rendering.
Pixel equivalence and GUI behavior therefore remain **unverified here**, not
reported as passing. No GUI binary was launched.

## Trace checkpoint (#7 / #9 / #4)

No new display-backed application trace was collected. Accordingly, this tranche
does **not** speculatively add row-identity/damage caches, retained background
geometry or incremental search. Full viewport hashing/snapshots, background
painting and synchronous full-history search remain as documented in the study.

On a machine with a working display/GPU, run the explicit GPU check and collect
separate release Time Profiler / Allocations traces with `COMETTY_PERF=1` using
`scripts/profile-macos.sh`. Compare one-row versus full-screen updates, repeated
decomposed/ZWJ text, cursor/selection/chrome-only frames, a later active tab under
earlier-tab floods, and dense search over 10k history rows. Capture default and
quiet logging separately. Check IME/Tab/find focus, DPI/resize/tab-bar placement,
atlas churn, minimize/restore and synchronization recovery. Choose the next
architectural optimization from those traces, not from independent phase medians
added together or headless timings presented as input-to-visible latency.
