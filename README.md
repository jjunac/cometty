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

Linux needs the GUI system deps first (see `.github/workflows/ci.yml`):

```sh
sudo apt-get install -y libx11-dev libxkbcommon-dev libgl1-mesa-dev
```

On macOS/Linux, closing a tab cancels unsent input and hangs up its shell and
foreground job. A shell that ignores hangup is force-terminated after a 250 ms
grace period. Cleanup and child reaping run off the UI thread; detached jobs are
not recursively hunted. Application exit waits up to two seconds for all PTY
sessions together.

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
scrollback_bytes = 134217728  # 128 MiB retained history per tab (cells + extras + capacity)
max_cells = 1048576         # live cells per buffer; also capped by max_dim / PTY u16
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
title_format = "$command | $cwd"  # $title (OSC), $command, $cwd (shell dir: OSC 7 else kernel), $tab; all empty -> "Tab N"
# + insets, corner radii, reserves, plus-button sizes (see src/config.rs)

[selection]
double_click_ms = 400
word_regex = "[A-Za-z0-9_]+"  # double-click match-words pattern; triple-click selects the line

[input]
copy_key = "c"
paste_key = "v"
new_tab_key = "t"
search_key = "f"
copy_ctrl_shift = true
copy_super = true
paste_ctrl_shift = true
paste_super = true
new_tab_ctrl = true
new_tab_super = true
search_ctrl = true      # Ctrl+F on Linux/Windows; false on macOS (Cmd+F only)
search_super = true
lines_per_tick = 7.0

[links]
open_modifier = "auto"  # auto (Cmd on macOS, Ctrl elsewhere) | cmd | ctrl | alt | none
underline = "hover"     # always | hover (while the modifier is held) | never
detect_urls = true      # also underline/click plain-text http(s)://, www., mailto:, ...

[clipboard]
osc52 = "write"  # off | write (apps may set the clipboard) | read-write (reads can exfiltrate)

