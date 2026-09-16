//! Search model: smart-case match finding over grid lines + view mapping.
//!
//! Pure, toolkit-free state (like [`crate::selection`]) so it stays
//! unit-testable: the app owns a [`crate::app::search::SearchPanel`], the
//! renderer paints a [`SearchView`] it is given.

use crate::grid::{Cell, Grid};

/// One search hit: global line index (`0` = oldest scrollback line) and an
/// inclusive column range. A wide cluster is covered through its
/// continuation, so `end` can point at a `width == 0` cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// Smart case: any uppercase character in the query makes matching
/// case-sensitive; otherwise matching folds case.
pub fn is_smart_case(query: &str) -> bool {
    query.chars().any(char::is_uppercase)
}

/// One cluster's slice of the per-row haystack.
struct Span {
    start: usize,
    len: usize,
    col: usize,
    width: u8,
}

impl Span {
    fn end(&self) -> usize {
        self.start + self.len
    }
}

/// How a row's haystack is case-folded.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FoldMode {
    /// Case-sensitive query: raw text.
    Raw,
    /// Case-insensitive over an all-ASCII row: raw text, folded in place
    /// with `make_ascii_lowercase` (much cheaper than per-char folding).
    AsciiFold,
    /// Case-insensitive with at least one non-ASCII cell: per-char
    /// `to_lowercase` while building the haystack.
    UnicodeFold,
}

/// Find every non-overlapping match of `query`.
///
/// Searches the whole scrollback plus the visible grid, or only the visible
/// rows while the alt screen is active (the scrollback belongs to the main
/// buffer). Rows are matched individually, so a hit never spans a line
/// wrap. Trailing blank cells are not searched: a lone-space query would
/// otherwise highlight every screen-wide blank tail.
pub fn find_matches(grid: &Grid, query: &str) -> Vec<Match> {
    if query.is_empty() {
        return Vec::new();
    }
    let sensitive = is_smart_case(query);
    let needle = if sensitive {
        query.to_string()
    } else {
        // Same per-char mapping as `push_folded` so both sides fold alike.
        fold_lower(query)
    };
    if needle.is_empty() {
        return Vec::new();
    }

    let scrollback_len = grid.scrollback_len();
    // Alt screen: visible rows only.
    let first = if grid.is_alt() { scrollback_len } else { 0 };
    let mut out = Vec::new();
    // Reused across rows so a full-buffer scan doesn't allocate per cell.
    let mut hay = String::new();
    let mut spans: Vec<Span> = Vec::new();
    for line in first..grid.total_lines() {
        let Some(row) = grid.global_line(line) else {
            continue;
        };
        hay.clear();
        spans.clear();
        let mut end = row.len();
        while end > 0 && is_blank(&row[end - 1]) {
            end -= 1;
        }
        let mode = fold_mode(&row[..end], sensitive);
        for (col, cell) in row[..end].iter().enumerate() {
            // Continuations carry no text of their own; their lead's span
            // covers the extra column.
            if cell.width == 0 {
                continue;
            }
            let start = hay.len();
            match mode {
                FoldMode::Raw | FoldMode::AsciiFold => {
                    hay.push(cell.ch);
                    if let Some(extra) = cell.extra.as_deref() {
                        hay.push_str(extra);
                    }
                }
                FoldMode::UnicodeFold => push_folded(&mut hay, cell),
            }
            let len = hay.len() - start;
            if len > 0 {
                spans.push(Span {
                    start,
                    len,
                    col,
                    width: cell.width,
                });
            }
        }
        if mode == FoldMode::AsciiFold {
            hay.make_ascii_lowercase();
        }
        if hay.is_empty() {
            continue;
        }
        let mut from = 0;
        while from < hay.len() {
            let Some(rel) = hay[from..].find(&needle) else {
                break;
            };
            let start = from + rel;
            let end = start + needle.len();
            if let Some((start_col, end_col)) = span_cover(&spans, start, end) {
                out.push(Match {
                    line,
                    start: start_col,
                    end: end_col,
                });
            }
            from = end;
        }
    }
    out
}

/// Minimal search highlight for one frame, in view space (`row 0` = top of
/// the screen) — what the background painter consumes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchView {
    /// Per visible row: sorted, inclusive column ranges of every match.
    pub rows: Vec<Vec<(usize, usize)>>,
    /// The current match when it is on screen.
    pub active: Option<(usize, usize, usize)>,
}

