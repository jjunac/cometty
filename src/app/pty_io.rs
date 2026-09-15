//! PTY drain + resize plumbing.

use super::App;
use crate::app::tab::{active_after_removals, term_height_px};
use crate::pty::PtyEvent;

impl App {
    pub(crate) fn scroll_terminal(&mut self, delta: isize) {
        if delta == 0 {
            return;
        }
        if let Some(t) = self.active_tab_mut()
            && t.terminal.scroll_by(delta)
            && let Some(w) = self.window.as_ref()
        {
            w.request_redraw();
        }
    }

    /// Drain all tabs' PTY output into their terminals.
    /// Returns true when no tabs remain (shell of the last tab exited)
    /// and the caller should exit the event loop.
    pub(crate) fn drain_pty(&mut self) -> bool {
        let mut got_data = false;
        let mut exited: Vec<usize> = Vec::new();
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            let mut got_bytes = false;
            loop {
                match tab.pty.try_recv() {
                    Some(PtyEvent::Data(bytes)) => {
                        tab.terminal.feed(&bytes);
                        // Query replies (DA / CPR / DSR / DECRQM) flow back.
                        let reply = tab.terminal.take_response();
                        if !reply.is_empty() {
                            tab.pty.write(reply);
                        }
                        got_data = true;
                        got_bytes = true;
                    }
                    Some(PtyEvent::Exit) => {
                        log::info!("shell exited (tab {i})");
                        exited.push(i);
                        got_data = true;
                        break;
                    }
                    None => break,
                }
            }
            // Output is the only reliable signal that the foreground job
            // or the shell's cwd changed; refresh `$command`/`$cwd` once
            // per burst rather than per 8KB chunk.
            if got_bytes {
                tab.refresh_title();
            }
        }
        // Remove exited tabs from the back so indices stay valid, then
        // remap the active index once (tabs before it may have vanished).
        let removed = !exited.is_empty();
        for i in exited.iter().rev() {
            self.tabs.remove(*i);
        }
        if self.tabs.is_empty() {
            return true;
        }
        if removed {
            self.active = active_after_removals(self.active, &exited, self.tabs.len());
        }
        if self.tabs.is_empty() {
            return true;
        }
        if removed {
            // Dropping to 1 tab hides the bar and hands space back.
            self.sync_tab_sizes();
        }
        if got_data {
            // New output invalidates the selected text; alt switches too.
            self.clear_selection();
            if let Some(tab) = self.active_tab_mut() {
                tab.is_alt = tab.terminal.grid().is_alt();
            }
            if let Some(r) = self.renderer.as_mut() {
                // Titles feed the tab bar; a changed title must repaint
                // even when the grid version is unchanged.
                r.invalidate();
            }
            // Synchronized output (`CSI ? 2026 h`): batch intermediate
            // frames; the end marker (`l`) clears `in_sync` so that drain
            // redraws once with the final state.
            let in_sync = self.active_tab().is_some_and(|t| t.terminal.in_sync());
            if !in_sync && let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
        false
    }

    pub(crate) fn sync_scale_factor(&mut self) -> f32 {
        let scale = self
            .window
            .as_ref()
            .map(|w| w.scale_factor() as f32)
            .unwrap_or(1.0);
        if let Some(r) = self.renderer.as_mut() {
            r.set_scale_factor(scale);
        }
        scale
    }

    pub(crate) fn apply_resize(&mut self, width: u32, height: u32, scale: f32) {
        if width == 0 || height == 0 {
            return;
        }
        // Display-sized drawables need the macOS fullscreen stripe
        // workaround; tell the renderer before it configures the surface.
        let fullscreen = self
            .window
            .as_ref()
            .is_some_and(|w| super::is_fullscreen_like(w, width, height));
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        renderer.set_scale_factor(scale);
        renderer.resize(width, height, fullscreen);
        let term_h = term_height_px(height, scale, self.tabs.len(), &self.config.tabbar);
        let (cols, rows) = super::compute_grid_size(
            width,
            term_h,
            renderer.cell_width,
            renderer.line_height,
            &self.config.terminal,
        );
        for tab in &mut self.tabs {
            if cols != tab.terminal.cols() || rows != tab.terminal.rows() {
                log::debug!(
                    "grid resized to {cols}x{rows} (window {width}x{height} @ {scale}x, term {term_h}px)"
                );
                tab.terminal.resize(cols, rows);
                tab.pty.resize(cols, rows);
                tab.selection = None;
                tab.selecting = false;
            }
        }
    }
}
