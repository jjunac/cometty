//! Mouse handling: drag-select, double-click word select, wheel scroll,
//! plus xterm mouse reporting (`1000/1002/1003/1006-SGR`).

use std::time::Instant;

use winit::event::{ElementState, MouseButton, MouseScrollDelta};

use super::App;
use crate::grid::MouseMode;
use crate::selection::{CellPos, Selection};

fn mouse_bit(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(1),
        MouseButton::Middle => Some(2),
        MouseButton::Right => Some(4),
        _ => None,
    }
}

impl App {
    fn held_button_base(&self) -> Option<u8> {
        if self.mouse_pressed & 1 != 0 {
            Some(0)
        } else if self.mouse_pressed & 2 != 0 {
            Some(1)
        } else if self.mouse_pressed & 4 != 0 {
            Some(2)
        } else {
            None
        }
    }

    /// Active mouse-reporting state, or `None` for local handling.
    /// `Shift` held forces local selection/scroll (override).
    fn mouse_report_state(&self) -> Option<(MouseMode, bool)> {
        if self.modifiers.shift_key() {
            return None;
        }
        let tab = self.active_tab()?;
        let mode = tab.terminal.mouse_mode();
        if !mode.enabled() {
            return None;
        }
        Some((mode, tab.terminal.mouse_sgr()))
    }

    fn send_to_active_pty(&self, bytes: Option<Vec<u8>>) {
        if let (Some(data), Some(tab)) = (bytes, self.active_tab()) {
            tab.pty.write(data);
        }
    }

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
        // Mouse reporting: forward motion instead of extending a selection.
        if let Some((mode, sgr)) = self.mouse_report_state() {
            let button_held = self.mouse_pressed != 0;
            if !mode.reports_motion(button_held) {
                return;
            }
            let Some((col, view_row)) = self.cell_under_cursor() else {
                return;
            };
            let bytes = match self.held_button_base() {
                Some(base) => {
                    crate::input::encode_mouse_drag(base, col, view_row, &self.modifiers, sgr)
                }
                None => crate::input::encode_mouse_hover(col, view_row, &self.modifiers, sgr),
            };
            self.send_to_active_pty(bytes);
            return;
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

    pub(crate) fn on_focused(&mut self, focused: bool) {
        if !focused {
            self.mouse_pressed = 0;
        }
        let report = self.active_tab().is_some_and(|t| t.terminal.focus_report());
        if report && let Some(tab) = self.active_tab() {
            tab.pty.write(if focused {
                b"\x1b[I".to_vec()
            } else {
                b"\x1b[O".to_vec()
            });
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    pub(crate) fn on_mouse_input(
        &mut self,
        button: MouseButton,
        state: ElementState,
        egui_consumed: bool,
    ) {
        // While the settings panel is open, presses over egui chrome belong
        // to the panel (window drag, widget clicks), not to
        // terminal selection. Releases always flow through so a drag started
        // before the panel opened can't stick.
        if self.settings.open && egui_consumed && state == ElementState::Pressed {
            return;
        }
        let is_grid_button = matches!(
            button,
            MouseButton::Left | MouseButton::Middle | MouseButton::Right
        );
        if is_grid_button {
            match state {
                ElementState::Pressed => {
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
                    if let Some(bit) = mouse_bit(button) {
                        self.mouse_pressed |= bit;
                    }
                    if let Some((_, sgr)) = self.mouse_report_state() {
                        let Some((col, view_row)) = self.cell_under_cursor() else {
                            return;
                        };
                        let bytes = crate::input::encode_mouse_press(
                            button,
                            col,
                            view_row,
                            &self.modifiers,
                            sgr,
                        );
                        self.send_to_active_pty(bytes);
                        return;
                    }
                    // Local fallback below only selects with the left button.
                    if button != MouseButton::Left {
                        return;
                    }
                }
                ElementState::Released => {
                    if let Some(bit) = mouse_bit(button) {
                        self.mouse_pressed &= !bit;
                    }
                    if let Some((_, sgr)) = self.mouse_report_state() {
                        let Some((col, view_row)) = self.cell_under_cursor() else {
                            return;
                        };
                        let bytes =
                            crate::input::encode_mouse_release(col, view_row, &self.modifiers, sgr);
                        self.send_to_active_pty(bytes);
                        return;
                    }
                    if button != MouseButton::Left {
                        return;
                    }
                }
            }
        }
        match (button, state) {
            (MouseButton::Left, ElementState::Pressed) => {
                let now = Instant::now();
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

    pub(crate) fn on_wheel(&mut self, delta: MouseScrollDelta, egui_consumed: bool) {
        // Scrolling over the open settings panel scrolls the panel.
        if self.settings.open && egui_consumed {
            return;
        }
        // Wheel over the find bar belongs to the bar (nothing scrollable
        // there), not to the terminal underneath it.
        if let Some((x, y)) = self.cursor_pos
            && self.press_on_search_bar(x, y)
        {
            return;
        }
        // Mouse reporting: forward wheel instead of scrolling locally.
        // `Shift` forces local scroll via `mouse_report_state`.
        if let Some((_, sgr)) = self.mouse_report_state() {
            let Some((col, view_row)) = self.cell_under_cursor() else {
                return;
            };
            match delta {
                MouseScrollDelta::LineDelta(_, y) => {
                    let mut n = y.round() as isize;
                    if n == 0 && y != 0.0 {
                        n = y.signum() as isize;
                    }
                    let up = n > 0;
                    for _ in 0..n.abs() {
                        let bytes = crate::input::encode_mouse_wheel(
                            up,
                            col,
                            view_row,
                            &self.modifiers,
                            sgr,
                        );
                        if let Some(data) = bytes
                            && let Some(tab) = self.active_tab()
                        {
                            tab.pty.write(data);
                        }
                    }
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
                        let up = lines > 0;
                        for _ in 0..lines.abs() {
                            let bytes = crate::input::encode_mouse_wheel(
                                up,
                                col,
                                view_row,
                                &self.modifiers,
                                sgr,
                            );
                            if let Some(data) = bytes
                                && let Some(tab) = self.active_tab()
                            {
                                tab.pty.write(data);
                            }
                        }
                    }
                }
            }
            return;
        }
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
