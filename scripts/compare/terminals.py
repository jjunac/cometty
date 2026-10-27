"""Per-terminal adapters for compare.py: normalized config, launch, tabs, quit.

Every adapter makes each window/tab run RUNDIR/bin/agent (driver.py agent) instead
of a shell, with Menlo 14 (what cometty's generic `monospace` resolves to on
macOS), 10,000 history lines (Ghostty: a byte budget), cursor blink off, bells
and ligatures off, no session/window restoration, and no user config files.
Window position and size are set afterwards through Accessibility.

Settings that cannot be isolated per launch are backed up and restored: the
Terminal.app and iTerm2 preference domains (`defaults export/import`) and the
temporary iTerm2 Dynamic Profile.
"""
import base64
import json
import os
import plistlib
import shutil
import subprocess
import time

PROFILE = "cometty-bench"
FONT_FAMILY, FONT_PS, FONT_SIZE = "Menlo", "Menlo-Regular", 14
SCROLLBACK_LINES = 10000


def sh(*cmd, check=True, **kw):
    r = subprocess.run(cmd, capture_output=True, text=True, **kw)
    if check and r.returncode != 0:
        raise RuntimeError("%s failed (%d): %s" % (" ".join(cmd), r.returncode, r.stderr.strip()))
    return r


def osascript(script, check=True):
    return sh("osascript", "-e", script, check=check)


def processes():
    """[(pid, executable path)] for every process of this user."""
    out = sh("ps", "-x", "-o", "pid=,comm=").stdout
    procs = []
    for line in out.splitlines():
        pid, _, path = line.strip().partition(" ")
        if pid.isdigit():
            procs.append((int(pid), path.strip()))
    return procs


def bundle_version(app):
    try:
        with open(os.path.join(app, "Contents", "Info.plist"), "rb") as f:
            info = plistlib.load(f)
        return "%s (%s)" % (info.get("CFBundleShortVersionString", "?"), info.get("CFBundleVersion", "?"))
    except OSError:
        return "missing"


class Terminal:
    name = ""
    app = ""          # .app bundle path
    bundle_id = ""
    helpers = ()      # extra executable path fragments that belong to the terminal

    def __init__(self, ctx):
        self.ctx = ctx                       # compare.Context: rundir, agent, geometry, tools
        self.cfgdir = os.path.join(ctx.rundir, "config", self.name)
        os.makedirs(self.cfgdir, exist_ok=True)

    # ---- discovery
    def available(self):
        return os.path.isdir(self.app)

    def version(self):
        return bundle_version(self.app)

    def pids(self):
        """Terminal-owned processes: the app plus helpers, never the agents."""
        prefix = os.path.join(self.app, "Contents") + "/"
        return [pid for pid, path in processes()
                if path.startswith(prefix) or any(h in path for h in self.helpers)]

    def ui_pid(self):
        main = os.path.join(self.app, "Contents", "MacOS") + "/"
        pids = [pid for pid, path in processes() if path.startswith(main)]
        return min(pids) if pids else None

    # ---- lifecycle
    def prepare(self):
        """Write configs before the first launch."""

    def launch(self):
        raise NotImplementedError

    def new_tab(self):
        self.ctx.activate(self.ui_pid())
        osascript('tell application "System Events" to keystroke "t" using command down')

    def quit(self):
        osascript('tell application id "%s" to quit' % self.bundle_id, check=False)

    def cleanup(self):
        """Undo prepare(); runs once at exit even after failures."""

    def _open(self, *args, new=True, env=None):
        cmd = ["open"] + (["-n"] if new else []) + ["-a", self.app]
        for k, v in (env or {}).items():
            cmd += ["--env", "%s=%s" % (k, v)]
        if args:
            cmd += ["--args", *args]
        sh(*cmd)


