//! Find-bar state + shortcut matcher.
//!
//! [`SearchPanel`] is per-tab view state (like the selection), owned by
//! [`crate::app::tab::Tab`]; the egui bar lives in
//! [`crate::renderer::search_ui`]. Everything here is headless-testable.

use winit::keyboard::{Key, ModifiersState};

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::App;
use super::tab::Tab;
use crate::config::InputConfig;
use crate::grid::Grid;
use crate::search::{self, Match, SearchView};

/// Throttle background rescans while output streams: a full 10k-line scan
/// per burst is `O(scrollback)` per frame; query edits still re-pick
/// immediately.
pub(crate) const RESCAN_DEBOUNCE: Duration = Duration::from_millis(150);

/// Small searches may retain their allocation on close; exceptionally dense
/// queries must not pin tens of MiB for the rest of a tab's lifetime.
const RETAINED_MATCH_BYTES: usize = 1 << 20;

#[derive(Clone, Copy, PartialEq, Eq)]
struct ProjectionKey {
    generation: u64,
    current: Option<usize>,
    base: usize,
    rows: usize,
    cols: usize,
}

pub(crate) fn should_skip_rescan(
    needs_pick: bool,
    query_empty: bool,
    searched_at: Option<Instant>,
    now: Instant,
) -> bool {
    if needs_pick || query_empty {
        return false;
    }
    searched_at.is_some_and(|t| now.duration_since(t) < RESCAN_DEBOUNCE)
}

/// Find-bar state for one tab.
pub struct SearchPanel {
    pub open: bool,
    pub query: String,
    matches: Vec<Match>,
    current: Option<usize>,
    /// Grid content version the matches were computed from
    /// (`u64::MAX` = stale). View-only scrolling does not bump it.
    searched_version: u64,
    /// Ask the overlay to focus the query field on the next frame.
    pub focus_requested: bool,
    /// The next recompute must re-pick `current` (query/open changed).
    needs_pick: bool,
    /// Last full scan time, for [`RESCAN_DEBOUNCE`] throttling.
    searched_at: Option<Instant>,
    matches_generation: u64,
    projection: Option<(ProjectionKey, Arc<SearchView>)>,
    row_ids: Vec<(u64, u64, u64)>,
    searched_dropped: u64,
}

impl Default for SearchPanel {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            matches: Vec::new(),
            current: None,
            searched_version: u64::MAX,
            focus_requested: false,
            needs_pick: false,
            searched_at: None,
            matches_generation: 0,
            projection: None,
            row_ids: Vec::new(),
            searched_dropped: 0,
        }
    }
}

impl SearchPanel {
    /// Search shortcut: open + focus, or just re-focus an open bar (never
    /// closes; `Esc` / the bar's `×` do). Returns true when it newly
    /// opened.
    pub fn focus_or_open(&mut self) -> bool {
        self.focus_requested = true;
        if self.open {
            return false;
        }
        self.open = true;
        self.needs_pick = true;
        self.searched_version = u64::MAX;
        true
    }

    pub fn close(&mut self) {
        self.open = false;
        self.matches.clear();
        if self.matches.capacity() > RETAINED_MATCH_BYTES / size_of::<Match>() {
            self.matches = Vec::new();
        }
        self.projection = None;
        self.row_ids = Vec::new();
        self.current = None;
        self.needs_pick = false;
        self.searched_at = None;
    }

    /// Drop the cached matches (grid content or tab changed).
    pub fn invalidate(&mut self) {
        self.searched_version = u64::MAX;
        self.searched_at = None;
        self.projection = None;
    }

    /// Query was edited in the bar: re-match and re-anchor `current`.
    pub fn query_changed(&mut self) {
        self.needs_pick = true;
        self.invalidate();
    }

