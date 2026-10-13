//! PTY drain + resize plumbing.

use std::time::{Duration, Instant};

use super::App;
use crate::app::tab::{active_after_removals, term_height_px};
use crate::pty::PtyEvent;

/// Per-frame drain budget: a burst (`cat` a huge file, paste flood) must
/// not block the UI thread. Excess stays queued and continues next frame.
pub(crate) const MAX_DRAIN_BYTES: usize = 256 * 1024;
pub(crate) const MAX_DRAIN_TIME: Duration = Duration::from_millis(4);

pub(crate) fn drain_budget_exceeded(bytes: usize, elapsed: Duration) -> bool {
    bytes >= MAX_DRAIN_BYTES || elapsed >= MAX_DRAIN_TIME
}

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
        // OSC 52 requests leave the terminal (which owns no clipboard), but
        // the app-wide arboard handle is borrowed mutably: collect first,
        // apply after the tab loop.
        let mut clipboard_ops: Vec<(usize, crate::term::ClipboardOp)> = Vec::new();
        let start = Instant::now();
        let mut drained_bytes: usize = 0;
        let mut budget_hit = false;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            let mut got_bytes = false;
            loop {
                match tab.pty.try_recv() {
                    Some(PtyEvent::Data(bytes)) => {
                        drained_bytes = drained_bytes.saturating_add(bytes.len());
                        tab.terminal.feed(&bytes);
                        // Query replies (DA / CPR / DSR / DECRQM) flow back.
                        let reply = tab.terminal.take_response();
                        if !reply.is_empty() {
                            tab.pty.write(reply);
                        }
                        clipboard_ops
                            .extend(tab.terminal.take_clipboard().into_iter().map(|op| (i, op)));
                        got_data = true;
                        got_bytes = true;
                        if drain_budget_exceeded(drained_bytes, start.elapsed()) {
                            budget_hit = true;
                            break;
                        }
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
            if budget_hit {
                break;
            }
        }
        // Apply OSC 52 requests while tab indices still match (a tab
        // removed below could shift another tab's reply target).
        if !clipboard_ops.is_empty() {
            self.handle_clipboard_ops(clipboard_ops);
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
        if removed {
            // Dropping to 1 tab hides the bar and hands space back.
            self.sync_tab_sizes();
        }
        if got_data {
            // Latency probe: keystroke -> first echo bytes back on the UI
            // thread. Only attributed when recent; older stamps are stale
            // background output, dropped silently.
            if let Some(t) = self.last_key_write.take() {
                let elapsed = t.elapsed();
                if elapsed <= Duration::from_secs(2) {
                    log::debug!("key-to-echo {elapsed:?} ({drained_bytes}B drained)");
                }
            }
            self.last_echo = Some(Instant::now());
            let drain_elapsed = start.elapsed();
            if drain_elapsed >= Duration::from_millis(4) {
                log::debug!("slow pty drain {drain_elapsed:?} ({drained_bytes}B)");
            }
        }
        if got_data && self.should_clear_selection() {
            // New output invalidates the selected text; alt switches too.
            // Skipped mid-drag (see `should_clear_selection`).
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
            // redraws once with the final state. A budget hit always
            // re-requests: queued bytes remain and need another drain.
            let in_sync = self.active_tab().is_some_and(|t| t.terminal.in_sync());
            if (budget_hit || !in_sync)
                && let Some(w) = self.window.as_ref()
            {
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
                tab.select_mode = crate::selection::SelectMode::Char;
                tab.select_anchor = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_budget_trips_on_bytes_or_time() {
        assert!(!drain_budget_exceeded(0, Duration::ZERO));
        assert!(!drain_budget_exceeded(MAX_DRAIN_BYTES - 1, Duration::ZERO));
        assert!(drain_budget_exceeded(MAX_DRAIN_BYTES, Duration::ZERO));
        assert!(drain_budget_exceeded(MAX_DRAIN_BYTES + 1, Duration::ZERO));
        assert!(drain_budget_exceeded(0, MAX_DRAIN_TIME));
        assert!(drain_budget_exceeded(
            0,
            MAX_DRAIN_TIME + Duration::from_millis(1)
        ));
    }

    #[test]
    fn output_clears_selection_only_outside_drag() {
        // The `drain_pty` guard: a completed (or absent) selection clears
        // on output, a mid-drag one survives.
        assert!(super::super::selection_view::should_clear_selection_for(
            false
        ));
        assert!(!super::super::selection_view::should_clear_selection_for(
            true
        ));
    }
}
