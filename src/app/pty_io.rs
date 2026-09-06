//! PTY drain + resize plumbing.

use super::App;
use crate::pty::PtyEvent;

impl App {
    pub(crate) fn scroll_terminal(&mut self, delta: isize) {
        if delta == 0 {
            return;
        }
        if let Some(t) = self.terminal.as_mut()
            && t.scroll_by(delta)
            && let Some(w) = self.window.as_ref()
        {
            w.request_redraw();
        }
    }

    pub(crate) fn drain_pty(&mut self) {
        let mut got_data = false;
        loop {
            let ev = self.pty.as_ref().and_then(|p| p.try_recv());
            match ev {
                Some(PtyEvent::Data(bytes)) => {
                    if let Some(t) = self.terminal.as_mut() {
                        t.feed(&bytes);
                    }
                    got_data = true;
                }
                Some(PtyEvent::Exit) => {
                    log::info!("shell exited");
                    got_data = true;
                    break;
                }
                None => break,
            }
        }
        if got_data {
            // New output invalidates the selected text; alt switches too.
            self.clear_selection();
            if let Some(t) = self.terminal.as_ref() {
                self.is_alt = t.grid().is_alt();
            }
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
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
        let (renderer, term, pty) = match (
            self.renderer.as_mut(),
            self.terminal.as_mut(),
            self.pty.as_ref(),
        ) {
            (Some(r), Some(t), Some(p)) => (r, t, p),
            _ => return,
        };
        renderer.set_scale_factor(scale);
        renderer.resize(width, height);
        let (cols, rows) =
            super::compute_grid_size(width, height, renderer.cell_width, renderer.line_height);
        if cols != term.cols() || rows != term.rows() {
            term.resize(cols, rows);
            pty.resize(cols, rows);
            self.clear_selection();
        }
    }
}