    /// Recompute matches when stale. Returns true when `current` was
    /// re-picked; callers reveal it, while a background refresh (new output,
    /// resize) must not scroll the view.
    pub fn refresh(&mut self, grid: &Grid) -> bool {
        let version = grid.content_version;
        if self.searched_version == version {
            return false;
        }
        // Streaming output while the bar is open: throttle full rescans,
        // but never delay a query/open re-pick or an empty query (cheap).
        let now = Instant::now();
        if should_skip_rescan(
            self.needs_pick,
            self.query.is_empty(),
            self.searched_at,
            now,
        ) {
            return false;
        }
        {
            let _timing = crate::perf::measure(crate::perf::Metric::Search);
            self.matches = search::find_matches(grid, &self.query);
            self.row_ids = (0..grid.total_lines())
                .map(|line| grid.global_line(line).unwrap().identity())
                .collect();
            self.searched_dropped = grid.history_dropped();
        }
        self.matches_generation = self.matches_generation.wrapping_add(1);
        self.projection = None;
        let picked = if self.matches.is_empty() {
            self.current = None;
            self.needs_pick = false;
            false
        } else if self.needs_pick || self.current.is_none() {
            let reveal = self.needs_pick;
            self.needs_pick = false;
            self.current =
                search::pick_initial(&self.matches, grid.scrollback_len(), grid.scroll_offset());
            reveal
        } else {
            // Live output shifts the list; keep the index, stay in range.
            self.current = self.current.map(|i| i.min(self.matches.len() - 1));
            false
        };
        self.searched_version = version;
        self.searched_at = Some(now);
        picked
    }

    /// Move to the next/previous match (wrap-around). Returns true when
    /// the current match changed.
    pub fn step(&mut self, forward: bool) -> bool {
        let next = search::step(self.current, self.matches.len(), forward);
        if next == self.current {
            return false;
        }
        self.current = next;
        true
    }

    pub fn current_match(&self) -> Option<Match> {
        self.current.and_then(|i| self.matches.get(i)).copied()
    }

    fn current_match_in(&self, grid: &Grid) -> Option<Match> {
        let mut hit = self.current_match()?;
        let Some(&(id, _, _)) = self.row_ids.get(hit.line) else {
            return Some(hit);
        };
        let delta = grid.history_dropped().saturating_sub(self.searched_dropped) as usize;
        hit.line = hit.line.checked_sub(delta)?;
        // Reveal follows row identity even if another cell on the line
        // changed: scroll the right line, then let the debounced rescan
        // refresh the stale columns.
        (grid.global_line(hit.line)?.identity().0 == id).then_some(hit)
    }

    /// 1-based `(current, total)` for the counter; `(0, 0)` without matches.
    pub fn counter(&self) -> (usize, usize) {
        (self.current.map(|i| i + 1).unwrap_or(0), self.matches.len())
    }

    /// Explicit trailing refresh deadline: stopping cursor blinks must not
    /// strand a final throttled scan until some unrelated input arrives.
    pub(crate) fn rescan_at(&self, grid: &Grid, now: Instant) -> Option<Instant> {
        if !self.open || self.searched_version == grid.content_version {
            return None;
        }
        if self.needs_pick || self.query.is_empty() {
            return Some(now);
        }
        Some(
            self.searched_at
                .and_then(|at| at.checked_add(RESCAN_DEBOUNCE))
                .unwrap_or(now),
        )
    }

    /// Cheap owned handle lets the renderer borrow this projection while
    /// the overlay mutates the same tab's find-bar state. No per-frame copy.
    pub fn view(&mut self, grid: &Grid) -> Option<Arc<SearchView>> {
        if !self.open || self.matches.is_empty() {
            return None;
        }
        let key = ProjectionKey {
            generation: self.matches_generation,
            current: self.current,
            base: grid.scrollback_len().saturating_sub(grid.scroll_offset()),
            rows: grid.rows(),
            cols: grid.cols(),
        };
        if self
            .projection
            .as_ref()
            .is_none_or(|(cached, _)| *cached != key)
        {
            let _timing = crate::perf::measure(crate::perf::Metric::Projection);
            let delta = grid.history_dropped().saturating_sub(self.searched_dropped) as usize;
            let mut view = SearchView::build(
                &self.matches,
                self.current,
                grid.scrollback_len().saturating_add(delta),
                grid.scroll_offset(),
                grid.rows(),
            );
            // A hand-built panel (tests/future imports) has no identity map;
            // only a complete map may suppress hits after eviction.
            if self.row_ids.len() == grid.total_lines().saturating_add(delta) {
                for (row, ranges) in view.rows.iter_mut().enumerate() {
                    let line = key.base + row;
                    let expected = self.row_ids.get(line.saturating_add(delta)).copied();
                    let actual = grid.global_line(line).map(|r| r.identity());
                    if actual != expected {
                        ranges.clear();
                        if view.active.is_some_and(|(y, _, _)| y == row) {
                            view.active = None;
                        }
                    }
                }
            }
            self.projection = Some((key, Arc::new(view)));
        }
        self.projection.as_ref().map(|(_, view)| Arc::clone(view))
    }
}

