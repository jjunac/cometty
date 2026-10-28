# Throughput fixes and diagnosis

## Shipped fixes (2026-10-28)

The investigation below is preserved as the **pre-fix baseline**, not a
description of current hot paths. Implemented fixes, in priority order:

1. **Font selection and final shaping:** explicit names are honored; generic
   monospace resolves to an installed named family once. Advanced fallback and
   fixed-cell normalization remain enabled. A documented local cosmic-text 0.19
   patch bounds final shaped-run reuse at **512 entries / 2 MiB of accounted
   capacities**, includes direction in the key, bypasses giant entries, and
   invalidates on font/database/metrics changes. Repeated runs avoid fallback and
   the six-plan cache's thrashing together; the opaque plan cache itself is not
   enlarged. See [rendering](rendering.md) and
   [patch maintenance](../vendor/cosmic-text/PATCHES.md).
2. **Presentation pacing:** coalesce output-only frames at at most 60 Hz and
   leave measured presentation-cost headroom for parsing. Input urgency and
   explicit UI redraws bypass pacing; a frame's own drain requests no redundant
   successor. Final output has a deadline, not an idle polling timer. Fair drain
   allowances, queues, synchronized output and GPU boundaries are unchanged.
3. **Unicode:** inspect borrowed UTF-8 chunks with `GraphemeCursor`, with no
   boundary-check ownership/concatenation. Append tails in place using amortized
   `String` growth, charging retained capacity. Pending wrap, wide leads, flags,
   prepend/Indic/ZWJ behavior and byte splits retain the general rules.
4. **SGR:** borrow grouped parameters through a bounded stack index and use fixed
   color scratch; no per-escape nested vectors. Existing malformed/colon/color,
   underline and OSC 8 semantics remain.
5. **ASCII/history:** a guarded ordinary ASCII overwrite avoids repeated width,
   joining and wide-repair work. Packing reuses preflight text ends and up to 64
   style runs in stack scratch, scans no implicit text suffix a second time, and
   retains the lossless/general high-diversity path. No new history pool or budget
   increase was added.

### Same-host checks and tradeoffs

New baselines and A/Bs use the **same Intel i5 / Iris Plus Mac**, AC power,
release/locked Rust 1.96.1, 118×37 cells, 14-point font at 2×, 1000×700-point
window, 10k history and quiet logging. They are not comparisons to the M3 Pro
rates in `BENCHMARK.md`, which remains unchanged. Unique artifact directories
under `target/perf/fixes-*` preserve dirty source, local dependency, lockfile,
payloads and binary hashes. Headline GUI runs have profiling/counters **off**.

Final visible/minimized control: medians of three 8 MiB DA1-synchronized samples
after one warm-up per fresh process. MiB/s; these are parsing completion rates
while real presentation runs, not final-pixel completion rates.

| Workload | Visible before | Visible after | Gain | Minimized before | Minimized after |
| --- | ---: | ---: | ---: | ---: | ---: |
| ASCII | 2.44 | 21.87 | 8.9× | 13.35 | 22.67 |
| SGR | 7.99 | 48.84 | 6.1× | 31.97 | 70.97 |
| Unicode | 0.475 | 13.08 | 27.5× | 8.90 | 16.30 |
| Scroll region | 4.39 | 35.67 | 8.1× | 22.72 | 44.91 |
| Redraw | 6.58 | 48.48 | 7.4× | 26.75 | 58.71 |

Visible process CPU seconds/MiB fell, respectively, **0.439→0.055,
0.116→0.029, 2.138→0.089, 0.248→0.037, 0.114→0.028**. All DA1 checks passed.
Two earlier final repetitions gave similar direction/magnitude (for example,
visible ASCII 21.50/21.83 and Unicode 13.33/13.60 MiB/s); do not interpret
single-digit variation between short runs as a revision gain.

Final production layout medians (15 warm samples) are **1.72 ms ASCII / 2.52 ms
Unicode**, against fresh pre-fix **16.16 / 79.21 ms**. The same glyph/font set as
the named uncached lane is used; this does not claim pixel equivalence to the
old generic fallback lane. The final cache holds 86 / 30 runs and accounts
195,792 / 136,282 bytes. Unicode allocation traffic is **2.95 MB/frame** versus
54.1 MB before; this is not live memory.

