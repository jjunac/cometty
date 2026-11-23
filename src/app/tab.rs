//! Tab model: each tab owns a terminal + PTY plus its per-view state.
//!
//! `App` holds `tabs: Vec<Tab>` and an `active` index; the renderer,
//! window, clipboard, and blink state stay shared. Tab management
//! (spawn/switch/close/reorder) lives here so `keyboard` / `pty_io` / `redraw`
//! stay thin.

use std::time::{Duration, Instant};

use crate::config::{Config, TabbarConfig};
use crate::procinfo;
use crate::pty::PtySession;
use crate::scrollbar::ScrollbarUi;
use crate::selection::Selection;
use crate::term::Terminal;

use super::search::SearchPanel;
use super::{App, UserEvent};

/// Kernel facts for the `$command` / `$cwd` label variables, refreshed
/// only while the tab's PTY is producing output (see
/// [`Tab::refresh_title`]). `pgid`/`command` describe the foreground job;
/// `cwd` is the *shell's* directory, the fallback behind `OSC 7`, and
/// `cwd_baseline` tells a live OSC 7 report from a finished job's leftover.
/// Kept here so label rendering stays syscall-free.
#[derive(Default)]
pub(crate) struct Foreground {
    pgid: Option<i32>,
    command: Option<String>,
    cwd: Option<String>,
    /// `Terminal::cwd_version` when the shell last owned the terminal;
    /// OSC 7 reports older than this are leftovers from a finished job.
    cwd_baseline: u64,
    /// `Terminal::title_version` when the shell last owned the terminal;
    /// a newer title was set by the running job (see [`title_is_live`]).
    title_baseline: u64,
}

/// Result of [`Tab::refresh_title`]: which cached facts moved.
pub(crate) struct TitleRefresh {
    /// The shell's working directory changed (session-snapshot input).
    pub cwd_changed: bool,
    /// Running/idle state changed (tray badge repaint input).
    pub chrome_changed: bool,
}

/// Cached view-space link runs for the hint underlines. Rebuilt only when
/// the grid view or the URL-detection toggle changed, so holding the hint
/// modifier does not re-scan the screen every frame.
pub(crate) struct LinkHintCache {
    version: u64,
    detect_urls: bool,
    view: crate::links::HintView,
}

impl Default for LinkHintCache {
    fn default() -> Self {
        Self {
            version: u64::MAX,
            detect_urls: true,
            view: crate::links::HintView::default(),
        }
    }
}

/// A single terminal tab.
pub struct Tab {
    /// Monotonic session identity; indices change on close and reorder.
    pub(crate) id: u64,
    pub(crate) terminal: Terminal,
    pub(crate) pty: PtySession,
    /// Parked in the background tray: hidden from the main tab strip,
    /// reachable through the tray popover and in-place peek.
    pub(crate) background: bool,
    /// A foreground job (not the shell prompt) owns the terminal, tracked
    /// by [`Tab::refresh_title`] from the kernel. Never polled at idle.
    pub(crate) running: bool,
    /// The foreground job finished while this tab was parked and unseen:
    /// the tray badge keeps an accent dot until the user peeks/unparks.
    pub(crate) bg_attention: bool,
    pub(crate) selection: Option<Selection>,
    pub(crate) selecting: bool,
    /// Granularity of the in-progress drag (char / word / line).
    pub(crate) select_mode: crate::selection::SelectMode,
    /// Raw press point (global) anchoring a word/line drag.
    pub(crate) select_anchor: Option<crate::selection::CellPos>,
    pub(crate) scrollbar: ScrollbarUi,
    pub(crate) is_alt: bool,
    pub(crate) foreground: Foreground,
    /// Per-tab find bar (query + matches + current).
    pub(crate) search: SearchPanel,
    /// Per-tab link hint cache (view only).
    pub(crate) link_hints: LinkHintCache,
    pub(crate) sync: super::scheduling::SyncPresentation,
    pub(crate) visual_dirty: bool,
    pub(crate) last_key_write: Option<Instant>,
    pub(crate) last_output: Option<Instant>,
    pub(crate) last_service: Option<Instant>,
    pub(crate) input_notice: Option<String>,
    pub(crate) pending_reply: Option<Vec<u8>>,
    /// Directory the shell was spawned in when it overrode `[shell] cwd`
    /// (a restored tab): the snapshot fallback until the first kernel query.
    pub(crate) spawn_cwd: Option<String>,
    /// Something in this tab wanted attention while it was unseen (bell,
    /// notification, OSC 7501 blocked/done/error): the start of its glow
    /// pulse. Cleared when the user sees the tab; see `App::tab_seen`.
    pub(crate) attention: Option<Instant>,
}

impl Tab {
    /// Global indices move when history is evicted. Keep a drag on the same
    /// surviving rows; if either endpoint disappeared, cancel rather than
    /// selecting unrelated recycled storage.
    pub(crate) fn rebase_selection(&mut self, dropped: usize) {
        if dropped == 0 {
            return;
        }
        let valid = self.selection.as_mut().is_none_or(|selection| {
            if let (Some(anchor), Some(active)) = (
                selection.anchor.y.checked_sub(dropped),
                selection.active.y.checked_sub(dropped),
            ) {
                selection.anchor.y = anchor;
                selection.active.y = active;
                true
            } else {
                false
            }
        });
        let anchor_valid = self.select_anchor.as_mut().is_none_or(|anchor| {
            if let Some(y) = anchor.y.checked_sub(dropped) {
                anchor.y = y;
                true
            } else {
                false
            }
        });
        if !valid || !anchor_valid {
            self.selection = None;
            self.select_anchor = None;
            self.selecting = false;
            self.select_mode = crate::selection::SelectMode::Char;
        }
    }

    pub(crate) fn new(id: u64, terminal: Terminal, pty: PtySession) -> Self {
        Self {
            id,
            terminal,
            pty,
            background: false,
            running: false,
            bg_attention: false,
            selection: None,
            selecting: false,
            select_mode: crate::selection::SelectMode::Char,
            select_anchor: None,
            scrollbar: ScrollbarUi::new(Instant::now()),
            is_alt: false,
            foreground: Foreground::default(),
            search: SearchPanel::default(),
            link_hints: LinkHintCache::default(),
            sync: super::scheduling::SyncPresentation::default(),
            visual_dirty: true,
            last_key_write: None,
            last_output: None,
            last_service: None,
            input_notice: None,
            pending_reply: None,
            spawn_cwd: None,
            attention: None,
        }
    }

    /// View-space link runs, rebuilt when the grid view changed.
    pub(crate) fn link_hints(&mut self, detect_urls: bool) -> &crate::links::HintView {
        let version = self.terminal.grid().version;
        if self.link_hints.version != version || self.link_hints.detect_urls != detect_urls {
            self.link_hints.version = version;
            self.link_hints.detect_urls = detect_urls;
            self.link_hints.view = crate::links::HintView::build(self.terminal.grid(), detect_urls);
        }
        &self.link_hints.view
    }

    /// Re-query the foreground process group behind `$command` / `$cwd`.
    ///
    /// Called after this tab's PTY produced output: a job taking or
    /// releasing the terminal and a shell `cd` both echo through the tty,
    /// so the foreground pid and the shell's cwd are re-read exactly when
    /// they can change. `command` is only resolved when the pid changed;
    /// `cwd` is re-read every time (same pid, different directory) but
    /// always from the *shell* process, never the foreground job: `ls -R`
    /// and `find` walk with `fts(3)`, which `chdir`s through the tree and
    /// would otherwise flicker the label. Failed queries keep the last
    /// known value: a job that exited between `tcgetpgrp` and the lookup
    /// must not blank the label for a frame. While the shell owns the
    /// terminal the OSC 7 baseline is refreshed, so only reports emitted
    /// by a job itself count as live (see [`osc7_is_live`]).
    ///
    /// Returns whether the shell's cwd changed (a session-snapshot input)
    /// and whether job state changed (a tray-badge repaint input).
    pub(crate) fn refresh_title(&mut self, displayed: bool) -> TitleRefresh {
        // Latency probe: this runs on the UI thread between echo and
        // paint, so a slow query delays every keystroke's frame.
        let start = Instant::now();
        let shell_pid = self.pty.shell_pid();
        let pgid = self.pty.foreground_pgid();
        if pgid != self.foreground.pgid {
            self.foreground.pgid = pgid;
            if let Some(command) = pgid.and_then(procinfo::command) {
                self.foreground.command = Some(command);
            }
        }
        // A shell prompt is idle; any other foreground process group is a
        // running job. This transition marks a parked, unseen tab as
        // "finished" without any timer of its own.
        let running = job_is_running(pgid, shell_pid);
        let mut chrome_changed = false;
        if running != self.running {
            self.running = running;
            chrome_changed = true;
            if !running && self.background && !displayed {
                self.bg_attention = true;
            }
            // OSC 7501: a finished job's working/blocked/idle records are
            // stale; its done/error results stay until the tab is seen.
            if !running {
                self.terminal.end_job();
            }
        }
        let mut cwd_changed = false;
        if let Some(cwd) = shell_pid.or(pgid).and_then(procinfo::cwd)
            && self.foreground.cwd.as_ref() != Some(&cwd)
        {
            self.foreground.cwd = Some(cwd);
            cwd_changed = true;
        }
        // Every OSC 7 seen while the shell owns the terminal is accounted
        // for; a later report (from the next job) is the only one allowed
        // to move `$cwd` away from the shell's own directory.
        if shell_pid.is_some_and(|pid| pgid == Some(pid)) {
            self.foreground.cwd_baseline = self.terminal.cwd_version();
            // Likewise titles: the prompt's own title never counts as a
            // job's, nor does one a finished job left behind.
            self.foreground.title_baseline = self.terminal.title_version();
        }
        let elapsed = start.elapsed();
        crate::perf::duration(crate::perf::Metric::TitleRefresh, elapsed);
        if elapsed >= Duration::from_millis(2) {
            log::debug!("slow refresh_title {elapsed:?}");
        }
        TitleRefresh {
            cwd_changed,
            chrome_changed,
        }
    }