class Cometty(Terminal):
    name = "cometty"
    bundle_id = "com.github.jjunac.cometty"

    def __init__(self, ctx):
        # A private copy, so a host cometty (any path) is never matched or quit.
        self.app = ctx.cometty_app
        super().__init__(ctx)

    def version(self):
        return "%s rev %s" % (bundle_version(self.app), self.ctx.revision)

    def prepare(self):
        self.home = os.path.join(self.cfgdir, "home")
        os.makedirs(self.home, exist_ok=True)
        g = self.ctx.geometry
        self.config = os.path.join(self.cfgdir, "config.toml")
        with open(self.config, "w") as f:
            f.write("""[font]
family = "monospace"
size = %.1f

[window]
width = %d
height = %d

[cursor]
default_blinking = false

[terminal]
scrollback_lines = %d

[shell]
shell = "%s"

[session]
restore = "off"

[log]
level = "off"
""" % (FONT_SIZE, g["w"], g["h"], SCROLLBACK_LINES, self.ctx.agent))

    def launch(self):
        env = {"HOME": self.home, "XDG_STATE_HOME": os.path.join(self.home, "state"),
               "XDG_CONFIG_HOME": os.path.join(self.home, "config"), "RUST_LOG": "off",
               "COMETTY_PERF": "0"}
        self._open("--config", self.config, env=env)


class Ghostty(Terminal):
    name = "ghostty"
    app = "/Applications/Ghostty.app"
    bundle_id = "com.mitchellh.ghostty"

    def prepare(self):
        self.config = os.path.join(self.cfgdir, "config")
        with open(self.config, "w") as f:
            f.write("""command = direct:%s
font-family = %s
font-size = %d
font-feature = -calt
font-feature = -liga
# Ghostty limits history by bytes, not lines: ~10k 80-column styled rows.
scrollback-limit = 10000000
cursor-style-blink = false
window-save-state = never
confirm-close-surface = false
quit-after-last-window-closed = true
shell-integration = none
auto-update = off
resize-overlay = never
background-opacity = 1
""" % (self.ctx.agent, FONT_FAMILY, FONT_SIZE))

    def launch(self):
        self._open("--config-default-files=false", "--config-file=%s" % self.config)


class Kitty(Terminal):
    name = "kitty"
    app = "/Applications/kitty.app"
    bundle_id = "net.kovidgoyal.kitty"

    def prepare(self):
        # kitty substitutes {kitty_pid}; without it, it appends "-<pid>" anyway.
        self.sock = os.path.join("/tmp", "cometty-bench-kitty-%d" % os.getpid())
        self.config = os.path.join(self.cfgdir, "kitty.conf")
        g = self.ctx.geometry
        with open(self.config, "w") as f:
            f.write("""font_family %s
font_size %d
disable_ligatures always
scrollback_lines %d
cursor_blink_interval 0
shell %s
shell_integration disabled
enable_audio_bell no
visual_bell_duration 0
confirm_os_window_close 0
allow_remote_control socket-only
listen_on unix:%s-{kitty_pid}
startup_session none
remember_window_size no
initial_window_width %d
initial_window_height %d
macos_quit_when_last_window_closed yes
update_check_interval 0
""" % (FONT_FAMILY, FONT_SIZE, SCROLLBACK_LINES, self.ctx.agent, self.sock, g["w"], g["h"]))

    def launch(self):
        env = {"KITTY_CONFIG_DIRECTORY": self.cfgdir}
        self._open("--config", self.config, "--single-instance=no", env=env)

    def new_tab(self):
        sh(os.path.join(self.app, "Contents", "MacOS", "kitten"), "@", "--to",
           "unix:%s-%d" % (self.sock, self.ui_pid()), "launch", "--type=tab")


class _PrefsBackup(Terminal):
    """Adapters that must touch a global preference domain back it up first."""

    def _backup(self):
        self.backup = os.path.join(self.cfgdir, "%s.prefs-backup.plist" % self.bundle_id)
        if not os.path.exists(self.backup):
            sh("defaults", "export", self.bundle_id, self.backup)

    def _restore(self):
        if not (getattr(self, "backup", None) and os.path.exists(self.backup)):
            return
        # `defaults import` merges, so first delete keys the run added.
        with open(self.backup, "rb") as f:
            before = plistlib.load(f)
        now = plistlib.loads(sh("defaults", "export", self.bundle_id, "-").stdout.encode())
        for key in set(now) - set(before):
            sh("defaults", "delete", self.bundle_id, key, check=False)
        sh("defaults", "import", self.bundle_id, self.backup)


