# User Guide

This guide covers everyday workflows, installation options, automatic updates,
privacy defaults, and known limitations in Cometty. For every config key and
default value, see the [Configuration Reference](configuration.md).

## Installing and launching

### macOS installer options

The [README](../README.md#install) shows the default one-line install for macOS
13 or later (universal binary for Apple Silicon and Intel). You can pass options
to `install.sh` via `sh -s --`:

```sh
curl -fsSL https://raw.githubusercontent.com/jjunac/cometty/main/install.sh | sh -s -- --version 0.3.0 --dir ~/Applications
```

Available flags:

- `--version X.Y.Z` (or `COMETTY_VERSION`) — install a specific release instead
  of the latest published release.
- `--dir PATH` (or `COMETTY_INSTALL_DIR`) — install directory (defaults to
  `/Applications`, falling back to `~/Applications` if not writable).
- `--force` — install even if Cometty is currently running from the destination.
- `-h` / `--help` — show installer help.

### Manual macOS installation

Download `Cometty-<version>-macos-universal.zip` from the
[releases page](https://github.com/jjunac/cometty/releases/latest), unzip it,
and move `Cometty.app` to `/Applications`.

Because release builds are ad-hoc signed and not notarized yet, macOS blocks a
browser-downloaded copy on first launch. Either clear its quarantine flag:

```sh
xattr -dr com.apple.quarantine /Applications/Cometty.app
```

or try opening Cometty once, then open **System Settings → Privacy & Security**
and click **Open Anyway** next to Cometty.

### Login shell and command-line flags

- When launched from `Cometty.app`, Cometty starts your default shell as a
  **login shell** (`-l`), so Finder launches inherit your normal login `PATH` and
  shell profiles. Set `[shell] shell` explicitly in `config.toml` if you prefer
  a non-login shell.
- When launched from a terminal or built from source (see
  [CONTRIBUTING.md](../CONTRIBUTING.md#building-from-source)), Cometty starts
  `$SHELL` in your home directory (falling back to `/bin/bash`), and requires a
  graphical desktop and working graphics adapter.
- Command-line flags:

```sh
cometty --theme tokyo-night
cometty --list-themes
cometty --config ~/.config/cometty/config.toml
cometty --help
```

`--theme` overrides your saved theme for the current session only; `--config`
selects the file to read and save.

## Tabs, status badges, and background tabs

### Tab titles and reordering

- Tabs show your foreground command and working directory by default
  (`$command | $cwd`). When a running program sets its own title (for example
  `opencode`), that live title replaces the template while it runs.
- Click a tab to switch to it, its **×** to close it, or **+** to open a new
  tab.
- Drag a tab left or right to reorder it; hold near either edge of a crowded tab
  strip to scroll. Release to drop, or press **Esc** to cancel. Dropping outside
  the strip cancels rather than opening a new window. Tab order is saved with
  your session.
- On macOS, drag the empty space around the tabs (not the tabs or their buttons)
  to move the window.

### Program status badges and attention glow

- Programs that report their state (such as `opencode`) show per-tab counts next
  to the tab title for sessions **waiting on you**, **working**, **finished**,
  **failed**, or **idle**. A session counts as finished until you view its tab
  (closing a busy session counts too).
- A tab you aren't currently viewing (an inactive tab, or the active tab while
  the window is unfocused) glows when its program needs your attention, rings
  the bell, or sends a notification. Configure the style with
  `[tabbar] attention` (`"pulse"`, `"static"`, or `"off"`).

### Background (parked) tabs

Use parked tabs for long-running processes like dev servers or file watchers
that you want to keep alive without taking up a slot in the tab strip:

- **Park:** Hover a tab and click **↓**, or press `Cmd+B` (macOS) /
  `Ctrl+Shift+B` (Linux) to park the active tab. It moves behind a compact count
  badge next to **+**.
- **Peek & manage:** Press `Cmd+Shift+B` (macOS) to peek at the most recent
  parked tab, or click the tray badge to list all parked tabs:
  - Click a row to preview it in place (the tab strip stays unchanged).
  - Click **↑** (or press `Cmd+B` / `Ctrl+Shift+B` while peeking) to move it
    back to the tab strip.
  - Click **×** to close it.
  - Click any foreground tab to leave the preview; the previewed tab stays
    parked.
- **Activity indicator:** A dot on the tray badge marks a running job and
  changes color when a background task finishes.
- **Last visible tab:** Parking the only visible foreground tab opens a fresh
  shell prompt in the same working directory so the window always keeps one
  foreground tab. Parked tabs are saved with your session.

## Search, selection, and clickable links

### Search

- Press `Cmd+F` (macOS) or `Ctrl+F` (Linux) to open or re-focus the search bar.
- Queries are **smart-case**: all-lowercase queries ignore case, while including
  any uppercase character makes the search case-sensitive.
- Press **Enter** / **Shift+Enter** (or **F3** / **Shift+F3**) to step forward
  and backward through matches with wrap-around; press **Esc** to close.

### Selection and mouse input

- Drag to select text, double-click to select a word, or triple-click to select
  a full line.
- Scroll through history with the mouse wheel, trackpad, scrollbar, or
  `Shift+PageUp` / `Shift+PageDown` / `Shift+Home` / `Shift+End`.
- Hold **Shift** to select or scroll locally when a terminal application (such
  as `vim` or `tmux`) is capturing mouse input.

### Clickable URLs and local paths

Hold **Cmd** (macOS) or **Ctrl** (Linux) and click to open:

- Web links (`http`, `https`, `ftp`, `mailto`, and `www.`).
- Local `file://` URLs (remote hosts are refused).
- Existing local file or directory paths—absolute, `~/…`, or relative to the
  tab's working directory—including `:line:col` and `(line,col)` compiler
  locations.

To open clicked files directly in your editor at the clicked line, set
`[links] file_opener` in `config.toml` (for example
`code -g {path}:{line}:{col}` or `zed {path}:{line}:{col}`). To disable local
file opening completely, set `[links] open_files = false`.

## Cursor, settings, logs, and session restore

### Live settings and Logs viewer

- Open **Settings** with `Cmd+,` (macOS) or `Ctrl+,` (Linux). Changes apply
  live and save automatically; modified rows show a reset button on hover, and
  each section (or the whole dialog) can be reset to defaults.
- Direct edits to `~/.config/cometty/config.toml` take effect on the next
  launch. Shell and starting-directory edits apply to newly opened tabs.
- Open the built-in **Logs** viewer with `Cmd+Shift+L` (macOS) or
  `Ctrl+Shift+L` (Linux) for troubleshooting. Display filters in the Logs window
  are independent of what is recorded; see
  [Configuration → Logs](configuration.md#logs).

### Cursor blinking

The cursor blinks only while the window is focused, and stops blinking 10
seconds after your last input; typing or returning to the window restarts it.
Set `[cursor] blink_timeout_ms = 0` to keep blinking as long as the window has
focus. When the window loses focus, a block cursor is drawn as an outline.

### Session restore

Quitting with tabs open (`Cmd+Q` or closing the window) saves your window size
and position, tab order (including parked tabs), each tab's working directory,
and its scrollback so they reopen on the next launch.

- Each restored tab shows its previous contents above a highlighted
  `Session restored: <date>` block.
- Closing every tab yourself ends the session so the next launch starts fresh
  with a single tab.
- Running programs are not restarted, and a fullscreen application (`vim`,
  `less`) left open at quit comes back as the underlying shell screen.
- Set `[session] restore = "tabs"` to reopen tabs without scrollback contents,
  or `"off"` to always start fresh.

## Automatic updates

Cometty checks for new releases about 5 seconds after launch and then every 24
hours. Only published releases on the
[releases page](https://github.com/jjunac/cometty/releases) are offered.

To turn automatic checks off, clear **Settings → Updates → Check for updates
automatically** (enabled by default), or set `[update] enabled = false` in
`config.toml`. Each check queries `api.github.com` for the latest release and
sends only a `User-Agent` header containing your Cometty version.

### In-place updates (`Cometty.app` on macOS)

When running from a writable Applications folder (such as `/Applications`),
Cometty downloads and verifies updates in the background and shows a small
notice in the bottom-right corner saying **Cometty X is ready.**

- **Restart to update** quits Cometty (saving your session like a normal quit),
  swaps the app bundle in place, and relaunches it.
- **Later** dismisses the notice until the next launch.
- If other Cometty windows from the same app bundle are running, the notice asks
  you to quit them first before restarting.
- If installation fails (for example, macOS denies modifying apps), the notice
  explains why and keeps **Restart to update** available so you can retry.
- Because release builds are ad-hoc signed, macOS may ask again for privacy
  permissions you granted Cometty (such as Full Disk Access) after an update.

### Notify-only installs

Some installs cannot update in place:

- The app runs from a read-only location.
- macOS runs it from a temporary App Translocation path because it was opened
  directly from Downloads without moving it first.
- You launched Cometty via `cargo install`, `cargo run`, or on Linux.

In these cases (and whenever an automatic download fails), the notice displays
**Cometty X is available.** with a **View release** button—and suggests moving
the app to a writable Applications folder when that would enable in-place
updates.

### What is verified

Cometty downloads updates over HTTPS from GitHub and verifies the archive
against the SHA-256 checksum published with the release. That catches corrupted
or truncated downloads, but not a compromised release account. Builds are not
notarized, and macOS Gatekeeper does not re-check updates downloaded by Cometty
itself. If you want stronger manual control, turn off automatic checks and
update from the releases page.

## Safety and privacy

- **Clipboard (`OSC 52`):** Applications can write to your clipboard by default,
  but cannot read it. Set `[clipboard] osc52 = "off"` to block all application
  clipboard access. Only enable `"read-write"` if you trust every application
  you run—including remote SSH sessions—to read your clipboard.
- **Local file opening:** Executable files (`+x` binaries) and app bundles or
  installers (`.app`, `.command`, `.pkg`, `.dmg`, `.desktop`, `.exe`, etc.) are
  revealed in Finder / your file manager rather than launched when using the OS
  default opener.
- **Restored scrollback on disk:** At quit, each tab's history—which may contain
  secrets printed to your terminal—is written to
  `${XDG_STATE_HOME:-$HOME/.local/state}/cometty/scrollback/` with owner-only
  permissions. It is deleted shortly after the next launch, when you close every
  tab, or when `[session] restore` is `"tabs"` or `"off"`. Use `"tabs"` or
  `"off"` if terminal history should never be written to disk.
- **Oversized paste / full input notices:** An input-full or oversized-paste
  notice means the input was **not sent**. Wait for the running program to drain
  input, then retry with a smaller paste.
- **Closing tabs:** Closing a tab stops its shell and requests that its
  foreground job stop, discarding any unsent input. There is currently **no
  confirmation prompt for running work**, so save or finish work first. Closing
  the last shell closes the window and starts fresh on the next launch.

## Known limitations

- **Bell:** The bell makes no sound and does not flash the screen; it only makes
  an unseen tab glow.
- **Controls:** There is no keyboard font zoom and no close confirmation for
  running jobs yet.
- **Program status & notifications:** Status messages are not displayed (only
  per-tab counts). Desktop notifications and terminal progress bars (`OSC 9;4`)
  are not supported yet.
- **Wrapped rows:** Search does not span displayed row boundaries, and wrapped
  plain-text URLs/paths are detected separately on each row.
- **Font picker in Settings:** Named installed fonts work via `[font] family` in
  `config.toml`, while the Settings UI dropdown currently lists only generic
  presets (showing a text field once a custom family is set).
- **Session restore edge cases:** Scrollback is captured only on a normal quit
  (after a crash, tabs restore without their previous contents). Restoring into
  a narrower window re-wraps long lines.
- **Memory caps:** Large scrollbacks, many open tabs, and dense searches can use
  significant memory; scrollback limits apply per tab, not as an application-wide
  RAM ceiling.
- **Platforms:** Prebuilt releases currently cover macOS 13+ only (ad-hoc
  signed, not notarized).
