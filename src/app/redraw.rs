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
        self.drain_pty();

        // Defensive: alt switches always drop the selection.
        let alt_now = self.terminal.as_ref().is_some_and(|t| t.grid().is_alt());
        if alt_now != self.is_alt {
            self.clear_selection();
            self.is_alt = alt_now;
        }

        let (renderer, terminal, window) = match (
            self.renderer.as_mut(),
            self.terminal.as_ref(),
            self.window.as_ref(),
        ) {
            (Some(r), Some(t), Some(w)) => (r, t, w),
            _ => return,
        };
        let grid = terminal.grid();
        let cursor = grid.cursor();
        let rows: Vec<Vec<grid::Cell>> = grid.view_rows().into_iter().cloned().collect();
        let version = grid.version;
        let effective_cursor =
            self.cursor_visible && terminal.cursor_visible() && terminal.scroll_offset() == 0;
        let total = grid.scrollback_len() + grid.rows();
        let selection_view = self.selection.as_ref().and_then(|sel| {
            crate::selection::selection_to_view(
                sel,
                grid.scrollback_len(),
                grid.scroll_offset(),
                grid.rows(),
            )
        });
        match renderer.render(
            &rows,
            (cursor.x, cursor.y),
            effective_cursor,
            version,
            selection_view,
            renderer::ScrollCtx {
                window,
                ui: &mut self.scrollbar,
                total,
                visible: grid.rows(),
                offset: terminal.scroll_offset(),
                is_alt: grid.is_alt(),
            },
        ) {
            Ok(scroll_to) => {
                if let Some(target) = scroll_to
                    && let Some(t) = self.terminal.as_mut()
                    && t.scroll_to_offset(target)
                    && let Some(w) = self.window.as_ref()
                {
                    w.request_redraw();
                }
                // Keep animating the fade without PTY traffic.
                let fading = self.scrollbar.opacity > 0.0 && self.scrollbar.opacity < 1.0;
                if let Some(w) = self.window.as_ref()
                    && (fading || self.scrollbar.is_dragging())
                {
                    w.request_redraw();
                }
            }
            Err(e) => {
                // Surface lost / outdated is recoverable via resize.
                log::warn!("render failed: {e:#}");
                let s = window.inner_size();
                renderer.resize(s.width.max(1), s.height.max(1));
            }
        }
    }
}
