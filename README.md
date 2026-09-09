<p align="center">
  <img src="logo.svg" width="160" alt="cometty logo" />
</p>

# cometty

Single-binary GPU terminal emulator in Rust (`winit` + `wgpu` + `glyphon` + `portable-pty` + `vte`).

## Run

```sh
cargo run -- --theme tokyo-night   # needs display + GPU; see --list-themes
cargo run -- --config ~/.config/cometty/config.toml
cargo test                         # headless-safe unit tests
cargo clippy -- -D warnings
cargo fmt --check
```

## Config

TOML file at `$HOME/.config/cometty/config.toml` (missing file or keys = defaults).
Precedence: `defaults < file < --theme/--config` CLI flags.

```toml
[theme]
name = "tokyo-night"  # tokyo-night | vscode | tomorrow-night

[font]
family = "monospace"  # monospace | sans | serif | cursive | fantasy
size = 14.0
line_height_factor = 1.25
cell_width_factor = 0.602

[window]
title = "cometty"
width = 800
height = 600

[terminal]
scrollback_lines = 10000
min_dim = 1
max_dim = 1024
tab_stop = 8
max_title_chars = 256

[shell]
shell = ""  # empty = $SHELL or /bin/bash
term = "xterm-256color"
cwd = ""    # empty = $HOME

[cursor]
blink_ms = 530
default_shape = "block"
default_blinking = true

[scrollbar]
track_width = 10.0
min_thumb = 20.0
pad = 2.0
fade_delay_ms = 800
fade_speed = 5.0

[tabbar]
height = 38.0
min_tab_width = 100.0
max_label_chars = 32
# + insets, corner radii, reserves, plus-button sizes (see src/config.rs)

[selection]
double_click_ms = 400
word_extra_chars = "_"

[input]
copy_key = "c"
paste_key = "v"
new_tab_key = "t"
copy_ctrl_shift = true
copy_super = true
paste_ctrl_shift = true
paste_super = true
new_tab_ctrl = true
new_tab_super = true
lines_per_tick = 7.0
```

## Features

- [x] PTY spawn (`$SHELL` → `/bin/bash`), `TERM=xterm-256color`, read/write threads, resize propagation
- [x] Window + event loop with deferred resize applied on redraw
- [x] Text / control handling: print with pending-wrap, `\n` / `\r` / `\b` / `\t`
- [x] CSI `A/B/C/D/E/F/G/H/J/K/L/M/S/T/d/s/u`, ESC `M/7/8/c`
- [x] SGR: bold, 16 + bright colors, `38;5` / `48;5`, `38;2` / `48;2` truecolor
- [x] Grid: scrollback buffer (10k lines, stored), insert/delete lines, erase display/line
- [x] Input: text passthrough, Enter → `\r`, Backspace → `0x7F`, Esc, arrows, Home/End/PgUp/PgDn/Ins/Del, Ctrl+letter, Ctrl+Space
- [x] Rendering: per-cell fg/bold/underline + bg quads, cursor shapes (block/underline/bar) + blink, version-cache to skip rebuild, surface-loss recovery
- [x] Themes (`tokyo-night` default, `vscode`, `tomorrow-night`) via `--theme NAME` / `--list-themes`
- [x] Alt screen (`?1049h/l`) + DECSET/DECRST (`?25`, `?2004`, …) for `vim` / `less` / `htop`
- [x] Scrollback viewing (Shift+PgUp/Dn = page-minus-1, Shift+Home/End = top/bottom, mouse wheel)
- [X] Scrollbar UI (no graphical scrollbar; use keys/wheel above)
- [x] Selection + copy/paste (mouse drag + double-click word, Ctrl+Shift+C/V, bracketed paste)
- [x] Tabs (Ctrl+T / Cmd+T, Ghostty-style strip hidden for 1 tab: inset pill, equal-width tabs down to min then scroll, click to switch, hover × on left to close, + for new, ⌘1-9 hints, OSC title labels)
- [x] Underline rendering + cursor shape (`?25`, `DECSCUSR` block/underline/bar, steady/blink) + window title (OSC `0/1/2`)
- [x] Consistent erase semantics (BCE: `ED/EL/IL/DL`/scroll fill use current pen bg)
- [x] Full keyboard: F1–F12, Ctrl/Shift+arrows, Alt+key → `ESC` prefix, keypad (+`DECCKM`/`DECKPAM`, `Ctrl+Tab` reserved, Opt+arrows word jump, Cmd+arrows line edges)
- [x] Unicode: double-width / emoji / combining chars (CJK wide, ZWJ sequences, flags, skin tones, VS16, combining marks; cluster cells with continuation placeholders)
- [x] Scroll regions (`CSI r`), origin / insert / auto-wrap modes
- [x] Exit behavior: last shell exit closes the window, other exits close just that tab
- [x] User config: TOML file (`$HOME/.config/cometty/config.toml`) for theme/font/window/terminal/shell/cursor/scrollbar/tabbar/selection/input (see `src/config.rs`); CLI `--theme` / `--config` override file
- [x] Settings UI (`Ctrl+,` / `Cmd+,` or the Settings button: sidebar panel, live-apply, auto-save to `config.toml`, per-field/section/global reset)
- [x] Mouse reporting (`1000/1002/1003/1006-SGR`) + focus (`1004`) + synced output (`2026`); `Shift`-override keeps local selection
- [x] Char ops + queries: `ICH/DCH/ECH` (`@/P/X`), `REP`, `DA`/`CPR`, `DECRQM` stub
- [x] Full SGR: dim / italic / inverse / strike / overline, underline styles + `58/59` colors, colon `38:2:r:g:b` form
- [ ] Tab switching: `Ctrl+Tab` / `Ctrl+Shift+Tab` + `Cmd+1-9` wired to tab switch
- [ ] Search: `Ctrl+F` overlay, match highlight, next/prev, `Esc` close
- [ ] Bell (visual flash; currently ignored) + font zoom (`Ctrl +/-/0`) + confirm-close on dirty foreground process
- [ ] Hyperlinks (`OSC 8` + `Cmd+click`) + clipboard (`OSC 52`) + URL hint + open
