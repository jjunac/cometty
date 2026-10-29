//! History widths are projected, never eagerly rewritten. Rows admitted
//! between resizes share a compact projection (a cohort). A projection
//! retains the surviving original prefix and runs of structural padding.
//! It records destructive shrink and the color of each growth/edge repair,
//! without a resize journal or cloning every cell on each width increment.

use super::{Cell, CellRef, Grid, Row};
use std::borrow::Cow;
use std::collections::BTreeMap;

mod packed;
mod storage;

pub(super) use storage::History;
use storage::StoredRow;

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

/// A borrowed, repaired view shared by rendering, search, selection and links.
/// Synthetic blanks are borrowed from the cohort, not allocated per cell/row.
#[derive(Clone, Copy, Debug)]
pub struct RowView<'a> {
    row: StoredRow<'a>,
    projection: Option<&'a Projection>,
}

impl<'a> RowView<'a> {
    pub fn identity(&self) -> (u64, u64, u64) {
        let (id, revision) = self.row.identity();
        (id, revision, self.projection.map_or(0, |p| p.revision))
    }
    pub fn len(&self) -> usize {
        self.projection.map_or(self.row.len(), |p| p.cols)
    }
    pub fn get(&self, col: usize) -> Option<CellRef<'a>> {
        self.get_with_cursor(col, None)
    }

    fn get_with_cursor(
        &self,
        col: usize,
        cursor: Option<&mut packed::Cursor>,
    ) -> Option<CellRef<'a>> {
        if col >= self.len() {
            return None;
        }
        if let Some(p) = self.projection
            && col >= p.kept
        {
            let run = p
                .padding
                .partition_point(|run| run.start <= col)
                .checked_sub(1)?;
            return Some((&p.padding[run].cell).into());
        }
        let cell = match self.row {
            StoredRow::Dense(row) => (&row[col]).into(),
            StoredRow::Packed(row) => match cursor {
                Some(cursor) => row.get_next(col, cursor),
                None => row.get(col),
            },
        };
        if let Some(p) = self.projection
            && col + 1 == p.kept
            && cell.width == 2
            && let Some(blank) = &p.edge
        {
            return Some(blank.into());
        }
        Some(cell)
    }

    pub fn iter(&self) -> RowIter<'a> {
        RowIter {
            view: *self,
            front: 0,
            back: self.len(),
            cursor: match self.row {
                StoredRow::Dense(_) => packed::Cursor::default(),
                StoredRow::Packed(row) => row.cursor(),
            },
        }
    }
    pub fn to_vec(self) -> Vec<Cell> {
        self.iter().map(CellRef::to_cell).collect()
    }

    /// Decode directly into the already bounded shared viewport snapshot.
    pub fn copy_into(self, dst: &mut Vec<Cell>) {
        dst.truncate(self.len());
        let existing = dst.len();
        if let Some(cells) = self.as_slice() {
            // Preserve the original dense live-screen copy path. Only compact
            // or width-projected rows need borrowed-cell decoding.
            for (dst, src) in dst.iter_mut().zip(cells) {
                dst.clone_from(src);
            }
            dst.extend_from_slice(&cells[existing..]);
            return;
        }
        let mut cells = self.iter();
        for cell in dst.iter_mut() {
            cells.next().expect("snapshot column").copy_into(cell);
        }
        dst.reserve(self.len() - existing);
        dst.extend(cells.map(CellRef::to_cell));
    }
    pub fn as_slice(&self) -> Option<&'a [Cell]> {
        match self.row {
            StoredRow::Dense(row)
                if self.projection.is_none_or(|p| {
                    p.kept == p.cols && p.cols == row.len() && p.edge.is_none()
                }) =>
            {
                Some(row)
            }
            _ => None,
        }
    }

    /// A compact ASCII row is already a search haystack with byte == column.
    /// Apply the same destructive prefix projection before exposing any text.
    pub fn ascii_text(&self) -> Option<&'a str> {
        let StoredRow::Packed(row) = self.row else {
            return None;
        };
        let text = row.ascii_text()?;
        let kept = self.projection.map_or(self.len(), |p| p.kept);
        Some(text[..text.len().min(kept)].trim_end_matches(' '))
    }
    pub fn materialize(self) -> Cow<'a, [Cell]> {
        self.as_slice()
            .map_or_else(|| Cow::Owned(self.to_vec()), Cow::Borrowed)
    }
}

