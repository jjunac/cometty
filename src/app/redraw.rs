//! Redraw path: resize apply, PTY drain, render-param gathering.

use super::App;
use crate::grid;
use crate::renderer;

impl App {
    pub(crate) fn on_redraw(&mut self) {
        // Scale can change without a Resized event (monitor move), so
        // always sync before applying a pending resize.
        let current_scale = self.sync_scale_factor();
        if let Some((w, h)) = self.pending_resize.take() {
            let scale = self.pending_scale.take().unwrap_or(current_scale);
            self.apply_resize(w, h, scale);
        }
        // Drain any pending PTY output that arrived between wake and draw.
        // True means the last tab's shell exited; the caller exits.
        if self.drain_pty() {
            return;
        }

        // Defensive: alt switches always drop the selection.
        if let Some(tab) = self.active_tab() {
            let alt_now = tab.terminal.grid().is_alt();
            if alt_now != tab.is_alt {
                self.clear_selection();
                if let Some(tab) = self.active_tab_mut() {
                    tab.is_alt = alt_now;
                }
            }
        }

        // Owned snapshot so the tabs borrow ends before `render`
        // mutably borrows the renderer and the active scrollbar.
        let snapshot = match self.active_tab() {
            Some(tab) => {
                let grid = tab.terminal.grid();
                let cursor = grid.cursor();
                let rows: Vec<Vec<grid::Cell>> = grid.view_rows().into_iter().cloned().collect();
                let version = grid.version;
                let style = tab.terminal.cursor_style();
                // Steady cursors (DECSCUSR 2/4/6) ignore the blink phase;
                // blinking shapes follow `self.cursor_visible`.
                let effective_cursor = tab.terminal.cursor_visible()
                    && tab.terminal.scroll_offset() == 0
                    && (!style.blinking || self.cursor_visible);
                let total = grid.scrollback_len() + grid.rows();
                let selection_view = tab.selection.as_ref().and_then(|sel| {
                    crate::selection::selection_to_view(
                        sel,
                        grid.scrollback_len(),
                        grid.scroll_offset(),
                        grid.rows(),
                    )
                });
                Some((
                    rows,
                    cursor,
                    version,
                    effective_cursor,
                    style.shape,
                    total,
                    grid.rows(),
                    tab.terminal.scroll_offset(),
                    grid.is_alt(),
                    selection_view,
                ))
            }
            None => None,
        };
        let Some((
            rows,
            cursor,
            version,
            effective_cursor,
            cursor_shape,
            total,
            visible,
            offset,
            is_alt,
            selection_view,
        )) = snapshot
        else {
            return;
        };
        // Keep the OS chrome in sync with the active tab's OSC title.
        self.sync_window_title();
        // Owned: the overlay borrows titles while also taking
        // `&mut` to the active tab's scrollbar.
        let titles = self.tab_titles();
        let active = self.active;

        let (renderer, window) = match (self.renderer.as_mut(), self.window.as_ref()) {
            (Some(r), Some(w)) => (r, w),
            _ => return,
        };
        let output = {
            let Some(tab) = self.tabs.get_mut(active) else {
                return;
            };
            match renderer.render(
                &rows,
                renderer::CursorCtx {
                    pos: (cursor.x, cursor.y),
                    visible: effective_cursor,
                    shape: cursor_shape,
                },
                version,
                selection_view,
                renderer::ScrollCtx {
                    window,
                    ui: &mut tab.scrollbar,
                    total,
                    visible,
                    offset,
                    is_alt,
                    tab_titles: &titles,
                    active_tab: active,
                },
            ) {
                Ok(o) => o,
                Err(e) => {
                    // Surface lost / outdated is recoverable via resize.
                    log::warn!("render failed: {e:#}");
                    let s = window.inner_size();
                    renderer.resize(s.width.max(1), s.height.max(1));
                    return;
                }
            }
        };

        if let Some(target) = output.scroll_to
            && let Some(tab) = self.active_tab_mut()
            && tab.terminal.scroll_to_offset(target)
            && let Some(w) = self.window.as_ref()
        {
            w.request_redraw();
        }
        if let Some(i) = output.selected_tab {
            self.switch_tab(i);
        }
        if output.new_tab {
            self.spawn_tab_for_window();
        }
        if let Some(i) = output.close_tab {
            self.close_tab(i);
        }
        if self.tabs.is_empty() {
            return;
        }
        // Keep animating the fade without PTY traffic.
        let animating = self.active_tab().is_some_and(|t| {
            (t.scrollbar.opacity > 0.0 && t.scrollbar.opacity < 1.0) || t.scrollbar.is_dragging()
        });
        if animating && let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
}