    /// The shell's last known working directory, for session snapshots.
    pub(crate) fn shell_cwd(&self) -> Option<&str> {
        self.foreground.cwd.as_deref().or(self.spawn_cwd.as_deref())
    }
}

/// Tab-bar height in logical points for `tab_count` tabs: hidden when
/// there is a single tab (or none) — except on macOS, where the strip
/// lives inside the OS titlebar (transparent titlebar + fullsize content
/// view, Brave-style) and is always shown so terminal content never
/// slides under the traffic lights. `tab_count` is the total including
/// parked tabs: one foreground tab plus one parked tab keeps the strip
/// (and its tray badge) visible.
pub fn bar_height_points(tab_count: usize, config: &TabbarConfig) -> f32 {
    if tab_count <= 1 && !cfg!(target_os = "macos") {
        0.0
    } else {
        config.height
    }
}

/// Tab-bar height in physical pixels for a given scale factor and tab count.
pub fn tab_bar_px(scale: f32, tab_count: usize, config: &TabbarConfig) -> f32 {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    bar_height_points(tab_count, config) * scale
}

/// Terminal-area height: window height minus the tab bar, at least 1px.
/// The bar is hidden for a single tab, so the grid gets the full height.
pub fn term_height_px(window_h: u32, scale: f32, tab_count: usize, config: &TabbarConfig) -> u32 {
    (window_h as f32 - tab_bar_px(scale, tab_count, config)).max(1.0) as u32
}

/// Label inputs for one tab, resolved from the terminal and the kernel.
pub struct TitleVars<'a> {
    /// Shell-reported `OSC 0/1/2` title (empty when the shell sets none).
    pub title: &'a str,
    /// Foreground process basename, when the kernel could report one.
    pub command: Option<&'a str>,
    /// Working directory: shell-reported `OSC 7` or the shell process's
    /// kernel cwd (see [`preferred_cwd`]), `~`-shortened.
    pub cwd: Option<&'a str>,
    /// 0-based tab index; the `$tab` variable is 1-based.
    pub tab: usize,
}

/// Render `[tabbar] title_format` for one tab.
///
/// Literal text (separators, brackets, spacing) is preserved verbatim;
/// `$title` / `$command` / `$cwd` / `$tab` are interpolated and missing
/// sources expand to nothing. When the template references variables but
/// none has a value (a fresh tab before its shell reports anything, or a
/// platform without process introspection), the label falls back to
/// `Tab N`; `max_label_chars` truncation is applied last.
pub fn format_title(vars: &TitleVars, config: &TabbarConfig) -> String {
    let tab = (vars.tab + 1).to_string();
    let rendered = interpolate(
        &config.title_format,
        &[
            ("title", vars.title),
            ("command", vars.command.unwrap_or("")),
            ("cwd", vars.cwd.unwrap_or("")),
            ("tab", tab.as_str()),
        ],
    );
    let text = rendered.text.trim();
    if text.is_empty() || (rendered.referenced > 0 && rendered.filled == 0) {
        return format!("Tab {}", vars.tab + 1);
    }
    truncate_label(text, config.max_label_chars)
}

/// [`interpolate`] result: rendered text plus how many known variables the
/// template referenced and how many of those had a non-empty value.
struct Rendered {
    text: String,
    referenced: usize,
    filled: usize,
}

/// Substitute `$name` and `${name}` tokens from `vars`.
///
/// `${name}` delimits a variable when more text follows (`${tab}st`);
/// `$$` is a literal `$`. Unknown names and lone `$`s stay literal, so a
/// typo shows up in the label instead of vanishing. Substitution is a
/// single pass: a `$` inside a value is never re-scanned.
fn interpolate(template: &str, vars: &[(&str, &str)]) -> Rendered {
    let lookup = |name: &str| {
        vars.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| *value)
    };
    let chars: Vec<char> = template.chars().collect();
    let mut out = Rendered {
        text: String::with_capacity(template.len()),
        referenced: 0,
        filled: 0,
    };
    let push_var = |out: &mut Rendered, value: &str| {
        out.referenced += 1;
        if !value.is_empty() {
            out.filled += 1;
        }
        out.text.push_str(value);
    };
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            out.text.push(chars[i]);
            i += 1;
            continue;
        }
        match chars.get(i + 1) {
            Some('$') => {
                out.text.push('$');
                i += 2;
            }
            Some('{') => match chars[i + 2..].iter().position(|&c| c == '}') {
                Some(offset) => {
                    let name: String = chars[i + 2..i + 2 + offset].iter().collect();
                    match lookup(&name) {
                        Some(value) => push_var(&mut out, value),
                        None => out.text.extend(&chars[i..=i + 2 + offset]),
                    }
                    i += offset + 3;
                }
                // Unterminated `${`: keep the `$` literal, scan on.
                None => {
                    out.text.push('$');
                    i += 1;
                }
            },
            Some(_) => {
                let start = i + 1;
                let mut end = start;
                while let Some(&c) = chars.get(end) {
                    if c == '_' || c.is_ascii_alphanumeric() {
                        end += 1;
                    } else {
                        break;
                    }
                }
                let ident: String = chars[start..end].iter().collect();
                let ident_starts = ident.starts_with(|c: char| c == '_' || c.is_ascii_alphabetic());
                if ident_starts {
                    match lookup(&ident) {
                        Some(value) => push_var(&mut out, value),
                        None => out.text.extend(&chars[i..end]),
                    }
                    i = end;
                } else {
                    // `$5`, `$-`, … stay literal.
                    out.text.push('$');
                    i += 1;
                }
            }
            None => {
                out.text.push('$');
                i += 1;
            }
        }
    }
    out
}

/// Ellipsis-truncate to `max_chars` characters (at least one).
fn truncate_label(label: &str, max_chars: usize) -> String {
    let max_chars = max_chars.max(1);
    if label.chars().count() <= max_chars {
        return label.to_string();
    }
    let kept: String = label.chars().take(max_chars - 1).collect();
    format!("{kept}…")
}

/// Source precedence for the `$cwd` label variable.
///
/// While a job owns the terminal, the shell's `OSC 7` wins: an integrated
/// remote (`ssh`) or nested shell reports its own directory, which the
/// local kernel query cannot see. The shell's kernel cwd is the fallback
/// and never the job's own cwd — `ls -R` / `find` walk with `fts(3)`,
/// which `chdir`s through the tree and would flicker the label.
///
/// While the shell itself owns the terminal its kernel cwd is
/// authoritative: a live query beats an `OSC 7` left behind by a job that
/// has since exited (a file manager, a remote shell) when the local shell
/// emits none.
pub fn preferred_cwd<'a>(
    osc7: Option<&'a str>,
    kernel: Option<&'a str>,
    shell_foreground: bool,
) -> Option<&'a str> {
    if shell_foreground {
        kernel.or(osc7)
    } else {
        osc7.or(kernel)
    }
}

