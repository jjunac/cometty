# Memory Study — cometty

Date: 2026-09-08. Scope: binary size + runtime RSS.
Single-binary Rust terminal (`winit` + `wgpu` + `glyphon`/`cosmic-text` + `egui` + `portable-pty` + `vte`).

## 1. Where memory goes today

### 1.1 Grid — dominant CPU RAM

* `src/grid/cell.rs:12-20`:
  ```rust
  pub struct Cell { ch: char, extra: Option<Box<str>>, width: u8, fg: Rgb, bg: Rgb, bold: bool, underline: bool }
  ```
  `src/theme.rs:9-13`: `Rgb { r:u8,g:u8,b:u8 }` = 3 B.
  Estimated `Cell` = 4 + 8 + 1 + 3 + 3 + 1 + 1 = 21 B + alignment -> 24-32 B on 64-bit, plus heap alloc when `extra: Some` (combining/ZWJ/flags/skin-tone/VS16 path in `src/grid/unicode.rs`, `src/grid/edit.rs`).
  `Cell` is `Clone` not `Copy`; scroll/resize clones per cell (`src/grid/mod.rs:206-268`).
* `src/grid/mod.rs:14-20`:
  `cells: Vec<Vec<Cell>>` (viewport) + `scrollback: VecDeque<Vec<Cell>>` (cap `src/config.rs:40-42,215-226`, default `10_000` in `src/config.rs:40-42`).
  Per-tab cost ~ `(rows + scrollback_len) * cols * size_of::<Cell>()`.
  Example at 24 B/Cell:
  * 80x24 + 10k scrollback: ~802k cells -> ~19 MB/tab + row Vec overhead.
  * 200x50 + 10k scrollback: ~2M cells -> ~48 MB/tab.
  * `max_dim=1024` (`src/config.rs:46-47`): 1024x1024 viewport alone ~25M cells -> unrealistic, but clamp allows large RSS if user maximizes window + large font cell counts stay small; real risk is scrollback x wide cols x tabs.
* `Vec<Vec<Cell>>` overhead: one allocation per row (10k+ rows in worst case), 24 B `Vec` header each + malloc rounding + fragmentation. `resize()` in `src/grid/mod.rs:206-268` allocates a full new grid and clones; `blank_row():163-165` / `erase_row():167-169` allocate per row (`vec![...; cols]`).
* Alt screen (`src/grid/alt.rs`, `saved_main_cells: Option<Vec<Vec<Cell>>>` in `src/grid/mod.rs:30`): doubles viewport while in `vim`/`less`/`htop`.
* Tabs (`src/app/tab.rs:19-26` `Tab { terminal, pty, selection, scrollbar, ... }`, `App { tabs: Vec<Tab> }` in `src/app/mod.rs:40-41`): grid cost scales linearly with tab count. Renderer/window/clipboard stay shared — good.
* Small, bounded: `window_title: String` (`src/app/mod.rs:54`, cap `max_title_chars=256` in `src/config.rs:52-53`), tab labels truncated to `max_label_chars=32` (`src/app/tab.rs:72-83`, `src/config.rs:127-128`), cursor/selection/pen structs are bytes.

### 1.2 Renderer / text — second largest, CPU + GPU

* `src/renderer/mod.rs:70-99` `Renderer` owns: `FontSystem`, `SwashCache`, `Viewport`, `TextAtlas`, `TextRenderer`, `Buffer`, `bg_pipeline/buf`, `egui_ctx/state/renderer`.
* `FontSystem::new()` (`src/renderer/mod.rs:164`): loads system fonts into memory (typically tens of MB RSS, shared per process — already shared across tabs, good).
* `Cache::new(&device)`, `TextAtlas::new(...)` (`src/renderer/mod.rs:166-170`): glyph atlas texture (e.g. 2048x2048 RGBA ~16 MB GPU + CPU-side cache) + `atlas.trim()` per frame (`src/renderer/mod.rs:563`) already trims.
* `Buffer` + `rebuild_buffer()` (`src/renderer/text.rs:97-116`): rebuilds all visible rows on `grid.version` change (`src/renderer/mod.rs:443-446`, `src/grid/mod.rs:191-193`). `build_buffer_lines()` (`src/renderer/text.rs:21-95`) allocates per frame: `clusters: Vec<(usize,String)>` per row, `cell.cluster(): String` per cell (`src/grid/cell.rs:25-37`), `line_string: String`, `BufferLine` per row, `AttrsList` spans per style run. This is transient allocator churn, not retained RSS, but drives peak RSS + fragmentation under fast output (`cat` large file).
* `bg_scratch: Vec<BgVertex>` (`src/renderer/mod.rs:84`, `src/renderer/background.rs:131-251`): already reused across frames via `mem::take`/`clear`; good pattern to copy for text path. `BgVertex` is 20 B (`[f32;2]+[f32;3]`); only non-default-bg/selection/cursor/underline cells emit quads, so usually small. Growth policy `next_power_of_two().max(1024)` (`src/renderer/background.rs:234-241`) is fine.
* `version` cache already skips rebuild when idle; `invalidate()` on tab switch (`src/renderer/mod.rs:388-390`, `src/app/tab.rs:205-207,252-254`) is required and correct.