/// What a view cell is part of.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hit {
    #[default]
    None,
    Match,
    Active,
}

impl SearchView {
    /// Project the app's match list onto the visible rows.
    pub fn build(
        matches: &[Match],
        current: Option<usize>,
        scrollback_len: usize,
        scroll_offset: usize,
        rows: usize,
    ) -> Self {
        let base = scrollback_len.saturating_sub(scroll_offset.min(scrollback_len));
        let mut view = Self {
            rows: vec![Vec::new(); rows],
            active: None,
        };
        for (index, m) in matches.iter().enumerate() {
            if m.line < base || m.line - base >= rows {
                continue;
            }
            let view_row = m.line - base;
            view.rows[view_row].push((m.start, m.end));
            if Some(index) == current {
                view.active = Some((view_row, m.start, m.end));
            }
        }
        view
    }

    /// Classify a view cell (`span` = 1 or 2 columns from `x`). Ranges are
    /// inclusive on both edges, so a wide cluster is hit from either half.
    pub fn hit(&self, view_row: usize, x: usize, span: usize) -> Hit {
        let right = x + span;
        if let Some((row, start, end)) = self.active
            && row == view_row
            && start < right
            && x <= end
        {
            return Hit::Active;
        }
        if self
            .rows
            .get(view_row)
            .is_some_and(|ranges| ranges.iter().any(|&(start, end)| start < right && x <= end))
        {
            return Hit::Match;
        }
        Hit::None
    }
}

/// First match at or after the top of the current view, else the last match
/// (nearest above the viewport). `None` without matches.
pub fn pick_initial(
    matches: &[Match],
    scrollback_len: usize,
    scroll_offset: usize,
) -> Option<usize> {
    let last = matches.len().checked_sub(1)?;
    let base = scrollback_len.saturating_sub(scroll_offset.min(scrollback_len));
    Some(matches.partition_point(|m| m.line < base).min(last))
}

/// Next/previous match index with wrap-around.
pub fn step(current: Option<usize>, len: usize, forward: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match current {
        Some(i) if i < len => {
            if forward {
                (i + 1) % len
            } else {
                (i + len - 1) % len
            }
        }
        _ if forward => 0,
        _ => len - 1,
    })
}

/// Scroll offset (`0` = live bottom) that brings `line` on screen with the
/// least movement: `None` when it already is visible.
pub fn reveal_offset(
    line: usize,
    rows: usize,
    scrollback_len: usize,
    scroll_offset: usize,
) -> Option<usize> {
    if rows == 0 {
        return None;
    }
    let offset = scroll_offset.min(scrollback_len);
    let base = scrollback_len - offset;
    if line >= base && line < base + rows {
        return None;
    }
    let target = if line < base {
        // Above the view: pin to the top row.
        scrollback_len.saturating_sub(line)
    } else {
        // Below the view: pin to the bottom row.
        scrollback_len.saturating_add(rows - 1).saturating_sub(line)
    };
    Some(target.min(scrollback_len))
}

/// True for a narrow, unstyled blank cell (trailing-blank trimming).
fn is_blank(cell: &Cell) -> bool {
    cell.width == 1 && cell.ch == ' ' && cell.extra.is_none()
}

/// Fold mode for one row: ASCII-only rows take the in-place fast path.
fn fold_mode(row: &[Cell], sensitive: bool) -> FoldMode {
    if sensitive {
        return FoldMode::Raw;
    }
    if row.iter().all(cell_is_ascii) {
        FoldMode::AsciiFold
    } else {
        FoldMode::UnicodeFold
    }
}

/// True when the cell's cluster folds with plain ASCII byte mapping.
fn cell_is_ascii(cell: &Cell) -> bool {
    cell.ch.is_ascii() && cell.extra.as_deref().is_none_or(str::is_ascii)
}

/// Append a cell's cluster folded with `char::to_lowercase`.
/// A fold can expand (e.g. `İ`), which is fine: the span table is keyed to
/// the folded byte length.
fn push_folded(out: &mut String, cell: &Cell) {
    for ch in std::iter::once(cell.ch).chain(cell.extra.as_deref().unwrap_or("").chars()) {
        for folded in ch.to_lowercase() {
            out.push(folded);
        }
    }
}

/// Lowercase a query with the same per-char mapping as [`push_folded`]
/// (`str::to_lowercase` has context-dependent mappings that would diverge).
fn fold_lower(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        for folded in ch.to_lowercase() {
            out.push(folded);
        }
    }
    out
}

