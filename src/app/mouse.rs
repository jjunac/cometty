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

/// What the pointer is over in egui chrome, judged from the last painted
/// frame. Only these can change appearance under a moving pointer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ChromeHover {
    /// The terminal grid, empty strip space, a faded scrollbar strip, or
    /// outside the window: nothing there paints hover.
    #[default]
    None,
    /// A tab, its close `×` / park `↓`, or the `+` button.
    Strip(crate::tabbar::StripTarget),
    /// The visible scrollbar track (highlighted while hovered).
    Scrollbar,
    /// The find bar: a text field and buttons with their own hover states.
    SearchBar,
    /// The background-tab tray popover: clickable rows/buttons.
    Popover,
    /// An open settings or log panel, including its popups and tooltips.
    Panel,
    /// The update notice: its body or one of its buttons (hover-tinted).
    UpdateNotice(crate::renderer::update_ui::NoticeTarget),
}

/// Pointer motion (`CursorMoved`/`CursorLeft`) repaints only when chrome under
/// it can look different: during native strip capture or while egui uses the
/// pointer (a scrollbar or panel drag), on every move over the find bar or an
/// open panel, and when the hovered strip element, scrollbar or update-notice
/// target changes, including one frame on leaving chrome to clear its hover.
/// Motion over the grid alone paints nothing.
pub(crate) fn pointer_repaint(
    previous: ChromeHover,
    current: ChromeHover,
    egui_using_pointer: bool,
) -> bool {
    egui_using_pointer
        || matches!(
            current,
            ChromeHover::SearchBar | ChromeHover::Popover | ChromeHover::Panel
        )
        || current != previous
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
        let strip_capture = self.tab_drag.captures_pointer();
        if strip_capture {
            let scale = self
                .renderer
                .as_ref()
                .map_or(1.0, |r| r.scale_factor().max(1.0));
            if let Some(id) = self.tab_drag.moved([x / scale, y / scale])
                && let Some(index) = self.tab_index(id)
            {
                self.switch_tab(index);
            }
        }
        // Hovering a link (modifier held) switches to the pointer cursor; no
        // frame is needed for that.
        self.update_link_cursor();
        self.update_chrome_hover(Some((x, y)));
        // A tab-strip press owns the whole gesture, including motion over the
        // grid. Never turn its held primary button into a TUI drag/hover report.
        if strip_capture {
            self.last_mouse_report = None;
            return;
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
        self.tab_drag.pointer_gone();
        self.set_link_cursor_icon(winit::window::CursorIcon::Default);
        self.update_chrome_hover(None);
    }

    /// Chrome element under physical `(x, y)`, judged like the last painted
    /// frame. Hit tests are explicit: egui's area rects cannot tell the grid
    /// from chrome, because the scrollbar area spans the window while shown.
    fn chrome_hover_at(&self, x: f32, y: f32) -> ChromeHover {
        let Some(renderer) = self.renderer.as_ref() else {
            return ChromeHover::None;
        };
        if self.settings.open || self.logs.open {
            return ChromeHover::Panel;
        }
        if self.search_open() && renderer.over_search_bar(x, y) {
            return ChromeHover::SearchBar;
        }
        if self.bg_popover_open && renderer.over_tab_popover(x, y) {
            return ChromeHover::Popover;
        }
        if let Some(target) = renderer.update_notice_target(x, y) {
            return ChromeHover::UpdateNotice(target);
        }
        if renderer.over_tab_bar(y, self.tabs.len()) {
            return match renderer.tab_strip_target(x, y) {
                crate::tabbar::StripTarget::Empty => ChromeHover::None,
                target => ChromeHover::Strip(target),
            };
        }
        if renderer.over_scrollbar(x)
            && self
                .active_tab()
                .is_some_and(|tab| tab.scrollbar.interactive(tab.terminal.grid().is_alt()))
        {
            return ChromeHover::Scrollbar;
        }
        ChromeHover::None
    }

    /// egui reports `repaint` for every pointer event; request a frame only
    /// when [`pointer_repaint`] says chrome can change. Otherwise collapse
    /// the pending egui motion, so unpainted moves cannot pile up.
    fn update_chrome_hover(&mut self, pos: Option<(f32, f32)>) {
        let current = pos.map_or(ChromeHover::None, |(x, y)| self.chrome_hover_at(x, y));
        let previous = std::mem::replace(&mut self.chrome_hover, current);
        let using = self.tab_drag.captures_pointer()
            || self
                .renderer
                .as_ref()
                .is_some_and(|r| r.egui_is_using_pointer());
        if pointer_repaint(previous, current, using) {
            if let Some(w) = self.window.as_ref() {
                crate::perf::add(crate::perf::Metric::RequestPointer, 1);
                w.request_redraw();
            }
        } else if let Some(r) = self.renderer.as_mut() {
            r.coalesce_pointer_motion();
        }
    }

    pub(crate) fn on_focused(&mut self, focused: bool) {
        self.focused = focused;
        // Unfocused windows draw a steady cursor; regained focus restarts the
        // blink, visible phase first, and its inactivity timeout. The redraw
        // below shows either change.
        let now = Instant::now();
        if focused {
            self.last_input = now;
            // The displayed tab is seen again; the redraw below shows it.
            self.tab_seen(self.active);
        }
        super::scheduling::blink_restart(now, &mut self.last_blink, &mut self.cursor_visible);
        if !focused {
            self.tab_drag.focus_lost();
            self.bg_popover_open = false;
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

    /// Update-notice element under the last known cursor position, as
    /// painted by the last egui pass.
    fn update_notice_under_cursor(&self) -> Option<crate::renderer::update_ui::NoticeTarget> {
        let (x, y) = self.cursor_pos?;
        self.renderer.as_ref()?.update_notice_target(x, y)
    }

    pub(crate) fn on_mouse_input(
        &mut self,
        button: MouseButton,
        state: ElementState,
        egui_consumed: bool,
    ) {
        let notice = self.update_notice_under_cursor();
        self.route_mouse_input(button, state, egui_consumed, notice);
    }

    /// [`Self::on_mouse_input`] with the update-notice hit-test resolved,
    /// so routing is testable without a renderer. egui has already seen the
    /// event (its buttons click there); here a press on the notice is chrome.
    fn route_mouse_input(
        &mut self,
        button: MouseButton,
        state: ElementState,
        egui_consumed: bool,
        notice: Option<crate::renderer::update_ui::NoticeTarget>,
    ) {
        self.last_mouse_report = None;
        // A press anywhere but the popover (or its tray toggle) dismisses
        // the menu, then continues as a normal click. The tray press is
        // excluded so its release toggles the popover exactly once.
        if button == MouseButton::Left
            && state == ElementState::Pressed
            && self.bg_popover_open
            && let Some((x, y)) = self.cursor_pos
        {
            let over_chrome = self.renderer.as_ref().is_some_and(|r| {
                r.over_tab_popover(x, y)
                    || matches!(
                        r.tab_strip_target(x, y),
                        crate::tabbar::StripTarget::BackgroundTray
                    )
            });
            if !over_chrome {
                self.bg_popover_open = false;
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
        }
        if button == MouseButton::Left {
            if state == ElementState::Pressed {
                // A fresh press also retires a focus-loss release guard when
                // the previous button was released outside this window.
                self.tab_drag = crate::tabbar::TabDrag::default();
            } else if self.tab_drag.release() {
                self.mouse_pressed &= !1;
                return;
            }
        }
        if self.tab_drag.captures_pointer() {
            if state == ElementState::Released
                && let Some(bit) = mouse_bit(button)
            {
                self.mouse_pressed &= !bit;
            }
            return;
        }
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
                    // The update notice floats over the grid: its press is
                    // chrome, so it never selects, reports, opens a link
                    // underneath, or takes focus from egui.
                    if notice.is_some() {
                        return;
                    }
                    if self
                        .cursor_pos
                        .is_some_and(|(x, y)| self.press_on_chrome(x, y))
                    {
                        if button == MouseButton::Left
                            && let Some((x, y)) = self.cursor_pos
                            && let Some(renderer) = self.renderer.as_ref()
                            && renderer.over_tab_bar(y, self.tabs.len())
                        {
                            let scale = renderer.scale_factor().max(1.0);
                            let source = renderer
                                .tab_strip_source(x, y)
                                .filter(|(id, _)| self.can_drag_tab(*id));
                            self.tab_drag.begin(source, [x / scale, y / scale]);
                        }
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
                    let held = mouse_bit(button).is_some_and(|bit| self.mouse_pressed & bit != 0);
                    if let Some(bit) = mouse_bit(button) {
                        self.mouse_pressed &= !bit;
                    }
                    // The press opened a link: swallow its release.
                    if button == MouseButton::Left && self.link_press {
                        self.link_press = false;
                        return;
                    }
                    // A release over the notice without a terminal press
                    // (a notice click) is not reported either. A grid drag
                    // ending there still finishes normally.
                    if notice.is_some() && !held {
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

    /// Escape/panel changes cancel the preview, but keep pointer capture until
    /// the primary release so it cannot leak into terminal mouse reporting.
    pub(crate) fn cancel_tab_drag(&mut self) -> bool {
        let cancelled = self.tab_drag.cancel();
        if cancelled && let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
        cancelled
    }

    /// Single click on empty titlebar chrome starts a native window drag;
    /// double-click toggles native macOS zoom (`set_maximized`, which maps
    /// to `zoom:`). Empty chrome only: tabs, the `+` button and the traffic
    /// lights are excluded by `is_titlebar_drag`. Compiled everywhere so
    /// Linux CI type-checks it; only called on macOS.
    fn handle_titlebar_press(&mut self, button: MouseButton) {
        // Ownership was chosen on the press. A real tab source must never fall
        // through to a native window move, even before its drag threshold.
        if self.tab_drag.source().is_some() {
            self.last_titlebar_click = None;
            return;
        }
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
            renderer.tab_strip_target(x_phys, y_phys),
            self.background_count(),
        ) {
            self.last_titlebar_click = None;
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
        // The horizontal strip consumes its own wheel/trackpad input. A held
        // strip gesture must not scroll/report to the terminal underneath it.
        if self.tab_drag.captures_pointer()
            || self.cursor_pos.is_some_and(|(_, y)| {
                self.renderer
                    .as_ref()
                    .is_some_and(|r| r.over_tab_bar(y, self.tabs.len()))
            })
        {
            return;
        }
        // Scrolling over the open settings panel scrolls the panel.
        if self.settings.open && egui_consumed {
            return;
        }
        // The tray popover owns wheel input (its own scroll area), never
        // the terminal underneath it.
        if self.bg_popover_open
            && self.cursor_pos.is_some_and(|(x, y)| {
                self.renderer
                    .as_ref()
                    .is_some_and(|r| r.over_tab_popover(x, y))
            })
        {
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
    use super::{
        ChromeHover, ClickState, is_titlebar_double_click, next_click_count, pointer_repaint,
    };
    use crate::tabbar::StripTarget;
    use std::time::{Duration, Instant};

    fn mouse_app() -> (super::App, Vec<crate::pty::input::InputReceiver>) {
        let mut app = super::App::new(
            None,
            crate::theme::Theme::default(),
            crate::config::Config::default(),
            crate::app::AppStartup {
                config_path_override: None,
                cli_theme: None,
                config_corrupt: false,
            },
            std::sync::Arc::new(std::sync::Mutex::new(crate::logbuf::LogBuffer::new(10))),
        );
        let mut inputs = Vec::new();
        for id in [37, 91] {
            let (pty, _, input) = crate::pty::test_support::session();
            let mut terminal = crate::term::Terminal::new(20, 2, crate::theme::Theme::default());
            terminal.feed(b"one\r\ntwo\r\nthree\r\nfour\r\n");
            app.tabs.push(crate::app::Tab::new(id, terminal, pty));
            inputs.push(input);
        }
        (app, inputs)
    }

    #[test]
    fn tab_drag_activates_once_and_captures_motion_wheel_other_presses_and_release() {
        use winit::event::{ElementState, MouseButton, MouseScrollDelta};
        let (mut app, inputs) = mouse_app();
        app.tabs[1].selecting = true;
        app.tabs[1].selection = Some(crate::selection::Selection::new(
            crate::selection::CellPos { x: 0, y: 1 },
        ));
        app.tab_drag
            .begin(Some((91, [100.0, 5.0, 200.0, 35.0])), [150.0, 20.0]);
        app.on_cursor_moved(154.0, 20.0);
        assert_eq!(app.active, 0, "jitter is still a click");
        app.on_cursor_moved(170.0, 100.0);
        assert_eq!(app.tabs[app.active].id, 91);
        app.on_mouse_input(MouseButton::Right, ElementState::Pressed, false);
        assert_eq!(
            app.mouse_pressed, 0,
            "chrome presses do not become terminal buttons"
        );
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, 3.0), false);
        assert_eq!(
            app.tabs[1].terminal.scroll_offset(),
            0,
            "wheel over the captured grid does not scroll it"
        );
        app.on_mouse_input(MouseButton::Left, ElementState::Released, false);
        assert!(
            app.tabs[1].selecting,
            "a strip release does not finalize a terminal selection"
        );
        assert!(inputs.iter().all(|input| input.try_recv().is_err()));
        assert!(!app.tab_drag.captures_pointer());
        assert!(app.terminal_owns_keyboard());
        app.on_ime(&winit::event::Ime::Commit("é".into()));
        assert_eq!(inputs[1].try_recv().unwrap(), "é".as_bytes());
        assert!(inputs[0].try_recv().is_err());
    }

    #[test]
    fn cancelled_drag_keeps_its_release_guard_across_escape_or_focus_loss() {
        use winit::event::{ElementState, MouseButton};
        for lose_focus in [false, true] {
            let (mut app, inputs) = mouse_app();
            app.tab_drag
                .begin(Some((91, [100.0, 5.0, 200.0, 35.0])), [150.0, 20.0]);
            app.on_cursor_moved(350.0, 20.0);
            app.tabs[1].selecting = true;
            if lose_focus {
                app.on_focused(false);
                assert!(!app.tab_drag.captures_pointer());
                app.on_focused(true);
            } else {
                assert!(app.cancel_tab_drag());
                assert!(app.tab_drag.captures_pointer());
            }
            assert_eq!(app.tab_drag.source(), None);
            app.on_mouse_input(MouseButton::Left, ElementState::Released, false);
            assert!(
                app.tabs[1].selecting,
                "cancelled release still belongs to the strip"
            );
            assert!(inputs.iter().all(|input| input.try_recv().is_err()));
            assert!(app.tab_drag.suppress_clicks());
        }
    }

    #[test]
    fn lone_foreground_press_captures_release_without_cancelling_the_click_or_losing_input() {
        use winit::event::{ElementState, MouseButton};
        for background_count in [1, 3] {
            let (mut app, inputs) = mouse_app();
            app.park_tab(91);
            for i in 1..background_count {
                let (pty, _, _) = crate::pty::test_support::session();
                let mut tab = crate::app::Tab::new(
                    100 + i,
                    crate::term::Terminal::new(20, 2, crate::theme::Theme::default()),
                    pty,
                );
                tab.background = true;
                app.tabs.push(tab);
            }
            app.peek_background_tab(91);
            assert_eq!(app.foreground_count(), 1);
            assert_eq!(app.background_count(), background_count as usize);
            // Use the same model rule as the native mouse press, rather than
            // the widget harness's independent foreground-label count.
            let source = app
                .can_drag_tab(37)
                .then_some((37, [100.0, 5.0, 200.0, 35.0]));
            app.tab_drag.begin(source, [150.0, 20.0]);
            app.reconcile_tab_drag();
            assert_eq!(app.tab_drag.source(), None);
            assert!(app.tab_drag.captures_pointer());
            app.on_cursor_moved(152.0, 20.0);
            app.on_mouse_input(MouseButton::Left, ElementState::Released, false);
            assert!(!app.tab_drag.suppress_clicks());
            assert!(!app.tab_drag.captures_pointer());
            assert!(inputs.iter().all(|input| input.try_recv().is_err()));

            // Apply the strip's selected stable ID as redraw does. Leaving
            // the peek must keep the background task parked and route input
            // to the foreground terminal again.
            app.switch_tab(app.tab_index(37).unwrap());
            assert!(!app.is_peeking());
            assert_eq!(app.tabs[app.active].id, 37);
            assert!(app.tabs[app.tab_index(91).unwrap()].background);
            assert!(app.terminal_owns_keyboard());
            app.on_ime(&winit::event::Ime::Commit("é".into()));
            assert_eq!(inputs[0].try_recv().unwrap(), "é".as_bytes());
            assert!(inputs[1].try_recv().is_err());
        }
    }

    #[test]
    fn pointer_motion_repaints_only_when_chrome_under_it_can_change() {
        use ChromeHover::*;
        let tab = |i| Strip(StripTarget::Tab(i));
        let close = |i| Strip(StripTarget::Close(i));
        let park = |i| Strip(StripTarget::Park(i));
        // Moving over the grid: no frames, whatever the position.
        assert!(!pointer_repaint(None, None, false));
        // Within one tab, × or `+`, or along the visible scrollbar: nothing
        // changes on screen. Entering, switching and leaving do.
        for steady in [
            tab(1),
            close(1),
            park(1),
            Strip(StripTarget::NewTab),
            Scrollbar,
        ] {
            assert!(!pointer_repaint(steady, steady, false), "{steady:?}");
            assert!(pointer_repaint(None, steady, false), "enter {steady:?}");
            assert!(
                pointer_repaint(steady, None, false),
                "exit frame {steady:?}"
            );
        }
        assert!(pointer_repaint(tab(0), tab(1), false));
        assert!(pointer_repaint(tab(1), close(1), false));
        assert!(pointer_repaint(tab(1), park(1), false));
        assert!(pointer_repaint(park(1), tab(1), false));
        assert!(pointer_repaint(Scrollbar, tab(0), false));
        // The find bar and open panels have per-widget hover: every move.
        for busy in [SearchBar, Panel] {
            assert!(pointer_repaint(busy, busy, false));
            assert!(pointer_repaint(busy, None, false));
        }
        // A drag egui owns (scrollbar thumb, panel window) follows every move,
        // even over the grid.
        assert!(pointer_repaint(None, None, true));
        assert!(pointer_repaint(Scrollbar, Scrollbar, true));
    }

    #[test]
    fn update_notice_hover_repaints_only_when_its_target_changes() {
        use crate::renderer::update_ui::NoticeTarget::{self, *};
        let notice = ChromeHover::UpdateNotice;
        let all: [NoticeTarget; 4] = [Body, Restart, Later, Open];
        for target in all {
            // Steady over one target: no frame.
            assert!(!pointer_repaint(notice(target), notice(target), false));
            // Entering from the grid or another chrome element: one frame.
            assert!(pointer_repaint(ChromeHover::None, notice(target), false));
            assert!(pointer_repaint(
                ChromeHover::Scrollbar,
                notice(target),
                false
            ));
            // Leaving to the grid: one frame to clear the hover, then none.
            assert!(pointer_repaint(notice(target), ChromeHover::None, false));
            assert!(!pointer_repaint(
                ChromeHover::None,
                ChromeHover::None,
                false
            ));
            for other in all.into_iter().filter(|&t| t != target) {
                assert!(
                    pointer_repaint(notice(target), notice(other), false),
                    "{target:?} -> {other:?}"
                );
            }
        }
    }

    #[test]
    fn presses_on_the_update_notice_are_chrome() {
        use crate::renderer::update_ui::NoticeTarget;
        use winit::event::{ElementState, MouseButton};
        let (mut app, inputs) = mouse_app();
        for target in [
            NoticeTarget::Body,
            NoticeTarget::Restart,
            NoticeTarget::Later,
            NoticeTarget::Open,
        ] {
            for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
                app.route_mouse_input(button, ElementState::Pressed, true, Some(target));
                assert_eq!(
                    app.mouse_pressed, 0,
                    "{target:?} {button:?} is no grid press"
                );
                assert!(!app.link_press);
                app.route_mouse_input(button, ElementState::Released, true, Some(target));
                assert_eq!(app.mouse_pressed, 0);
            }
        }
        assert!(
            app.tabs
                .iter()
                .all(|t| !t.selecting && t.selection.is_none())
        );
        assert!(inputs.iter().all(|input| input.try_recv().is_err()));

        // Control: the same press off the notice is a terminal press, and a
        // drag that ends over the notice still finishes its selection.
        app.route_mouse_input(MouseButton::Left, ElementState::Pressed, false, None);
        assert_eq!(app.mouse_pressed, 1);
        app.tabs[0].selecting = true;
        app.route_mouse_input(
            MouseButton::Left,
            ElementState::Released,
            false,
            Some(NoticeTarget::Later),
        );
        assert_eq!(app.mouse_pressed, 0);
        assert!(!app.tabs[0].selecting, "release finalizes the drag");
    }

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
