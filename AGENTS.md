# AGENTS.md

Single-binary Rust terminal emulator (`cometty`). Edition 2024, no workspace, no CI.

## Commands

- `cargo test` — 16 unit tests in `grid`/`term`/`input`; runs instantly, no services needed.
- `cargo run` — launches GUI window (needs display + GPU surface). Do not run headless; prefer `cargo test` / `cargo check`.
- `cargo clippy -- -D warnings` and `cargo fmt --check` — no configs committed; keep code warning- and rustfmt-clean.

## Architecture (`src/`)

- `main.rs` — winit `ApplicationHandler`: owns `Window` + `Renderer` + `Terminal` + `PtySession`. Event flow: PTY reader thread → `UserEvent::PtyAvailable` → `drain_pty()` → `terminal.feed()` → `request_redraw()`; key input → `input::key_to_bytes` → `pty.write()`. Resize is deferred via `pending_resize` and applied in `RedrawRequested` (`apply_resize`: renderer → terminal → pty).
- `grid.rs` — terminal state: `cells`, cursor, `Pen` (fg/bg/bold/underline), 10k-line scrollback, monotonic `version`. Wrap uses pending-wrap protocol (`put_char` leaves `cursor.x == cols`; `Terminal::print` resolves on next char). Renderer skips rebuild when `grid.version` unchanged.
- `term.rs` — `vte::Perform` impl feeding `Grid`. `feed()` swaps `Parser` out via `mem::replace` to satisfy borrow checker — keep that pattern. Supports CSI `A-H`, `J/K/L/M/S/T`, `s/u`, `E/F/G/d`, ESC `M/7/8/c`; ignores OSC/bell. CSI intermediates (`?`, etc.) are ignored.
- `pty.rs` — `portable-pty`, shell from `$SHELL` (fallback `/bin/bash`), `TERM=xterm-256color`, cwd `$HOME`. Two detached threads (read 8KB loop / write loop). `PtyEvent::Exit` fires on EOF/read-error.
- `renderer.rs` — wgpu + glyphon/cosmic-text. Fixed metrics with `cell_width = font_size * 0.602` (see constants in `Renderer::new`). Bg quads via custom WGSL pipeline; text via `TextRenderer`. Recovers from lost/outdated surface by reconfiguring. `cols_for_width`/`rows_for_height` drive PTY resize (clamped 1–1024).
- `input.rs` — pure `map_key()` (unit-testable) + `key_to_bytes()` wrapper. Ctrl+letter synthesizes `0x01–0x1A`; Enter→`\r`, Backspace→`0x7F`; releases return `None`. `KeyEvent` can't be struct-constructed (private fields), so tests target `map_key` only.

## Gotchas

- GUI binary: `cargo run`/`cargo build` need windowing/GPU; verification in headless sessions = `cargo test` + `cargo check`.
- `grid::cell()`, `pen()`, `scrollback_len()` are `#[allow(dead_code)]` test/inspection helpers — do not remove.
- Erase semantics are inconsistent by design-in-progress: `erase_in_display(0)` uses current pen bg, mode 1 and `erase_in_line` use `Cell::default()` — preserve unless fixing deliberately.