### 1.3 Chrome / PTY / misc

* `egui_ctx/state/renderer` (`src/renderer/mod.rs:96-98`, `src/renderer/overlay.rs`, `src/renderer/settings_ui.rs`, `src/app/settings.rs`): `egui::Context::default()` + font + texture cache retained even when settings closed / scrollbar faded / single-tab bar hidden (`src/app/tab.rs:46-52`).
* PTY (`src/pty.rs`): two threads + 8 KB read loop; negligible vs grid/atlas. `PtySession` per tab.
* Clipboard (`arboard:3` in `Cargo.toml:8`, `Option<Clipboard>` in `src/app/mod.rs:53`): lazy — good.
* Config (`src/config.rs:433-455`): all `String` fields cloned into `Renderer.user_config` + `App.config` + per-`Grid`; tiny.

### 1.4 Binary size drivers

`Cargo.toml:6-31`: `wgpu:30` (+ `naga`, `wgpu-hal` all backends), `glyphon:0.12` + `cosmic-text:0.19` (+ `fontdb`, `ttf-parser`, `swash`), `egui:0.36` + `egui-winit` + `egui-wgpu`, `winit:0.30`, `portable-pty:0.9`, `vte:0.15`, `image:0.25` (png only — good), `arboard:3`, `muda:0.19` (already `default-features=false`), `env_logger:0.11`, `unicode-width`, `unicode-segmentation`, `toml`, `serde/derive`.
No `[profile.release]` currently — debug symbols + no LTO + default `opt-level=3` + multiple codegen units.

## 2. Binary-size techniques

1. Add `[profile.release]`:
   ```toml
   [profile.release]
   strip = true
   opt-level = "z"
   lto = true
   codegen-units = 1
   panic = "abort"
   ```
   Optional: `split-debuginfo = "off"` (macOS/Linux differ), `-C strip-symbols` via `RUSTFLAGS`. Verify with `ls -lh target/release/cometty` before/after.
2. Audit features with `cargo tree -e features -i wgpu|glyphon|winit|egui` + `cargo bloat --release --crates` + `cargo bloat --release -n 50`. Disable unused backends/codecs: `wgpu` backends, `winit` x11/wayland extras if targeting one, `glyphon`/`cosmic-text` shaping backends if a simpler shaper suffices.
3. Biggest lever: remove or feature-gate `egui` stack. It exists only for scrollbar chrome, tab strip, settings panel (`src/renderer/overlay.rs`, `src/renderer/settings_ui.rs`). Custom `wgpu` quads (like `background.rs` already does) would drop MBs. If kept, gate settings UI behind a Cargo feature so a minimal build skips `egui`.
4. Replace/trim small-but-wide deps: `env_logger` -> compile-time `log` max-level / no-op in release; confirm `image/png` stays minimal; lazy-load `arboard`/`muda` only on platforms that need them (already partial for `muda`).
5. Don't embed fonts. Keep `FontSystem::new()` system-font path; bundling TTFs grows binary 1:1.
6. Last resort: `upx --lzma` (slower startup, AV false-positive risk, macOS signing friction) — measure, don't default to it.

## 3. Runtime-memory techniques

### 3.1 Pack `Cell` (highest ROI)

* Bitpack flags: `width:2b + bold:1b + underline:1b (+ spare)` into one `u8`; store `fg/bg` as packed `u32` or `enum { Default, Ansi(u8), Rgb(u8,u8,u8) }` — most cells are theme defaults, so 1 B discriminant covers the common case and avoids comparing/storing full `Rgb` per cell.
* Move `extra: Option<Box<str>>` out of the hot struct into a side table `HashMap<(u32,u32), Box<str>>` / per-row sparse vec. Keeps `Cell: Copy` 8-12 B (vs 24-32 B + heap), removes allocator traffic in scroll/resize/erase paths, enables `memcpy` row moves. Target: ~2-3x grid RSS reduction.
* Make `Cell: Copy` after the above; replace per-cell `clone()` loops in `resize()`/`scroll`/`IL/DL` with `copy_from_slice` / `ptr::copy`.

### 3.2 Grid storage + scrollback policy

