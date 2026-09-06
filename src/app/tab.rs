//! Tab model: each tab owns a terminal + PTY plus its per-view state.
//!
//! `App` holds `tabs: Vec<Tab>` and an `active` index; the renderer,
//! window, clipboard, and blink state stay shared. Tab management
//! (spawn/switch/close) lives here so `keyboard` / `pty_io` / `redraw`
//! stay thin.

use std::time::Instant;

use crate::pty::PtySession;
use crate::renderer::TAB_BAR_HEIGHT_POINTS;
use crate::scrollbar::ScrollbarUi;
use crate::selection::Selection;
use crate::term::Terminal;

use super::{App, UserEvent};

/// Maximum tab-label width in chars; longer OSC titles are truncated
/// with an ellipsis so one noisy tab can't crowd out the rest.
const MAX_LABEL_CHARS: usize = 32;

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

/// Tab-bar height in physical pixels for a given scale factor.
pub fn tab_bar_px(scale: f32) -> f32 {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    TAB_BAR_HEIGHT_POINTS * scale
}

/// Terminal-area height: window height minus the tab bar, at least 1px.
pub fn term_height_px(window_h: u32, scale: f32) -> u32 {
    (window_h as f32 - tab_bar_px(scale)).max(1.0) as u32
}

/// Label for tab `index` (0-based): the shell's OSC title when set,
/// otherwise `Tab N`. Long titles are truncated with an ellipsis.
pub fn display_title(index: usize, osc_title: &str) -> String {
    if osc_title.is_empty() {
        return format!("Tab {}", index + 1);
    }
    let count = osc_title.chars().count();
    if count <= MAX_LABEL_CHARS {
        return osc_title.to_string();
    }
    let kept: String = osc_title.chars().take(MAX_LABEL_CHARS - 1).collect();
    format!("{kept}…")
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
            .map(|(i, t)| display_title(i, t.terminal.title()))
            .collect()
    }

    fn waker(&self) -> impl Fn() + Send + 'static {
        let proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>> = self.proxy.clone();
        move || {
            if let Some(p) = proxy.as_ref() {
                let _ = p.send_event(UserEvent::PtyAvailable);
            }
        }
    }

    /// Spawn a tab at `cols`x`rows` and switch to it. Returns false when
    /// the PTY failed to spawn (error already logged).
    pub(crate) fn spawn_tab(&mut self, cols: usize, rows: usize) -> bool {
        let terminal = Terminal::new(cols, rows, self.theme);
        let pty = match PtySession::spawn_with_size(cols, rows, self.waker()) {
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
        let h = term_height_px(size.height.max(1), renderer.scale_factor());
        let (cols, rows) =
            super::compute_grid_size(w, h, renderer.cell_width, renderer.line_height);
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
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        if let Some(r) = self.renderer.as_mut() {
            r.invalidate();
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_falls_back_to_tab_number() {
        assert_eq!(display_title(0, ""), "Tab 1");
        assert_eq!(display_title(2, ""), "Tab 3");
    }

    #[test]
    fn short_osc_title_passes_through() {
        assert_eq!(display_title(0, "nvim | ~/dev"), "nvim | ~/dev");
    }

    #[test]
    fn long_osc_title_truncates_with_ellipsis() {
        let long = "a".repeat(100);
        let label = display_title(0, &long);
        assert_eq!(label.chars().count(), MAX_LABEL_CHARS);
        assert!(label.ends_with('…'));
    }

    #[test]
    fn tab_bar_px_follows_scale() {
        assert_eq!(tab_bar_px(1.0), TAB_BAR_HEIGHT_POINTS);
        assert_eq!(tab_bar_px(2.0), TAB_BAR_HEIGHT_POINTS * 2.0);
        assert_eq!(tab_bar_px(0.0), TAB_BAR_HEIGHT_POINTS);
        assert_eq!(tab_bar_px(f32::NAN), TAB_BAR_HEIGHT_POINTS);
    }

    #[test]
    fn term_height_subtracts_tab_bar() {
        assert_eq!(term_height_px(600, 1.0), 600 - TAB_BAR_HEIGHT_POINTS as u32);
        assert_eq!(term_height_px(10, 1.0), 1);
        assert_eq!(term_height_px(0, 1.0), 1);
    }
}
