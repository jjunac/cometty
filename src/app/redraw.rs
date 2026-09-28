//! Redraw path: resize apply, PTY drain, render-param gathering.

use std::time::{Duration, Instant};

use super::App;
use crate::grid;
use crate::renderer;

/// Resolve egui's requested next-frame delay into an absolute deadline.
/// `Duration::MAX` means "no frame pending" (the UI is idle); a zero delay
/// is applied by the caller as an immediate repaint, never stored.
fn egui_deadline(now: Instant, delay: Duration) -> Option<Instant> {
    if delay == Duration::MAX {
        None
    } else {
        now.checked_add(delay)
    }
}

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

        // Link hint underlines for this frame (owned snapshot; `None` when
        // the hint modifier is not held and `underline = "hover"`).
        let link_hints = self.link_hint_view();

        let active = self.active;
        let Some(version) = self.tabs.get(active).map(|t| t.terminal.grid().version) else {
            return;
        };
        // Version-gated snapshot: idle blink frames reuse the last clone
        // instead of re-cloning `rows x cols` Cells every frame. A tab
        // switch changes `active`, so two tabs sharing a version still
        // rebuild (their content differs).
        let snap_before = (self.snapshot_tab, self.snapshot_version);
        if self.snapshot_tab != active || self.snapshot_version != version {
            let rows: Vec<Vec<grid::Cell>> = self.tabs[active]
                .terminal
                .grid()
                .view_rows()
                .into_iter()
                .cloned()
                .collect();
            self.snapshot_rows = rows;
            self.snapshot_version = version;
            self.snapshot_tab = active;
        }
        // Latency probe: only a rebuilt snapshot can carry fresh echo to
        // the screen, so blink-only frames must not consume `last_echo`.
        let rebuilt = (self.snapshot_tab, self.snapshot_version) != snap_before;

        // Small owned fields; the heavy `rows` stay in the cache above and
        // are borrowed for `render` (disjoint from the `&mut` borrows below).
        let (
            cursor,
            effective_cursor,
            cursor_shape,
            total,
            visible,
            offset,
            is_alt,
            selection_view,
        ) = match self.tabs.get(active) {
            Some(tab) => {
                let grid = tab.terminal.grid();
                let cursor = grid.cursor();
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
                (
                    cursor,
                    effective_cursor,
                    style.shape,
                    total,
                    grid.rows(),
                    tab.terminal.scroll_offset(),
                    grid.is_alt(),
                    selection_view,
                )
            }
            None => return,
        };
        // Highlight snapshot for the find bar (owned, so the tab can be
        // borrowed mutably for the overlay below).
        let search_view = self.search_view();
        // Keep the OS chrome in sync with the active tab's OSC title.
        self.sync_window_title();
        // Owned: the overlay borrows titles while also taking
        // `&mut` to the active tab's scrollbar.
        let titles = self.tab_titles();

        // Snapshot for settings change detection: the overlay mutates
        // `self.config` in place during `render`; any diff is live-applied
        // and auto-saved afterwards (both Ok and surface-error paths).
        // Gated on the panel: idle frames skip the full `Config` clone.
        let config_before: Option<crate::config::Config> = if self.settings.open {
            Some(self.config.clone())
        } else {
            None
        };
        // Same idea for the find bar: the query field mutates `tab.search`
        // in place during the frame; a change re-matches on the next redraw.
        // Closed bars can't edit, so skip that `String` clone too.
        let search_query_before: Option<String> = if self.search_open() {
            self.tabs.get(active).map(|t| t.search.query.clone())
        } else {
            None
        };
        // Borrow the cached rows (disjoint from the `&mut` borrows below).
        let (output, config_changed, query_changed) = {
            let (Some(renderer), Some(window)) = (self.renderer.as_mut(), self.window.as_ref())
            else {
                return;
            };
            let Some(tab) = self.tabs.get_mut(active) else {
                return;
            };
            let rows = &self.snapshot_rows;
            let out = match renderer.render(
                rows,
                renderer::CursorCtx {
                    pos: (cursor.x, cursor.y),
                    visible: effective_cursor,
                    shape: cursor_shape,
                },
                version,
                renderer::HighlightCtx {
                    selection: selection_view,
                    search: search_view.as_ref(),
                    links: link_hints.as_ref(),
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
                    if let Some(before) = config_before.as_ref()
                        && self.config != *before
                    {
                        self.apply_settings_changes(before);
                        self.save_config_from_settings();
                    }
                    return;
                }
            };
            // Did the frame mutate live state? Compare against the gated
            // snapshots (None = panel closed = no mutation possible).
            let cfg_changed = config_before
                .as_ref()
                .is_some_and(|before| self.config != *before);
            let q_changed = search_query_before.as_ref().is_some_and(|before| {
                self.tabs
                    .get(active)
                    .is_some_and(|t| t.search.query != *before)
            });
            (out, cfg_changed, q_changed)
        };
        if rebuilt {
            // Latency probe: echo bytes -> painted frame carrying them.
            if let Some(t) = self.last_echo.take() {
                let elapsed = t.elapsed();
                if elapsed <= Duration::from_secs(2) {
                    log::debug!("echo-to-paint {elapsed:?}");
                }
            }
        } else {
            // No new content reached the screen, so a pending echo stamp
            // is spent: this frame already reflects everything drained.
            // Dropping it keeps a later, unrelated frame from attributing
            // a stale echo to itself.
            self.last_echo.take();
        }

        // egui's own next-frame request (hover fades, panel animations,
        // smooth scrolling, tooltips): zero means "right now" — every input
        // event repaints directly, so this only adds egui's settle pass. A
        // finite delay is scheduled for `about_to_wait`; `MAX` means the UI
        // is idle and the loop stays reactive (PTY output / cursor blink).
        let delay = output.egui_repaint_delay;
        if delay.is_zero() {
            self.egui_repaint_at = None;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        } else {
            self.egui_repaint_at = egui_deadline(Instant::now(), delay);
        }

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
        if query_changed {
            if let Some(tab) = self.tabs.get_mut(active) {
                tab.search.query_changed();
            }
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }

        // Link hover feedback follows the grid under the pointer (content
        // may have scrolled or changed since the last mouse move).
        if self.hints_active() {
            self.update_link_cursor();
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
        // `config_before` is `None` when the panel was closed (no clone, no
        // mutation possible); `config_changed` was computed right after the
        // frame while the snapshot was still alive.
        if config_changed && let Some(before) = config_before.as_ref() {
            self.apply_settings_changes(before);
            self.save_config_from_settings();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::egui_deadline;
    use std::time::{Duration, Instant};

    /// egui's `repaint_delay`: `MAX` must not schedule anything (idle stays
    /// reactive) and finite delays become absolute deadlines.
    #[test]
    fn egui_deadline_maps_delays_to_deadlines() {
        let now = Instant::now();
        assert_eq!(egui_deadline(now, Duration::MAX), None);
        assert_eq!(
            egui_deadline(now, Duration::from_millis(50)),
            Some(now + Duration::from_millis(50))
        );
        // Zero is normally handled as an immediate repaint by the caller,
        // but the mapping itself still degenerates to "now".
        assert_eq!(egui_deadline(now, Duration::ZERO), Some(now));
    }
}