pub struct RowIter<'a> {
    view: RowView<'a>,
    front: usize,
    back: usize,
    cursor: packed::Cursor,
}

impl<'a> Iterator for RowIter<'a> {
    type Item = CellRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.front == self.back {
            return None;
        }
        let cell = self
            .view
            .get_with_cursor(self.front, Some(&mut self.cursor));
        self.front += 1;
        cell
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.back - self.front;
        (remaining, Some(remaining))
    }
}

impl DoubleEndedIterator for RowIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.front == self.back {
            return None;
        }
        self.back -= 1;
        self.view.get(self.back)
    }
}

impl ExactSizeIterator for RowIter<'_> {}

impl PartialEq for RowView<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}

impl Grid {
    pub(super) fn live_row_view(row: &Row) -> RowView<'_> {
        RowView {
            row: StoredRow::Dense(row),
            projection: None,
        }
    }

    pub(super) fn history_row_view(&self, index: usize) -> Option<RowView<'_>> {
        let (cohort, row) = self.scrollback.get(index)?;
        Some(RowView {
            row,
            projection: self.history_widths.get(&cohort),
        })
    }

    pub(super) fn resize_history(&mut self, cols: usize, blank: &Cell) {
        if cols == self.cols {
            return;
        }
        // A chunk never mixes admission widths. Sealing does not materialize
        // old projections or change logical row identities.
        self.scrollback.seal();
        self.scrollback.discard_spares();
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
        let (cohort, row) = self.scrollback.pop_front()?;
        let p = self
            .history_widths
            .get_mut(&cohort)
            .expect("history cohort");
        p.count -= 1;
        if p.count == 0 {
            self.history_projection_bytes -= p.bytes();
            self.history_widths.remove(&cohort);
        }
        self.history_dropped = self.history_dropped.saturating_add(1);
        row
    }

    pub fn history_bytes(&self) -> usize {
        self.scrollback.bytes() + self.history_projection_bytes
    }

    pub fn history_dropped(&self) -> u64 {
        self.history_dropped
    }

    pub(super) fn enforce_history_budget(&mut self) {
        if self.max_scrollback == 0 || self.max_history_bytes == 0 {
            self.reset_history();
            return;
        }
        if self.history_bytes() > self.max_history_bytes {
            self.scrollback.seal();
            self.scrollback.release_unused();
        }
        while !self.scrollback.is_empty()
            && (self.scrollback.len() > self.max_scrollback
                || self.history_bytes() > self.max_history_bytes)
        {
            self.pop_history();
            if self.history_bytes() > self.max_history_bytes {
                self.scrollback.release_unused();
            }
        }
        if self.scrollback.is_empty() || self.history_bytes() > self.max_history_bytes {
            self.scrollback.release_unused();
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
            let p = self
                .history_widths
                .entry(self.history_cohort)
                .or_insert_with(|| {
                    let p = Projection::new(self.cols);
                    self.history_projection_bytes += p.bytes();
                    p
                });
            p.count += 1;
            self.scrollback.push(row, self.history_cohort);
            if spare.is_none() {
                spare = self.scrollback.take_spare();
            }
            if self.history_bytes() > self.max_history_bytes {
                self.scrollback.seal();
                if spare.is_none() {
                    spare = self.scrollback.take_spare();
                }
                self.scrollback.release_unused();
            }
            while !self.scrollback.is_empty() && self.history_bytes() > self.max_history_bytes {
                let evicted = self.pop_history();
                if spare.is_none() {
                    spare = evicted;
                }
                if self.history_bytes() > self.max_history_bytes {
                    self.scrollback.release_unused();
                }
            }
            if self.scrollback.is_empty() || self.history_bytes() > self.max_history_bytes {
                self.scrollback.release_unused();
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
        self.scrollback = History::default();
        self.history_widths = BTreeMap::new();
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
            g.scrollback.seal();
            let ptrs = g.scrollback.storage_addresses();
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
            }
            assert_eq!(
                g.scrollback.storage_addresses(),
                ptrs,
                "sealed history never rewritten on resize"
            );
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
            g.cells[0][0].extra = Some("x".repeat(2000));
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

    #[test]
    fn chunk_boundaries_and_partial_eviction_keep_exact_line_caps_and_identities() {
        use std::collections::VecDeque;
        for cap in [1, 13, 32, 33, 65, 100] {
            let mut cfg = Config::default();
            cfg.terminal.scrollback_lines = cap;
            let mut grid = Grid::new_with_config(40, 2, Theme::default(), &cfg);
            let mut expected = VecDeque::new();
            for i in 0_usize..250 {
                grid.set_cursor(0, 0);
                for ch in format!("row {i:03}").chars() {
                    grid.put_char(ch);
                }
                expected.push_back((grid.cells[0].id, grid.cells[0].to_vec()));
                if expected.len() > cap {
                    expected.pop_front();
                }
                grid.scroll_up(1);
                assert_eq!(grid.scrollback_len(), expected.len());
                assert_eq!(grid.history_dropped(), (i + 1).saturating_sub(cap) as u64);
                for (line, (id, cells)) in expected.iter().enumerate() {
                    let view = grid.global_line(line).unwrap();
                    assert_eq!(view.identity().0, *id, "cap {cap}, output {i}, line {line}");
                    assert_eq!(view.to_vec(), *cells);
                }
                assert!(grid.history_bytes() <= cfg.terminal.scrollback_bytes);
            }
        }
    }

    #[test]
    fn complete_chunks_keep_resize_projection_unicode_links_and_snapshot_equivalent() {
        let mut grid = Grid::new(80, 2, Theme::default());
        let mut reference = Vec::new();
        for index in 0..96 {
            grid.set_cursor(0, 0);
            grid.set_hyperlink(Some("https://example.com"));
            for ch in "e\u{301} 👨\u{200d}👩\u{200d}👧 中 https://example.com".chars() {
                grid.put_char(ch);
            }
            if index == 0 {
                // The first sealed chunk needs wide text offsets; later chunks
                // use narrow records. Projection/copy must treat both alike.
                grid.cells[0][0]
                    .extra
                    .as_mut()
                    .unwrap()
                    .push_str(&"x".repeat(usize::from(u16::MAX)));
            }
            grid.set_hyperlink(None);
            grid.sgr(&[44]);
            grid.erase_in_line(0);
            reference.push(grid.cells[0].to_vec());
            grid.scroll_up(1);
        }
        let original = grid.scrollback.storage_addresses();
        for (n, width) in [81, 18, 60, 3, 80, 1, 90].into_iter().enumerate() {
            grid.set_theme(if n % 2 == 0 {
                Theme::vscode()
            } else {
                Theme::default()
            });
            let blank = grid.blank_cell();
            grid.resize(width, 2);
            assert_eq!(grid.scrollback.storage_addresses(), original);
            for (line, expected) in reference.iter_mut().enumerate() {
                expected.resize(width, blank.clone());
                Grid::fix_row_wide(expected, &blank);
                let row = grid.global_line(line).unwrap();
                assert_eq!(row.to_vec(), *expected);
                assert_eq!(
                    row.iter().rev().map(CellRef::to_cell).collect::<Vec<_>>(),
                    expected.iter().rev().cloned().collect::<Vec<_>>()
                );
                let mut mixed = row.iter();
                if width > 1 {
                    assert_eq!(mixed.next().unwrap().to_cell(), expected[0]);
                    assert_eq!(mixed.next_back().unwrap().to_cell(), expected[width - 1]);
                    assert_eq!(
                        mixed.map(CellRef::to_cell).collect::<Vec<_>>(),
                        expected[1..width - 1]
                    );
                }
                let mut snapshot = vec![Cell::default(); width + 10];
                row.copy_into(&mut snapshot);
                assert_eq!(snapshot, *expected);
            }
        }
        assert!(crate::search::find_matches(&grid, "https").is_empty());
        assert!(crate::links::detect_urls(&grid.global_line(0).unwrap().materialize()).is_empty());
        grid.clear_scrollback();
        assert_eq!(grid.history_bytes(), 0);
        assert_eq!(grid.history_dropped(), 96);
    }

    #[test]
    fn tiny_budgets_lowered_caps_and_zero_release_chunks_and_packing_buffers() {
        for budget in [1, 1024, 4096, 16 * 1024, 64 * 1024] {
            let mut cfg = Config::default();
            cfg.terminal.scrollback_bytes = budget;
            let mut grid = Grid::new_with_config(200, 2, Theme::default(), &cfg);
            for i in 0..300 {
                grid.set_cursor(0, 0);
                grid.put_char(if i % 2 == 0 { 'a' } else { 'b' });
                grid.scroll_up(1);
                assert!(
                    grid.history_bytes() <= budget,
                    "budget {budget}, output {i}"
                );
                assert_eq!(
                    grid.history_dropped() as usize + grid.scrollback_len(),
                    i + 1
                );
            }
            cfg.terminal.scrollback_bytes = 512;
            grid.apply_terminal_config(&cfg.terminal);
            assert!(grid.history_bytes() <= 512);
            cfg.terminal.scrollback_lines = 0;
            grid.apply_terminal_config(&cfg.terminal);
            assert_eq!(grid.history_bytes(), 0);
            assert_eq!(grid.history_dropped(), 300);
        }
    }

    #[test]
    fn searching_compact_cluster_text_does_not_allocate_owned_cell_tails() {
        let mut grid = Grid::new(80, 2, Theme::default());
        for _ in 0..96 {
            grid.set_cursor(0, 0);
            for ch in "e\u{301}".repeat(60).chars() {
                grid.put_char(ch);
            }
            grid.scroll_up(1);
        }
        let before = grid.history_bytes();
        let counts = crate::perf_bench::allocation_stats(|| {
            assert!(crate::search::find_matches(&grid, "missing").is_empty());
        });
        assert!(counts.largest_request < 8192);
        // One haystack/span workspace, not 5,760 separately boxed tails.
        assert!(
            crate::perf_bench::allocation_counts(|| {
                assert!(crate::search::find_matches(&grid, "missing").is_empty());
            })
            .0 < 10
        );
        assert_eq!(
            grid.history_bytes(),
            before,
            "search never retains expanded history"
        );
    }

    #[test]
    fn compact_ascii_search_matches_dense_search_and_never_exposes_clipped_text() {
        let mut grid = Grid::new(60, 2, Theme::default());
        for _ in 0..20 {
            grid.set_cursor(0, 0);
            for ch in "Aa aa  Aaa https://example.com".chars() {
                grid.put_char(ch);
            }
            grid.scroll_up(1);
        }
        let queries = [
            "a",
            "Aa",
            "aa",
            " ",
            "  ",
            "https",
            "https://example.com",
            "missing",
            "é",
        ];
        let expected: Vec<_> = queries
            .iter()
            .map(|q| crate::search::find_matches(&grid, q))
            .collect();
        let identities: Vec<_> = (0..20)
            .map(|i| grid.global_line(i).unwrap().identity())
            .collect();
        grid.scrollback.seal();
        assert!(grid.global_line(0).unwrap().ascii_text().is_some());
        for (query, expected) in queries.iter().zip(expected) {
            assert_eq!(
                crate::search::find_matches(&grid, query),
                expected,
                "query {query:?}"
            );
        }
        for (i, identity) in identities.iter().enumerate() {
            assert_eq!(grid.global_line(i).unwrap().identity(), *identity);
        }
        grid.scroll_to_top();
        assert!(!crate::links::HintView::build(&grid, true).rows[0].is_empty());
        grid.resize(5, 2);
        grid.resize(60, 2);
        assert!(crate::search::find_matches(&grid, "https").is_empty());
        assert!(crate::links::HintView::build(&grid, true).rows[0].is_empty());
        assert_eq!(grid.global_line(0).unwrap().ascii_text(), Some("Aa aa"));
    }

    #[test]
    fn saturated_compact_history_reuses_buffers_without_per_line_allocations() {
        let mut terminal = crate::term::Terminal::new(200, 2, Theme::default());
        for _ in 0..20_000 {
            terminal.feed(b"x\r\n");
        }
        assert_eq!(terminal.grid().scrollback_len(), 10_000);
        assert!(terminal.grid().history_bytes() < 10_000 * 200 * size_of::<Cell>() / 10);
        assert_eq!(
            crate::perf_bench::allocation_counts(|| {
                for _ in 0..2000 {
                    terminal.feed(b"x\r\n");
                }
            }),
            (0, 0, 0)
        );
    }

    #[test]
    fn mixed_resize_eviction_clear_and_budget_changes_match_dense_reference() {
        use std::collections::VecDeque;
        let mut config = Config::default();
        config.terminal.scrollback_lines = 128;
        let mut grid = Grid::new_with_config(80, 2, Theme::default(), &config);
        let mut expected: VecDeque<(u64, Vec<Cell>)> = VecDeque::new();
        let mut dropped = 0;
        let mut seed = 19_u64;
        for step in 0..1000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let n = (seed >> 32) as usize;
            match if step % 100 < 80 { 0 } else { n % 4 } {
                0 => {
                    grid.set_cursor(0, 0);
                    grid.sgr(&[if n & 1 == 0 { 44 } else { 0 }]);
                    grid.erase_in_line(2);
                    grid.set_hyperlink((n & 2 == 0).then_some("https://example.com"));
                    for ch in ["a", "e\u{301}", "中é", "👨\u{200d}👩"][n % 4].chars() {
                        grid.put_char(ch);
                    }
                    expected.push_back((grid.cells[0].id, grid.cells[0].to_vec()));
                    grid.scroll_up(1);
                }
                1 => {
                    grid.set_theme(if n & 4 == 0 {
                        Theme::vscode()
                    } else {
                        Theme::default()
                    });
                    let blank = grid.blank_cell();
                    let width = [8, 16, 40, 80][n % 4];
                    grid.resize(width, 2);
                    for (_, cells) in &mut expected {
                        cells.resize(width, blank.clone());
                        Grid::fix_row_wide(cells, &blank);
                    }
                }
                2 => {
                    config.terminal.scrollback_lines = [0, 1, 17, 33, 64, 128][n % 6];
                    config.terminal.scrollback_bytes = [0, 512, 4096, 65536, 1024 * 1024][n % 5];
                    grid.apply_terminal_config(&config.terminal);
                }
                _ => grid.clear_scrollback(),
            }
            let delta = (grid.history_dropped() - dropped) as usize;
            assert!(delta <= expected.len());
            expected.drain(..delta);
            dropped = grid.history_dropped();
            assert_eq!(grid.scrollback_len(), expected.len());
            assert_eq!(
                grid.history_widths.values().map(|p| p.count).sum::<usize>(),
                expected.len()
            );
            assert!(grid.history_bytes() <= config.terminal.scrollback_bytes);
            for (line, (id, cells)) in expected.iter().enumerate() {
                let row = grid.global_line(line).unwrap();
                assert_eq!(row.identity().0, *id, "step {step}, line {line}");
                assert_eq!(row.to_vec(), *cells, "step {step}, line {line}");
            }
        }
    }
}
