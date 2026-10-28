//! PTY drain + resize plumbing.

use std::time::{Duration, Instant};

use super::App;
use crate::app::tab::{active_after_removals, term_height_px};
use crate::pty::PtyEvent;

use super::scheduling::MAX_DRAIN_TIME;

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
        self.drain_pty_inner(true)
    }

    pub(crate) fn drain_pty_before_frame(&mut self) -> bool {
        self.drain_pty_inner(false)
    }

    fn drain_pty_inner(&mut self, request_frame: bool) -> bool {
        let mut notice_changed = false;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if let Some(notice) = tab.pty.take_input_notice() {
                if tab.input_notice.as_ref() != Some(&notice) {
                    log::warn!("tab {}: {notice}", tab.id);
                }
                tab.input_notice = Some(notice);
                notice_changed |= i == self.active;
            }
        }
        if notice_changed
            && request_frame
            && !self.visuals_suspended()
            && let Some(w) = self.window.as_ref()
        {
            w.request_redraw();
        }
        self.pty_pending = self.tabs.iter().any(|tab| {
            !tab.pty.reply_blocked() && (tab.pty.output_pending() || tab.pending_reply.is_some())
        });
        if !self.pty_pending || self.pty_schedule.exhausted(Duration::ZERO) {
            return self.tabs.is_empty();
        }
        let _timing = crate::perf::measure(crate::perf::Metric::Drain);
        let mut exited: Vec<usize> = Vec::new();
        // OSC 52 requests leave the terminal (which owns no clipboard), but
        // the app-wide arboard handle is borrowed mutably: collect first,
        // apply after the tab loop.
        let mut clipboard_ops: Vec<(usize, crate::term::ClipboardOp)> = Vec::new();
        let start = Instant::now();
        let mut drained_bytes: usize = 0;
        let mut titles_before = vec![None; self.tabs.len()];
        let mut empty_visits = 0;
        // One chunk per visit is the per-tab quantum. The persistent order
        // also survives an expensive callback that overshoots the deadline.
        while !self.pty_schedule.exhausted(start.elapsed()) {
            let Some(i) = self.pty_schedule.next_tab(self.tabs.len(), self.active) else {
                break;
            };
            let tab = &mut self.tabs[i];
            if !tab.pty.reply_blocked()
                && let Some(reply) = tab.pending_reply.take()
            {
                tab.pending_reply = tab.pty.reply(reply);
            }
            let event = if exited.contains(&i) || tab.pty.reply_blocked() {
                None
            } else {
                tab.pty.try_recv()
            };
            match event {
                Some(PtyEvent::Data(bytes)) => {
                    if titles_before[i].is_none() {
                        titles_before[i] = Some(self.title_for_tab(i, &self.tabs[i]));
                    }
                    empty_visits = 0;
                    let tab = &mut self.tabs[i];
                    let now = Instant::now();
                    if crate::perf::enabled()
                        && let Some(last) = tab.last_service.replace(now)
                    {
                        crate::perf::tab_duration(
                            tab.id,
                            crate::perf::TabMetric::ServiceGap,
                            now.saturating_duration_since(last),
                        );
                    }
                    if let Some(input) = tab.last_key_write.take() {
                        let elapsed = now.saturating_duration_since(input);
                        if elapsed <= Duration::from_secs(2) {
                            crate::perf::tab_duration(
                                tab.id,
                                crate::perf::TabMetric::InputOutput,
                                elapsed,
                            );
                            log::debug!("input-to-first-output tab={} {elapsed:?}", tab.id);
                        }
                    }
                    drained_bytes = drained_bytes.saturating_add(bytes.len());
                    self.pty_schedule.add_bytes(bytes.len());
                    let before = tab.terminal.grid().version;
                    let evicted_before = tab.terminal.grid().history_dropped();
                    {
                        let _timing = crate::perf::measure(crate::perf::Metric::Parse);
                        tab.terminal.feed(&bytes);
                    }
                    let evicted = tab
                        .terminal
                        .grid()
                        .history_dropped()
                        .saturating_sub(evicted_before);
                    tab.rebase_selection(evicted as usize);
                    crate::perf::add(crate::perf::Metric::ParsedBytes, bytes.len());
                    // Query replies (DA / CPR / DSR / DECRQM) flow back.
                    let reply = tab.terminal.take_response();
                    if !reply.is_empty() {
                        tab.pending_reply = tab.pty.reply(reply);
                    }
                    clipboard_ops
                        .extend(tab.terminal.take_clipboard().into_iter().map(|op| (i, op)));
                    if before != tab.terminal.grid().version {
                        tab.visual_dirty = true;
                        // Keep the oldest unpresented output stamp, not
                        // the most recent chunk in a continuous flood.
                        if i == self.active {
                            tab.last_output.get_or_insert(now);
                        }
                    }
                    tab.sync.update(tab.terminal.in_sync(), now);
                }
                Some(PtyEvent::Exit) => {
                    log::info!("shell exited (tab {i})");
                    exited.push(i);
                    empty_visits += 1;
                }
                None => {
                    self.tabs[i].last_service = None;
                    empty_visits += 1;
                }
            }
            // A weighted round visits every queue within at most 2N slots.
            if empty_visits >= self.tabs.len().saturating_mul(2) {
                break;
            }
        }
        let mut chrome_changed = false;
        let mut cwd_changed = false;
        for (i, before) in titles_before.iter().enumerate() {
            if let Some(before) = before {
                // Metadata queries stay off the render path and run once
                // per touched tab, not once per scheduling quantum.
                let tab = &mut self.tabs[i];
                cwd_changed |= tab.refresh_title();
                if tab.visual_dirty && !tab.selecting {
                    tab.selection = None;
                    tab.select_mode = crate::selection::SelectMode::Char;
                    tab.select_anchor = None;
                }
                chrome_changed |= *before != self.title_for_tab(i, &self.tabs[i]);
            }
        }
        let active_output = titles_before.get(self.active).is_some_and(Option::is_some);
        if active_output {
            self.sync_window_title();
        }
        // A background tab cannot consume the active tab's selection.
        if active_output
            && self.should_clear_selection()
            && self.active_tab().is_some_and(|t| t.visual_dirty)
        {
            self.clear_selection();
            if let Some(tab) = self.active_tab_mut() {
                tab.is_alt = tab.terminal.grid().is_alt();
            }
        }
        // Apply OSC 52 requests while tab indices still match (a tab
        // removed below could shift another tab's reply target).
        if !clipboard_ops.is_empty() {
            self.handle_clipboard_ops(clipboard_ops);
        }
        // Remove exited tabs from the back so indices stay valid, then
        // remap the active index once (tabs before it may have vanished).
        exited.sort_unstable();
        exited.dedup();
        let removed = !exited.is_empty();
        for i in exited.iter().rev() {
            self.tabs.remove(*i);
        }
        if removed || cwd_changed {
            self.session_changed();
        }
        if self.tabs.is_empty() {
            return true;
        }
        if removed {
            self.active = active_after_removals(self.active, &exited, self.tabs.len());
            self.pty_schedule.removed(&exited, self.tabs.len());
            if let Some(renderer) = self.renderer.as_mut() {
                renderer.invalidate();
            }
            // Dropping to 1 tab hides the bar and hands space back.
            self.sync_tab_sizes();
        }
        self.pty_pending = self.tabs.iter().any(|tab| {
            !tab.pty.reply_blocked() && (tab.pty.output_pending() || tab.pending_reply.is_some())
        });
        let drain_elapsed = start.elapsed();
        self.pty_schedule.finish_call(drain_elapsed);
        if self.pty_pending && self.pty_schedule.exhausted(Duration::ZERO) {
            crate::perf::add(crate::perf::Metric::DrainYield, 1);
        }
        if drained_bytes > 0
            && drain_elapsed >= MAX_DRAIN_TIME
            && self
                .last_slow_drain
                .is_none_or(|last| last.elapsed() >= Duration::from_secs(2))
        {
            self.last_slow_drain = Some(Instant::now());
            log::debug!("slow pty drain {drain_elapsed:?} ({drained_bytes}B)");
        }
        // Parsing continuation is scheduled separately by about_to_wait;
        // queued background bytes are not a reason to present the active view.
        let visible_changed = active_output
            && self.active_tab().is_some_and(|tab| {
                tab.visual_dirty && !tab.selecting && !tab.sync.blocked(Instant::now())
            });
        if request_frame && !self.visuals_suspended() {
            let output_due = visible_changed && self.output_presentation.dirty(Instant::now());
            if (output_due || chrome_changed || removed)
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
    use crate::app::scheduling::{MAX_DRAIN_BYTES, drain_budget_exceeded};

    fn app() -> App {
        let mut config = crate::config::Config::default();
        config.clipboard.osc52 = "off".into();
        App::new(
            None,
            crate::theme::Theme::default(),
            config,
            crate::app::AppStartup {
                config_path_override: None,
                cli_theme: None,
                config_corrupt: false,
            },
            std::sync::Arc::new(std::sync::Mutex::new(crate::logbuf::LogBuffer::new(1000))),
        )
    }

    fn add_tab(app: &mut App) -> (impl Fn(PtyEvent) + use<>, crate::pty::input::InputReceiver) {
        let (pty, output, input) = crate::pty::test_support::session();
        let terminal = crate::term::Terminal::new(20, 2, crate::theme::Theme::default());
        let mut tab = crate::app::Tab::new(app.next_tab_id, terminal, pty);
        tab.visual_dirty = false; // model an already presented frame
        app.next_tab_id += 1;
        app.tabs.push(tab);
        (output, input)
    }

    fn finish_draining(app: &mut App) {
        for _ in 0..128 {
            app.pty_schedule.begin_turn();
            if app.drain_pty() || !app.pty_pending {
                return;
            }
        }
        panic!("stranded PTY continuation");
    }

    #[test]
    fn background_output_does_not_dirty_or_clear_the_active_view_or_steal_input_stamp() {
        let mut app = app();
        let (background, _) = add_tab(&mut app);
        let (_foreground, _) = add_tab(&mut app);
        app.active = 1;
        let selection = crate::selection::Selection::new(crate::selection::CellPos { x: 0, y: 0 });
        app.tabs[1].selection = Some(selection);
        app.tabs[1].last_key_write = Some(Instant::now());
        background(PtyEvent::Data(b"background output".to_vec()));
        finish_draining(&mut app);
        assert!(app.tabs[0].visual_dirty);
        assert!(!app.tabs[1].visual_dirty);
        assert_eq!(app.tabs[1].selection, Some(selection));
        assert!(app.tabs[1].last_key_write.is_some());
        assert!(app.tabs[1].last_output.is_none());
    }

    #[test]
    fn budget_continuation_and_replies_work_while_occluded_dragging_and_synchronized() {
        let mut app = app();
        let (output, input) = add_tab(&mut app);
        app.occluded = true;
        app.tabs[0].selecting = true;
        output(PtyEvent::Data(b"\x1b[?2026hhel".to_vec()));
        output(PtyEvent::Data(b"lo\x1b[6n".to_vec()));
        app.pty_schedule.add_bytes(MAX_DRAIN_BYTES);
        assert!(!app.drain_pty());
        assert!(app.pty_pending);
        assert!(input.try_recv().is_err());
        finish_draining(&mut app);
        assert_eq!(input.try_recv().unwrap(), b"\x1b[1;6R");
        assert!(app.tabs[0].selecting);
        assert!(app.tabs[0].sync.deadline().is_some());
        assert_eq!(app.tabs[0].terminal.grid().cell(0, 0).unwrap().ch, 'h');
        output(PtyEvent::Data(b"\x1b[?2026l".to_vec()));
        finish_draining(&mut app);
        assert!(app.tabs[0].sync.deadline().is_none());
        assert!(app.tabs[0].visual_dirty);
    }

    #[test]
    fn out_of_order_exits_keep_reply_targets_and_surviving_tab_identity() {
        let mut app = app();
        let (first, _) = add_tab(&mut app);
        let (second, _) = add_tab(&mut app);
        let (third, replies) = add_tab(&mut app);
        let (_survivor, survivor_input) = add_tab(&mut app);
        app.active = 2;
        app.snapshot_tab = Some(app.tabs[2].id);
        // Keep the reply target alive while earlier tabs exit and shift its
        // index. Once it too exits, shutdown deliberately cancels unsent input.
        third(PtyEvent::Data(b"\x1b[6n\x1b]52;c;?\x07".to_vec()));
        first(PtyEvent::Exit);
        second(PtyEvent::Exit);
        finish_draining(&mut app);
        assert_eq!(replies.try_recv().unwrap(), b"\x1b[1;1R");
        assert!(replies.try_recv().is_err()); // clipboard policy was preserved
        assert!(survivor_input.try_recv().is_err());
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.tabs[0].id, 2);
        assert_eq!(app.active, 0);
        third(PtyEvent::Exit);
        finish_draining(&mut app);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].id, 3);
        assert_eq!(app.active, 0);
        assert_ne!(app.snapshot_tab, Some(app.tabs[0].id));
    }

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
