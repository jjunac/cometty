//! Tab model: each tab owns a terminal + PTY plus its per-view state.
//!
//! `App` holds `tabs: Vec<Tab>` and an `active` index; the renderer,
//! window, clipboard, and blink state stay shared. Tab management
//! (spawn/switch/close) lives here so `keyboard` / `pty_io` / `redraw`
//! stay thin.

use std::time::Instant;

use crate::config::{Config, TabbarConfig};
use crate::pty::PtySession;
use crate::scrollbar::ScrollbarUi;
use crate::selection::Selection;
use crate::term::Terminal;

use super::{App, UserEvent};

/// A single terminal tab.
pub struct Tab {
    pub(crate) terminal: Terminal,
    pub(crate) pty: PtySession,
    pub(crate) selection: Option<Selection>,
    pub(crate) selecting: bool,
    pub(crate) scrollbar: ScrollbarUi,
    pub(crate) is_alt: bool,
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

/// Label for tab `index` (0-based): the shell's OSC title when set,
/// otherwise `Tab N`. Long titles are truncated with an ellipsis.
pub fn display_title(index: usize, osc_title: &str, config: &TabbarConfig) -> String {
    if osc_title.is_empty() {
        return format!("Tab {}", index + 1);
    }
    let max_chars = config.max_label_chars.max(1);
    let count = osc_title.chars().count();
    if count <= max_chars {
        return osc_title.to_string();
    }
    let kept: String = osc_title.chars().take(max_chars - 1).collect();
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
            .map(|(i, t)| display_title(i, t.terminal.title(), &self.config.tabbar))
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
        self.tabs.push(Tab::new(terminal, pty));
        self.active = self.tabs.len() - 1;
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
        if let Some(r) = self.renderer.as_mut() {
            r.invalidate();
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Close tab `index`. Returns true when no tabs remain and the
    /// caller should exit the event loop.
    pub(crate) fn close_tab(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() {
            return self.tabs.is_empty();
        }
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

    #[test]
    fn title_falls_back_to_tab_number() {
        let cfg = cfg();
        assert_eq!(display_title(0, "", &cfg), "Tab 1");
        assert_eq!(display_title(2, "", &cfg), "Tab 3");
    }

    #[test]
    fn short_osc_title_passes_through() {
        let cfg = cfg();
        assert_eq!(display_title(0, "nvim | ~/dev", &cfg), "nvim | ~/dev");
    }

    #[test]
    fn long_osc_title_truncates_with_ellipsis() {
        let cfg = cfg();
        let long = "a".repeat(100);
        let label = display_title(0, &long, &cfg);
        assert_eq!(label.chars().count(), cfg.max_label_chars);
        assert!(label.ends_with('…'));
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
}
