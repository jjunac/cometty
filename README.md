<p align="center">
  <img src="logo.svg" width="160" alt="cometty logo" />
</p>

# cometty

A GPU-accelerated terminal emulator with tabs, themes, searchable scrollback,
and Unicode support.

## Features

- Multiple tabs with command and working-directory labels.
- Search across your screen and scrollback, with highlighted matches and
  wrap-around navigation.
- Three built-in themes: **tokyo-night**, **vscode**, and **tomorrow-night**.
- Live settings with automatic saving and reset controls.
- Mouse selection, word/line selection, copy/paste, and clickable links.
- Truecolor, styled text, emoji, combining characters, and double-width text.
- Support for terminal applications such as vim, less, and htop, including
  application mouse input and bracketed paste.
- A built-in log viewer for troubleshooting.

## Install and launch

### macOS (prebuilt app)

Download the latest `Cometty-<version>-macos-universal.zip` from the
[releases page](https://github.com/jjunac/cometty/releases/latest), unzip it,
and move `Cometty.app` to `/Applications`. The build is universal (Apple
Silicon and Intel) and targets macOS 13 or later.

The bundled app starts your shell as a login shell, so Finder launches see your
normal login PATH and profiles. Set `shell.shell` explicitly if you prefer a
non-login shell.

Release builds are ad-hoc signed, so macOS blocks the first launch. Try opening
Cometty once, then open **System Settings → Privacy & Security** and click
**Open Anyway** next to Cometty. Builds are not notarized yet.

Prebuilt releases currently cover macOS only.

### From source

To build from a source checkout, you need a current
[Rust toolchain](https://rustup.rs/). On Debian/Ubuntu, install these packages first:

```sh
sudo apt-get install -y libx11-dev libxkbcommon-dev libgl1-mesa-dev
```

From the cometty checkout:

```sh
cargo install --path . --locked
cometty
```

Ensure Cargo's install directory (usually `~/.cargo/bin`) is on your PATH.
Launching cometty requires a graphical desktop and a working graphics adapter.
On macOS/Linux, it starts your `$SHELL` in your home directory, falling back to
`/bin/bash` if needed.

```sh
cometty --theme tokyo-night
cometty --list-themes
cometty --config ~/.config/cometty/config.toml
cometty --help
```

## Keyboard and mouse

| Action | macOS | Linux |
| --- | --- | --- |
| New tab | Cmd+T | Ctrl+T |
| Copy / paste | Cmd+C / Cmd+V | Ctrl+Shift+C / Ctrl+Shift+V |
| Next / previous tab | Ctrl+Tab / Ctrl+Shift+Tab | Ctrl+Tab / Ctrl+Shift+Tab |
| Jump to tab 1–9 | Cmd+1–9 | Super+1–9 |
| Find / re-focus search | Cmd+F | Ctrl+F |
| Settings | Cmd+, | Ctrl+, |
| Logs | Cmd+Shift+L | Ctrl+Shift+L |
| Page through history | Shift+PageUp / Shift+PageDown | Shift+PageUp / Shift+PageDown |
| History top / live bottom | Shift+Home / Shift+End | Shift+Home / Shift+End |

While search is open, **Enter** / **Shift+Enter** (or **F3** / **Shift+F3**) moves
to the next/previous match; **Esc** closes it. Lowercase queries ignore case;
including an uppercase character makes a query case-sensitive.

Drag to select text, double-click a word, or triple-click a line. Use the wheel
or scrollbar to browse history. Hold **Shift** to select/scroll locally when an
application is handling the mouse.

Hold **Cmd** on macOS or **Ctrl** on Linux and click a link to open it. Click a
tab to switch, its **×** to close, or **+** to open another.

## Preferences

Open Settings with the shortcut above, or edit
`$HOME/.config/cometty/config.toml`. Missing files or settings use defaults:

```toml
[theme]
name = "tokyo-night"

[font]
family = "monospace"
size = 14.0

[terminal]
scrollback_lines = 10000

[tabbar]
title_format = "$command | $cwd"
```

Settings changes save automatically. File edits take effect on the next launch;
shell/starting-directory changes in Settings apply to new tabs. `--theme`
overrides your saved theme for the current session, and `--config` selects a
different preferences file.

The cursor blinks only while the window has focus, and stops blinking 10 seconds
after you last typed; typing or returning to the window restarts it. Set
`[cursor] blink_timeout_ms = 0` to blink for as long as the window has focus. In
a window without focus, the block cursor is drawn as an outline.

Quitting with tabs open (Cmd+Q, or closing the window) reopens the same tabs, in
their working directories, at the next launch, with their scrollback and the
window size and position. Each restored tab shows its previous contents followed
by a highlighted `Session restored: <date>` block. Closing every tab yourself starts
fresh next time. Running programs are not restored, and a fullscreen program
(vim, less) left open at quit comes back as the shell screen underneath it. Set
`[session] restore = "tabs"` to restore tabs without their contents, or `"off"`
to always start fresh.

See the [configuration reference](docs/configuration.md) for all options,
custom shortcuts, tab labels, and logging preferences.

## Clipboard, links, and closing tabs

- Applications can set your clipboard by default, but cannot read it. Set
  `[clipboard] osc52 = "off"` to disable application clipboard access. Only enable
  `"read-write"` if you trust applications—including remote ones—to read your
  clipboard.
- Web, FTP, and email links can open; local `file` links are refused.
- **Restored scrollback is saved to disk.** At quit, each tab's history (which
  may include secrets you printed) is written to
  `${XDG_STATE_HOME:-$HOME/.local/state}/cometty/scrollback/`, readable only by
  you. It is deleted shortly after the next launch, or when you close every tab
  or set `[session] restore` to `"tabs"` or `"off"`. Use one of those settings
  if the history should never touch the disk.
- An input-full or oversized-paste notice means that input was **not sent**.
  Wait for the program to read, then retry with a smaller paste.
- Closing a tab on macOS/Linux ends its shell and requests that its foreground
  job stop. Unsent input is discarded. There is currently **no confirmation for
  running work**, so save or finish it first. Closing the last shell closes the
  window, and the next launch starts with a single fresh tab.

## Current limitations

- No audible/visual bell, keyboard font zoom, or running-job close confirmation.
- Search does not span displayed row boundaries; wrapped plain-text links are
  detected separately on each row.
- Font choice currently uses generic families rather than individual font names.
- Scrollback is saved only when you quit: after a crash, tabs come back without
  their contents. Restoring into a narrower window re-wraps long lines.
- Large histories, many tabs, and dense searches can use substantial memory;
  scrollback limits are per tab, not an application-wide RAM limit.