Headless exact-corpus history replay (five samples, 2 MiB/feed corpus, 8 KiB
feeds): ASCII **14.51→29.77**, SGR **36.00→92.92**, Unicode **8.68→17.15**, scroll
region **23.74→54.12**, redraw **28.91→76.61 MiB/s**. CJK allocations fell
**670k→0/MiB**, SGR **378k→0/MiB**; mixed Unicode went from **848k allocations +
405k reallocations** to **101k + 61k/MiB**. Remaining real tails are retained
losslessly. These replay gains alone would not establish the GUI gains above.

A separate steady ten-second Unicode GUI diagnostic interval spent **7.84 s
parsing, 0.98 s rebuilding, 0.03 s acquiring**, with about 42 presents/s. Before,
rebuilding occupied 91–92% of elapsed time. Timing phases nest and include
preemption; these are not exact CPU percentages or scanned-out frame timings.

Intermediate A/B evidence (not substituted for the final GUI comparison):

- Named-font + bounded shaping reuse + pacing, before model changes: visible
  ASCII **2.45→8.47 MiB/s**, Unicode **0.475→5.37 MiB/s** (three 8 MiB samples).
- Warm forced production layout: ASCII **16.16→1.53 ms**, Unicode
  **79.21→2.40 ms** (15 samples). The final-run cache retained **196 / 136 KiB**
  respectively; requested Unicode allocation traffic fell **54.1→2.95 MB/frame**.
  This is layout-only evidence, not GUI presentation or total live memory.
- After the scalar/allocation fixes, an 8-second native parser sample put
  **3,151 / 6,033 parser-thread samples** in history estimate/pack. Reusing the
  bounded preflight raised ASCII/history replay **20.34→29.30 MiB/s** (seven
  samples), with zero steady allocations and identical retained history bytes.

The growable tail adds one capacity word per dense cell (**48→56 bytes** on this
host); history charges actual cell size and tail capacities. Packed payloads and
all source/history caps remain unchanged. CJK boundaries and SGR allocate nothing
in steady-state replay; real cluster tails still need storage. Long live tails
remain uncapped: amortized append does not bound boundary scans, snapshots or
shaping. See [memory](memory.md).

Loaded key-to-pixel validation was attempted but **not executed**: the host lacks
Screen Recording permission, so the committed ScreenCaptureKit harness failed
preflight. Deterministic input-bypass/fairness tests do not replace that check.
The release scheduling probe passed with identified responses and a third-tab
exit; two runs had p50 224–225 µs and p99 442–6,100 µs (including an OS-preempted
tail). It excludes OS PTYs, metadata, UI dispatch and presentation, and is not a
GUI latency result.

Separate native controls used the committed private agent and `procstat.py`, with
blinking/logging off and cleanup by owned PID only. One 20-second idle sample
used **0.031→0.033 s CPU** (0.15→0.17% of one core), about **35.6→36.0 interrupt
wakes/s**; no idle spin appeared. Energy counters were zero/unavailable, so no
energy improvement is claimed. After 10k ASCII and three successive 10k Unicode
fills, parent physical footprint was **32.1→48.9→49.5→49.3 MiB before** and
**31.5→49.7→50.7→50.9 MiB after**. This short native-memory control is not a
long-session plateau or an attribution of all driver/GPU memory.

### Correctness and memory evidence

- Final `make verify` passed: **459 ordinary tests, six opt-in probes ignored**,
  locked check, all-target Clippy with warnings denied, rustfmt and diff checks.
- New regressions compare the guarded ASCII path against scalar writes at every
  feed split with margins, insert/wrap/alternate modes, BCE, SGR and OSC 8; compare
  borrowed grapheme boundaries against contiguous segmentation over 40,000 mixed
  scalar/cluster cases; and enforce allocation-free CJK/SGR and amortized long
  appends, including pending-wrap wide leads.
- Cache tests cover complete key/direction equality, uncached complex layout,
  font-generation invalidation, entry/byte pressure, oversized **capacities**,
  cloning, allocator-requested live/peak storage and balanced drop. History tests
  retain exact styles (including blank suffixes), IDs, row eviction, bounded
  batches, lazy resize, recycling and zero-cap release.
