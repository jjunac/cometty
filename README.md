# cometty

Single-binary GPU terminal emulator in Rust (`winit` + `wgpu` + `glyphon` + `portable-pty` + `vte`).

## Run

```sh
cargo run -- --theme tokyo-night   # needs display + GPU; see --list-themes
cargo test                         # headless-safe unit tests
cargo clippy -- -D warnings
cargo fmt --check
```

## Features

- [x] PTY spawn (`$SHELL` → `/bin/bash`), `TERM=xterm-256color`, read/write threads, resize propagation
- [x] Window + event loop with deferred resize applied on redraw
- [x] Text / control handling: print with pending-wrap, `\n` / `\r` / `\b` / `\t`
- [x] CSI `A/B/C/D/E/F/G/H/J/K/L/M/S/T/d/s/u`, ESC `M/7/8/c`
- [x] SGR: bold, 16 + bright colors, `38;5` / `48;5`, `38;2` / `48;2` truecolor
- [x] Grid: scrollback buffer (10k lines, stored), insert/delete lines, erase display/line
- [x] Input: text passthrough, Enter → `\r`, Backspace → `0x7F`, Esc, arrows, Home/End/PgUp/PgDn/Ins/Del, Ctrl+letter, Ctrl+Space
- [x] Rendering: per-cell fg/bold + bg quads, block cursor + blink, version-cache to skip rebuild, surface-loss recovery
- [x] Themes (`tokyo-night` default, `vscode`, `tomorrow-night`) via `--theme NAME` / `--list-themes`
- [x] Alt screen (`?1049h/l`) + DECSET/DECRST (`?25`, `?2004`, …) for `vim` / `less` / `htop`
- [x] Scrollback viewing (Shift+PgUp/Dn = page-minus-1, Shift+Home/End = top/bottom, mouse wheel)
- [X] Scrollbar UI (no graphical scrollbar; use keys/wheel above)
- [x] Selection + copy/paste (mouse drag + double-click word, Ctrl+Shift+C/V, bracketed paste)
- [x] Tabs (Ctrl+T / Cmd+T, Ghostty-style strip hidden for 1 tab: inset pill, equal-width tabs down to min then scroll, click to switch, hover × on left to close, + for new, ⌘1-9 hints, OSC title labels)
- [ ] Underline rendering (parsed, not drawn), cursor visibility / shape (`?25`, `DECSCUSR`), window title (OSC)
- [ ] Consistent erase semantics (`ED 0` vs others)
- [ ] Full keyboard: F1–F12, Ctrl/Shift+arrows, Alt+key → `ESC` prefix, keypad
- [ ] Unicode: double-width / emoji / combining chars
- [ ] Scroll regions (`CSI r`), origin / insert / auto-wrap modes
- [ ] Exit behavior: close window when shell exits (currently just logs)
- [ ] User config: font/size selection (theme is wired: `--theme` / `--list-themes`)
- [ ] Bell (currently ignored)