* Replace `Vec<Vec<Cell>>` + `VecDeque<Vec<Cell>>` with flat ring: single `Vec<Cell>` of `max_rows*cols` + head index, or `VecDeque<Box<[Cell]>>` with row pooling (`take`/`recycle` instead of `vec![blank; cols]` per scroll line). Eliminates 10k allocs, improves locality for `paint_bg`/`build_buffer_lines` scans.
* Share one static blank row / `Arc<[Cell]>` for all-blank scrollback rows; RLE or `enum Row { Blank, Cells(Box<[Cell]>) }`.
* Enforce caps already present (`scrollback_lines`, `min_dim/max_dim`, `tab_stop`, `max_title_chars`, `max_label_chars`) and consider: lower default 10k if memory matters, per-tab cap + global cap across tabs, drop oldest on pressure, `ED3` already clears (`src/grid/mod.rs:511-521` tests cover it).
* Avoid full-grid clone on `resize()` (`src/grid/mod.rs:206-268`): grow/shrink in place, `fix_row_wide` in place, only touch changed rows.

### 3.3 Text/shaping allocations

* Apply the `bg_scratch` reuse pattern to `build_buffer_lines()`: pass `&mut String` / `&mut Vec<BufferLine>` / scratch `clusters` buffer; change `Cell::cluster() -> String` to `fn write_cluster(&self, &mut String)` or `-> Cow<str>`. Avoids `cols x rows` short-lived `String`s per PTY burst.
* Skip shaping for fully-blank/trailing-blank rows (already trims trailing spaces in `src/renderer/text.rs:37-41` — extend to skip `line_layout` for blank rows).
* Only `rebuild_buffer()` the active tab; keep inactive tabs' `Buffer` empty until switched (already invalidates on switch — don't pre-render background tabs in `redraw`).
* Cap `cosmic-text` font fallback list to mono family (`font_family()` in `src/renderer/text.rs:11-19` already maps everything unknown to monospace); avoid loading CJK/emoji fallback stacks unless needed.

### 3.4 GPU/text caches

* Tune `glyphon::Cache` size / `TextAtlas` dimensions for terminal workload (glyph set is small vs rich text); keep `atlas.trim()`; consider shrinking atlas when single font/size is used.
* `Viewport`/`buffer.set_size()` already tracks window size (`src/renderer/mod.rs:331-344`); don't retain larger-than-window buffers after shrink.
* `bg_vertex_buf` growth is monotonic (`src/renderer/background.rs:234-241`); fine, but cap at `rows*cols*6` verts and reuse.

### 3.5 Gate `egui`

* Skip `paint_overlay` / `egui` begin/pass when: single tab on non-macOS (bar hidden), scrollbar fully faded (`fade_delay_ms`/`fade_speed` in `src/config.rs:88-92`), settings closed. Currently `render()` always builds `paint_jobs` + updates `egui_renderer` buffers (`src/renderer/mod.rs:457-464,521-527,558-559`).
* Drop `egui` textures when settings closed for a long time (`egui_ctx.clear_cache()` / free textures) if keeping `egui`.

### 3.6 Tabs / PTY / clipboard

* Per-tab scrollback cap is already live-applied (`apply_terminal_config()` truncates front in `src/grid/mod.rs:162-173`); add global budget: `sum(scrollback) <= N`, evict oldest inactive tab rows first.
* PTY 8 KB loop is fine; ensure `drain_pty` batches feed into `Term::feed` without per-byte `String`s (`src/app/pty_io.rs`, `src/term.rs` `mem::replace` Parser pattern — keep).
* Selection copy builds one `String` (`src/app/selection_view.rs`, `src/selection.rs`); cap by viewport + scrollback window, stream to clipboard for huge selections.

## 4. Suggested order (impact / effort)

1. `[profile.release]` + `cargo bloat` feature audit — minutes, large binary win.
2. `Cell` packing + side-table `extra` + `Copy` — largest RSS win, localized to `src/grid/cell.rs`, `src/theme.rs`, call sites in `src/grid/edit.rs`, `src/renderer/text.rs`, `src/renderer/background.rs`.
3. Row pooling / flat ring + blank-row sharing — removes 10k allocs, helps scroll-heavy workloads.
4. Text-path scratch reuse (`cluster()` without `String`) — reduces peak/churn, easy after (2).
5. Gate `egui` when chrome hidden — frees steady-state CPU/GPU + some RSS.
6. Atlas/`FontSystem` tuning + global scrollback budget — polish once measured.

## 5. How to validate

```sh
cargo bloat --release --crates
cargo llvm-lines | head -30
ls -lh target/release/cometty
cargo test
cargo clippy -- -D warnings
cargo fmt --check
# runtime (pick one):
heaptrack ./target/release/cometty
samply record ./target/release/cometty
# macOS:
leaks -atExit -- ./target/release/cometty
```

Baseline to record before changes: release binary bytes, idle RSS (1 tab, 80x24), RSS after `cat` 50 MB file, RSS with 10k scrollback full, RSS with 5 tabs, retained `Cell` bytes via `size_of::<Cell>()` test.