- The ignored release memory harness passed: after another 20,000 lines, all
  sparse/dense 80/200/400/1,024-column cases reported **zero later requested-live
  growth**, with drop balance assertions. This measures Rust allocations, not
  native resources or an application RAM ceiling.
- The Iris Plus/Metal retained-glyph probe **passed**, including named-font
  cached/uncached complex-text pixel equivalence and atlas reuse/churn.
- The failed-upload probe was attempted but **failed its precondition**: Metal
  returned no usable native buffer counters. Its native byte-reclamation and
  recovery assertions were **not executed**. The ordinary production-boundary
  fake-backend tests passed, but are not a replacement for those native checks.

Primary reproducible artifacts (disposable, not documentation prerequisites):

- `target/perf/fixes-baseline-{replay,layout,gui-visible,gui-minimized}-20261027a/`:
  fresh pre-fix controls and exact source.
- `target/perf/fixes-final-gui-{visible,minimized}-20261028c/` and
  `fixes-final-validation-20261028c/`: final binary, source snapshots, raw GUI
  samples and complete verification log.
- `target/perf/fixes-final-{replay,layout}-20261028b/`: complete replay/allocation
  lanes and final forced production layout. Subsequent cache-clone hardening does
  not change the exercised shaping/parse paths; final GUI/verification uses it.
- `target/perf/fixes-final-gui-profile-20261028a/`: phase logs and native samples;
  `fixes-history-profile-20261028a/`: the parser profile selecting packing work.
- `target/perf/fixes-final-probes-20261028a/` and
  `fixes-final-validation-20261028b/`: memory, scheduling and GPU probe logs,
  including the native-counter precondition failure.
- `target/perf/fixes-native-checks-20261028a/`: script, private configs, agent
  operations, binary hashes and raw idle/footprint checkpoints.

Not executed: loaded key-to-pixel assertions, native failed-upload resource
assertions, full Instruments/energy attribution, the complete manual interaction
checklist, other-platform/M3 Pro validation, and a full cross-terminal comparison.

Remaining bottlenecks: unique-run fallback/shape-plan construction, synchronous
viewport layout and OS surface acquisition on the UI thread, scalar grid work,
general high-diversity packing, unbounded live clusters and synchronous history
search. There is still no application-wide RAM or GUI-latency guarantee.

## Investigation baseline (before fixes)

Investigation of `BENCHMARK.md` at `a6d04f7` (terminal code unchanged from the
reported `40cac47`). **These are measurements on 2026-10-27, not performance
guarantees. The investigation itself installed diagnostics only; the fixes above
were implemented afterwards. Code-path descriptions below refer to that baseline.**

### Investigation outcome

The largest GUI throughput loss is **synchronous text shaping/layout competing
with parsing on the UI thread**, amplified by font fallback and the scheduling
policy. It is not primarily vte's byte state machine, the OSC input guard, a
history allocation leak, or PTY bandwidth. There are also substantial independent
model costs: ASCII's per-character mutation path and history packing, Unicode
cluster allocation/resegmentation, and SGR parameter allocation.

“DA1-synchronized; parse, not presentation” describes the **completion condition**.
It does not isolate parsing from concurrent rendering work. The final reply can
be sent before final presentation while earlier frames have already stalled
parsing many times.

## Controls and measurement boundaries

The original comparison used an M3 Pro and a 120 Hz display. This investigation
used a **2 GHz quad-core Intel i5 MacBook Pro, Iris Plus, macOS 26.7.1, AC power,
Rust 1.96.1**, native x86-64 release builds. Do **not** compare these absolute
rates to the M3 Pro table as a regression or improvement.

- Real GUI: identical release binary/config, fresh private process per workload,
  exact comparison agent, 118×37 cells, 1000×700 points, font 14 at 2×, 10k history,
  no shell/session restore/logging. One warm-up, three 8 MiB samples per case.
- Visible versus minimized changes only OS window state. Both retain the real
  PTY, worker queues, fair draining, terminal model/history, and DA1 response.
  Rate table below has phase counters and native sampling **off**.
- Separately collected phase counters and 8-second / 1 ms native samples locate
  work; their elapsed proportions are not exact process CPU percentages. Warm-up
  and startup can appear in native samples. Nested phases are not summed.
