# Configuration

cometty reads TOML from `$HOME/.config/cometty/config.toml`. Missing files or
keys use defaults. An unreadable/invalid file falls back to defaults and shows a
warning in Settings.

```sh
cometty --config /path/to/config.toml
cometty --theme vscode
cometty --list-themes
```

`--config` selects the file to read **and save**. `--theme` overrides the theme
for that session. Open Settings with **Cmd+,** on macOS or **Ctrl+,** elsewhere
to edit and automatically save preferences. Changes are normally live; shell,
starting directory, and TERM changes affect new tabs only. A session theme
override stays active even if you save a different preference.

External file edits are read on the next launch, not merged into an open session.
Settings edits can overwrite them. Use absolute paths for shell/cwd settings;
shell arguments support quotes and backslashes, not `$VARIABLE` or `~` expansion.

## Common settings

Only include values you want to change:

```toml
[theme]
name = "tokyo-night"

[font]
family = "monospace"
size = 14.0

[terminal]
scrollback_lines = 10000
scrollback_bytes = 134217728 # 128 MiB history budget per tab

[tabbar]
title_format = "$command | $cwd"

[links]
open_modifier = "auto"
underline = "hover"
detect_urls = true

[clipboard]
osc52 = "write"

[session]
restore = "tabs"
```

## Appearance

| Section / key | Default | Meaning |
| --- | --- | --- |
| `theme.name` | `"tokyo-night"` | `tokyo-night`, `vscode`, or `tomorrow-night` |
| `font.family` | `"monospace"` | Installed family name, or generic `monospace`, `sans`/`sans-serif`, `serif`, `cursive`, `fantasy`. Generic monospace resolves to an installed platform-appropriate family; missing glyphs/names use fallback |
| `font.size` | `14.0` | Logical font size |
| `font.line_height_factor` | `1.25` | Line height relative to font size |
| `font.cell_width_factor` | `0.602` | Column width relative to font size |
| `font.underline_factor` | `0.07` | Text underline thickness relative to line height |
| `window.title` | `"cometty"` | Fallback window title when the shell supplies none |
| `window.width`, `window.height` | `800`, `600` | Initial logical window size |
| `cursor.blink_ms` | `530` | Blink interval in milliseconds |
| `cursor.default_shape` | `"block"` | `block`, `underline`, or `bar`; applications can change cursor style |
| `cursor.default_blinking` | `true` | Initial cursor blinking |
| `cursor.underline_factor` | `0.14` | Underline cursor thickness relative to line height |
| `cursor.bar_width_factor` | `0.3` | Bar cursor width relative to column width |

### Scrollbar and tab strip

| Section / key | Default | Meaning |
| --- | --- | --- |
| `scrollbar.track_width` | `10.0` | Track width |
| `scrollbar.min_thumb` | `20.0` | Minimum thumb height |
| `scrollbar.pad` | `2.0` | Track padding |
| `scrollbar.fade_delay_ms` | `800` | Delay before hiding at the live bottom |
| `scrollbar.fade_speed` | `5.0` | Opacity transition speed |
| `tabbar.height` | `38.0` | Strip height |
| `tabbar.min_tab_width` | `100.0` | Minimum tab width before horizontal scrolling |
| `tabbar.max_label_chars` | `32` | Label length including an ellipsis |
| `tabbar.title_format` | `"$command \| $cwd"` | Tab-label template |
| `tabbar.corner_radius_tab`, `tabbar.corner_radius_bar` | `8`, `10` | Tab/container corner radii |
| `tabbar.inset_x`, `tabbar.inset_y` | `8.0`, `5.0` | Strip insets |
| `tabbar.close_reserve`, `tabbar.shortcut_reserve` | `24.0`, `34.0` | Space for close and shortcut labels |
| `tabbar.gap` | `4.0` | Tab spacing |
| `tabbar.plus_diameter`, `tabbar.plus_gap` | `28.0`, `8.0` | New-tab button size and spacing |

Widths/heights/padding are logical display units. The strip remains visible on
macOS; on other platforms it hides when only one tab is open.

Templates support `$title` (application-supplied title), `$command` (foreground
command), `$cwd` (shell directory), and `$tab` (one-based tab number). Use
`${name}` before adjacent text and `$$` for a literal dollar sign. Unknown
variables stay literal; missing values become empty without removing separators.
If no referenced variable has a value, the label is `Tab N`.

## Terminal and shell

| Section / key | Default | Meaning |
| --- | --- | --- |
| `terminal.scrollback_lines` | `10000` | Maximum retained history rows per tab; `0` disables history |
| `terminal.scrollback_bytes` | `134217728` | History storage budget per tab, not total application RAM; `0` disables history |
| `terminal.max_cells` | `1048576` | Maximum visible cells per screen buffer; cannot exceed this hard ceiling |
| `terminal.min_dim`, `terminal.max_dim` | `1`, `1024` | Requested minimum/maximum rows or columns, also subject to total-cell limits |
| `terminal.tab_stop` | `8` | Tab-stop interval |
| `terminal.max_title_chars` | `256` | Maximum application title/cwd-label characters |
| `shell.shell` | `""` | Empty chooses `$SHELL` or `/bin/bash` on Unix; `$COMSPEC` or `cmd.exe` on Windows. The macOS app adds `-l` to this default; explicit values are verbatim |
| `shell.term` | `"xterm-256color"` | TERM advertised to programs |
| `shell.cwd` | `""` | Empty starts in `$HOME`, or `/` if unset |