[log]
level = "debug"        # in-app buffer: cometty's own logs (off | error | warn | info | debug | trace)
filter = ""            # RUST_LOG-style override, e.g. "cometty=debug,wgpu=debug,warn"
buffer_lines = 1000    # ring capacity; stderr output still follows RUST_LOG
```

## Features

- [x] PTY spawn (`$SHELL` → `/bin/bash`), `TERM=xterm-256color`, read/write threads, resize propagation
- [x] Window + event loop with deferred resize applied on redraw
- [x] Text / control handling: print with pending-wrap, `\n` / `\r` / `\b` / `\t`
- [x] CSI `A/B/C/D/E/F/G/H/J/K/L/M/S/T/d/s/u`, ESC `M/7/8/c`
- [x] SGR: bold, 16 + bright colors, `38;5` / `48;5`, `38;2` / `48;2` truecolor
- [x] Grid: scrollback buffer (10k lines, stored), insert/delete lines, erase display/line; per-tab history byte budget, lazy width projection, recycled row storage
- [x] Input: text passthrough, Enter → `\r`, Backspace → `0x7F`, Esc, arrows, Home/End/PgUp/PgDn/Ins/Del, Ctrl+letter, Ctrl+Space
- [x] Rendering: per-cell fg/bold/underline + bg quads, cursor shapes (block/underline/bar) + blink, version-cache to skip rebuild, surface-loss recovery
- [x] Themes (`tokyo-night` default, `vscode`, `tomorrow-night`) via `--theme NAME` / `--list-themes`
- [x] Alt screen (`?1049h/l`) + DECSET/DECRST (`?25`, `?2004`, …) for `vim` / `less` / `htop`
- [x] Scrollback viewing (Shift+PgUp/Dn = page-minus-1, Shift+Home/End = top/bottom, mouse wheel)
- [x] Scrollbar UI (graphical overlay in `src/scrollbar.rs`: fade/thumb geometry, mouse wheel, Shift+PgUp/Dn page, Shift+Home/End top/bottom)
- [x] Selection + copy/paste (mouse drag + double-click word, Ctrl+Shift+C/V, bracketed paste)
- [x] Tabs (Ctrl+T / Cmd+T, Ghostty-style strip hidden for 1 tab: inset pill, equal-width tabs down to min then scroll, click to switch, hover × on left to close, + for new, ⌘1-9 hints, OSC title labels)
- [x] Underline rendering + cursor shape (`?25`, `DECSCUSR` block/underline/bar, steady/blink) + window title (OSC `0/1/2`)
- [x] Consistent erase semantics (BCE: `ED/EL/IL/DL`/scroll fill use current pen bg)
- [x] Full keyboard: F1–F12, Ctrl/Shift+arrows, Alt+key → `ESC` prefix, keypad (+`DECCKM`/`DECKPAM`, `Ctrl+Tab` reserved, Opt+arrows word jump, Cmd+arrows line edges)
- [x] Unicode: double-width / emoji / combining chars (CJK wide, ZWJ sequences, flags, skin tones, VS16, combining marks; cluster cells with continuation placeholders)
- [x] Scroll regions (`CSI r`), origin / insert / auto-wrap modes
- [x] Exit behavior: last shell exit closes the window, other exits close just that tab
- [x] User config: TOML file (`$HOME/.config/cometty/config.toml`) for theme/font/window/terminal/shell/cursor/scrollbar/tabbar/selection/input/log (see `src/config.rs`); CLI `--theme` / `--config` override file
- [x] Settings UI (`Ctrl+,` / `Cmd+,` or the Settings button: sidebar panel, live-apply, debounced auto-save to `config.toml` via an ordered worker with flush on shutdown, per-field/section/global reset)
- [x] Log viewer (`Ctrl+Shift+L` / `Cmd+Shift+L` or Cometty > Logs…: in-memory ring, level + text filter, follow, clear/copy; every detected OSC sequence is logged — handled or not — with a compact payload summary; records by target so dependency debug spam stays out while their warnings still show, bounded by `[log] filter`/`level`/`buffer_lines`; incremental visible-only filtering and coalesced wakeups; stderr keeps following `RUST_LOG`)
- [x] Mouse reporting (`1000/1002/1003/1006-SGR`) + focus (`1004`) + synced output (`2026`); `Shift`-override keeps local selection
- [x] Char ops + queries: `ICH/DCH/ECH` (`@/P/X`), `REP`, `DA`/`CPR`, `DECRQM` stub
- [x] Full SGR: dim / italic / inverse / strike / overline, underline styles + `58/59` colors, colon `38:2:r:g:b` form
- [x] Tab switching: `Ctrl+Tab` / `Ctrl+Shift+Tab` + `Cmd+1-9` wired to tab switch
- [x] Search: `Cmd+F` on macOS / `Ctrl+F` on Linux/Windows (override with `[input] search_key`/`search_ctrl`/`search_super`) overlay (top-right find bar), smart-case match highlight over scrollback + screen (current match brighter), `Enter`/`Shift+Enter` (or `F3`/`Shift+F3`) next/prev with wrap-around, `Esc` close
- [x] Hyperlinks: `OSC 8` runs stored per cell; hold the open modifier (`Cmd` on macOS, `Ctrl` elsewhere; `[links] open_modifier`) to underline every link — plus detected plain-text URLs — and click to open in the OS browser (scheme allowlist `http/https/ftp/mailto` — `file` is refused so untrusted output can't open local paths; no shell involved on macOS/Linux, and URIs with `cmd` metacharacters are refused on Windows); `[links] underline` (`always`/`hover`/`never`) and `detect_urls` tune the hints
- [x] Clipboard: `OSC 52` writes from tmux/nvim/… gated by `[clipboard] osc52` (`off`/`write`/`read-write`); reads are opt-in because a `?` request can exfiltrate the clipboard; decoded bytes and final UTF-8 text are capped at 1 MiB and redacted in the log panel; paste is normalized/wrapped in one pass and admitted through a byte-bounded, nonblocking input queue (explicit notice on overflow, never a silent drop)
- [x] Bounded OSC parsing: every OSC (including unsupported commands and when OSC 52 is off) has a hard 2 MiB wire-byte limit after `ESC ]`, excluding its terminator; overflow discards the whole command through termination/cancellation, then normal parsing resumes, with one size-only overflow warning per tab. Parser storage is released after completed/cancelled OSCs longer than 64 KiB; smaller sequences reuse capacity. A normal base64-encoded 1 MiB clipboard transfer, including line wrapping, fits the raw limit.
- [ ] Bell (visual flash; currently ignored) + font zoom (`Ctrl +/-/0`) + confirm-close on dirty foreground process