- Headless replay uses identical payload generators and 8 KiB feeds, without
  PTY/window/rendering. Allocation measurements are separate from timing. The
  test allocator's inactive TLS checks are absent from the application binary.

### Same-binary real-PTY control

MiB/s, medians of three samples:

| Workload | Visible | Minimized | Minimized / visible |
| --- | ---: | ---: | ---: |
| ASCII | 2.52 | 14.19 | 5.6× |
| SGR | 8.15 | 32.38 | 4.0× |
| Unicode | 0.477 | 9.01 | 18.9× |
| Scroll region | 4.40 | 22.86 | 5.2× |
| Redraw | 6.59 | 27.36 | 4.2× |

Minimized rates are close to headless model replay on this host. This is a
causal presentation-path control, not an optimization: a minimized terminal
does not show output. It does establish that raising PTY queue/chunk limits
alone cannot remove the largest visible-window penalty.

## 1. Text layout and font fallback dominate visible scrolling

Sources: `src/renderer/text.rs::{font_family,build_buffer_line,rebuild_buffer}`,
`src/renderer/mod.rs::render_frame`, `src/app/redraw.rs::on_redraw`.

- ASCII's first ten-second diagnostic interval spent **6.84 s rebuilding text**
  versus **1.35 s parsing**. Almost all 37 rows were reshaped on each changed
  frame. Native main-thread samples put 74% in `Buffer::line_layout` and 56%
  in its descendant `FontFallbackIter::next`.
- Steady Unicode intervals spent **91–92% of elapsed time rebuilding**, versus
  5–6% parsing. Native main-thread samples put 88% in final layout, including
  36% in constructing harfrust shape plans. Natural-advance cache misses were
  **zero** during these steady intervals. More entries in `AdvanceCache` will
  not fix final shaping.
- Snapshot copying, viewport hashing, title/process queries, and upload/submit
  work were small next to layout in these scrolling cases. The bottleneck is
  CPU text preparation, not evidence of slow GPU glyph execution.

The row cache is keyed by **screen position and content hash**, not retained
layout for a moved row. Scrolling changes almost every position. Each changed
row gets a new `BufferLine` and an explicit, synchronous `line_layout`; cached
cluster advances do not reuse its final shaping/font fallback.

### The font-selection policy is a concrete multiplier

`font_family()` returns `Family::Monospace` for both `"monospace"` **and arbitrary
names such as `"Menlo"`**. Merely editing the family config to `Menlo` therefore
does not select a named font. `FontSystem::new()` from locked cosmic-text 0.19
sets its generic monospace mapping to `Noto Sans Mono`; that family was absent
among this host's 787 loaded faces. Fallback eventually selected Menlo for ASCII,
but repeatedly performed generic monospace selection work for shaped runs.

The layout-only diagnostic forces the same actual captured viewport through
production helpers at the GUI's physical metrics. Fifteen warmed samples;
milliseconds per complete viewport:

| Font-selection lane | ASCII | Unicode |
| --- | ---: | ---: |
| Current generic policy | 16.36 | 79.01 |
| Generic mapping explicitly set to Menlo | 13.30 | 76.40 |
| Explicit `Family::Name("Menlo")` attributes | 3.32 | 21.52 |

ASCII uses **Menlo-Regular in all three lanes**, with identical advanced shaping,
features and cell metrics. So this is not “a faster font” or basic-shaping mode.
Resolving the missing generic default helps, but **does not eliminate** the
generic monospace fallback algorithm's cost.

For Unicode, the named lane also changes a fallback from `GB18030Bitmap` to
`ArialUnicodeMS`; it is not a pixel-equivalence proof or a ready-to-ship fix.
The current lane allocates **241,831 times plus 52,236 reallocations and requests
54.1 MB per rebuilt Unicode viewport**. Those are allocation traffic, **not live
memory or footprint growth**. The named lane requests 13.9 MB.

### The Unicode shape-plan cache thrashes

Locked cosmic-text's shape-plan cache has only **six entries**, keyed by font
and shaping inputs. A native trace attributes 36% of main-thread samples to
constructing plans, despite warmed fonts and advances. To distinguish actual
cache thrashing from inherently expensive glyph shaping, an isolated source/
dependency copy changed **only** `NUM_SHAPE_PLANS` from 6 to 64:

