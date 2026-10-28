# Rendering

Sources: [`renderer/mod.rs`](../src/renderer/mod.rs),
[`text.rs`](../src/renderer/text.rs),
[`background.rs`](../src/renderer/background.rs),
[`overlay.rs`](../src/renderer/overlay.rs), and
[`frame.rs`](../src/renderer/frame.rs).

One shared renderer uses wgpu for the surface/pipeline, cosmic-text for layout,
glyphon for glyph preparation/drawing, and egui for chrome. A new surface frame
still draws terminal text even when its layout/preparation can be retained.

## Cache layers and invalidation

Do not use “grid unchanged” as a universal cache key. Each layer has different
inputs and resource-lifetime requirements:

| Layer | Identity / invalidation |
| --- | --- |
| Shared app snapshot | Stable tab ID + grid version; each visible row compares `(row ID, revision, projection revision)` before copying |
| Shaped rows | Per-position hashes of cluster text, width, bold/italic, effective foreground; clear on metrics/theme/viewport changes |
| Natural advances | Cluster text + weight/style within a font/metrics generation |
| Final advanced shaping runs | Text + full attributes (including spacing/features/color) + direction, within the shared font database generation; 512 entries / 2 MiB accounted capacities |
| Prepared glyph instances | Shared text-buffer revision + surface width/height + tab-bar offset + default text color |
| Background/chrome | Repaint from current cursor, decorations, selection/search/link state and UI; no retained base-geometry cache |

On tab switch, force a content comparison even if both tabs have the same numeric
grid version. Identical shaped text may reuse preparation if placement/color also
match. Do not retain a shaped line for all history rows to optimize scrolling:
current layouts are viewport-sized and tied to positions, not a scroll-aware cache.
The bounded run cache is position-independent: moved/repeated text can reuse final
shaping/fallback, but layouts and GPU preparation still follow their own keys.

The snapshot copies only changed visible rows into reusable vectors, decoding
compact history directly. A changed grid version can still hash every visible
cell; row-keyed snapshot reuse is not an end-to-end damage-tracking implementation.

## Fixed-cell shaping

Physical metrics derive from configured font size × DPI scale, with configurable
line-height/cell-width factors (defaults 1.25 / 0.602). Text does not wrap inside
cosmic-text. Advanced shaping/fallback stays enabled for complex clusters; kern,
standard/contextual/discretionary ligatures, and contextual alternates are
disabled to prevent neighboring terminal columns merging or drifting.

Explicit font names stay `Family::Name`, rather than silently becoming generic
monospace. Generic monospace resolves once to an installed named monospace family:
the database's mapping if available, then platform preferences, then a stable
installed-family fallback. An empty font database retains generic fallback.
This avoids cosmic-text's expensive generic-monospace selection on every run;
changing only the database's generic mapping is not equivalent. Named fonts still
use advanced fallback, including CJK, emoji and complex scripts. A fallback face
may differ from the former generic policy; this is not pixel equivalence to that
old policy or a cross-platform Menlo requirement.

Line construction:

1. Locate the last relevant lead before allocation; skip wide continuations.
2. Append lead scalar/borrowed tail into one pre-sized string and reuse bounded
   byte/column descriptors; do not allocate a string per cell.
3. Measure natural cluster advances, normalize to one/two cell widths through
   per-span letter spacing, and split attributes by effective foreground,
   bold/italic, and spacing. Ignore nonfinite fallback measurements safely.
4. Move text/attributes into a `BufferLine` and explicitly call `line_layout` for
   replaced lines. Directly replacing `buffer.lines` bypasses ordinary dirty
   flags; `shape_until_scroll` alone can leave no drawable glyphs.

Underline/strike/overline/link IDs are excluded from text hashes: they paint as
geometry. Effective foreground includes inverse/dim; changes to it do reshape.
Do not globally switch to basic shaping as a performance shortcut.

`AdvanceCache` bounds single-character entries at **4,096**, cluster entries at
**1,024**, total cluster keys at **128 KiB**, and individual keys at **512 bytes**.
Separate style maps permit borrowed lookup. Capacity retires a generation rather
than maintaining an unbounded LRU. Font/metrics changes clear it. Descriptor
scratch retains at most 4,096 slots; oversized measurement buffers are discarded.
These limits do **not** bound live cell tails or final complex-text layout cost.

Final shaping uses a small, documented local cosmic-text 0.19 patch (shared with
glyphon), not a modified Cargo registry: see
[`vendor/cosmic-text/PATCHES.md`](../vendor/cosmic-text/PATCHES.md). Repeated runs
reuse the advanced result, avoiding both fallback work and thrashing of the
upstream six-entry harfrust plan cache. That opaque plan cache is not enlarged.
The run cache has explicit **512-entry / 2 MiB** admission, charging vector
capacities, text, nested attributes/features and glyphs. Oversized entries bypass
retention; capacity pressure retires a generation. Hash matches also compare full
keys. Font database mutations and font/metrics changes clear it; theme colors are
part of the key. No all-history layout cache, age-only retention, or GPU resource
is added. Unique runs still incur fallback/plan construction. The budget excludes
font/native resources, allocator overhead, and transient shaping work.

