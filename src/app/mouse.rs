//! Mouse handling: drag-select, double-click word select, wheel scroll.

use std::time::Instant;

use winit::event::{ElementState, MouseButton, MouseScrollDelta};

use super::{App, DOUBLE_CLICK_MS};
use crate::selection::{CellPos, Selection};

impl App {
    pub(crate) fn on_cursor_moved(&mut self, x: f32, y: f32) {
        self.cursor_pos = Some((x, y));
        if !self.selecting {
            return;
        }
        let cell = self.cell_under_cursor();
        let global = cell.and_then(|(_, row)| self.view_to_global(row));
        if let (Some((col, _)), Some(g)) = (cell, global)
            && let Some(sel) = self.selection.as_mut()
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
                    .is_some_and(|(x, _)| self.press_on_chrome(x))
                {
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
                    && now.duration_since(t).as_millis() <= DOUBLE_CLICK_MS
                    && (pc, pr) == (col, view_row)
                    && let Some(term) = self.terminal.as_ref()
                {
                    let row_chars: Vec<char> = term
                        .grid()
                        .view_rows()
                        .get(view_row)
                        .map(|r| r.iter().map(|c| c.ch).collect())
                        .unwrap_or_default();
                    let (sx, ex) = crate::selection::expand_word(&row_chars, col);
                    // Clamp to visible cols so a resized row can't overflow.
                    let cols = term.cols();
                    let sx = sx.min(cols.saturating_sub(1));
                    let ex = ex.min(cols.saturating_sub(1));
                    self.selection = Some(Selection {
                        anchor: CellPos { x: sx, y: global },
                        active: CellPos { x: ex, y: global },
                    });
                    self.selecting = false;
                    self.last_click = None;
                    if let Some(w) = self.window.as_ref() {
                        w.request_redraw();
                    }
                    return;
                }
                self.selection = Some(Selection::new(CellPos { x: col, y: global }));
                self.selecting = true;
                self.last_click = Some((now, (col, view_row)));
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            (MouseButton::Left, ElementState::Released) if self.selecting => {
                self.selecting = false;
                if self.selection.as_ref().is_some_and(|s| s.is_empty()) {
                    self.selection = None;
                }
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }

    pub(crate) fn on_wheel(&mut self, delta: MouseScrollDelta) {
        const LINES_PER_TICK: f64 = 7.0;
        match delta {
            MouseScrollDelta::LineDelta(_, y) => {
                self.scroll_terminal((y as f64 * LINES_PER_TICK).round() as isize);
            }
            MouseScrollDelta::PixelDelta(pos) => {
                let line_height = self
                    .renderer
                    .as_ref()
                    .map(|r| f64::from(r.line_height))
                    .unwrap_or(20.0)
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