| Unicode viewport experiment | 6 plans | 64 plans |
| --- | ---: | ---: |
| Current generic policy, ms | 78.12 | 40.58 |
| Current policy, requested allocation MB/frame | 54.10 | 15.29 |
| Named Menlo policy, ms | 21.60 | 13.03 |

Actual glyph font sets were unchanged within each cache-capacity A/B. This
confirms a second, independent shaping multiplier: the mixed-script/font working
set outgrows the tiny plan cache and repeatedly rebuilds plans. It is **not**
just cold font discovery or natural-advance cache misses. The experiment changed
neither the repository's dependency declarations/lockfile nor the Cargo registry;
the copied dependency lives only under ignored `target/perf/`.

This is not a shipped fix or a retained-memory measurement. A cache-policy
change needs working-set/byte-budget and visual-correctness checks; blindly
enabling an additional unbudgeted run cache is not a solution.

## 2. Rendering and drain scheduling are coupled

Sources: `src/app/scheduling.rs::DrainSchedule`, `src/app/pty_io.rs::drain_pty`,
`src/main.rs::{new_events,about_to_wait}`, `src/renderer/mod.rs::render_frame`.

One event-loop turn permits 256 KiB / 4 ms of draining. Active output requests
another redraw, which synchronously rebuilds/layouts text and acquires a surface
before the next turn can parse more. A 4 ms parsing slice followed by tens of
milliseconds of layout gives parsing a small share of wall time despite a full
output queue. The drain budget protects responsiveness but is not a presentation
work budget. Fairness is not the defect; unbudgeted work between drain turns is.

For the less layout-heavy redraw workload, the Intel host also exhibited
surface-acquisition waits: a separate 10-second interval spent about **21% parsing,
25% rebuilding, and 37% acquiring**. A native sample likewise showed acquisition
as a large main-thread wait. That is a host/display/backend-specific additional
limit, **not** proof that the original M3 Pro/120 Hz run had the same wait ratio.

Keep presentation pacing separate from parsing progress. Do not “fix” this by
removing fair input/PTY budgets, disabling complex-text shaping, permanent
polling, or unbounded output queues. Recheck loaded key latency after any change.

## 3. ASCII model work: per-character handling plus compact-history scans

Sources: `src/term.rs::Perform::print`, `src/grid/edit.rs`,
`src/grid/history/{storage,packed}.rs`.

In two headless exact-corpus replays, ASCII is about **14–15 MiB/s with history**
versus **23–24 MiB/s without it**. An overwrite-only control reaches 26–28 MiB/s.
No steady ASCII heap allocations were recorded. The loss is computation, not
an allocator leak:

- Native parser-thread samples attributed about **35% to compact-history
  estimation + packing**, and 52% to the printable-character path. These are
  disjoint branches in that sample; newline includes the history work.
- Sealing scans a row batch to estimate it, then scans again to pack text and
  style runs, including untouched blank columns. This happens continually after
  saturation, even when row/chunk buffers are successfully recycled.
- The ordinary ASCII path still enters append-target/grapheme/width logic and
  general narrow/wide/insert/overwrite mutation per scalar. Common nonblank
  ASCII pairs can call `char_width` **four times per character** through nested
  joining checks and fresh insertion. Cell/row/grid revisions and styles are
  updated at scalar granularity. There is no bulk ASCII grid-write path.
- The OSC guard with counted callbacks handles ASCII at hundreds of MiB/s;
  its byte scanning is not the explanation for 15 MiB/s model throughput.

Compaction is an intentional memory/CPU tradeoff. Preserve lossless styles,
row IDs, capacity accounting, bounded batches and lazy resize; disabling history
is a diagnostic control, not a recommendation to discard users' scrollback.

## 4. Unicode model work: allocating even to reject a join

Sources: `src/grid/unicode.rs::{continues_cell,continues_cluster}` and
`src/grid/edit.rs::{append_to_prev,append_to_cell}`.

- A nontrivial boundary builds `lead.cluster()` as an owned string, then copies
  it into another string with the incoming scalar and counts graphemes. Adjacent
  independent CJK characters pay this even when no append occurs.
