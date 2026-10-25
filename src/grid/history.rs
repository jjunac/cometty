//! History widths are projected, never eagerly rewritten. Rows admitted
//! between resizes share a compact projection (a cohort). A projection
//! retains the surviving original prefix and runs of structural padding.
//! It records destructive shrink and the color of each growth/edge repair,
//! without a resize journal or cloning every cell on each width increment.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::Index;

use super::{Cell, Grid, Row};

#[derive(Debug)]
struct Padding {
    start: usize,
    cell: Cell,
}

#[derive(Debug)]
pub(super) struct Projection {
    kept: usize,
    cols: usize,
    edge: Option<Cell>,
    padding: Vec<Padding>,
    count: usize,
    revision: u64,
}

impl Projection {
    fn new(cols: usize) -> Self {
        Self {
            kept: cols,
            cols,
            edge: None,
            padding: Vec::new(),
            count: 0,
            revision: 0,
        }
    }

    fn resize(&mut self, cols: usize, blank: &Cell) {
        if cols != self.cols {
            self.revision = self.revision.wrapping_add(1);
        }
        if cols < self.cols {
            if cols < self.kept {
                self.kept = cols;
                self.edge = Some(blank.clone());
            }
            self.padding.retain(|p| p.start < cols);
        } else if cols > self.cols && self.padding.last().is_none_or(|p| p.cell != *blank) {
            self.padding.push(Padding {
                start: self.cols,
                cell: blank.clone(),
            });
        }
        self.cols = cols;
    }

    fn bytes(&self) -> usize {
        size_of::<Self>() + self.padding.capacity() * size_of::<Padding>()
            // Conservative allowance for BTreeMap node/key overhead.
            + 64
    }
}

#[derive(Debug)]
pub(super) struct HistoryRow {
    pub row: Row,
    cohort: u64,
    bytes: usize,
}

/// A borrowed, repaired view shared by rendering, search, selection and links.
/// Synthetic blanks are borrowed from the cohort, not allocated per cell/row.
#[derive(Clone, Copy, Debug)]
pub struct RowView<'a> {
    row: &'a Row,
    projection: Option<&'a Projection>,
}

impl<'a> RowView<'a> {
    pub fn identity(&self) -> (u64, u64, u64) {
        (
            self.row.id,
            self.row.revision,
            self.projection.map_or(0, |p| p.revision),
        )
    }
    pub fn len(&self) -> usize {
        self.projection.map_or(self.row.len(), |p| p.cols)
    }
    pub fn get(&self, col: usize) -> Option<&'a Cell> {
        if col >= self.len() {
            return None;
        }
        let Some(p) = self.projection else {
            return self.row.get(col);
        };
        if col < p.kept {
            let cell = &self.row[col];
            if col + 1 == p.kept
                && cell.width == 2
                && let Some(blank) = p.edge.as_ref()
            {
                return Some(blank);
            }
            return Some(cell);
        }
        let run = p
            .padding
            .partition_point(|run| run.start <= col)
            .checked_sub(1)?;
        Some(&p.padding[run].cell)
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &'a Cell> + DoubleEndedIterator + '_ {
        (0..self.len()).map(|col| self.get(col).expect("projected column"))
    }
    pub fn to_vec(self) -> Vec<Cell> {
        self.iter().cloned().collect()
    }
    pub fn as_slice(&self) -> Option<&'a [Cell]> {
        self.projection
            .is_none_or(|p| p.kept == p.cols && p.cols == self.row.len() && p.edge.is_none())
            .then(|| &self.row[..])
    }
    pub fn materialize(self) -> Cow<'a, [Cell]> {
        self.as_slice()
            .map_or_else(|| Cow::Owned(self.to_vec()), Cow::Borrowed)
    }
}

impl Index<usize> for RowView<'_> {
    type Output = Cell;
    fn index(&self, col: usize) -> &Cell {
        self.get(col).expect("row column")
    }
}

impl PartialEq for RowView<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}