/// The configured search shortcut (`Cmd+F` on macOS, `Ctrl+F` elsewhere
/// by default) opens (or re-focuses) the find bar, gated by
/// [`InputConfig`]. Shift/Alt stay out so `Ctrl+Shift+F` and the shell's
/// `Alt+F` keep their bindings; exactly one of Ctrl/Super is required.
pub fn is_search_toggle(
    logical_key: &Key,
    modifiers: &ModifiersState,
    config: &InputConfig,
) -> bool {
    let Key::Character(s) = logical_key else {
        return false;
    };
    if !s.eq_ignore_ascii_case(&config.search_key) {
        return false;
    }
    if modifiers.shift_key() || modifiers.alt_key() {
        return false;
    }
    let ctrl = config.search_ctrl && modifiers.control_key() && !modifiers.super_key();
    let sup = config.search_super && modifiers.super_key() && !modifiers.control_key();
    ctrl != sup
}

impl App {
    pub(crate) fn search_open(&self) -> bool {
        self.active_tab().is_some_and(|t| t.search.open)
    }

    /// True between opening/re-focusing the bar and the frame that hands
    /// keyboard focus to the query field, so the first keystrokes can't
    /// slip past egui to the shell.
    pub(crate) fn search_focus_pending(&self) -> bool {
        self.active_tab()
            .is_some_and(|t| t.search.open && t.search.focus_requested)
    }

