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

        // Find-bar matches follow the grid: recompute when stale (new
        // output, resize, tab switch) and reveal a freshly picked match.
        // A background refresh never scrolls on its own.
        self.refresh_search();

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
        // Highlight snapshot for the find bar (owned, so the tab can be
        // borrowed mutably for the overlay below).
        let search_view = self.search_view();
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
        // Snapshot for settings change detection: the overlay mutates
        // `self.config` in place during `render`; any diff is live-applied
        // and auto-saved afterwards (both Ok and surface-error paths).
        let config_before = self.config.clone();
        // Same idea for the find bar: the query field mutates `tab.search`
        // in place during the frame; a change re-matches on the next redraw.
        let search_query_before = self
            .tabs
            .get(active)
            .map(|t| t.search.query.clone())
            .unwrap_or_default();
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
                renderer::HighlightCtx {
                    selection: selection_view,
                    search: search_view.as_ref(),
                },
                renderer::ScrollCtx {
                    window,
                    ui: &mut tab.scrollbar,
                    total,
                    visible,
                    offset,
                    is_alt,
                    tab_titles: &titles,
                    active_tab: active,
                    settings: &mut self.settings,
                    config: &mut self.config,
                    logs: &mut self.logs,
                    log_buffer: &self.log_buffer,
                    search: &mut tab.search,
                },
            ) {
                Ok(o) => o,
                Err(e) => {
                    // Surface lost / outdated is recoverable via resize.
                    log::warn!("render failed: {e:#}");
                    let s = window.inner_size();
                    let fullscreen =
                        super::is_fullscreen_like(window, s.width.max(1), s.height.max(1));
                    renderer.resize(s.width.max(1), s.height.max(1), fullscreen);
                    if self.config != config_before {
                        self.apply_settings_changes(&config_before);
                        self.save_config_from_settings();
                    }
                    return;
                }
            }
        };

        // Find bar: buttons clicked this frame act on the tab that was
        // rendered; apply them before any tab switch so they can't land on
        // the wrong tab.
        if output.search.close {
            self.close_search();
        }
        if output.search.prev {
            self.step_search(false);
        }
        if output.search.next {
            self.step_search(true);
        }
        // The bar edited the query during this frame: re-match + reveal on
        // the next redraw (this frame's highlight was already shaped).
        if self
            .tabs
            .get(active)
            .is_some_and(|t| t.search.query != search_query_before)
        {
            if let Some(tab) = self.tabs.get_mut(active) {
                tab.search.query_changed();
            }
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }

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
        // Settings overlay edited the live config during this frame:
        // route to the session and auto-save (GUI wins over external edits).
        if self.config != config_before {
            self.apply_settings_changes(&config_before);
            self.save_config_from_settings();
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
