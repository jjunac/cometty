# Local cosmic-text 0.19.0 patch

Source: crates.io `cosmic-text` 0.19.0 (MIT OR Apache-2.0). `src/` and the
licenses are unchanged except for the three files listed below. No registry
files are modified. The normalized manifest omits upstream integration-test and
benchmark targets whose assets are not included. Those suites are not run as
part of this application.

- `shape_run_cache.rs`: explicit entry/retained-byte admission (disabled until
  configured), capacity accounting, oversized-entry bypass, generation eviction.
  Uses a capacity-bounded vector with hashed lookup rather than an opaque table
  allocation. This caches *final advanced shaping*, not text layout or GPU data.
- `shape.rs`: include run direction in the key; bypass key construction when
  disabled or text is oversized. The upstream six-plan cache stays unchanged.
- `font/system.rs`: clear shaped runs when the font database is mutated.

Cometty configures 512 entries / 2 MiB, clears on font/metrics changes, and tests
cached/uncached glyph equivalence, direction, admission and retained capacity in
`src/renderer/text_tests.rs`. Caching repeated shaped runs avoids fallback and
six-plan thrashing together without enlarging harfrust's opaque plan storage.
Unique runs still pay the uncached shaping/plan cost. No age-only retention or
unbounded `shape-run-cache` is enabled.

On upgrade, re-evaluate these three small changes against upstream, rerun the
layout A/B and offscreen GPU probes, and remove the crates.io patch when upstream
provides budgeted admission and complete keys. Keep glyphon and Cometty on the
same cosmic-text instance. The benchmark runners snapshot `vendor/` too.
