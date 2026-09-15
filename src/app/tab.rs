//! Tab model: each tab owns a terminal + PTY plus its per-view state.
//!
//! `App` holds `tabs: Vec<Tab>` and an `active` index; the renderer,
//! window, clipboard, and blink state stay shared. Tab management
//! (spawn/switch/close) lives here so `keyboard` / `pty_io` / `redraw`
//! stay thin.

use std::time::Instant;

use crate::config::{Config, TabbarConfig};
use crate::procinfo;
use crate::pty::PtySession;
use crate::scrollbar::ScrollbarUi;
use crate::selection::Selection;
use crate::term::Terminal;

use super::{App, UserEvent};

/// Kernel facts for the `$command` / `$cwd` label variables, refreshed
/// only while the tab's PTY is producing output (see
/// [`Tab::refresh_title`]). Kept here so label rendering stays syscall-free.
#[derive(Default)]
pub(crate) struct Foreground {
    pgid: Option<i32>,
    command: Option<String>,
    cwd: Option<String>,
}

/// A single terminal tab.
pub struct Tab {
    pub(crate) terminal: Terminal,
    pub(crate) pty: PtySession,
    pub(crate) selection: Option<Selection>,
    pub(crate) selecting: bool,
    pub(crate) scrollbar: ScrollbarUi,
    pub(crate) is_alt: bool,
    pub(crate) foreground: Foreground,
}

impl Tab {
    pub(crate) fn new(terminal: Terminal, pty: PtySession) -> Self {
        Self {
            terminal,
            pty,
            selection: None,
            selecting: false,
            scrollbar: ScrollbarUi::new(Instant::now()),
            is_alt: false,
            foreground: Foreground::default(),
        }
    }

    /// Re-query the foreground process group behind `$command` / `$cwd`.
    ///
    /// Called after this tab's PTY produced output: a job taking or
    /// releasing the terminal and a shell `cd` both echo through the tty,
    /// so the foreground pid and cwd are re-read exactly when they can
    /// change. `command` is only resolved when the pid changed; `cwd` is
    /// re-read every time (same pid, different directory). Failed queries
    /// keep the last known value: a job that exited between `tcgetpgrp`
    /// and the lookup must not blank the label for a frame.
    pub(crate) fn refresh_title(&mut self) {
        let pgid = self.pty.foreground_pgid();
        if pgid != self.foreground.pgid {
            self.foreground.pgid = pgid;
            if let Some(command) = pgid.and_then(procinfo::command) {
                self.foreground.command = Some(command);
            }
        }
        if let Some(cwd) = pgid.and_then(procinfo::cwd) {
            self.foreground.cwd = Some(cwd);
        }
    }
}

/// Tab-bar height in logical points for `tab_count` tabs: hidden when
/// there is a single tab (or none) — except on macOS, where the strip
/// lives inside the OS titlebar (transparent titlebar + fullsize content
/// view, Brave-style) and is always shown so terminal content never
/// slides under the traffic lights.
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
    /// Working directory (kernel-reported, else `OSC 7`), `~`-shortened.
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

    /// Labels for every tab in order, for the tab-bar overlay.
    pub(crate) fn tab_titles(&self) -> Vec<String> {
        self.tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                // Kernel-reported cwd, else the shell's OSC 7 (shortened
                // at the last moment so the cache keeps raw paths).
                let cwd = t
                    .foreground
                    .cwd
                    .as_deref()
                    .or_else(|| t.terminal.cwd())
                    .map(procinfo::shorten_home);
                format_title(
                    &TitleVars {
                        title: t.terminal.title(),
                        command: t.foreground.command.as_deref(),
                        cwd: cwd.as_deref(),
                        tab: i,
                    },
                    &self.config.tabbar,
                )
            })
            .collect()
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

    fn waker(&self) -> impl Fn() + Send + 'static {
        let proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>> = self.proxy.clone();
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

    /// Spawn a tab at `cols`x`rows` and switch to it. Returns false when
    /// the PTY failed to spawn (error already logged).
    pub(crate) fn spawn_tab(&mut self, cols: usize, rows: usize) -> bool {
        let terminal = Terminal::new_with_config(cols, rows, self.theme, &self.config);
        let pty = match PtySession::spawn_with_size(cols, rows, self.waker(), &self.config) {
            Ok(p) => p,
            Err(e) => {
                log::error!("failed to spawn pty: {e:#}");
                return false;
            }
        };
        // The label stays `Tab N` until the first prompt output triggers
        // `refresh_title`: a spawn-time query would race the child's exec
        // and may report cometty's own image as `$command`.
        self.tabs.push(Tab::new(terminal, pty));
        self.active = self.tabs.len() - 1;
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
    pub(crate) fn spawn_tab_for_window(&mut self) {
        let Some(renderer) = self.renderer.as_ref() else {
            return;
        };
        let Some(window) = self.window.as_ref() else {
            return;
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
        self.spawn_tab(cols, rows);
    }

    /// Switch to tab `index`. No-op when out of range or already active.
    pub(crate) fn switch_tab(&mut self, index: usize) {
        if index >= self.tabs.len() || index == self.active {
            return;
        }
        self.active = index;
        log::debug!("switched to tab {index}");
        if let Some(r) = self.renderer.as_mut() {
            r.invalidate();
        }
        self.sync_window_title();
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Cycle tabs by `delta` with wrap-around (`+1` next, `-1` prev).
    /// No-op with fewer than 2 tabs.
    pub(crate) fn switch_relative(&mut self, delta: isize) {
        let next = next_tab_index(self.active, self.tabs.len(), delta);
        self.switch_tab(next);
    }

    /// Close tab `index`. Returns true when no tabs remain and the
    /// caller should exit the event loop.
    pub(crate) fn close_tab(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() {
            return self.tabs.is_empty();
        }
        log::debug!("closed tab {index} ({} left)", self.tabs.len() - 1);
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            return true;
        }
        self.active = active_after_removals(self.active, &[index], self.tabs.len());
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TabbarConfig};

    fn cfg() -> TabbarConfig {
        TabbarConfig::default()
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
}
