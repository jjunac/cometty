//! Mouse handling: drag-select, double-click word select, wheel scroll.

use std::time::Instant;

use winit::event::{ElementState, MouseButton, MouseScrollDelta};

use super::App;
use crate::selection::{CellPos, Selection};

impl App {
    pub(crate) fn on_cursor_moved(&mut self, x: f32, y: f32) {
        self.cursor_pos = Some((x, y));
        // Hovering the tab strip must repaint (hover highlight / × reveal)
        // even when no selection drag is in progress.
        let tab_count = self.tabs.len();
        if self
            .renderer
            .as_ref()
            .is_some_and(|r| r.over_tab_bar(y, tab_count))
            && let Some(w) = self.window.as_ref()
        {
            w.request_redraw();
        }
        let selecting = self.active_tab().is_some_and(|t| t.selecting);
        if !selecting {
            return;
        }
        let cell = self.cell_under_cursor();
        let global = cell.and_then(|(_, row)| self.view_to_global(row));
        if let (Some((col, _)), Some(g)) = (cell, global)
            && let Some(tab) = self.active_tab_mut()
            && let Some(sel) = tab.selection.as_mut()
        {
            sel.update(CellPos { x: col, y: g });
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }

    pub(crate) fn on_cursor_left(&mut self) {
        self.cursor_pos = None;
    }

    pub(crate) fn on_mouse_input(&mut self, button: MouseButton, state: ElementState) {
        match (button, state) {
            (MouseButton::Left, ElementState::Pressed) => {
                let now = Instant::now();
                if self
                    .cursor_pos
                    .is_some_and(|(x, y)| self.press_on_chrome(x, y))
                {
                    // Merged macOS titlebar: dragging empty chrome moves
                    // the window like Brave. No-op on other platforms.
                    if cfg!(target_os = "macos") {
                        self.maybe_drag_titlebar();
                    }
                    return;
                }
                let Some((col, view_row)) = self.cell_under_cursor() else {
                    return;
                };
                let Some(global) = self.view_to_global(view_row) else {
                    return;
                };
                // Double-click: expand to word on the same visual row.
                if let Some((t, (pc, pr))) = self.last_click
                    && now.duration_since(t).as_millis()
                        <= u128::from(self.config.selection.double_click_ms)
                    && (pc, pr) == (col, view_row)
                    && let Some(tab) = self.active_tab()
                {
                    let row_cells: Vec<crate::selection::SelCell> = tab
                        .terminal
                        .grid()
                        .view_rows()
                        .get(view_row)
                        .map(|r| {
                            r.iter()
                                .map(|c| crate::selection::SelCell {
                                    text: c.cluster(),
                                    width: c.width,
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let (sx, ex) =
                        crate::selection::expand_word(&row_cells, col, &self.config.selection);
                    // Clamp to visible cols so a resized row can't overflow.
                    let cols = tab.terminal.cols();
                    let sx = sx.min(cols.saturating_sub(1));
                    let ex = ex.min(cols.saturating_sub(1));
                    if let Some(tab) = self.active_tab_mut() {
                        tab.selection = Some(Selection {
                            anchor: CellPos { x: sx, y: global },
                            active: CellPos { x: ex, y: global },
                        });
                        tab.selecting = false;
                    }
                    self.last_click = None;
                    if let Some(w) = self.window.as_ref() {
                        w.request_redraw();
                    }
                    return;
                }
                if let Some(tab) = self.active_tab_mut() {
                    tab.selection = Some(Selection::new(CellPos { x: col, y: global }));
                    tab.selecting = true;
                }
                self.last_click = Some((now, (col, view_row)));
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            (MouseButton::Left, ElementState::Released) => {
                let selecting = self.active_tab().is_some_and(|t| t.selecting);
                if !selecting {
                    return;
                }
                if let Some(tab) = self.active_tab_mut() {
                    tab.selecting = false;
                    if tab.selection.as_ref().is_some_and(|s| s.is_empty()) {
                        tab.selection = None;
                    }
                }
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }

    /// Start a native window drag when the press landed on empty
    /// titlebar chrome (macOS merged titlebar). Does nothing on tabs,
    /// the `+` button, or the traffic lights. Compiled everywhere so
    /// Linux CI type-checks it; only called on macOS.
    fn maybe_drag_titlebar(&self) {
        let Some((x_phys, y_phys)) = self.cursor_pos else {
            return;
        };
        let Some(renderer) = self.renderer.as_ref() else {
            return;
        };
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let scale = renderer.scale_factor().max(1.0);
        let screen_w_pts = window.inner_size().width.max(1) as f32 / scale;
        let bar_h = super::tab::bar_height_points(self.tabs.len(), &self.config.tabbar);
        if crate::tabbar::is_titlebar_drag(
            x_phys / scale,
            y_phys / scale,
            screen_w_pts,
            bar_h,
            &self.config.tabbar,
        ) {
            let _ = window.drag_window();
        }
    }

    pub(crate) fn on_wheel(&mut self, delta: MouseScrollDelta) {
        let lines_per_tick = self.config.input.lines_per_tick;
        match delta {
            MouseScrollDelta::LineDelta(_, y) => {
                self.scroll_terminal((f64::from(y) * lines_per_tick).round() as isize);
            }
            MouseScrollDelta::PixelDelta(pos) => {
                let fallback = f64::from(self.config.input.pixel_fallback_line_height).max(1.0);
                let line_height = self
                    .renderer
                    .as_ref()
                    .map(|r| f64::from(r.line_height))
                    .unwrap_or(fallback)
                    .max(1.0);
                self.wheel_accum += pos.y / line_height;
                let lines = self.wheel_accum.trunc() as isize;
                if lines != 0 {
                    self.wheel_accum -= lines as f64;
                    self.scroll_terminal(lines);
                }
            }
        }
    }
}
