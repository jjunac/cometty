# AGENTS.md

Single-binary Rust terminal emulator (`cometty`). Edition 2024. CI runs
`cargo test` + `cargo clippy -- -D warnings` + `cargo fmt --check` (see
`.github/workflows/ci.yml`).

## Commands

- `cargo test` — 61 unit tests in `grid`/`term`/`input`/`app::geometry`/`scrollbar`/`selection`/`theme`/`renderer`; runs instantly, no services needed.
- `cargo run` — launches GUI window (needs display + GPU surface). Do not run headless; prefer `cargo test` / `cargo check`.
- `cargo clippy -- -D warnings` and `cargo fmt --check` — no configs committed; keep code warning- and rustfmt-clean.

## Architecture (`src/`)

- `main.rs` — thin winit `ApplicationHandler`: window create, egui pre-dispatch, then delegates to `app::*`. Also parses `--theme NAME` / `--list-themes` / `--help`.
- `app/` — event/state split from the old `window_event` god function: `mod` (App state + `UserEvent`), `keyboard` (copy/paste shortcuts, Shift+scroll, PTY send), `mouse` (drag-select, double-click word via `DOUBLE_CLICK_MS`, wheel), `redraw` (resize apply → PTY drain → `Renderer::render`), `clipboard`, `pty_io` (`drain_pty`, `apply_resize`), `selection_view`, `geometry` (`MAX_DIM`/`compute_grid_size` single source of truth for all resize/PTY clamps). Resize is deferred via `pending_resize` and applied in `RedrawRequested` (`apply_resize`: renderer → terminal → pty).
- `grid/` — terminal state split by concern: `mod` (struct + `new`/`resize` + accessors + tests), `cell` (Cell/Pen/Cursor + blanks), `scroll` (scrollback + view offsets), `edit` (write/erase/insert/delete), `style` (SGR), `alt` (alt buffer + mode flags). 10k-line scrollback, monotonic `version`. Wrap uses pending-wrap protocol (`put_char` leaves `cursor.x == cols`; `Terminal::print` resolves on next char). Renderer skips rebuild when `grid.version` unchanged.
- `term.rs` — `vte::Perform` impl feeding `Grid`. `feed()` swaps `Parser` out via `mem::replace` to satisfy borrow checker — keep that pattern. Supports CSI `A-H`, `J/K/L/M/S/T`, `s/u`, `E/F/G/d`, private modes `?25` (cursor), `?2004` (bracketed paste), `?47/?1047/?1048/?1049` (alt screen), ESC `M/7/8/c`, SGR `38;5`/`48;5` + `38;2`/`48;2` truecolor; ignores OSC/bell. Other CSI intermediates are ignored.
- `pty.rs` — `portable-pty`, shell from `$SHELL` (fallback `/bin/bash`), `TERM=xterm-256color`, cwd `$HOME`. Two detached threads (read 8KB loop / write loop). `PtyEvent::Exit` fires on EOF/read-error. Size clamps reuse `app::geometry::{MIN_DIM, MAX_DIM}`.
- `renderer/` — wgpu + glyphon/cosmic-text split from the old `render` god function: `mod` (struct + `new` + thin `render` orchestrator), `text` (buffer shaping + explicit layout), `background` (`BgVertex`/`BG_SHADER`, `paint_bg` with reused `bg_scratch` buffer), `overlay` (egui scrollbar chrome). Fixed metrics with `cell_width = font_size * 0.602` (see `scaled_metrics`). Bg quads via custom WGSL pipeline; text via `TextRenderer`. Recovers from lost/outdated surface by reconfiguring. `cols_for_width`/`rows_for_height` + `app::geometry` drive PTY resize.
- `input.rs` — pure `map_key()` (unit-testable) + `key_to_bytes()` wrapper. Ctrl+letter synthesizes `0x01–0x1A`; Enter→`\r`, Backspace→`0x7F`; releases return `None`. `KeyEvent` can't be struct-constructed (private fields), so tests target `map_key` only.

## Gotchas

- GUI binary: `cargo run`/`cargo build` need windowing/GPU; verification in headless sessions = `cargo test` + `cargo check`.
- `grid::cell()`, `pen()`, `scrollback_len()` are `#[allow(dead_code)]` test/inspection helpers — do not remove. Same for `Renderer::set_theme` (future runtime switching) and `term` inspectors.
- Erase semantics are inconsistent by design-in-progress: `erase_in_display(0)` uses current pen bg, mode 1 and `erase_in_line` use `blank_cell()` — preserve unless fixing deliberately.