    /// Find-bar shortcut: open the find bar (or re-focus it) and repaint.
    pub(crate) fn focus_search(&mut self) {
        if let Some(tab) = self.active_tab_mut()
            && tab.search.focus_or_open()
        {
            log::debug!("search opened");
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    pub(crate) fn close_search(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.search.close();
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Next/previous match, scrolling it on screen.
    pub(crate) fn step_search(&mut self, forward: bool) {
        {
            let Some(tab) = self.active_tab_mut() else {
                return;
            };
            if !tab.search.step(forward) {
                return;
            }
            reveal_search_match(tab);
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Recompute the active tab's matches when stale; a re-picked match is
    /// scrolled into view. Called before the frame snapshot, so no extra
    /// repaint is scheduled.
    pub(crate) fn refresh_search(&mut self) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        if !tab.search.open {
            return;
        }
        let grid = tab.terminal.grid();
        if tab.search.refresh(grid) {
            reveal_search_match(tab);
        }
    }

    /// The active tab's highlight snapshot for the renderer.
    pub(crate) fn search_view(&mut self) -> Option<Arc<SearchView>> {
        let tab = self.active_tab_mut()?;
        tab.search.view(tab.terminal.grid())
    }

    /// True when physical `(x, y)` is over the painted find bar, so presses
    /// there are chrome rather than terminal selection.
    pub(crate) fn press_on_search_bar(&self, x_phys: f32, y_phys: f32) -> bool {
        self.search_open()
            && self
                .renderer
                .as_ref()
                .is_some_and(|r| r.over_search_bar(x_phys, y_phys))
    }
}

/// Scroll the current match on screen with the least movement.
fn reveal_search_match(tab: &mut Tab) -> bool {
    let grid = tab.terminal.grid();
    let Some(m) = tab.search.current_match_in(grid) else {
        return false;
    };
    let Some(offset) = search::reveal_offset(
        m.line,
        grid.rows(),
        grid.scrollback_len(),
        grid.scroll_offset(),
    ) else {
        return false;
    };
    tab.terminal.scroll_to_offset(offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use winit::keyboard::NamedKey;

    #[test]
    fn shortcut_matches_ctrl_and_super_only() {
        // The default may be Cmd-only on macOS; the matcher itself supports
        // both, so test with both explicitly enabled.
        let cfg = InputConfig {
            search_ctrl: true,
            ..InputConfig::default()
        };
        let f: Key = Key::Character("f".into());
        let upper: Key = Key::Character("F".into());
        assert!(is_search_toggle(&f, &ModifiersState::CONTROL, &cfg));
        assert!(is_search_toggle(&f, &ModifiersState::SUPER, &cfg));
        assert!(is_search_toggle(&upper, &ModifiersState::CONTROL, &cfg));
        assert!(!is_search_toggle(&f, &ModifiersState::empty(), &cfg));
        assert!(!is_search_toggle(
            &f,
            &(ModifiersState::CONTROL | ModifiersState::SHIFT),
            &cfg
        ));
        assert!(!is_search_toggle(
            &f,
            &(ModifiersState::CONTROL | ModifiersState::ALT),
            &cfg
        ));
        assert!(!is_search_toggle(
            &f,
            &(ModifiersState::CONTROL | ModifiersState::SUPER),
            &cfg
        ));
        assert!(!is_search_toggle(
            &Key::Named(NamedKey::Escape),
            &ModifiersState::CONTROL,
            &cfg
        ));
        // Config gates each modifier and the key itself.
        let mut off = cfg.clone();
        off.search_ctrl = false;
        assert!(!is_search_toggle(&f, &ModifiersState::CONTROL, &off));
        assert!(is_search_toggle(&f, &ModifiersState::SUPER, &off));
        let mut custom = cfg.clone();
        custom.search_key = "s".to_string();
        let s: Key = Key::Character("s".into());
        assert!(is_search_toggle(&s, &ModifiersState::CONTROL, &custom));
        assert!(!is_search_toggle(&f, &ModifiersState::CONTROL, &custom));
    }

    #[test]
    fn focus_or_open_never_closes() {
        let mut panel = SearchPanel::default();
        assert!(panel.focus_or_open());
        assert!(panel.open && panel.focus_requested);
        // A second press only re-focuses.
        panel.focus_requested = false;
        assert!(!panel.focus_or_open());
        assert!(panel.focus_requested && panel.open);
        panel.close();
        assert!(!panel.open && panel.matches.is_empty() && panel.current.is_none());
    }

    #[test]
    fn refresh_picks_the_first_visible_match_and_navigates() {
        let mut g = Grid::new(20, 3, Theme::default());
        for (i, line) in ["needle a", "other", "needle b"].iter().enumerate() {
            if i > 0 {
                g.newline();
            }
            for ch in line.chars() {
                g.put_char(ch);
            }
        }
        let mut panel = SearchPanel::default();
        panel.focus_or_open();
        panel.query = "needle".to_string();
        panel.query_changed();
        assert!(
            panel.refresh(&g),
            "a query change re-picks the current match"
        );
        assert_eq!(panel.matches.len(), 2);
        assert_eq!(panel.current, Some(0));
        assert_eq!(panel.counter(), (1, 2));

        let view = panel.view(&g).unwrap();
        assert_eq!(view.rows[0], vec![(0, 5)]);
        assert_eq!(view.active, Some((0, 0, 5)));

        // Next wraps around both ways.
        assert!(panel.step(true));
        assert_eq!(panel.counter(), (2, 2));
        assert!(panel.step(true));
        assert_eq!(panel.counter(), (1, 2));
        assert!(panel.step(false));
        assert_eq!(panel.counter(), (2, 2));

        // Same grid version: no recompute, no re-pick.
        assert!(!panel.refresh(&g));
        // New output (version bump) keeps the index but clamps to range.
        // Background rescans are debounced: force the window open so the
        // test is deterministic regardless of wall-clock timing.
        g.newline();
        panel.searched_at = None;
        assert!(!panel.refresh(&g));
        assert_eq!(panel.current, Some(1));
        // An empty result drops the current match.
        panel.query = "nope".to_string();
        panel.query_changed();
        assert!(!panel.refresh(&g));
        assert_eq!(panel.current, None);
        assert_eq!(panel.counter(), (0, 0));
        assert!(panel.view(&g).is_none());
        assert!(!panel.step(true));
    }

    #[test]
    fn rescan_debounce_skips_background_but_not_query() {
        use std::time::Instant;
        let now = Instant::now();
        // Query edits and empty queries never throttle (cheap / interactive).
        assert!(!should_skip_rescan(true, false, Some(now), now));
        assert!(!should_skip_rescan(false, true, Some(now), now));
        assert!(!should_skip_rescan(false, false, None, now));
        // Background rescan within the window skips.
        assert!(should_skip_rescan(false, false, Some(now), now));
        // Outside the window it rescans.
        let old = now - RESCAN_DEBOUNCE - std::time::Duration::from_millis(1);
        assert!(!should_skip_rescan(false, false, Some(old), now));
    }

    #[test]
    fn projection_cache_tracks_results_current_viewport_and_dimensions() {
        let mut g = Grid::new(5, 2, Theme::default());
        for _ in 0..5 {
            g.put_char('a');
            g.newline();
        }
        let mut panel = SearchPanel::default();
        panel.focus_or_open();
        panel.query = "a".into();
        panel.query_changed();
        panel.refresh(&g);
        let first = panel.view(&g).unwrap();
        assert!(Arc::ptr_eq(&first, &panel.view(&g).unwrap()));
        assert_eq!(
            crate::perf_bench::allocation_counts(|| {
                panel.view(&g);
            }),
            (0, 0, 0)
        );
        panel.step(true);
        let stepped = panel.view(&g).unwrap();
        assert!(!Arc::ptr_eq(&first, &stepped));
        g.scroll_by(1);
        let scrolled = panel.view(&g).unwrap();
        assert!(!Arc::ptr_eq(&stepped, &scrolled));
        g.resize(6, 2);
        let resized = panel.view(&g).unwrap();
        assert!(!Arc::ptr_eq(&scrolled, &resized));
        panel.searched_at = None;
        panel.refresh(&g);
        assert!(!Arc::ptr_eq(&resized, &panel.view(&g).unwrap()));
        panel.close();
        assert!(panel.projection.is_none());
        assert!(panel.view(&g).is_none());
    }

    #[test]
    fn dense_query_memory_is_visible_only_and_large_capacity_is_released_on_close() {
        let g = Grid::new(200, 60, Theme::default());
        let matches = (0..10_059)
            .flat_map(|line| {
                (0..199).map(move |col| Match {
                    line,
                    start: col,
                    end: col,
                })
            })
            .collect();
        let mut panel = SearchPanel {
            open: true,
            matches,
            ..Default::default()
        };
        assert_eq!(panel.matches.len(), 2_001_741);
        assert!(panel.matches.capacity() * size_of::<Match>() >= 48_000_000);
        let view = panel.view(&g).unwrap();
        assert_eq!(view.rows.iter().map(Vec::len).sum::<usize>(), 60 * 199);
        assert!(
            view.rows
                .iter()
                .map(|r| r.capacity() * size_of::<(usize, usize)>())
                .sum::<usize>()
                < 256 * 1024
        );
        panel.close();
        assert_eq!(panel.matches.capacity(), 0);
        assert!(panel.projection.is_none());
    }

    #[test]
    fn stale_search_has_its_own_deadline_without_cursor_blinks() {
        let mut g = Grid::new(10, 2, Theme::default());
        let mut panel = SearchPanel::default();
        panel.focus_or_open();
        panel.query = "a".into();
        panel.refresh(&g);
        let at = panel.searched_at.unwrap();
        assert!(panel.rescan_at(&g, at).is_none());
        g.put_char('a');
        assert_eq!(panel.rescan_at(&g, at), Some(at + RESCAN_DEBOUNCE));
        panel.query_changed();
        assert_eq!(panel.rescan_at(&g, at), Some(at));
        panel.close();
        assert!(panel.rescan_at(&g, at).is_none());
    }

    #[test]
    fn first_background_hit_does_not_request_reveal() {
        let mut g = Grid::new(10, 2, Theme::default());
        let mut panel = SearchPanel::default();
        panel.focus_or_open();
        panel.query = "a".into();
        assert!(!panel.refresh(&g));
        g.put_char('a');
        panel.searched_at = None;
        assert!(!panel.refresh(&g), "background refresh must never scroll");
        assert_eq!(panel.counter(), (1, 1));
    }

    #[test]
    fn evicted_history_shifts_cached_matches_by_surviving_row_identity() {
        let mut cfg = crate::config::Config::default();
        cfg.terminal.scrollback_lines = 4;
        let mut g = Grid::new_with_config(5, 2, Theme::default(), &cfg);
        for line in ["aaaa", "bbbb", "cccc", "dddd"] {
            for ch in line.chars() {
                g.put_char(ch);
            }
            g.newline();
        }
        for ch in "neee".chars() {
            g.put_char(ch);
        }
        let mut panel = SearchPanel::default();
        panel.focus_or_open();
        panel.query = "neee".into();
        panel.refresh(&g);
        assert_eq!(panel.current_match().unwrap().line, 4);
        assert_eq!(panel.view(&g).unwrap().rows[1], vec![(0, 3)]);

        // Growing history below its cap keeps global IDs stable.
        g.newline();
        assert_eq!(panel.view(&g).unwrap().rows[0], vec![(0, 3)]);
        // At the cap one row is evicted: every surviving line shifts down.
        g.newline();
        assert_eq!(g.history_dropped(), 1);
        assert!(g.scroll_to_offset(1));
        let view = panel.view(&g).unwrap();
        assert_eq!(view.rows[0], vec![(0, 3)], "same row, shifted global line");
        assert!(view.rows[1].is_empty(), "recycled rows inherit no old hits");
        assert_eq!(panel.current_match_in(&g).unwrap().line, 3);
    }
}
