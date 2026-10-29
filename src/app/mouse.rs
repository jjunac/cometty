//! Mouse handling: drag-select, double-click word select, wheel scroll,
//! plus xterm mouse reporting (`1000/1002/1003/1006-SGR`).

use std::time::Instant;

use winit::event::{ElementState, MouseButton, MouseScrollDelta};

use super::App;
use crate::grid::MouseMode;
use crate::selection::{CellPos, Selection};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MotionState {
    tab: u64,
    cell: (usize, usize),
    button: Option<u8>,
    modifiers: winit::keyboard::ModifiersState,
    mode: MouseMode,
    sgr: bool,
}

/// True when a titlebar press at `now` completes a double-click: a previous
/// press exists and falls within `threshold_ms`. Pure for headless testing.
pub(crate) fn is_titlebar_double_click(
    now: Instant,
    last: Option<Instant>,
    threshold_ms: u64,
) -> bool {
    matches!(last, Some(t) if now.duration_since(t).as_millis() <= u128::from(threshold_ms))
}

/// Multi-click chain: last grid press time, view cell, and count
/// (1 single, 2 word, 3 line). The 3rd press matches anywhere on the same
/// view row (browsers do this); the 2nd needs the exact cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClickState {
    pub time: Instant,
    pub pos: (usize, usize),
    pub count: u8,
}

/// Next click count for a press at `pos`/`now`. Pure for headless testing:
/// outside the threshold or after a triple the chain restarts at 1.
pub(crate) fn next_click_count(
    now: Instant,
    last: Option<ClickState>,
    pos: (usize, usize),
    threshold_ms: u64,
) -> u8 {
    let Some(last) = last else {
        return 1;
    };
    if now.duration_since(last.time).as_millis() > u128::from(threshold_ms) {
        return 1;
    }
    if last.count >= 3 {
        return 1;
    }
    if last.count == 2 {
        // Triple-click: same view row is enough.
        if last.pos.1 == pos.1 {
            return 3;
        }
        return 1;
    }
    if last.pos == pos {
        return 2;
    }
    1
}