- Actual combining/ZWJ appends rebuild the whole cluster, append, collect its
  tail into another string, and convert to a boxed string. Growing and shrinking
  buffers generates reallocations too. Repeated long appends are quadratic;
  the comparison's normal short clusters already incur substantial overhead.
- Mixed Unicode replay recorded about **848k allocations + 405k reallocations
  per MiB**. Disabling history leaves essentially the same counts and only a
  modest throughput improvement. Pure CJK still records about **670k allocations
  per MiB**, despite having no cluster tails to retain.

This is independent of the larger GUI shaping penalty. Allocation-free boundary
inspection and less copying on real appends need Unicode/pending-wrap/flag/
prepend equivalence tests, not an assumption that every non-ASCII scalar starts
a new cell.

## 5. SGR model work: nested vectors for every escape

Source: `src/term.rs::csi_dispatch('m')` and `src/grid/style.rs`.

The parser already provides borrowed parameter groups. Every SGR clones them
into `Vec<Vec<u16>>`, then immediately drops those vectors. A typical `38;5;n`
allocates the outer vector plus three inner vectors; the corpus does this for
both foreground and background on every cell.

Replay recorded about **378k allocations/MiB**. Native parser-thread sampling
attributed about **39% to constructing those vectors and 16% to dropping them**,
versus roughly 5% in the actual SGR/palette application. Borrowed group handling
is therefore a much more direct target than tuning palette arithmetic.

SGR's higher MiB/s than ASCII is not evidence of cheaper useful work: this corpus
prints approximately **22 times fewer characters/rows per input byte**. Compare
printed cells, lines and escape dispatches as well as wire bytes.

## Recommended order and outstanding verification

1. Fix/measure font-family resolution, generic fallback selection and shape-plan
   cache thrashing; retain advanced complex-text behavior and bound retained
   cache storage. Avoid repeatedly laying out unchanged/moved content where safe.
2. Decouple presentation cost/pacing from parsing progress, including surface
   waits. Preserve per-turn fairness, input latency, sync-output and GPU lifetime
   boundaries. Re-run visible throughput **and loaded key-to-pixel latency**.
3. Remove SGR temporary ownership and Unicode boundary/append churn.
4. Optimize ASCII scalar writes and history packing using profiles, without
   undoing compact-history memory/resize semantics.

The first two dominate the measured visible-window deficit. Parser-only changes
cannot recover time spent in final layout/surface waits. Conversely, fixing
rendering exposes the remaining model ceilings; both layers need attention to
approach the fastest terminals.

No other terminals were re-ranked on this Intel host, no GPU execution trace was
collected, and no fixes were validated on the original M3 Pro. Font-selection
ablations are headless layout experiments, not screenshot correctness checks.
The original `BENCHMARK.md` is unchanged.

## Reproduction and artifacts

Use the committed [diagnostic commands](performance.md#diagnosing-throughput-rather-than-ranking-terminals).
The new harnesses preserve their source/config/payloads so a dirty measured tree
is identifiable. Primary local artifacts (disposable, not required by the code):

- `target/perf/throughput-gui-visible-clean/` and `throughput-gui-minimized-clean/`:
  uninstrumented same-binary real-PTY controls.
- `target/perf/throughput-gui-1/`: initial native samples/phase logs.
- `target/perf/throughput-gui-frame-phases/`: additional redraw/acquisition phases.
- `target/perf/throughput-investigation-final/`: exact-corpus replay/allocation
  controls with history saturated before timing; `throughput-investigation-replay-1/`
  is the initial replay; `throughput-native-samples/` holds parser-thread samples.
- `target/perf/throughput-layout-fonts-final/`: actual-viewport font-selection
  ablations, 15 warmed samples and actual glyph font names.
- `target/perf/plan-cache-{6,64}.log`, `plan-cache-source/`, and
  `plan-cache-vendor/`: isolated dependency-capacity ablation and its source.

Keep artifacts externally if needed; future contributors can regenerate them
with the harness. Re-measure before using these host-specific proportions to
choose a different revision's next optimization.

Initial investigation verification: `make verify` passed (447 ordinary tests, six explicitly ignored
probes; locked check, all-target Clippy with warnings denied, rustfmt, and diff
checks). Both new ignored release diagnostics and real-GUI control modes were
run explicitly. GPU correctness probes and post-optimization latency checks
were not run; no renderer optimization is being shipped here.
