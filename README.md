<p align="center">
  <img src="logo.svg" width="160" alt="cometty logo" />
</p>

# cometty

A fast, GPU-accelerated terminal emulator built for everyday workflows—with
smart tabs, background sessions, clickable links, and automatic session restore.

> **Platforms:** Prebuilt universal app for **macOS 13+** (Apple Silicon &
> Intel). Builds from source on Linux.

## Install

### macOS (recommended)

```sh
curl -fsSL https://raw.githubusercontent.com/jjunac/cometty/main/install.sh | sh
```

This downloads the latest release from GitHub, verifies its SHA-256 checksum,
and installs `Cometty.app` to `/Applications` (or `~/Applications` if
`/Applications` is not writable). Because it is not browser-quarantined, Cometty
opens immediately without a Gatekeeper prompt.

<details>
<summary>Manual download or installer options</summary>

Download `Cometty-<version>-macos-universal.zip` from the
[releases page](https://github.com/jjunac/cometty/releases/latest), unzip it,
and move `Cometty.app` to `/Applications`.

Release builds are ad-hoc signed and not notarized yet, so macOS blocks a
browser-downloaded copy on first launch. Either clear its quarantine attribute:

```sh
xattr -dr com.apple.quarantine /Applications/Cometty.app
```

or try opening Cometty once, then click **Open Anyway** in **System Settings →
Privacy & Security**.

For installer flags (`--version`, `--dir`, `--force`), see the
[User Guide](docs/usage.md#installing-and-launching).

</details>

### Linux and building from source

With a current [Rust toolchain](https://rustup.rs/) and platform graphics
libraries installed:

```sh
cargo install --path . --locked
cometty
```

See [CONTRIBUTING.md](CONTRIBUTING.md#building-from-source) for Debian/Ubuntu
package prerequisites and build instructions.

## Highlights

- **Smart tabs & attention glow** — Tabs display the running command and working
  directory (or a live program title), support drag-to-reorder, and glow when an
  unseen tab needs you. Programs that report session status (such as `opencode`)
  also show per-tab status counts.
- **Background (parked) tabs** — Tuck dev servers or watchers out of the tab
  strip (`Cmd+B` / `Ctrl+Shift+B` or hover `↓`) into a compact tray badge, and
  peek at them anytime without losing your foreground layout.
- **Automatic session restore** — Quit with tabs open and pick up where you left
  off: window size and position, tab order, working directories, and scrollback
  return on next launch.
- **Searchable scrollback** — Find text across your screen and history
  (`Cmd+F` / `Ctrl+F`) with smart-case matching and wrap-around navigation.
- **Clickable links & file paths** — Hold `Cmd` (macOS) or `Ctrl` (Linux) and
  click web URLs, `file://` links, or local paths (including `path:line:col`
  compiler output) to open them in your browser or editor.
- **Built-in themes & live settings** — Includes **tokyo-night**, **vscode**,
  and **tomorrow-night**, an in-app Settings dialog (`Cmd+,` / `Ctrl+,`) that
  applies live and saves automatically, and a built-in Logs viewer
  (`Cmd+Shift+L` / `Ctrl+Shift+L`).
- **Modern terminal support** — Truecolor, styled text, emoji, combining
  characters, double-width text, mouse reporting, bracketed paste, and full
  support for TUI apps like `vim`, `less`, and `htop`.
- **Automatic updates (macOS)** — Checks GitHub Releases in the background,
  verifies SHA-256 checksums, and updates on restart. Can be turned off in
  Settings.

## Shortcuts

| Action | macOS | Linux |
| --- | --- | --- |
| New tab | `Cmd+T` | `Ctrl+T` |
| Copy / paste | `Cmd+C` / `Cmd+V` | `Ctrl+Shift+C` / `Ctrl+Shift+V` |
| Next / previous tab | `Ctrl+Tab` / `Ctrl+Shift+Tab` | `Ctrl+Tab` / `Ctrl+Shift+Tab` |
| Jump to tab 1–9 | `Cmd+1–9` | `Super+1–9` |
| Park / unpark background tab | `Cmd+B` | `Ctrl+Shift+B` |
| Peek background tab | `Cmd+Shift+B` | Click the tray badge |
| Find / re-focus search | `Cmd+F` | `Ctrl+F` |
| Settings | `Cmd+,` | `Ctrl+,` |
| Logs | `Cmd+Shift+L` | `Ctrl+Shift+L` |
| Page through history | `Shift+PageUp` / `Shift+PageDown` | `Shift+PageUp` / `Shift+PageDown` |
| History top / live bottom | `Shift+Home` / `Shift+End` | `Shift+Home` / `Shift+End` |

- **Mouse:** Drag to select, double-click a word, or triple-click a line. Hold
  `Shift` to select or scroll locally when a terminal app is capturing the mouse.
- **Links:** Hold `Cmd` (macOS) or `Ctrl` (Linux) and click any URL or existing
  local path to open it.
- **Tabs:** Click `+` for a new tab, `×` to close, `↓` to park, or drag tabs to
  reorder. On macOS, drag the empty space around the tabs to move the window.

See the [User Guide](docs/usage.md) for search keys, parked-tab workflows,
status badges, and cursor/session behavior.

## Configuration

Open **Settings** with `Cmd+,` (macOS, or **Cometty → Settings…**) or `Ctrl+,`
(Linux). Changes apply immediately and save to `~/.config/cometty/config.toml`.

You can also edit `config.toml` directly (only include keys you want to change):

```toml
[theme]
name = "tokyo-night"          # "tokyo-night", "vscode", or "tomorrow-night"

[font]
family = "monospace"          # or an installed font family name
size = 14.0

[links]
file_opener = "code -g {path}:{line}:{col}"  # optional editor command
```

Pass `--theme NAME`, `--config PATH`, or `--list-themes` on the command line for
one-off overrides. See the [Configuration Reference](docs/configuration.md) for
every option, custom shortcuts, tab-label templates, and logging filters.

## Good to know

- **Clipboard safety:** Terminal apps can write to your clipboard by default,
  but cannot read it (`[clipboard] osc52 = "write"`).
- **Local links:** Executable files and app bundles are revealed in Finder /
  your file manager instead of launched directly.
- **Session scrollback on disk:** At quit, tab history is saved in an owner-only
  state directory and removed after the next launch. Set
  `[session] restore = "tabs"` or `"off"` if history should never touch disk.
- **Closing tabs:** Closing a tab stops its shell without a confirmation prompt.
- **Current limitations:** No audible/visual bell (unseen tabs glow instead), no
  keyboard font zoom yet, and prebuilt releases are currently macOS-only
  (ad-hoc signed, not notarized).

See the [User Guide](docs/usage.md#safety-and-privacy) for full details on
updates, privacy, and known limitations.

## Documentation & Contributing

- **For users:** [User Guide](docs/usage.md) ·
  [Configuration Reference](docs/configuration.md) · [Roadmap](ROADMAP.md)
- **For contributors:** [Contributing Guide](CONTRIBUTING.md) ·
  [Architecture & Technical Docs](docs/README.md)