/// `SelCell` snapshot of one global line for word/line math.
fn sel_cells_for_global(grid: &crate::grid::Grid, global: usize) -> Vec<crate::selection::SelCell> {
    grid.global_line_cells(global)
        .map(|row| {
            row.into_iter()
                .map(|c| crate::selection::SelCell {
                    text: c.cluster(),
                    width: c.width,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Word bounds at (`col`, `global`), clamped to the grid width.
fn word_bounds_at(
    grid: &crate::grid::Grid,
    col: usize,
    global: usize,
    config: &crate::config::SelectionConfig,
) -> (usize, usize) {
    let cols = grid.cols();
    if cols == 0 {
        return (0, 0);
    }
    let row = sel_cells_for_global(grid, global);
    if row.is_empty() {
        return (
            col.min(cols.saturating_sub(1)),
            col.min(cols.saturating_sub(1)),
        );
    }
    let (mut sx, mut ex) = crate::selection::expand_word(&row, col, config);
    sx = sx.min(cols.saturating_sub(1));
    ex = ex.min(cols.saturating_sub(1));
    (sx, ex)
}

/// Trimmed line end (inclusive) at `global`, clamped to the grid width.
fn line_end_at(grid: &crate::grid::Grid, global: usize) -> usize {
    let cols = grid.cols();
    if cols == 0 {
        return 0;
    }
    let row = sel_cells_for_global(grid, global);
    if row.is_empty() {
        return cols.saturating_sub(1);
    }
    crate::selection::expand_line(&row)
        .1
        .min(cols.saturating_sub(1))
}

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

    fn send_to_active_pty(&self, bytes: Option<Vec<u8>>) -> bool {
        if let (Some(data), Some(tab)) = (bytes, self.active_tab()) {
            tab.pty.write(data)
        } else {
            false
        }
    }

    pub(crate) fn on_cursor_moved(&mut self, x: f32, y: f32) {
        self.cursor_pos = Some((x, y));
        // Hovering a link (modifier held) switches to the pointer cursor;
        // hovering the tab strip must repaint (hover highlight / × reveal)
        // even when no selection drag is in progress.
        self.update_link_cursor();
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
                self.last_mouse_report = None;
                return;
            }
            let Some((col, view_row)) = self.cell_under_cursor() else {
                return;
            };
            let state = MotionState {
                tab: self.active_tab().unwrap().id,
                cell: (col, view_row),
                button: self.held_button_base(),
                modifiers: self.modifiers,
                mode,
                sgr,
            };
            if self.last_mouse_report == Some(state) {
                return;
            }
            let bytes = match self.held_button_base() {
                Some(base) => {
                    crate::input::encode_mouse_drag(base, col, view_row, &self.modifiers, sgr)
                }
                None => crate::input::encode_mouse_hover(col, view_row, &self.modifiers, sgr),
            };
            if self.send_to_active_pty(bytes) {
                self.last_mouse_report = Some(state);
            }
            return;
        }
        self.last_mouse_report = None;
        let selecting = self.active_tab().is_some_and(|t| t.selecting);
        if !selecting {
            return;
        }
        let cell = self.cell_under_cursor();
        let global = cell.and_then(|(_, row)| self.view_to_global(row));
        let (Some((col, _)), Some(g)) = (cell, global) else {
            return;
        };
        // Drag granularity comes from the press chain: chars follow the
        // cursor, words span whole matches, lines span whole rows.
        let (mode, anchor) = self
            .active_tab()
            .and_then(|t| t.select_anchor.map(|a| (t.select_mode, a)))
            .unwrap_or((crate::selection::SelectMode::Char, CellPos { x: col, y: g }));
        let (anchor, active) = {
            let Some(tab) = self.active_tab() else {
                return;
            };
            let grid = tab.terminal.grid();
            match mode {
                crate::selection::SelectMode::Char => (
                    CellPos {
                        x: anchor.x,
                        y: anchor.y,
                    },
                    CellPos { x: col, y: g },
                ),
                crate::selection::SelectMode::Word => {
                    let (asx, aex) =
                        word_bounds_at(grid, anchor.x, anchor.y, &self.config.selection);
                    let (csx, cex) = word_bounds_at(grid, col, g, &self.config.selection);
                    // Same global line: span both words on that line.
                    if anchor.y == g {
                        let s = asx.min(csx);
                        let e = aex.max(cex);
                        // Direction follows the drag so further motion is stable.
                        if (g, col) < (anchor.y, anchor.x) {
                            (CellPos { x: e, y: g }, CellPos { x: s, y: g })
                        } else {
                            (CellPos { x: s, y: g }, CellPos { x: e, y: g })
                        }
                    } else if g > anchor.y {
                        (
                            CellPos {
                                x: asx,
                                y: anchor.y,
                            },
                            CellPos { x: cex, y: g },
                        )
                    } else {
                        (
                            CellPos {
                                x: aex,
                                y: anchor.y,
                            },
                            CellPos { x: csx, y: g },
                        )
                    }
                }
                crate::selection::SelectMode::Line => {
                    let (lo, hi) = (anchor.y.min(g), anchor.y.max(g));
                    let end = line_end_at(grid, hi);
                    // Anchor stays at the pressed end so direction survives.
                    if g < anchor.y {
                        (
                            CellPos {
                                x: end,
                                y: anchor.y,
                            },
                            CellPos { x: 0, y: lo },
                        )
                    } else {
                        (CellPos { x: 0, y: lo }, CellPos { x: end, y: hi })
                    }
                }
            }
        };
        if let Some(tab) = self.active_tab_mut()
            && let Some(sel) = tab.selection.as_mut()
        {
            sel.anchor = anchor;
            sel.update(active);
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }

    pub(crate) fn on_cursor_left(&mut self) {
        self.last_mouse_report = None;
        self.cursor_pos = None;
        self.set_link_cursor_icon(winit::window::CursorIcon::Default);
    }

    pub(crate) fn on_focused(&mut self, focused: bool) {
        self.focused = focused;
        // Unfocused windows draw a steady cursor; regained focus restarts the
        // blink, visible phase first, and its inactivity timeout. The redraw
        // below shows either change.
        let now = Instant::now();
        if focused {
            self.last_input = now;
        }
        super::scheduling::blink_restart(now, &mut self.last_blink, &mut self.cursor_visible);
        if !focused {
            self.last_mouse_report = None;
            self.mouse_pressed = 0;
            self.link_press = false;
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
        self.last_mouse_report = None;
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
                        // Merged macOS titlebar: single click on empty chrome
                        // drags the window like Brave, double-click toggles
                        // native zoom. No-op on other platforms.
                        if cfg!(target_os = "macos") {
                            self.handle_titlebar_press(button);
                        }
                        return;
                    }
                    // Grid press takes keyboard focus back from egui: a
                    // stale text/chrome focus would otherwise keep eating
                    // keys through the `consumed` flag. The settings panel
                    // keeps its focus (it owns the keyboard while open).
                    if !self.settings.open
                        && let Some(r) = self.renderer.as_mut()
                    {
                        r.surrender_egui_focus();
                    }
                    // Cmd/Ctrl+click on a link opens it and never reaches
                    // the app or the selection (checked before mouse
                    // reporting so links stay clickable inside TUIs).
                    if button == MouseButton::Left && self.open_link_at_cursor() {
                        self.link_press = true;
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
                    // The press opened a link: swallow its release.
                    if button == MouseButton::Left && self.link_press {
                        self.link_press = false;
                        return;
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
                let count = next_click_count(
                    now,
                    self.last_click,
                    (col, view_row),
                    self.config.selection.double_click_ms,
                );
                self.last_click = Some(ClickState {
                    time: now,
                    pos: (col, view_row),
                    count,
                });
                match count {
                    2 => {
                        // Double-click: word on this visual row; hold-and-drag
                        // extends by words (`select_mode` drives the motion).
                        let (sx, ex, cols) = match self.active_tab() {
                            Some(tab) => {
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
                                let (sx, ex) = crate::selection::expand_word(
                                    &row_cells,
                                    col,
                                    &self.config.selection,
                                );
                                (sx, ex, tab.terminal.cols())
                            }
                            None => return,
                        };
                        // Clamp to visible cols so a resized row can't overflow.
                        let sx = sx.min(cols.saturating_sub(1));
                        let ex = ex.min(cols.saturating_sub(1));
                        if let Some(tab) = self.active_tab_mut() {
                            tab.selection = Some(Selection {
                                anchor: CellPos { x: sx, y: global },
                                active: CellPos { x: ex, y: global },
                            });
                            tab.selecting = true;
                            tab.select_mode = crate::selection::SelectMode::Word;
                            tab.select_anchor = Some(CellPos { x: col, y: global });
                        }
                        if let Some(w) = self.window.as_ref() {
                            w.request_redraw();
                        }
                        return;
                    }
                    3 => {
                        // Triple-click: whole visual line (trimmed); drag
                        // extends by lines.
                        let (sx, ex) = match self.active_tab() {
                            Some(tab) => {
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
                                crate::selection::expand_line(&row_cells)
                            }
                            None => return,
                        };
                        let cols = self.active_tab().map(|t| t.terminal.cols()).unwrap_or(1);
                        let sx = sx.min(cols.saturating_sub(1));
                        let ex = ex.min(cols.saturating_sub(1));
                        if let Some(tab) = self.active_tab_mut() {
                            tab.selection = Some(Selection {
                                anchor: CellPos { x: sx, y: global },
                                active: CellPos { x: ex, y: global },
                            });
                            tab.selecting = true;
                            tab.select_mode = crate::selection::SelectMode::Line;
                            tab.select_anchor = Some(CellPos { x: col, y: global });
                        }
                        if let Some(w) = self.window.as_ref() {
                            w.request_redraw();
                        }
                        return;
                    }
                    _ => {}
                }
                if let Some(tab) = self.active_tab_mut() {
                    tab.selection = Some(Selection::new(CellPos { x: col, y: global }));
                    tab.selecting = true;
                    tab.select_mode = crate::selection::SelectMode::Char;
                    tab.select_anchor = Some(CellPos { x: col, y: global });
                }
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
                    tab.select_anchor = None;
                    tab.select_mode = crate::selection::SelectMode::Char;
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

    /// Single click on empty titlebar chrome starts a native window drag;
    /// double-click toggles native macOS zoom (`set_maximized`, which maps
    /// to `zoom:`). Empty chrome only: tabs, the `+` button and the traffic
    /// lights are excluded by `is_titlebar_drag`. Compiled everywhere so
    /// Linux CI type-checks it; only called on macOS.
    fn handle_titlebar_press(&mut self, button: MouseButton) {
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
        if !crate::tabbar::is_titlebar_drag(
            x_phys / scale,
            y_phys / scale,
            screen_w_pts,
            bar_h,
            &self.config.tabbar,
        ) {
            return;
        }
        if button != MouseButton::Left {
            return;
        }
        let now = Instant::now();
        if is_titlebar_double_click(
            now,
            self.last_titlebar_click,
            self.config.selection.double_click_ms,
        ) {
            self.last_titlebar_click = None;
            window.set_maximized(!window.is_maximized());
        } else {
            self.last_titlebar_click = Some(now);
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

#[cfg(test)]
mod tests {
    use super::{ClickState, is_titlebar_double_click, next_click_count};
    use std::time::{Duration, Instant};

    #[test]
    fn titlebar_double_click_needs_a_prior_press() {
        let now = Instant::now();
        assert!(!is_titlebar_double_click(now, None, 400));
    }

    #[test]
    fn titlebar_double_click_within_threshold() {
        let first = Instant::now();
        let second = first + Duration::from_millis(200);
        assert!(is_titlebar_double_click(second, Some(first), 400));
    }

    #[test]
    fn titlebar_double_click_outside_threshold() {
        let first = Instant::now();
        let second = first + Duration::from_millis(401);
        assert!(!is_titlebar_double_click(second, Some(first), 400));
    }

    fn click_at(time: Instant, pos: (usize, usize), count: u8) -> ClickState {
        ClickState { time, pos, count }
    }

    #[test]
    fn click_chain_single_double_triple() {
        let t0 = Instant::now();
        assert_eq!(next_click_count(t0, None, (5, 2), 400), 1);
        let t1 = t0 + Duration::from_millis(100);
        assert_eq!(
            next_click_count(t1, Some(click_at(t0, (5, 2), 1)), (5, 2), 400),
            2
        );
        // Third press anywhere on the same row counts as triple.
        let t2 = t1 + Duration::from_millis(100);
        assert_eq!(
            next_click_count(t2, Some(click_at(t1, (5, 2), 2)), (9, 2), 400),
            3
        );
        // Fourth press restarts the chain.
        let t3 = t2 + Duration::from_millis(100);
        assert_eq!(
            next_click_count(t3, Some(click_at(t2, (9, 2), 3)), (9, 2), 400),
            1
        );
    }

    #[test]
    fn click_chain_resets_on_timeout_or_move() {
        let t0 = Instant::now();
        let late = t0 + Duration::from_millis(401);
        assert_eq!(
            next_click_count(late, Some(click_at(t0, (5, 2), 1)), (5, 2), 400),
            1
        );
        let t1 = t0 + Duration::from_millis(50);
        // Different cell breaks a double.
        assert_eq!(
            next_click_count(t1, Some(click_at(t0, (5, 2), 1)), (6, 2), 400),
            1
        );
        // Different row breaks a triple.
        assert_eq!(
            next_click_count(t1, Some(click_at(t0, (5, 2), 2)), (5, 3), 400),
            1
        );
    }
}