/// Columns covered by haystack bytes `[start, end)`. Spans are contiguous
/// and ordered, so the covering clusters are the first one ending after
/// `start` and the last one starting before `end` (binary search: a dense
/// query would otherwise be quadratic per row).
fn span_cover(spans: &[Span], start: usize, end: usize) -> Option<(usize, usize)> {
    let first = spans.partition_point(|s| s.end() <= start);
    if first == spans.len() {
        return None;
    }
    let past_last = spans.partition_point(|s| s.start < end);
    if past_last == 0 {
        return None;
    }
    let last = past_last - 1;
    if last < first {
        return None;
    }
    let end_col = spans[last].col + usize::from(spans[last].width).saturating_sub(1);
    Some((spans[first].col, end_col))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn grid_with(lines: &[&str], cols: usize, rows: usize) -> Grid {
        let mut g = Grid::new(cols, rows, Theme::default());
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                g.newline();
            }
            for ch in line.chars() {
                g.put_char(ch);
            }
        }
        g
    }

    #[test]
    fn finds_matches_in_visible_rows() {
        let g = grid_with(&["alpha", "beta", "alpha"], 20, 3);
        assert_eq!(g.scrollback_len(), 0);
        assert_eq!(
            find_matches(&g, "alpha"),
            vec![
                Match {
                    line: 0,
                    start: 0,
                    end: 4,
                },
                Match {
                    line: 2,
                    start: 0,
                    end: 4,
                },
            ]
        );
        assert_eq!(find_matches(&g, "beta")[0].line, 1);
        assert!(find_matches(&g, "gamma").is_empty());
        assert!(find_matches(&g, "").is_empty());
    }

    #[test]
    fn searches_scrollback() {
        // Two rows of scrollback ("one", "two") plus the visible rows.
        let g = grid_with(&["one", "two", "three", "four"], 8, 2);
        assert_eq!(g.scrollback_len(), 2);
        assert_eq!(
            find_matches(&g, "two"),
            vec![Match {
                line: 1,
                start: 0,
                end: 2,
            }]
        );
        assert_eq!(find_matches(&g, "three")[0].line, 2);
        assert_eq!(find_matches(&g, "four")[0].line, 3);
    }

    #[test]
    fn smart_case_folds_lowercase_only() {
        let g = grid_with(&["Error: boom", "error: small"], 20, 2);
        assert!(!is_smart_case("error"));
        assert!(is_smart_case("Error"));
        assert!(is_smart_case("É"));
        assert_eq!(find_matches(&g, "error").len(), 2);
        assert_eq!(
            find_matches(&g, "Error"),
            vec![Match {
                line: 0,
                start: 0,
                end: 4,
            }]
        );
        // An all-caps query is case-sensitive too.
        assert!(find_matches(&g, "BOOM").is_empty());
        assert_eq!(find_matches(&g, "boom").len(), 1);
    }

    #[test]
    fn folds_non_ascii_case_consistently() {
        let g = grid_with(&["ÉCOLE"], 10, 1);
        assert!(!is_smart_case("école"));
        assert_eq!(find_matches(&g, "école").len(), 1);
        // Uppercase in the query flips to case-sensitive.
        assert!(find_matches(&g, "École").is_empty());
        assert_eq!(find_matches(&g, "ÉCOLE").len(), 1);
    }

    #[test]
    fn non_overlapping_matches() {
        let g = grid_with(&["aaaa"], 10, 1);
        assert_eq!(find_matches(&g, "aa").len(), 2);
    }

    #[test]
    fn wide_clusters_map_to_whole_cells() {
        // 中 cols 0-1, 文 cols 2-3, x col 4.
        let g = grid_with(&["中文x"], 10, 1);
        assert_eq!(
            find_matches(&g, "中文"),
            vec![Match {
                line: 0,
                start: 0,
                end: 3,
            }]
        );
        assert_eq!(
            find_matches(&g, "文x"),
            vec![Match {
                line: 0,
                start: 2,
                end: 4,
            }]
        );
    }

    #[test]
    fn trailing_blanks_are_not_searched() {
        let g = grid_with(&["hi"], 8, 1);
        assert!(find_matches(&g, " ").is_empty());
        // Inner spaces still match.
        let g = grid_with(&["a b"], 8, 1);
        assert_eq!(
            find_matches(&g, "a b"),
            vec![Match {
                line: 0,
                start: 0,
                end: 2,
            }]
        );
    }

    #[test]
    fn alt_screen_searches_visible_rows_only() {
        let mut g = grid_with(&["old", "history"], 20, 2);
        assert_eq!(find_matches(&g, "old").len(), 1);
        g.enter_alt(true);
        for ch in "fresh target".chars() {
            g.put_char(ch);
        }
        // Main-buffer history is out of scope while the alt screen is up.
        assert!(find_matches(&g, "old").is_empty());
        let m = find_matches(&g, "target");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].line, g.scrollback_len());
    }

    #[test]
    fn view_maps_to_visible_rows_and_marks_active() {
        let matches = vec![
            Match {
                line: 3,
                start: 0,
                end: 2,
            },
            Match {
                line: 4,
                start: 1,
                end: 1,
            },
            Match {
                line: 6,
                start: 5,
                end: 6,
            },
        ];
        // scrollback 5, offset 1: base = 4, rows 0..3 = lines 4..7.
        let view = SearchView::build(&matches, Some(1), 5, 1, 4);
        assert_eq!(view.rows[0], vec![(1, 1)]);
        assert_eq!(view.rows[2], vec![(5, 6)]);
        assert_eq!(view.rows[1], Vec::<(usize, usize)>::new());
        assert_eq!(view.active, Some((0, 1, 1)));
        assert_eq!(view.hit(0, 1, 1), Hit::Active);
        assert_eq!(view.hit(2, 5, 1), Hit::Match);
        assert_eq!(view.hit(1, 0, 1), Hit::None);
        // Live view with 2 rows: only line 6 (row 1) is on screen.
        let view = SearchView::build(&matches, None, 5, 0, 2);
        assert_eq!(view.rows.len(), 2);
        assert!(view.rows[0].is_empty());
        assert_eq!(view.rows[1], vec![(5, 6)]);
        // Scrolled up 2: base = 3, so line 3 is row 0 and the active one.
        let view = SearchView::build(&matches, Some(0), 5, 2, 2);
        assert_eq!(view.rows[0], vec![(0, 2)]);
        assert_eq!(view.active, Some((0, 0, 2)));
        // A wide cluster is hit from either half.
        let view = SearchView::build(
            &[Match {
                line: 0,
                start: 2,
                end: 3,
            }],
            None,
            0,
            0,
            1,
        );
        assert_eq!(view.hit(0, 2, 2), Hit::Match);
        assert_eq!(view.hit(0, 3, 1), Hit::Match);
        assert_eq!(view.hit(0, 1, 1), Hit::None);
    }

    #[test]
    fn pick_initial_prefers_the_current_viewport() {
        let matches = vec![
            Match {
                line: 0,
                start: 0,
                end: 0,
            },
            Match {
                line: 10,
                start: 0,
                end: 0,
            },
            Match {
                line: 20,
                start: 0,
                end: 0,
            },
        ];
        // base = 5: first match at/after the viewport top.
        assert_eq!(pick_initial(&matches, 30, 25), Some(1));
        // All matches above the view: nearest one, not the oldest.
        assert_eq!(pick_initial(&matches, 30, 0), Some(2));
        assert_eq!(pick_initial(&[], 30, 0), None);
    }

    #[test]
    fn step_wraps_both_ways() {
        assert_eq!(step(None, 3, true), Some(0));
        assert_eq!(step(None, 3, false), Some(2));
        assert_eq!(step(Some(2), 3, true), Some(0));
        assert_eq!(step(Some(0), 3, false), Some(2));
        assert_eq!(step(Some(1), 3, false), Some(0));
        assert_eq!(step(None, 0, true), None);
        assert_eq!(step(Some(0), 0, true), None);
    }

    #[test]
    fn reveal_offset_moves_the_least_amount() {
        // 4 rows, 10 scrollback lines: the live view shows lines 10..13.
        assert_eq!(reveal_offset(10, 4, 10, 0), None);
        assert_eq!(reveal_offset(13, 4, 10, 0), None);
        // Above the view: pin to the top row.
        assert_eq!(reveal_offset(8, 4, 10, 0), Some(2));
        // Below the view (scrolled up 5): pin to the bottom row.
        assert_eq!(reveal_offset(9, 4, 10, 5), Some(4));
        // Never scrolls past the oldest line.
        assert_eq!(reveal_offset(0, 4, 10, 0), Some(10));
        assert_eq!(reveal_offset(0, 0, 10, 0), None);
    }
}
