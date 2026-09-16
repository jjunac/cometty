//! Find-bar state + shortcut matcher.
//!
//! [`SearchPanel`] is per-tab view state (like the selection), owned by
//! [`crate::app::tab::Tab`]; the egui bar lives in
//! [`crate::renderer::search_ui`]. Everything here is headless-testable.

use winit::keyboard::{Key, ModifiersState};

use super::App;
use super::tab::Tab;
use crate::config::InputConfig;
use crate::grid::Grid;
use crate::search::{self, Match, SearchView};

/// Find-bar state for one tab.
pub struct SearchPanel {
    pub open: bool,
    pub query: String,
    pub matches: Vec<Match>,
    pub current: Option<usize>,
    /// Grid content version the matches were computed from
    /// (`u64::MAX` = stale). View-only scrolling does not bump it.
    searched_version: u64,
    /// Ask the overlay to focus the query field on the next frame.
    pub focus_requested: bool,
    /// The next recompute must re-pick `current` (query/open changed).
    needs_pick: bool,
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
        }
    }
}

impl SearchPanel {
    /// Ctrl/Cmd+F: open + focus, or just re-focus an open bar (never
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
        self.current = None;
        self.needs_pick = false;
    }

    /// Drop the cached matches (grid content or tab changed).
    pub fn invalidate(&mut self) {
        self.searched_version = u64::MAX;
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
        self.matches = search::find_matches(grid, &self.query);
        let picked = if self.matches.is_empty() {
            self.current = None;
            self.needs_pick = false;
            false
        } else if self.needs_pick || self.current.is_none() {
            self.needs_pick = false;
            self.current =
                search::pick_initial(&self.matches, grid.scrollback_len(), grid.scroll_offset());
            true
        } else {
            // Live output shifts the list; keep the index, stay in range.
            self.current = self.current.map(|i| i.min(self.matches.len() - 1));
            false
        };
        self.searched_version = version;
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

    /// 1-based `(current, total)` for the counter; `(0, 0)` without matches.
    pub fn counter(&self) -> (usize, usize) {
        (self.current.map(|i| i + 1).unwrap_or(0), self.matches.len())
    }

    /// Highlight for the current frame, if any.
    pub fn view(&self, grid: &Grid) -> Option<SearchView> {
        if !self.open || self.matches.is_empty() {
            return None;
        }
        Some(SearchView::build(
            &self.matches,
            self.current,
            grid.scrollback_len(),
            grid.scroll_offset(),
            grid.rows(),
        ))
    }
}

/// `Ctrl+F` / `Cmd+F` opens (or re-focuses) the find bar, gated by
/// [`InputConfig`]. Shift/Alt stay out so `Ctrl+Shift+F` and the shell's
/// `Alt+F` keep their bindings; exactly one of Ctrl/Super per config.
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

    /// Ctrl/Cmd+F: open the find bar (or re-focus it) and repaint.
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
    pub(crate) fn search_view(&self) -> Option<SearchView> {
        let tab = self.active_tab()?;
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
    let Some(m) = tab.search.current_match() else {
        return false;
    };
    let grid = tab.terminal.grid();
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
        let cfg = InputConfig::default();
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
        g.newline();
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
}