class ITerm2(_PrefsBackup):
    name = "iterm2"
    app = "/Applications/iTerm.app"
    bundle_id = "com.googlecode.iterm2"
    helpers = ("/iTermServer-",)

    def prepare(self):
        self._backup()
        d = os.path.expanduser("~/Library/Application Support/iTerm2/DynamicProfiles")
        os.makedirs(d, exist_ok=True)
        self.dynamic = os.path.join(d, PROFILE + ".json")
        g = self.ctx.geometry
        profile = {
            "Name": PROFILE, "Guid": PROFILE + "-0001",
            "Custom Command": "Yes", "Command": self.ctx.agent,
            "Normal Font": "%s %d" % (FONT_PS, FONT_SIZE), "Use Non-ASCII Font": False,
            "ASCII Ligatures": False, "Non-ASCII Ligatures": False,
            "Scrollback Lines": SCROLLBACK_LINES, "Unlimited Scrollback": False,
            "Blinking Cursor": False, "Silence Bell": True, "Visual Bell": False,
            "Flashing Bell": False, "Transparency": 0, "Blur": False,
            "Close Sessions On End": True, "Prompt Before Closing 2": 0,
            "Columns": max(20, g["w"] // 8), "Rows": max(10, g["h"] // 17),
        }
        with open(self.dynamic, "w") as f:
            json.dump({"Profiles": [profile]}, f, indent=2)
        shutil.copy(self.dynamic, os.path.join(self.cfgdir, PROFILE + ".json"))

    def launch(self):
        # Argument-domain overrides apply to this launch only.
        self._open("-OpenNoWindowsAtStartup", "YES", "-OpenArrangementAtStartup", "NO",
                   "-NSQuitAlwaysKeepsWindows", "NO", "-PromptOnQuit", "NO",
                   "-SUEnableAutomaticChecks", "NO", new=False)
        deadline = time.time() + 30
        while time.time() < deadline:
            r = osascript('tell application "iTerm" to create window with profile "%s"' % PROFILE,
                          check=False)
            if r.returncode == 0:
                return
            time.sleep(0.2)  # profile not loaded / app not scriptable yet
        raise RuntimeError("iTerm2 did not accept create window: " + r.stderr)

    def new_tab(self):
        osascript('tell application "iTerm" to tell current window to create tab with profile "%s"'
                  % PROFILE)

    def cleanup(self):
        if getattr(self, "dynamic", None) and os.path.exists(self.dynamic):
            os.unlink(self.dynamic)
        self._restore()


class TerminalApp(_PrefsBackup):
    name = "terminal"
    app = "/System/Applications/Utilities/Terminal.app"
    bundle_id = "com.apple.Terminal"

    def prepare(self):
        self._backup()
        font = json.loads(sh(self.ctx.termlat, "font-archive", "--name", FONT_PS,
                             "--size", str(FONT_SIZE)).stdout)["base64"]
        g = self.ctx.geometry
        profile = {
            "name": PROFILE, "type": "Window Settings", "ProfileCurrentVersion": 2.07,
            "CommandString": self.ctx.agent, "RunCommandAsShell": True,
            "Font": base64.b64decode(font), "FontAntialias": True,
            "ShouldLimitScrollback": 1, "ScrollbackLines": SCROLLBACK_LINES,
            "CursorBlink": False, "Bell": False, "VisualBell": False,
            "shellExitAction": 0, "warnOnShellCloseAction": 0,
            "columnCount": max(20, g["w"] // 8), "rowCount": max(10, g["h"] // 17),
        }
        with open(self.backup, "rb") as f:
            prefs = plistlib.load(f)
        prefs.setdefault("Window Settings", {})[PROFILE] = profile
        prefs["Default Window Settings"] = PROFILE
        prefs["Startup Window Settings"] = PROFILE
        prefs["NSQuitAlwaysKeepsWindows"] = False
        staged = os.path.join(self.cfgdir, "com.apple.Terminal.bench.plist")
        with open(staged, "wb") as f:
            plistlib.dump(prefs, f, fmt=plistlib.FMT_BINARY)
        sh("defaults", "import", self.bundle_id, staged)

    def launch(self):
        self._open("-NSQuitAlwaysKeepsWindows", "NO", new=False)

    def cleanup(self):
        self._restore()


ADAPTERS = {c.name: c for c in (Cometty, TerminalApp, ITerm2, Ghostty, Kitty)}
