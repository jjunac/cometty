//! Selection + chrome-hit-test helpers (viewed from `App`).

use super::App;
use crate::scrollbar;

impl App {
    /// Visible `(col, view_row)` under the last known cursor position.
    /// `None` on chrome (tab bar / scrollbar) or outside the grid.
    pub(crate) fn cell_under_cursor(&self) -> Option<(usize, usize)> {
        let (x, y) = self.cursor_pos?;
        let tab_count = self.tabs.len();
        self.renderer.as_ref()?.cell_at_pos(x, y, tab_count)
    }

    /// Whether the overlay scrollbar is currently painted.
    pub(crate) fn scrollbar_visible(&self) -> bool {
        let Some(t) = self.active_tab() else {
            return false;
        };
        let grid = t.terminal.grid();
        !grid.is_alt()
            && scrollbar::geometry(
                100.0,
                grid.scrollback_len() + grid.rows(),
                grid.rows(),
                grid.scroll_offset(),
                &self.config.scrollbar,
            )
            .is_some()
    }

    /// True when physical `x` is on scrollbar chrome (and it is visible).
    /// egui's `consumed` flag claims presses across the whole window, so
    /// selection must use this explicit hit-test instead.
    pub(crate) fn press_on_chrome(&self, x_phys: f32, y_phys: f32) -> bool {
        let tab_count = self.tabs.len();
        if self
            .renderer
            .as_ref()
            .is_some_and(|r| r.over_tab_bar(y_phys, tab_count))
        {
            return true;
        }
        self.scrollbar_visible()
            && self
                .renderer
                .as_ref()
                .is_some_and(|r| r.over_scrollbar(x_phys))
    }

    pub(crate) fn view_to_global(&self, view_row: usize) -> Option<usize> {
        let t = self.active_tab()?;
        let grid = t.terminal.grid();
        Some(crate::selection::view_to_global(
            view_row,
            grid.scrollback_len(),
            grid.scroll_offset(),
        ))
    }

    pub(crate) fn clear_selection(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.selection = None;
            tab.selecting = false;
        }
    }
}