## Decision: retain preparation while keeping atlas pins

`rebuild_buffer` reports whether shaped lines/layout inputs were replaced; only
then does the shared `text_revision` change. Cursor blinking, selection,
search-current, link hints, and chrome-only updates need not prepare/upload
identical glyph instances again.

The atlas is private to this single terminal `TextRenderer`. glyphon's `trim()`
clears the in-use pins; **it does not shrink textures**. Call it immediately
before replacing the **entire** preparation, not on reuse frames. Reuse may not
introduce atlas allocations/writes that could evict a live glyph slot. A failed
prepare or returned render error invalidates the key; surface loss also clears it.
Future shared atlases/multiple text renderers need a new lifetime policy.

## Pixel geometry and composition

Backgrounds, block elements, decorations, hints, and cursor shapes use custom
WGSL quads. Each rectangle emits six 20-byte vertices. GPU buffer capacity grows
on overflow and shrinks below quarter utilization; CPU scratch currently retains
its high-water capacity.

- Snap both cell edges from the grid origin to device pixels. Shared adjacent
  edges avoid seams; include fractional tab-bar/DPI offsets in row snapping.
- Wide leads cover both columns for selection, search, and block cursors.
- Background priority is **active search > selection > other search > cell
  background**. Blocks/decorations/hints follow, then cursor geometry; text and
  egui paint afterward.
- U+2580–U+259F block elements use cell-filling rectangles where supported, with
  spaces shaped in place of their glyphs. U+2591–U+2593 shades retain font glyphs.
  Font line boxes alone leave gaps in stacked terminal graphics.
- Curly/dotted/dashed underline styles are stored distinctly but currently paint
  as a single strip; double underline has two strips.
- Background clear/geometry colors must be linear-light values for the sRGB
  surface. Plain byte/255 colors would double-apply the transfer curve.
- Hint underlines depend on modifier state, not only grid versions. Existing
  application underlines retain their styling.

The macOS Intel Iris Plus/Metal fullscreen workaround deliberately crops the
surface **one pixel narrower** when fullscreen/display-sized. Exactly
display-sized cleared drawables can show stripes (`wgpu#3415`); geometry remains
correct. Keep `shrink_fullscreen_surface` and `is_fullscreen_like` coordinated;
the one-pixel mismatch is not an off-by-one defect.

## Decision: every returned frame outcome has one upload boundary

Queue writes allocate staging resources **before submission**. Repeated surface
failures must not retain a new never-submitted upload on every attempt. Startup
and resize update CPU layout immediately, but background/viewport GPU writes wait
for successful acquisition.

```text
frame::run
  CPU text diff + egui input/actions/platform output/repaint requests
  apply egui texture sets (even if acquisition will fail)
  acquire surface
  upload viewport/background → prepare if dirty → encode
  submit callback commands + complete render commands → present
cleanup for every returned success/skip/error:
  if uploads remain and device is live: submit([])
  free egui textures after submission
```

Mark upload-capable calls **before** invoking dependencies: glyph preparation
may write part of an atlas and then fail. A normal submission clears pending
uploads, so cleanup does not submit twice. An upload-free skip does not submit;
an error drops incomplete draw commands rather than submitting them for cleanup.

The overlay returns owned texture deltas instead of destroying/uploading textures
itself. Preserve full/partial patch order; keeping only the last patch is wrong.
Apply sets on skipped attempts rather than building a deferred CPU backlog.
Frees must follow normal/upload-only submission because `free_texture` destroys
the native resource, not merely a reference. Cleanup does not wait for the GPU.

Confirmed **device loss** is distinct from surface loss: stop further uploads,
submissions, reconfiguration, and retry/animation scheduling; consume unusable
egui deltas. Automatic device recreation is not implemented—rendering requires
restart. An empty submission is not recovery for a lost device.

Transient timeout/lost/outdated surfaces use backed-off retries independent of
cursor blinking. Explicit occlusion adds no surface polling timer. Startup prefers
Mailbox presentation when supported, otherwise AutoVsync, with a downlevel device
request fallback. None of this establishes a measured display-latency guarantee.

## Verification boundaries

```sh
cargo test --locked renderer::
```

Headless tests cover text/layout equivalence, cache inputs/bounds, geometry,
focus/repaint helpers, and the **production** frame boundary with a fake backend
(including thousands of failures). Those tests validate control flow and modeled
pending bytes, not native/GPU memory or windowed recovery.

Two ignored offscreen GPU probes check retained-versus-fresh pixels under atlas
churn and staging-resource reclamation/recovery. They need a working adapter,
not a window. See [performance](performance.md#explicit-scheduling-and-gpu-probes)
for exact commands and [development](development.md#manual-verification) for
display-backed checks. Submission is not completion: in-flight GPU backlog and
high-water atlas/geometry storage remain separate [memory concerns](memory.md).