/// Whether the terminal is owned by a foreground job rather than the shell
/// prompt. An unknown shell pid still counts a known foreground group as
/// running; no foreground group at all is the prompt.
pub(crate) fn job_is_running(pgid: Option<i32>, shell_pid: Option<i32>) -> bool {
    match (pgid, shell_pid) {
        (Some(pgid), Some(shell)) => pgid != shell,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// Whether an `OSC 7` report is live for the current foreground job.
///
/// While a job owns the terminal only a report emitted since the shell
/// last had control counts — an integrated `ssh`/nested shell or a file
/// manager reporting as it runs. A leftover from a finished job (a remote
/// path after `ssh` exits, a file manager's last directory) must not
/// resurface during the next job. At the prompt the shell's own report is
/// always current.
pub fn osc7_is_live(reported: u64, baseline: u64, shell_foreground: bool) -> bool {
    shell_foreground || reported != baseline
}

/// Whether the user is looking at tab `index`: it is the displayed tab (a
/// foreground tab or an in-place peek) and the window has focus. Parked
/// tabs, other strip tabs, and everything while the window is in the
/// background are unseen.
pub(crate) fn tab_is_seen(index: usize, active: usize, focused: bool) -> bool {
    index == active && focused
}

/// Whether the `OSC 0/2` title was set by the job that owns the terminal,
/// so it may replace `[tabbar] title_format` (`live_title`). Never at the
/// prompt (`at_prompt`): shells that title every prompt keep the template,
/// and a title a finished job left behind (`version == baseline`) does not
/// resurface.
pub fn title_is_live(version: u64, baseline: u64, at_prompt: bool, title: &str) -> bool {
    !at_prompt && version != baseline && !title.is_empty()
}

/// Map an old `active` tab index across removal of `removed` indices.
///
/// Counts how many removed tabs sat before `active` and shifts it down
/// accordingly, clamping to the surviving range. When `active` itself
/// was removed this lands on the tab that slid into its slot (or the
/// new last tab when the old last tab went away).
pub(crate) fn active_after_removals(active: usize, removed: &[usize], new_len: usize) -> usize {
    if new_len == 0 {
        return 0;
    }
    let shift = removed.iter().filter(|&&i| i < active).count();
    active.saturating_sub(shift).min(new_len.saturating_sub(1))
}

/// Next active index when cycling by `delta` with wrap-around
/// (`+1` next, `-1` prev). Returns `active` when there is nothing to
/// cycle (`len < 2`).
pub(crate) fn next_tab_index(active: usize, len: usize, delta: isize) -> usize {
    if len < 2 {
        return active;
    }
    (active as isize + delta).rem_euclid(len as isize) as usize
}

/// OS window title for the active tab: the shell's OSC title when set,
/// otherwise the app name from config.
pub fn window_title_for<'a>(osc_title: &'a str, config: &'a Config) -> &'a str {
    if osc_title.is_empty() {
        &config.window.title
    } else {
        osc_title
    }
}

impl App {
    pub(crate) fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub(crate) fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    /// Labels for the foreground tabs in order, for the tab-bar strip.
    pub(crate) fn tab_labels(&self) -> Vec<crate::tabbar::TabLabel> {
        self.tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.background)
            .map(|(i, t)| crate::tabbar::TabLabel {
                id: t.id,
                title: self.title_for_tab(i, t),
                status: t.terminal.status().summary(),
                glow: t.attention,
            })
            .collect()
    }

    /// Labels for the parked tabs, for the tray popover.
    pub(crate) fn background_labels(&self) -> Vec<crate::tabbar::BackgroundTabLabel> {
        self.tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| t.background)
            .map(|(i, t)| crate::tabbar::BackgroundTabLabel {
                id: t.id,
                title: self.title_for_tab(i, t),
                running: t.running,
                attention: t.bg_attention,
                status: t.terminal.status().summary(),
                glow: t.attention,
            })
            .collect()
    }

    /// The user sees tab `index` (activated, peeked, unparked, or the
    /// window regained focus over it): its glow stops and its `OSC 7501`
    /// `done`/`error` results count as read. Returns whether chrome changed.
    pub(crate) fn tab_seen(&mut self, index: usize) -> bool {
        let Some(tab) = self.tabs.get_mut(index) else {
            return false;
        };
        let glowed = tab.attention.take().is_some();
        tab.terminal.acknowledge_status() || glowed
    }

    /// Route one tab's attention signal (bell, notification, OSC 7501
    /// transition) after a drain. A tab the user is looking at in a focused
    /// window has nothing to flag: its results are read on the spot. An
    /// unseen tab starts (or, after [`crate::tabbar::GLOW_REPULSE`],
    /// restarts) its glow. Returns whether chrome changed.
    pub(crate) fn tab_wants_attention(&mut self, index: usize, now: Instant) -> bool {
        if tab_is_seen(index, self.active, self.focused) {
            return self.tab_seen(index);
        }
        let mode = crate::tabbar::AttentionMode::from_config(&self.config.tabbar.attention);
        let Some(tab) = self.tabs.get_mut(index) else {
            return false;
        };
        if mode == crate::tabbar::AttentionMode::Off {
            return false;
        }
        match tab.attention {
            Some(started)
                if now.saturating_duration_since(started) < crate::tabbar::GLOW_REPULSE =>
            {
                false
            }
            _ => {
                tab.attention = Some(now);
                true
            }
        }
    }

    /// Foreground tabs occupy a prefix of `App::tabs`, so strip indices,
    /// `Cmd+1..9`, and `Ctrl+Tab` need no translation.
    pub(crate) fn foreground_count(&self) -> usize {
        self.tabs.iter().take_while(|tab| !tab.background).count()
    }

    pub(crate) fn background_count(&self) -> usize {
        self.tabs.len() - self.foreground_count()
    }

    pub(crate) fn is_peeking(&self) -> bool {
        self.active_tab().is_some_and(|tab| tab.background)
    }

    /// Foreground index to return to when leaving a peek: the last viewed
    /// foreground tab, else the first one.
    pub(crate) fn focus_foreground_index(&self) -> usize {
        let fg = self.foreground_count();
        if fg == 0 {
            return 0;
        }
        self.last_foreground_tab
            .and_then(|id| self.tab_index(id))
            .filter(|&index| index < fg)
            .unwrap_or(0)
    }

    pub(crate) fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    /// Commit one drop, moving the entire tab (including its transport and
    /// caches). A positional change is not a content switch or a grid resize.
    /// Only foreground tabs participate in the strip.
    pub(crate) fn reorder_tab(&mut self, movement: crate::tabbar::TabMove) -> bool {
        let foreground = self.foreground_count();
        let Some(from) = self.tab_index(movement.id).filter(|&i| i < foreground) else {
            return false;
        };
        let to = movement.to;
        if to >= foreground || from == to {
            return false;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = crate::tabbar::index_after_move(self.active, from, to);
        self.pty_schedule.moved(from, to);
        self.session_changed();
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
        true
    }

    /// Only foreground tab bodies can start a reorder, and only when the
    /// strip has a second foreground tab. Parked tabs do not make a lone
    /// foreground tab draggable: cancelling that source would swallow its click.
    pub(crate) fn can_drag_tab(&self, id: u64) -> bool {
        let foreground = self.foreground_count();
        foreground > 1 && self.tab_index(id).is_some_and(|i| i < foreground)
    }

    pub(crate) fn reconcile_tab_drag(&mut self) {
        let valid = self
            .tab_drag
            .source()
            .is_some_and(|id| self.can_drag_tab(id));
        self.tab_drag.retain_source(valid);
    }

    pub(crate) fn title_for_tab(&self, i: usize, t: &Tab) -> String {
        // Shell-reported OSC 7 (only while it is live for the
        // current job) or the shell process's kernel cwd, picked by
        // who owns the terminal (see `preferred_cwd`); shortened at
        // the last moment so the cache keeps raw paths.
        let shell_foreground = t
            .pty
            .shell_pid()
            .is_some_and(|pid| t.foreground.pgid == Some(pid));
        let osc7_live = osc7_is_live(
            t.terminal.cwd_version(),
            t.foreground.cwd_baseline,
            shell_foreground,
        );
        let cwd = preferred_cwd(
            t.terminal.cwd().filter(|_| osc7_live),
            t.foreground.cwd.as_deref(),
            shell_foreground,
        )
        .map(procinfo::shorten_home);
        // `running` (not `!shell_foreground`): with no known foreground
        // group nothing is a job, so a title can never stick.
        if self.config.tabbar.live_title
            && title_is_live(
                t.terminal.title_version(),
                t.foreground.title_baseline,
                !t.running,
                t.terminal.title().trim(),
            )
        {
            return truncate_label(
                t.terminal.title().trim(),
                self.config.tabbar.max_label_chars,
            );
        }
        format_title(
            &TitleVars {
                title: t.terminal.title(),
                command: t.foreground.command.as_deref(),
                cwd: cwd.as_deref(),
                tab: i,
            },
            &self.config.tabbar,
        )
    }

    /// Push the active tab's OSC title to the OS window chrome.
    /// No-op when unchanged or when the window isn't ready yet.
    pub(crate) fn sync_window_title(&mut self) {
        let desired = self
            .active_tab()
            .map(|t| window_title_for(t.terminal.title(), &self.config).to_string())
            .unwrap_or_else(|| self.config.window.title.clone());
        if desired == self.window_title {
            return;
        }
        self.window_title = desired.clone();
        if let Some(w) = self.window.as_ref() {
            w.set_title(&desired);
        }
    }

    fn waker(&self) -> impl Fn() + Send + Sync + 'static {
        let proxy = self.proxy.clone();
        move || {
            if let Some(p) = proxy.as_ref() {
                let _ = p.send_event(UserEvent::PtyAvailable);
            }
        }
    }

    /// Resize every tab to fit the current window and tab count.
    /// Required when the bar appears/disappears (1 <-> 2 tabs): the grid
    /// gains or loses tab-bar height of vertical space.
    pub(crate) fn sync_tab_sizes(&mut self) {
        let (w, h, scale, cell_w, line_h) = match (self.window.as_ref(), self.renderer.as_ref()) {
            (Some(w), Some(r)) => {
                let s = w.inner_size();
                (
                    s.width.max(1),
                    s.height.max(1),
                    r.scale_factor(),
                    r.cell_width,
                    r.line_height,
                )
            }
            _ => return,
        };
        let term_h = term_height_px(h, scale, self.tabs.len(), &self.config.tabbar);
        let (cols, rows) =
            super::compute_grid_size(w, term_h, cell_w, line_h, &self.config.terminal);
        let mut changed = false;
        for tab in &mut self.tabs {
            if cols != tab.terminal.cols() || rows != tab.terminal.rows() {
                tab.terminal.resize(cols, rows);
                tab.pty.resize(cols, rows);
                tab.selection = None;
                tab.selecting = false;
                tab.select_mode = crate::selection::SelectMode::Char;
                tab.select_anchor = None;
                changed = true;
            }
        }
        if changed {
            if let Some(r) = self.renderer.as_mut() {
                r.invalidate();
            }
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }

    /// Spawn a tab at `cols`x`rows` and switch to it; `cwd` (a restored
    /// tab's directory) overrides `[shell] cwd`. Returns false when the PTY
    /// failed to spawn (error already logged).
    pub(crate) fn spawn_tab(&mut self, cols: usize, rows: usize, cwd: Option<&str>) -> bool {
        let terminal = Terminal::new_with_config(cols, rows, self.theme, &self.config);
        let pty = match PtySession::spawn_with_size(cols, rows, self.waker(), &self.config, cwd) {
            Ok(p) => p,
            Err(e) => {
                log::error!("failed to spawn pty: {e:#}");
                return false;
            }
        };
        // The label stays `Tab N` until the first prompt output triggers
        // `refresh_title`: a spawn-time query would race the child's exec
        // and may report cometty's own image as `$command`.
        let id = self.next_tab_id;
        self.next_tab_id = self.next_tab_id.wrapping_add(1);
        if let Some(old) = self.active_tab_mut() {
            old.last_output = None;
            old.last_key_write = None;
        }
        let mut tab = Tab::new(id, terminal, pty);
        tab.spawn_cwd = cwd.map(str::to_string);
        self.cancel_tab_drag();
        self.bg_popover_open = false;
        // New foreground tabs go at the end of the foreground prefix so
        // parked tabs stay contiguous behind it.
        let insert = self.foreground_count();
        self.tabs.insert(insert, tab);
        self.active = insert;
        self.last_foreground_tab = Some(id);
        self.spawned_any = true;
        self.session_changed();
        log::debug!("spawned tab {} ({}x{})", self.tabs.len(), cols, rows);
        if let Some(r) = self.renderer.as_mut() {
            r.invalidate();
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        // Showing the bar for the 2nd tab steals vertical space from all
        // grids; keep every tab the same size.
        self.sync_tab_sizes();
        true
    }

    /// Spawn a tab sized for the current window, for `Ctrl+T` / `Cmd+T`.
    /// Returns false when the window/renderer is not ready or the PTY
    /// failed to spawn.
    pub(crate) fn spawn_tab_for_window(&mut self) -> bool {
        self.spawn_tab_for_window_at(None)
    }

    /// [`Self::spawn_tab_for_window`] with a cwd for the new shell (parking
    /// the only foreground tab leaves its replacement in the same place).
    fn spawn_tab_for_window_at(&mut self, cwd: Option<&str>) -> bool {
        let Some(renderer) = self.renderer.as_ref() else {
            return false;
        };
        let Some(window) = self.window.as_ref() else {
            return false;
        };
        let size = window.inner_size();
        let w = size.width.max(1);
        // The new tab makes `len + 1` tabs, which decides whether the bar
        // (and its height) is visible.
        let future = self.tabs.len() + 1;
        let h = term_height_px(
            size.height.max(1),
            renderer.scale_factor(),
            future,
            &self.config.tabbar,
        );
        let (cols, rows) = super::compute_grid_size(
            w,
            h,
            renderer.cell_width,
            renderer.line_height,
            &self.config.terminal,
        );
        self.spawn_tab(cols, rows, cwd)
    }

    /// Switch to tab `index`. No-op when out of range.
    ///
    /// A foreground index shows that tab and records it as the return point
    /// for a later peek. A background index is an in-place peek: the main
    /// strip keeps its foreground tabs without an inner active pill.
    pub(crate) fn switch_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if index == self.active {
            if self.bg_popover_open {
                self.bg_popover_open = false;
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            return;
        }
        if self
            .tab_drag
            .source()
            .is_some_and(|id| self.tabs[index].id != id)
        {
            self.cancel_tab_drag();
        }
        if let Some(old) = self.active_tab_mut() {
            old.last_output = None;
            old.last_key_write = None;
        }
        self.active = index;
        // The frame below repaints the strip either way.
        self.tab_seen(index);
        let id = self.tabs[index].id;
        if self.tabs[index].background {
            // Seeing the tab clears its "job finished" marker.
            self.tabs[index].bg_attention = false;
            self.last_background_tab = Some(id);
        } else {
            self.last_foreground_tab = Some(id);
        }
        self.bg_popover_open = false;
        self.session_changed();
        log::debug!("switched to tab {index}");
        // Two tabs can share a grid version while holding different
        // content: the new tab's cached matches must be recomputed.
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.search.invalidate();
        }
        if let Some(r) = self.renderer.as_mut() {
            r.invalidate();
        }
        self.sync_window_title();
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Cycle foreground tabs by `delta` with wrap-around (`+1` next, `-1`
    /// prev). A peek exits to the last foreground tab first, so the main
    /// strip keeps its usual cycle.
    pub(crate) fn switch_relative(&mut self, delta: isize) {
        let foreground = self.foreground_count();
        let current = if self.is_peeking() {
            self.focus_foreground_index()
        } else {
            self.active
        };
        if foreground >= 2 {
            let next = next_tab_index(current, foreground, delta);
            self.switch_tab(next);
        } else if self.is_peeking() {
            self.switch_tab(current);
        }
    }

    /// Park the active foreground tab in the background tray, or unpark it
    /// when the view is a peek.
    pub(crate) fn toggle_background_tab(&mut self) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let id = tab.id;
        if tab.background {
            self.unpark_tab(id);
        } else {
            self.park_tab_for_window(id);
        }
    }

    /// Park the requested foreground tab, shared by the strip button and
    /// shortcut. Parking the last one first needs a replacement prompt.
    pub(crate) fn park_tab_for_window(&mut self, id: u64) {
        let Some(index) = self.tab_index(id) else {
            return;
        };
        let tab = &self.tabs[index];
        if tab.background {
            return;
        }
        if self.foreground_count() > 1 {
            self.park_tab(id);
            return;
        }
        // Parking the only main tab would leave no foreground tab. Spawn a
        // fresh prompt in the same directory first, then park the old tab.
        let cwd = tab.shell_cwd().map(str::to_string);
        let displayed = self
            .active_tab()
            .map(|tab| tab.id)
            .filter(|&active| active != id);
        if self.spawn_tab_for_window_at(cwd.as_deref()) {
            // A button on the lone foreground tab may be clicked during a
            // background peek. Keep displaying that other terminal.
            if let Some(index) = displayed.and_then(|id| self.tab_index(id)) {
                self.switch_tab(index);
            }
            self.park_tab(id);
        }
    }

    /// Toggle an in-place peek at the most recent background tab, returning
    /// to the last foreground tab when already peeking.
    pub(crate) fn toggle_peek_background(&mut self) {
        if self.is_peeking() {
            self.switch_tab(self.focus_foreground_index());
            return;
        }
        if let Some(index) = self.pick_background_index() {
            self.switch_tab(index);
        }
    }

    fn pick_background_index(&self) -> Option<usize> {
        let foreground = self.foreground_count();
        if foreground >= self.tabs.len() {
            return None;
        }
        self.last_background_tab
            .and_then(|id| self.tab_index(id))
            .filter(|&index| self.tabs[index].background)
            .or_else(|| {
                (foreground..self.tabs.len())
                    .rev()
                    .find(|&i| self.tabs[i].background)
            })
    }

    /// Show a parked tab in place, without moving it back to the strip.
    pub(crate) fn peek_background_tab(&mut self, id: u64) {
        if let Some(index) = self
            .tab_index(id)
            .filter(|&index| self.tabs[index].background)
        {
            self.switch_tab(index);
        }
    }

    /// Move one foreground tab behind the strip. No-op for a background tab
    /// or the last foreground tab (callers spawn a replacement first).
    pub(crate) fn park_tab(&mut self, id: u64) {
        let Some(from) = self.tab_index(id) else {
            return;
        };
        if self.tabs[from].background || self.foreground_count() <= 1 {
            return;
        }
        let target = if from == self.active {
            let fallback = self.focus_foreground_index();
            if fallback != from {
                fallback
            } else if from > 0 {
                // `last_foreground_tab` is normally the active tab itself;
                // park to its previous neighbour, or the next for tab 0.
                from - 1
            } else {
                1
            }
        } else {
            self.active
        };
        let mut tab = self.tabs.remove(from);
        tab.background = true;
        tab.bg_attention = false;
        self.tabs.push(tab);
        self.active = if target > from { target - 1 } else { target };
        self.pty_schedule.moved(from, self.tabs.len() - 1);
        self.last_background_tab = Some(id);
        if let Some(tab) = self.tabs.get(self.active)
            && !tab.background
        {
            self.last_foreground_tab = Some(tab.id);
        }
        self.reconcile_tab_drag();
        self.session_changed();
        log::debug!("parked tab {id} ({} background)", self.background_count());
        if let Some(r) = self.renderer.as_mut() {
            r.invalidate();
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        self.sync_tab_sizes();
    }

    /// Bring a parked tab back to the end of the main strip and show it.
    pub(crate) fn unpark_tab(&mut self, id: u64) {
        let Some(from) = self.tab_index(id) else {
            return;
        };
        if !self.tabs[from].background {
            return;
        }
        let mut tab = self.tabs.remove(from);
        tab.background = false;
        tab.bg_attention = false;
        let insert = self.foreground_count();
        self.tabs.insert(insert, tab);
        self.active = insert;
        self.tab_seen(insert);
        self.last_foreground_tab = Some(id);
        self.bg_popover_open = false;
        self.pty_schedule.moved(from, insert);
        if self.background_count() == 0 {
            self.last_background_tab = None;
        }
        self.session_changed();
        log::debug!("unparked tab {id}");
        if let Some(r) = self.renderer.as_mut() {
            r.invalidate();
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        self.sync_tab_sizes();
    }

    /// Close tab `index`. Returns true when no tabs remain and the
    /// caller should exit the event loop.
    pub(crate) fn close_tab(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() {
            return self.tabs.is_empty();
        }
        log::debug!("closed tab {index} ({} left)", self.tabs.len() - 1);
        let active_id = (index != self.active).then(|| self.tabs[self.active].id);
        self.tabs.remove(index);
        self.pty_schedule.removed(&[index], self.tabs.len());
        self.reconcile_tab_drag();
        self.session_changed();
        if self.tabs.is_empty() {
            return true;
        }
        self.reconcile_removed_tabs(active_id, index);
        if let Some(r) = self.renderer.as_mut() {
            r.invalidate();
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        // Closing down to 1 tab hides the bar and hands the space back.
        self.sync_tab_sizes();
        false
    }

    /// Restore active/focus bookkeeping after removals. Keeps the surviving
    /// displayed tab by identity (a peek stays a peek), leaves background
    /// tabs out of the main strip, and promotes a parked tab when the last
    /// foreground tab disappeared so the window never becomes all-background.
    pub(crate) fn reconcile_removed_tabs(&mut self, active_before: Option<u64>, slot: usize) {
        if self.tabs.is_empty() {
            return;
        }
        self.active = match active_before.and_then(|id| self.tab_index(id)) {
            Some(index) => index,
            None => {
                let index = slot.min(self.tabs.len() - 1);
                if self.tabs[index].background {
                    self.focus_foreground_index()
                } else {
                    index
                }
            }
        };
        if self.foreground_count() == 0 {
            let promote = self
                .tabs
                .get(self.active)
                .filter(|tab| tab.background)
                .map(|_| self.active)
                .unwrap_or(self.tabs.len() - 1);
            let mut tab = self.tabs.remove(promote);
            tab.background = false;
            tab.bg_attention = false;
            self.tabs.insert(0, tab);
            self.pty_schedule.moved(promote, 0);
            self.active = 0;
        }
        // Whatever is displayed now is seen if the window has focus; the
        // callers repaint after reconciling.
        if self.focused {
            self.tab_seen(self.active);
        }
        let (id, background) = self
            .tabs
            .get(self.active)
            .map(|tab| (tab.id, tab.background))
            .unwrap_or((0, false));
        if background {
            self.last_background_tab = Some(id);
        } else {
            self.last_foreground_tab = Some(id);
        }
        if self
            .last_foreground_tab
            .and_then(|id| self.tab_index(id))
            .is_none_or(|index| self.tabs[index].background)
        {
            self.last_foreground_tab = self
                .tabs
                .iter()
                .find(|tab| !tab.background)
                .map(|tab| tab.id);
        }
        if self
            .last_background_tab
            .and_then(|id| self.tab_index(id))
            .is_none_or(|index| !self.tabs[index].background)
        {
            self.last_background_tab = self
                .tabs
                .iter()
                .rev()
                .find(|tab| tab.background)
                .map(|tab| tab.id);
        }
        if self.background_count() == 0 {
            self.bg_popover_open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TabbarConfig};
    use crate::selection::{CellPos, Selection};
    use crate::term::Terminal;

    fn cfg() -> TabbarConfig {
        TabbarConfig::default()
    }

    fn app_with_tabs(count: usize) -> (App, Vec<crate::pty::input::InputReceiver>) {
        let mut config = Config::default();
        config.tabbar.title_format = "$tab".into();
        let mut app = App::new(
            None,
            crate::theme::Theme::default(),
            config,
            crate::app::AppStartup {
                config_path_override: None,
                cli_theme: None,
                config_corrupt: false,
            },
            std::sync::Arc::new(std::sync::Mutex::new(crate::logbuf::LogBuffer::new(10))),
        );
        let mut inputs = Vec::new();
        for id in 0..count {
            let (pty, _, input) = crate::pty::test_support::session();
            let mut terminal = Terminal::new(20, 2, crate::theme::Theme::default());
            terminal.feed(format!("tab {id}\r\nhistory {id}\r\nscreen {id}").as_bytes());
            let mut tab = Tab::new(id as u64, terminal, pty);
            tab.selection = Some(Selection::new(CellPos { x: 1, y: 1 }));
            tab.search.focus_or_open();
            tab.search.query = id.to_string();
            tab.search.refresh(tab.terminal.grid());
            app.tabs.push(tab);
            inputs.push(input);
        }
        app.next_tab_id = count as u64;
        (app, inputs)
    }

    #[test]
    fn reorder_moves_the_whole_tab_without_switching_content_or_invalidating_views() {
        let (mut app, inputs) = app_with_tabs(5);
        app.active = 2;
        let active_id = app.tabs[2].id;
        app.snapshot_tab = Some(active_id);
        app.snapshot_version = app.tabs[2].terminal.grid().version;
        let snapshot_version = app.snapshot_version;
        let versions: Vec<_> = app
            .tabs
            .iter()
            .map(|tab| tab.terminal.grid().version)
            .collect();
        let views: Vec<_> = app
            .tabs
            .iter_mut()
            .map(|tab| tab.search.view(tab.terminal.grid()).unwrap())
            .collect();
        app.pty_schedule
            .add_bytes(super::super::scheduling::MAX_DRAIN_BYTES);

        for movement in [
            crate::tabbar::TabMove { id: 0, to: 4 },
            crate::tabbar::TabMove { id: 2, to: 0 },
            crate::tabbar::TabMove { id: 4, to: 1 },
            crate::tabbar::TabMove { id: 2, to: 4 },
        ] {
            assert!(app.reorder_tab(movement));
            assert_eq!(app.tabs[app.active].id, active_id);
            assert_eq!(app.snapshot_tab, Some(active_id));
            assert_eq!(app.snapshot_version, snapshot_version);
            assert!(
                app.pty_schedule.exhausted(Duration::ZERO),
                "a move cannot reset the shared turn budget"
            );
            for tab in &mut app.tabs {
                let id = tab.id as usize;
                assert_eq!(tab.terminal.grid().version, versions[id]);
                assert_eq!(tab.terminal.grid().scrollback_len(), 1);
                assert!(tab.selection.is_some());
                assert_eq!(tab.search.query, id.to_string());
                let view = tab.search.view(tab.terminal.grid()).unwrap();
                assert!(
                    std::sync::Arc::ptr_eq(&view, &views[id]),
                    "search cache travels with the tab"
                );
            }
        }
        for tab in &app.tabs {
            assert!(tab.pty.write(vec![tab.id as u8]));
        }
        for (id, input) in inputs.iter().enumerate() {
            assert_eq!(
                input.try_recv().unwrap(),
                [id as u8],
                "transport stays attached to identity"
            );
        }
        let labels = app.tab_labels();
        for (index, label) in labels.iter().enumerate() {
            assert_eq!(label.id, app.tabs[index].id);
            assert_eq!(
                label.title,
                (index + 1).to_string(),
                "$tab follows committed order"
            );
        }
    }

    #[test]
    fn reorder_ignores_noop_out_of_range_and_vanished_sources() {
        let (mut app, _) = app_with_tabs(3);
        app.active = 1;
        for movement in [
            crate::tabbar::TabMove { id: 1, to: 1 },
            crate::tabbar::TabMove { id: 1, to: 3 },
            crate::tabbar::TabMove { id: 99, to: 0 },
        ] {
            assert!(!app.reorder_tab(movement));
            assert_eq!(
                app.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
                [0, 1, 2]
            );
            assert_eq!(app.active, 1);
        }
        app.tabs.clear();
        assert!(!app.reorder_tab(crate::tabbar::TabMove { id: 1, to: 0 }));
    }

    #[test]
    fn parking_switches_to_a_neighbour_and_keeps_foreground_indices_dense() {
        let (mut app, _) = app_with_tabs(3);
        app.active = 1;
        app.park_tab(1);
        assert_eq!(
            app.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
            [0, 2, 1]
        );
        assert_eq!(app.tabs[app.active].id, 0);
        assert_eq!(app.foreground_count(), 2);
        assert_eq!(app.background_count(), 1);
        assert_eq!(
            app.tab_labels()
                .iter()
                .map(|label| label.id)
                .collect::<Vec<_>>(),
            [0, 2]
        );
        let background = app.background_labels();
        assert_eq!(background.len(), 1);
        assert_eq!(background[0].id, 1);
        assert_eq!(background[0].title, "3", "$tab keeps the saved position");

        // Ctrl+Tab cycles the two remaining foreground tabs only.
        app.switch_relative(1);
        assert_eq!(app.tabs[app.active].id, 2);
        app.switch_relative(1);
        assert_eq!(app.tabs[app.active].id, 0);
    }

    #[test]
    fn parking_button_targets_the_requested_tab_and_keeps_its_terminal_and_transport() {
        for active in [0, 1] {
            let (mut app, inputs) = app_with_tabs(3);
            app.active = active;
            let version = app.tabs[1].terminal.grid().version;
            let tab = &mut app.tabs[1];
            let search = tab.search.view(tab.terminal.grid()).unwrap();
            app.park_tab_for_window(1);
            assert_eq!(app.tabs[app.active].id, 0);
            assert_eq!(app.foreground_count(), 2);
            assert_eq!(app.tabs[2].id, 1);
            assert!(app.tabs[2].background);
            let tab = &mut app.tabs[2];
            assert_eq!(tab.terminal.grid().version, version);
            assert_eq!(tab.terminal.grid().scrollback_len(), 1);
            assert!(tab.selection.is_some());
            assert_eq!(tab.search.query, "1");
            assert!(std::sync::Arc::ptr_eq(
                &search,
                &tab.search.view(tab.terminal.grid()).unwrap()
            ));
            assert!(inputs.iter().all(|input| input.try_recv().is_err()));
            assert!(tab.pty.write(b"same transport".to_vec()));
            assert_eq!(inputs[1].try_recv().unwrap(), b"same transport");
            assert!(inputs[0].try_recv().is_err());
            assert!(inputs[2].try_recv().is_err());
        }
    }

    #[test]
    fn parking_button_during_a_peek_parks_its_target_without_unparking_the_preview() {
        let (mut app, _) = app_with_tabs(4);
        app.park_tab(3);
        app.peek_background_tab(3);
        app.park_tab_for_window(1);
        assert_eq!(app.tabs[app.active].id, 3);
        assert!(app.is_peeking());
        assert_eq!(app.foreground_count(), 2);
        assert_eq!(app.background_count(), 2);
        assert!(app.tabs[app.tab_index(1).unwrap()].background);
        assert!(app.tabs[app.tab_index(3).unwrap()].background);
        assert_eq!(app.focus_foreground_index(), 0);
        for id in [1, 3, 99] {
            app.park_tab_for_window(id);
            assert_eq!(app.tabs.len(), 4, "parked and vanished targets are no-ops");
            assert_eq!(app.tabs[app.active].id, 3);
        }
    }

    #[test]
    fn parking_the_lone_foreground_tab_keeps_it_if_a_replacement_cannot_spawn() {
        for peeking in [false, true] {
            let (mut app, _) = app_with_tabs(2);
            app.park_tab(1);
            if peeking {
                app.peek_background_tab(1);
            }
            let active = app.tabs[app.active].id;
            let version = app.tabs[0].terminal.grid().version;
            // Headless App has no window/renderer for a replacement spawn.
            app.park_tab_for_window(0);
            assert_eq!(app.tabs.len(), 2);
            assert_eq!(app.foreground_count(), 1);
            assert!(!app.tabs[0].background);
            assert_eq!(app.tabs[app.active].id, active);
            assert_eq!(app.tabs[0].terminal.grid().version, version);
            assert!(app.tabs[0].selection.is_some());
        }
    }

    #[test]
    fn peek_shows_a_parked_tab_without_moving_it_and_ctrl_tab_returns_to_the_strip() {
        let (mut app, _) = app_with_tabs(2);
        let parked = app.tabs[1].id;
        app.active = 0;
        app.park_tab(parked);
        assert!(!app.is_peeking());
        app.toggle_peek_background();
        assert!(app.is_peeking());
        assert_eq!(app.tabs[app.active].id, parked);
        assert_eq!(app.foreground_count(), 1);
        assert!(app.tab_labels().iter().all(|label| label.id != parked));

        // Ctrl+Tab exits the peek and cycles the foreground strip.
        app.switch_relative(1);
        assert!(!app.is_peeking());
        assert_eq!(app.tabs[app.active].id, 0);

        app.peek_background_tab(parked);
        assert!(app.is_peeking());
        app.unpark_tab(parked);
        assert!(!app.is_peeking());
        assert_eq!(app.tabs[app.active].id, parked);
        assert_eq!(app.foreground_count(), 2);
        assert!(app.tabs.iter().all(|tab| !tab.background));
    }

    #[test]
    fn closing_the_last_foreground_tab_promotes_a_parked_task_instead_of_quitting() {
        let (mut app, _) = app_with_tabs(2);
        let parked = app.tabs[1].id;
        app.active = 1;
        app.park_tab(parked);
        assert_eq!(app.foreground_count(), 1);
        // Closing the only visible tab must not kill the running task.
        assert!(!app.close_tab(0));
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].id, parked);
        assert!(!app.tabs[0].background);
        assert_eq!(app.active, 0);
        assert_eq!(app.foreground_count(), 1);
    }

    #[test]
    fn reorder_and_drag_only_consider_foreground_tabs() {
        let (mut app, _) = app_with_tabs(3);
        app.park_tab(2);
        assert!(app.can_drag_tab(0));
        assert!(app.can_drag_tab(1));
        assert!(!app.can_drag_tab(2));
        assert!(!app.can_drag_tab(99));
        assert!(!app.reorder_tab(crate::tabbar::TabMove { id: 2, to: 0 }));
        assert!(app.reorder_tab(crate::tabbar::TabMove { id: 0, to: 1 }));
        assert_eq!(
            app.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
            [1, 0, 2]
        );
        app.tab_drag
            .begin(Some((0, [100.0, 5.0, 200.0, 35.0])), [150.0, 20.0]);
        app.tab_drag.moved([350.0, 20.0]);
        app.reconcile_tab_drag();
        assert_eq!(app.tab_drag.source(), Some(0));
        app.park_tab(1);
        assert_eq!(app.foreground_count(), 1);
        assert!(
            !app.can_drag_tab(0),
            "parked tabs do not make the lone foreground draggable"
        );
        assert_eq!(app.tab_drag.source(), None);
        assert!(app.tab_drag.captures_pointer());
        assert!(app.tab_drag.release());
        assert!(
            app.tab_drag.suppress_clicks(),
            "an existing reorder still cancels safely"
        );
    }

    #[test]
    fn job_state_distinguishes_a_prompt_from_a_foreground_task() {
        assert!(!job_is_running(None, Some(10)));
        assert!(!job_is_running(Some(10), Some(10)));
        assert!(job_is_running(Some(11), Some(10)));
        // Unknown shell: a known foreground group still counts as a job.
        assert!(job_is_running(Some(11), None));
    }

    #[test]
    fn closing_a_neighbour_retains_the_drag_but_closing_its_source_cancels_it() {
        let (mut app, _) = app_with_tabs(4);
        app.tab_drag
            .begin(Some((2, [100.0, 5.0, 200.0, 35.0])), [150.0, 20.0]);
        app.tab_drag.moved([350.0, 20.0]);
        app.close_tab(0);
        assert_eq!(app.tab_drag.source(), Some(2));
        assert_eq!(app.tab_index(2), Some(1));
        app.close_tab(1);
        assert_eq!(app.tab_drag.source(), None);
        assert!(app.tab_drag.captures_pointer());
        assert!(app.tab_drag.release());
        assert!(app.tab_drag.suppress_clicks());
    }

    fn vars<'a>(
        title: &'a str,
        command: Option<&'a str>,
        cwd: Option<&'a str>,
        tab: usize,
    ) -> TitleVars<'a> {
        TitleVars {
            title,
            command,
            cwd,
            tab,
        }
    }

    #[test]
    fn default_format_uses_command_and_cwd() {
        let cfg = cfg();
        assert_eq!(
            format_title(&vars("", Some("zsh"), Some("~/dev"), 0), &cfg),
            "zsh | ~/dev"
        );
        assert_eq!(
            format_title(&vars("", Some("cargo"), Some("~/dev/cometty"), 1), &cfg),
            "cargo | ~/dev/cometty"
        );
    }

    #[test]
    fn cwd_prefers_osc7_while_a_job_is_foreground() {
        // An integrated `ssh` / nested shell reports its own directory.
        assert_eq!(
            preferred_cwd(Some("/remote"), Some("/local"), false),
            Some("/remote")
        );
        // Stock shell (no OSC 7): the shell's kernel cwd, never the job's.
        assert_eq!(preferred_cwd(None, Some("/local"), false), Some("/local"));
        assert_eq!(preferred_cwd(None, None, false), None);
    }

    #[test]
    fn cwd_prefers_kernel_at_the_prompt() {
        // Live kernel query beats an OSC 7 left by an exited job.
        assert_eq!(
            preferred_cwd(Some("/stale"), Some("/local"), true),
            Some("/local")
        );
        // Kernel query unavailable: fall back to OSC 7.
        assert_eq!(
            preferred_cwd(Some("/reported"), None, true),
            Some("/reported")
        );
        assert_eq!(preferred_cwd(None, None, true), None);
    }

    #[test]
    fn title_liveness_requires_a_job_title_newer_than_the_prompt() {
        assert!(title_is_live(2, 1, false, "OC | fix"));
        assert!(!title_is_live(1, 1, false, "left behind"));
        assert!(!title_is_live(2, 1, true, "prompt title"));
        assert!(!title_is_live(2, 1, false, ""));
    }

    #[test]
    fn live_title_replaces_the_template_only_while_a_job_set_it() {
        let (mut app, _inputs) = app_with_tabs(1);
        let label = |app: &App| app.title_for_tab(0, &app.tabs[0]);
        // The shell titles its prompt: the template ("$tab") still applies.
        app.tabs[0].terminal.feed(b"\x1b]2;zsh: ~/dev\x07");
        app.tabs[0].foreground.title_baseline = app.tabs[0].terminal.title_version();
        assert_eq!(label(&app), "1");
        // A job without a title of its own keeps the template too.
        app.tabs[0].running = true;
        assert_eq!(label(&app), "1");
        app.tabs[0].terminal.feed(b"\x1b]2;OC | fix the bug\x07");
        assert_eq!(label(&app), "OC | fix the bug");
        app.config.tabbar.max_label_chars = 5;
        assert_eq!(label(&app), "OC |\u{2026}");
        app.config.tabbar.max_label_chars = 32;
        app.config.tabbar.live_title = false;
        assert_eq!(label(&app), "1");
        app.config.tabbar.live_title = true;
        // An empty title (opencode resets it on exit) is no title.
        app.tabs[0].terminal.feed(b"\x1b]2;\x07");
        assert_eq!(label(&app), "1");
        // Back at the prompt, a leftover title never resurfaces.
        app.tabs[0].terminal.feed(b"\x1b]2;stale\x07");
        app.tabs[0].running = false;
        app.tabs[0].foreground.title_baseline = app.tabs[0].terminal.title_version();
        assert_eq!(label(&app), "1");
        app.tabs[0].running = true;
        assert_eq!(label(&app), "1");
    }

    #[test]
    fn attention_glows_only_unseen_tabs_and_viewing_clears_it() {
        let (mut app, _inputs) = app_with_tabs(3);
        let now = Instant::now();
        app.active = 0;
        app.focused = true;
        // The watched tab never glows; its results are read on the spot.
        app.tabs[0].terminal.feed(b"\x1b]7501;state=done\x1b\\");
        assert!(app.tabs[0].terminal.take_attention());
        app.tab_wants_attention(0, now);
        assert!(app.tabs[0].attention.is_none());
        assert_eq!(app.tabs[0].terminal.status().len(), 0);
        // Another strip tab is unseen.
        app.tabs[1].terminal.feed(b"\x1b]7501;state=error\x1b\\");
        assert!(app.tab_wants_attention(1, now));
        assert_eq!(app.tabs[1].attention, Some(now));
        // A burst does not restart the pulse until the repulse delay.
        assert!(!app.tab_wants_attention(1, now + Duration::from_secs(1)));
        assert_eq!(app.tabs[1].attention, Some(now));
        let later = now + crate::tabbar::GLOW_REPULSE;
        assert!(app.tab_wants_attention(1, later));
        assert_eq!(app.tabs[1].attention, Some(later));
        // With the window in the background even the displayed tab glows,
        // until focus returns.
        app.focused = false;
        assert!(app.tab_wants_attention(0, now));
        app.on_focused(true);
        assert!(app.tabs[0].attention.is_none());
        // Activating tab 1 clears its glow and reads its error result.
        app.switch_tab(1);
        assert!(app.tabs[1].attention.is_none());
        assert_eq!(app.tabs[1].terminal.status().len(), 0);
        // `off` never glows.
        app.config.tabbar.attention = "off".into();
        assert!(!app.tab_wants_attention(2, now));
        assert!(app.tabs[2].attention.is_none());
    }

    #[test]
    fn peeking_and_unparking_clear_a_parked_tab_glow() {
        let (mut app, _inputs) = app_with_tabs(3);
        app.focused = true;
        app.park_tab(app.tabs[2].id);
        let parked = app.tabs.iter().position(|t| t.background).unwrap();
        let id = app.tabs[parked].id;
        assert!(app.tab_wants_attention(parked, Instant::now()));
        assert!(app.background_labels()[0].glow.is_some());
        app.peek_background_tab(id);
        let index = app.tab_index(id).unwrap();
        assert!(app.tabs[index].attention.is_none());
        app.switch_tab(0);
        assert!(app.tab_wants_attention(index, Instant::now()));
        app.unpark_tab(id);
        let index = app.tab_index(id).unwrap();
        assert!(app.tabs[index].attention.is_none());
    }

    #[test]
    fn osc7_liveness_follows_terminal_ownership() {
        // Reported after the shell last owned the terminal: live.
        assert!(osc7_is_live(2, 1, false));
        // Leftover from a finished job (`ssh`, file manager): stale.
        assert!(!osc7_is_live(1, 1, false));
        assert!(!osc7_is_live(0, 0, false));
        // At the prompt the shell's own report is always current.
        assert!(osc7_is_live(1, 1, true));
    }

    #[test]
    fn title_falls_back_to_tab_number() {
        // Templates that render to nothing fall back: `$title` alone on a
        // shell that never sets one, or a blank template.
        let cfg = TabbarConfig {
            title_format: "$title".to_string(),
            ..TabbarConfig::default()
        };
        assert_eq!(format_title(&vars("", None, None, 0), &cfg), "Tab 1");
        assert_eq!(format_title(&vars("", None, None, 2), &cfg), "Tab 3");
    }

    #[test]
    fn osc_title_is_opt_in() {
        // The default template doesn't mention `$title`, so it's ignored.
        let cfg = cfg();
        assert_eq!(
            format_title(&vars("nvim | ~/dev", Some("nvim"), Some("~/dev"), 0), &cfg),
            "nvim | ~/dev"
        );
        // ...but it can be composed explicitly.
        let cfg = TabbarConfig {
            title_format: "$title | $command | $cwd".to_string(),
            ..TabbarConfig::default()
        };
        assert_eq!(
            format_title(&vars("nvim | ~/dev", Some("nvim"), Some("~/dev"), 0), &cfg),
            "nvim | ~/dev | nvim | ~/dev"
        );
    }

    #[test]
    fn literal_text_is_preserved() {
        let brackets = TabbarConfig {
            title_format: "$command [$cwd] ($tab)".to_string(),
            ..TabbarConfig::default()
        };
        assert_eq!(
            format_title(&vars("", Some("zsh"), Some("~/dev"), 1), &brackets),
            "zsh [~/dev] (2)"
        );
        // A template without variables is its own label, verbatim.
        let literal = TabbarConfig {
            title_format: "my terminal".to_string(),
            ..TabbarConfig::default()
        };
        assert_eq!(
            format_title(&vars("", None, None, 0), &literal),
            "my terminal"
        );
    }

    #[test]
    fn all_empty_variables_fall_back_to_tab_number() {
        // Nothing resolved yet (fresh tab, unsupported platform): the
        // stock `$command | $cwd` shows the tab number rather than a bare
        // separator, and surrounding literals don't keep an empty label.
        let stock = cfg();
        assert_eq!(format_title(&vars("", None, None, 0), &stock), "Tab 1");
        assert_eq!(format_title(&vars("", None, None, 2), &stock), "Tab 3");
        let wrapped = TabbarConfig {
            title_format: "a[$command]b".to_string(),
            ..TabbarConfig::default()
        };
        assert_eq!(format_title(&vars("", None, None, 0), &wrapped), "Tab 1");
        // Any one non-empty variable is enough to render the template.
        assert_eq!(
            format_title(&vars("", None, Some("~/dev"), 0), &stock),
            "| ~/dev"
        );
    }

    #[test]
    fn braced_variables_delimit_names() {
        let cfg = TabbarConfig {
            title_format: "${command}s ${tab}!".to_string(),
            ..TabbarConfig::default()
        };
        assert_eq!(
            format_title(&vars("", Some("zsh"), None, 1), &cfg),
            "zshs 2!"
        );
        // Unterminated brace stays literal.
        let cfg = TabbarConfig {
            title_format: "${command".to_string(),
            ..TabbarConfig::default()
        };
        assert_eq!(
            format_title(&vars("", Some("zsh"), None, 0), &cfg),
            "${command"
        );
    }

    #[test]
    fn dollar_literals_and_unknown_names_stay_visible() {
        let cfg = TabbarConfig {
            title_format: "$$5 $5 $nope $ $cwd".to_string(),
            ..TabbarConfig::default()
        };
        assert_eq!(
            format_title(&vars("", None, Some("/tmp"), 0), &cfg),
            "$5 $5 $nope $ /tmp"
        );
    }

    #[test]
    fn values_are_not_rescanned() {
        let cfg = TabbarConfig {
            title_format: "[$cwd]".to_string(),
            ..TabbarConfig::default()
        };
        assert_eq!(
            format_title(&vars("", None, Some("/tmp/$weird/$cwd"), 0), &cfg),
            "[/tmp/$weird/$cwd]"
        );
    }

    #[test]
    fn blank_template_falls_back_to_tab_number() {
        for template in ["", "   ", "$command", "$command $cwd"] {
            let cfg = TabbarConfig {
                title_format: template.to_string(),
                ..TabbarConfig::default()
            };
            assert_eq!(
                format_title(&vars("", None, None, 1), &cfg),
                "Tab 2",
                "template {template:?}"
            );
        }
    }

    #[test]
    fn long_composed_label_truncates_with_ellipsis() {
        let cfg = cfg();
        let long = "a".repeat(100);
        let label = format_title(&vars("", Some(&long), None, 0), &cfg);
        assert_eq!(label.chars().count(), cfg.max_label_chars);
        assert!(label.ends_with('…'));
        // Truncation happens after interpolation, at the final char count.
        let cfg = TabbarConfig {
            max_label_chars: 3,
            title_format: "x $tab y".to_string(),
            ..TabbarConfig::default()
        };
        assert_eq!(format_title(&vars("", None, None, 0), &cfg), "x …");
    }

    #[test]
    fn bar_visibility_follows_platform() {
        let cfg = cfg();
        if cfg!(target_os = "macos") {
            // Merged titlebar: always reserved so content never slides
            // under the traffic lights.
            assert_eq!(bar_height_points(0, &cfg), cfg.height);
            assert_eq!(bar_height_points(1, &cfg), cfg.height);
            assert_eq!(bar_height_points(2, &cfg), cfg.height);
        } else {
            assert_eq!(bar_height_points(0, &cfg), 0.0);
            assert_eq!(bar_height_points(1, &cfg), 0.0);
            assert_eq!(bar_height_points(2, &cfg), cfg.height);
        }
        if cfg!(target_os = "macos") {
            assert_eq!(tab_bar_px(2.0, 1, &cfg), cfg.height * 2.0);
        } else {
            assert_eq!(tab_bar_px(2.0, 1, &cfg), 0.0);
        }
        assert_eq!(tab_bar_px(2.0, 2, &cfg), cfg.height * 2.0);
    }

    #[test]
    fn tab_bar_px_follows_scale() {
        let cfg = cfg();
        assert_eq!(tab_bar_px(1.0, 2, &cfg), cfg.height);
        assert_eq!(tab_bar_px(2.0, 2, &cfg), cfg.height * 2.0);
        assert_eq!(tab_bar_px(0.0, 2, &cfg), cfg.height);
        assert_eq!(tab_bar_px(f32::NAN, 2, &cfg), cfg.height);
    }

    #[test]
    fn term_height_subtracts_tab_bar() {
        let cfg = cfg();
        assert_eq!(term_height_px(600, 1.0, 2, &cfg), 600 - cfg.height as u32);
        if cfg!(target_os = "macos") {
            // Merged titlebar: the single-tab bar still takes space.
            assert_eq!(term_height_px(600, 1.0, 1, &cfg), 600 - cfg.height as u32);
        } else {
            assert_eq!(term_height_px(600, 1.0, 1, &cfg), 600);
        }
        assert_eq!(term_height_px(10, 1.0, 2, &cfg), 1);
        assert_eq!(term_height_px(0, 1.0, 2, &cfg), 1);
    }

    #[test]
    fn window_title_falls_back_to_app_name() {
        let config = Config::default();
        assert_eq!(window_title_for("", &config), "cometty");
        assert_eq!(window_title_for("nvim | ~/dev", &config), "nvim | ~/dev");
    }

    #[test]
    fn active_index_ignores_removals_after_it() {
        assert_eq!(active_after_removals(0, &[2], 2), 0);
        assert_eq!(active_after_removals(1, &[2], 2), 1);
    }

    #[test]
    fn active_index_shifts_down_past_removed_tabs() {
        // Tab 0 exits while tab 1 is active: old tab 1 slides to 0.
        assert_eq!(active_after_removals(1, &[0], 2), 0);
        assert_eq!(active_after_removals(2, &[0], 2), 1);
        assert_eq!(active_after_removals(2, &[0, 1], 1), 0);
    }

    #[test]
    fn active_index_lands_on_next_tab_when_it_is_removed() {
        // Active tab itself exits: focus the tab sliding into its slot.
        assert_eq!(active_after_removals(1, &[1], 2), 1);
        // ...or the new last tab when the old last tab went away.
        assert_eq!(active_after_removals(2, &[2], 2), 1);
        assert_eq!(active_after_removals(0, &[0], 1), 0);
    }

    #[test]
    fn active_index_empty_tabs_stays_zero() {
        assert_eq!(active_after_removals(0, &[0], 0), 0);
    }

    #[test]
    fn cycle_wraps_around_both_directions() {
        assert_eq!(next_tab_index(0, 3, 1), 1);
        assert_eq!(next_tab_index(2, 3, 1), 0);
        assert_eq!(next_tab_index(0, 3, -1), 2);
        assert_eq!(next_tab_index(1, 3, -1), 0);
        // Single tab (or none) stays put.
        assert_eq!(next_tab_index(0, 1, 1), 0);
        assert_eq!(next_tab_index(0, 1, -1), 0);
        assert_eq!(next_tab_index(0, 0, 1), 0);
    }

    #[test]
    fn history_eviction_rebases_or_cancels_selections() {
        let (pty, _, _) = crate::pty::test_support::session();
        let mut tab = Tab::new(0, Terminal::new(4, 2, crate::theme::Theme::default()), pty);
        tab.selection = Some(Selection {
            anchor: CellPos { x: 0, y: 3 },
            active: CellPos { x: 2, y: 3 },
        });
        tab.select_anchor = Some(CellPos { x: 0, y: 3 });
        tab.selecting = true;
        tab.rebase_selection(1);
        let selection = tab.selection.unwrap();
        assert_eq!((selection.anchor.y, selection.active.y), (2, 2));
        assert_eq!(tab.select_anchor.unwrap().y, 2);
        // The anchor row itself was evicted: don't silently select whatever
        // recycled storage now occupies that line.
        tab.rebase_selection(3);
        assert!(tab.selection.is_none());
        assert!(tab.select_anchor.is_none());
        assert!(!tab.selecting);
    }
}