When `shell.shell` is empty and cometty runs from its macOS app bundle, the
shell starts as a login shell (`-l`) so Finder launches read login profiles and
`path_helper` instead of launchd's minimal PATH. Set `shell.shell` explicitly
(for example `"/bin/zsh"`) to force a non-login shell.

History retains fewer rows if its storage budget is reached first. Lowering
limits can discard old output or constrain terminal dimensions. Neither history
limit controls the memory used by programs running inside the terminal.

## Input and selection

Shortcut letters are case-insensitive. `super` means Cmd on macOS and the
Super/Windows key elsewhere. The default shortcuts are listed in the
[README](../README.md#keyboard-and-mouse).

| Section / key | Default | Meaning |
| --- | --- | --- |
| `input.copy_key`, `input.paste_key` | `"c"`, `"v"` | Copy/paste shortcut letters |
| `input.new_tab_key`, `input.search_key` | `"t"`, `"f"` | New-tab/search shortcut letters |
| `input.copy_ctrl_shift`, `input.paste_ctrl_shift` | `true`, `true` | Enable Ctrl+Shift copy/paste |
| `input.copy_super`, `input.paste_super` | `true`, `true` | Enable Super copy/paste |
| `input.new_tab_ctrl`, `input.new_tab_super` | `true`, `true` | Enable Ctrl / Super new-tab shortcuts |
| `input.search_ctrl` | `false` on macOS; `true` elsewhere | Enable Ctrl search shortcut; macOS preserves shell Ctrl+F by default |
| `input.search_super` | `true` | Enable Super search shortcut |
| `input.tab_switch_ctrl`, `input.tab_switch_super` | `true`, `true` | Enable Ctrl / Super tab switching |
| `input.shift_page_scroll` | `true` | Enable Shift+PageUp/PageDown/Home/End history navigation |
| `input.lines_per_tick` | `7.0` | Mouse-wheel scroll distance |
| `input.pixel_fallback_line_height` | `20.0` | Fallback conversion for pixel-based wheel input |
| `selection.double_click_ms` | `400` | Multi-click timing window |
| `selection.word_regex` | `"[A-Za-z0-9_]+"` | Word pattern for double-click/word dragging; invalid/empty patterns use the default |

The old `selection.word_extra_chars` setting is migrated when loading old files;
use `word_regex` for new configurations.

## Links and clipboard permissions

| Section / key | Default | Meaning |
| --- | --- | --- |
| `links.open_modifier` | `"auto"` | `auto` = Cmd on macOS, Ctrl elsewhere; alternatives: `cmd`, `ctrl`, `alt`, `none` (disables activation) |
| `links.underline` | `"hover"` | `always`, `hover` (while the modifier is held), or `never` |
| `links.detect_urls` | `true` | Detect plain-text URLs as well as application-provided links |
| `clipboard.osc52` | `"write"` | `off`, `write`, or `read-write` for application clipboard requests |

Only `http`, `https`, `ftp`, and `mailto` links open; local `file` links are
refused. Wrapped plain-text URLs are detected separately on each displayed row.

**Clipboard reads are a privacy risk:** `read-write` allows a terminal program
(including a remote program) to request your clipboard contents. `write` allows
programs to set it but not read it. OSC 52 transfers are limited to 1 MiB of text;
normal copy/paste is separate. An input-full/oversized-paste notice means that
input was not sent; wait for the program to read and retry with a smaller paste.

## Session restore

| Section / key | Default | Meaning |
| --- | --- | --- |
| `session.restore` | `"scrollback"` | `scrollback` reopens the tabs with their history and screen contents; `tabs` reopens the tabs, their working directories, the active tab, and the window size/position without contents; `off` forgets the session at quit |

The session is saved when you quit (Cmd+Q, or closing the window) with tabs open.
Closing every tab, so the window closes by itself, starts fresh next time. Shells
start fresh in their saved directories: running programs and fullscreen state are
not restored, and directories that no longer exist fall back to `shell.cwd`.
With `scrollback`, each tab's history and primary screen (never a fullscreen
program's screen) are replayed above a bold accent-colored `Session restored: <date>` block that
names the setting to change. Contents are captured only at quit, so a crash
restores tabs without them. While one cometty instance is running, other
instances start fresh and do not change its saved session. The snapshot lives in
`${XDG_STATE_HOME:-$HOME/.local/state}/cometty/session.toml`, and scrollback
dumps in the owner-only `scrollback/` directory next to it; dumps are deleted
once no snapshot references them.

## Logs

| Section / key | Default | Meaning |
| --- | --- | --- |
| `log.level` | `"debug"` | cometty recording level: `off`, `error`, `warn`, `info`, `debug`, `trace` |
| `log.filter` | `""` | Optional RUST_LOG-style recording directives overriding `level` |
| `log.buffer_lines` | `1000` | Retained log entries, clamped to 100–100,000; large messages may reduce the retained count |

The default records cometty debug messages and dependency warnings. To add
graphics diagnostics, for example:

```toml
[log]
filter = "cometty=debug,wgpu=debug,warn"
```

The Logs window's level/text filters only change what is displayed. Recording
continues while it is closed; its contents are in-memory, not a persistent log
file. Invalid recording filters keep the previous valid filter and show an error.
stderr logging independently follows `RUST_LOG` (default `error`). To disable
**all** recording, use `filter = "off"` and launch with `RUST_LOG=off`.

OSC clipboard contents are redacted, but other logs can include titles, paths,
and URL previews. Review copied logs before sharing them.