impl Grid {
    pub(super) fn live_row_view(row: &Row) -> RowView<'_> {
        RowView {
            row,
            projection: None,
        }
    }

    pub(super) fn history_row_view<'a>(&'a self, row: &'a HistoryRow) -> RowView<'a> {
        RowView {
            row: &row.row,
            projection: self.history_widths.get(&row.cohort),
        }
    }

    pub(super) fn resize_history(&mut self, cols: usize, blank: &Cell) {
        if cols == self.cols {
            return;
        }
        // One small update per cohort, not per history cell. Cohorts and
        // padding storage are included in the history budget and retired
        // with their last row. Padding runs are bounded by column count.
        for p in self.history_widths.values_mut() {
            p.resize(cols, blank);
        }
        self.history_projection_bytes = self.history_widths.values().map(Projection::bytes).sum();
        self.history_cohort = self.history_cohort.wrapping_add(1);
        self.enforce_history_budget();
    }

    fn pop_history(&mut self) -> Option<Row> {
        let old = self.scrollback.pop_front()?;
        self.history_cell_bytes -= old.bytes;
        let p = self
            .history_widths
            .get_mut(&old.cohort)
            .expect("history cohort");
        p.count -= 1;
        if p.count == 0 {
            self.history_projection_bytes -= p.bytes();
            self.history_widths.remove(&old.cohort);
        }
        self.history_dropped = self.history_dropped.saturating_add(1);
        Some(old.row)
    }

    pub fn history_bytes(&self) -> usize {
        self.history_cell_bytes
            + self.scrollback.capacity() * size_of::<HistoryRow>()
            + self.history_projection_bytes
    }

    pub fn history_dropped(&self) -> u64 {
        self.history_dropped
    }

    pub(super) fn enforce_history_budget(&mut self) {
        while !self.scrollback.is_empty()
            && (self.scrollback.len() > self.max_scrollback
                || self.history_bytes() > self.max_history_bytes)
        {
            self.pop_history();
        }
        // Release an oversized deque on explicit cap changes or emptying;
        // don't retain capacity outside the accounting ceiling.
        if self.scrollback.is_empty() || self.history_bytes() > self.max_history_bytes {
            self.scrollback.shrink_to_fit();
        }
        self.scroll_offset = self.scroll_offset.min(self.scrollback.len());
    }

    /// Move a live row into history and directly recycle an evicted buffer.
    /// A zero line/byte cap retains no history at all.
    pub(super) fn archive_row(&mut self, row: Row) -> Row {
        let fill = self.erase_cell();
        let mut spare = None;
        if self.max_scrollback == 0 || self.max_history_bytes == 0 {
            // The row leaves the global line sequence even though it was
            // never retained: surviving lines still rebase by one.
            self.history_dropped = self.history_dropped.saturating_add(1);
            spare = Some(row);
        } else {
            if self.scrollback.len() >= self.max_scrollback {
                spare = self.pop_history();
            }
            let bytes = row.bytes();
            self.history_cell_bytes += bytes;
            let p = self
                .history_widths
                .entry(self.history_cohort)
                .or_insert_with(|| {
                    let p = Projection::new(self.cols);
                    self.history_projection_bytes += p.bytes();
                    p
                });
            p.count += 1;
            self.scrollback.push_back(HistoryRow {
                row,
                cohort: self.history_cohort,
                bytes,
            });
            while !self.scrollback.is_empty() && self.history_bytes() > self.max_history_bytes {
                let evicted = self.pop_history();
                if spare.is_none() {
                    spare = evicted;
                }
            }
            if self.history_bytes() > self.max_history_bytes {
                self.scrollback.shrink_to_fit();
            }
        }
        let mut next = spare.unwrap_or_else(|| Row::new(Vec::new()));
        next.recycle(self.cols, &fill);
        next
    }

    pub(super) fn reset_history(&mut self) {
        self.history_dropped = self
            .history_dropped
            .saturating_add(self.scrollback.len() as u64);
        self.scrollback.clear();
        self.scrollback.shrink_to_fit();
        self.history_widths = BTreeMap::new();
        self.history_cell_bytes = 0;
        self.history_projection_bytes = 0;
        self.scroll_offset = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, theme::Theme};

    #[test]
    fn lazy_widths_match_eager_resize_with_theme_changes_and_new_cohorts() {
        let mut g = Grid::new(9, 2, Theme::default());
        let mut reference: Vec<Vec<Cell>> = Vec::new();
        for (i, width) in [12, 7, 11, 4, 8, 3, 9, 2, 6, 13]
            .into_iter()
            .cycle()
            .take(150)
            .enumerate()
        {
            g.set_cursor(0, 0);
            g.put_char('e');
            g.put_char('\u{301}');
            if g.cols() >= 3 {
                g.put_char('中');
            }
            while g.cursor().x < g.cols() {
                g.put_char('x');
            }
            reference.push(g.cells[0].to_vec());
            g.scroll_up(1);
            let ptrs: Vec<_> = g.scrollback.iter().map(|r| r.row.as_ptr()).collect();
            if i % 3 == 0 {
                g.set_theme(Theme::vscode());
            }
            if i % 3 == 1 {
                g.set_theme(Theme::default());
            }
            let blank = g.blank_cell();
            g.resize(width, 2);
            for row in &mut reference {
                row.resize(width, blank.clone());
                Grid::fix_row_wide(row, &blank);
            }
            for (line, expected) in reference.iter().enumerate() {
                assert_eq!(
                    g.global_line(line).unwrap().to_vec(),
                    *expected,
                    "step {i} line {line}"
                );
                assert_eq!(
                    g.scrollback[line].row.as_ptr(),
                    ptrs[line],
                    "history storage never resized"
                );
            }
            assert_eq!(
                g.history_projection_bytes,
                g.history_widths
                    .values()
                    .map(Projection::bytes)
                    .sum::<usize>()
            );
        }
    }

    #[test]
    fn row_recycling_preserves_moves_but_never_reuses_logical_identity() {
        let mut cfg = Config::default();
        cfg.terminal.scrollback_lines = 1;
        let mut g = Grid::new_with_config(10, 2, Theme::default(), &cfg);
        g.put_char('a');
        let id = g.cells[0].id;
        let ptr = g.cells[0].as_ptr();
        g.scroll_up(1);
        assert_eq!(g.global_line(0).unwrap().identity().0, id);
        g.sgr(&[44]);
        g.scroll_up(1);
        assert_eq!(g.cells[1].as_ptr(), ptr);
        assert_ne!(g.cells[1].id, id);
        assert!(
            g.cells[1]
                .iter()
                .all(|c| c.bg == g.pen.bg && c.extra.is_none())
        );
        assert!(g.history_bytes() <= cfg.terminal.scrollback_bytes);
        assert_eq!(g.history_widths.len(), 1);
    }

    #[test]
    fn history_budget_counts_extras_and_capacity_and_zero_caps_retain_nothing() {
        let mut cfg = Config::default();
        cfg.terminal.scrollback_bytes = 4096;
        let mut g = Grid::new_with_config(4, 2, Theme::default(), &cfg);
        for _ in 0..100 {
            g.set_cursor(0, 0);
            g.cells[0][0].extra = Some("x".repeat(2000).into_boxed_str());
            g.scroll_up(1);
            assert!(g.history_bytes() <= 4096);
        }
        assert!(g.history_dropped() > 0);
        cfg.terminal.scrollback_bytes = 0;
        g.apply_terminal_config(&cfg.terminal);
        let dropped = g.history_dropped();
        g.scroll_up(5);
        assert_eq!(g.scrollback_len(), 0);
        assert_eq!(g.history_bytes(), 0);
        assert_eq!(
            g.history_dropped(),
            dropped + 5,
            "discarded rows still shift global line identities"
        );
        cfg.terminal.scrollback_bytes = 4096;
        cfg.terminal.scrollback_lines = 0;
        g.apply_terminal_config(&cfg.terminal);
        g.scroll_up(5);
        assert_eq!(g.history_bytes(), 0);
    }

    #[test]
    fn clipped_history_is_identical_for_search_links_and_copy() {
        let mut g = Grid::new(20, 2, Theme::default());
        for c in "prefix https://x.y".chars() {
            g.put_char(c);
        }
        g.scroll_up(1);
        g.resize(6, 2);
        g.resize(20, 2);
        assert!(crate::search::find_matches(&g, "https").is_empty());
        assert!(crate::links::detect_urls(&g.global_line(0).unwrap().materialize()).is_empty());
        assert_eq!(
            g.global_line_cells(0)
                .unwrap()
                .iter()
                .map(|c| c.ch)
                .collect::<String>(),
            "prefix              "
        );
    }
}
