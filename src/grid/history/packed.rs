//! Lossless, independently addressable rows in one bounded compact chunk.
//! ASCII columns need one byte and no glyph records. A trailing run of narrow
//! spaces needs no text bytes at all; styles still cover every column. Sparse
//! records preserve the original boundaries/widths of all nontrivial clusters.

use std::collections::VecDeque;

use crate::grid::cell::CellStyle;
use crate::grid::{CellRef, Row};

#[derive(Debug)]
struct PackedRow {
    id: u64,
    revision: u64,
    text_start: u32,
    text_cols: u32,
    special_start: u32,
    special_end: u32,
}

#[derive(Debug)]
struct Special {
    col: u32,
    start: u32,
    end: u32,
    width: u8,
    extra: bool,
}

#[derive(Clone, Copy, Debug)]
struct StyleRun {
    end: u32,
    style: CellStyle,
}

// Most scrolling batches have only one/two styles. Retain their already-scanned
// runs in bounded stack scratch rather than extracting/comparing every style a
// second time while packing. Diverse batches keep the old dense/adaptive path.
const PREFLIGHT_STYLES: usize = 64;

pub(super) struct Estimate {
    rows: usize,
    text: usize,
    special: usize,
    styles: usize,
    ends: [u32; super::storage::BATCH_ROWS],
    runs: [Option<StyleRun>; PREFLIGHT_STYLES],
}

impl Default for Estimate {
    fn default() -> Self {
        Self {
            rows: 0,
            text: 0,
            special: 0,
            styles: 0,
            ends: [0; super::storage::BATCH_ROWS],
            runs: [None; PREFLIGHT_STYLES],
        }
    }
}

impl Estimate {
    pub fn bytes(&self) -> usize {
        self.rows * size_of::<PackedRow>()
            + self.text
            + self.special * size_of::<Special>()
            + self.styles * size_of::<StyleRun>()
    }
}

#[derive(Debug, Default)]
pub(super) struct PackedChunk {
    cols: usize,
    rows: Vec<PackedRow>,
    text: String,
    special: Vec<Special>,
    styles: Vec<StyleRun>,
}

fn text_cols(row: &Row) -> usize {
    row.iter()
        .rposition(|c| c.ch != ' ' || c.width != 1 || c.extra.is_some())
        .map_or(0, |col| col + 1)
}

fn special(c: &crate::grid::Cell) -> bool {
    !c.ch.is_ascii() || c.width != 1 || c.extra.is_some()
}

impl PackedChunk {
    pub fn estimate(rows: &VecDeque<Row>) -> Option<Estimate> {
        let cols = rows.front()?.len();
        if rows.len() > super::storage::BATCH_ROWS {
            return None;
        }
        u32::try_from(cols.checked_mul(rows.len())?).ok()?;
        let mut estimate = Estimate {
            rows: rows.len(),
            ..Estimate::default()
        };
        let mut last_style = None;
        let mut column = 0;
        for (index, row) in rows.iter().enumerate() {
            if row.len() != cols {
                return None;
            }
            let end = text_cols(row);
            estimate.ends[index] = end as u32;
            for (col, c) in row.iter().enumerate() {
                let style = CellStyle::from(c);
                if last_style != Some(style) {
                    if let Some(Some(last)) = estimate
                        .styles
                        .checked_sub(1)
                        .and_then(|i| estimate.runs.get_mut(i))
                    {
                        last.end = column;
                    }
                    if let Some(run) = estimate.runs.get_mut(estimate.styles) {
                        *run = Some(StyleRun { end: 0, style });
                    }
                    estimate.styles += 1;
                    last_style = Some(style);
                }
                column += 1;
                if col < end {
                    estimate.text = estimate
                        .text
                        .checked_add(c.ch.len_utf8())?
                        .checked_add(c.extra.as_ref().map_or(0, |s| s.len()))?;
                    estimate.special += usize::from(special(c));
                }
            }
        }
        if let Some(Some(last)) = estimate
            .styles
            .checked_sub(1)
            .and_then(|i| estimate.runs.get_mut(i))
        {
            last.end = column;
        }
        u32::try_from(estimate.text).ok()?;
        Some(estimate)
    }

    pub fn pack(&mut self, rows: &VecDeque<Row>, estimate: &Estimate) {
        self.clear();
        self.cols = rows[0].len();
        self.rows.reserve_exact(estimate.rows);
        self.text.reserve_exact(estimate.text);
        self.special.reserve_exact(estimate.special);
        self.styles.reserve_exact(estimate.styles);
        if estimate.styles <= PREFLIGHT_STYLES {
            self.styles.extend(
                estimate.runs[..estimate.styles]
                    .iter()
                    .map(|r| r.expect("preflight style")),
            );
        } else {
            let mut column = 0;
            for row in rows {
                for c in row.iter() {
                    column += 1;
                    let style = CellStyle::from(c);
                    if let Some(last) = self.styles.last_mut().filter(|r| r.style == style) {
                        last.end = column;
                    } else {
                        self.styles.push(StyleRun { end: column, style });
                    }
                }
            }
        }
        for (index, row) in rows.iter().enumerate() {
            let mut record = PackedRow {
                id: row.id,
                revision: row.revision,
                text_start: self.text.len() as u32,
                text_cols: estimate.ends[index],
                special_start: self.special.len() as u32,
                special_end: 0,
            };
            for (col, c) in row[..record.text_cols as usize].iter().enumerate() {
                let start = self.text.len() as u32;
                self.text.push(c.ch);
                if let Some(extra) = &c.extra {
                    self.text.push_str(extra);
                }
                if special(c) {
                    self.special.push(Special {
                        col: col as u32,
                        start,
                        end: self.text.len() as u32,
                        width: c.width,
                        extra: c.extra.is_some(),
                    });
                }
            }
            record.special_end = self.special.len() as u32;
            self.rows.push(record);
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn bytes(&self) -> usize {
        self.rows.capacity() * size_of::<PackedRow>()
            + self.text.capacity()
            + self.special.capacity() * size_of::<Special>()
            + self.styles.capacity() * size_of::<StyleRun>()
    }

    pub fn clear(&mut self) {
        self.rows.clear();
        self.text.clear();
        self.special.clear();
        self.styles.clear();
    }

    pub fn row(&self, index: usize) -> PackedRowRef<'_> {
        PackedRowRef { chunk: self, index }
    }

    #[cfg(test)]
    pub fn storage_address(&self) -> usize {
        self.rows.as_ptr() as usize
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PackedRowRef<'a> {
    chunk: &'a PackedChunk,
    index: usize,
}

/// Forward scans advance through runs once, rather than binary-searching each
/// column. Random access and reverse trimming remain independently indexed.
#[derive(Default)]
pub(super) struct Cursor {
    special: usize,
    style: usize,
}

impl<'a> PackedRowRef<'a> {
    pub fn identity(self) -> (u64, u64) {
        let row = &self.chunk.rows[self.index];
        (row.id, row.revision)
    }

    pub fn len(self) -> usize {
        self.chunk.cols
    }

    /// Every stored byte is one narrow column; the omitted suffix is spaces.
    /// Search can consume this directly without decoding cells or style runs.
    pub fn ascii_text(self) -> Option<&'a str> {
        let row = &self.chunk.rows[self.index];
        (row.special_start == row.special_end).then(|| {
            &self.chunk.text
                [row.text_start as usize..row.text_start as usize + row.text_cols as usize]
        })
    }

    pub fn cursor(self) -> Cursor {
        Cursor {
            special: 0,
            style: self
                .chunk
                .styles
                .partition_point(|r| r.end as usize <= self.index * self.len()),
        }
    }

    pub fn get(self, col: usize) -> CellRef<'a> {
        let specials = self.specials();
        let mut cursor = Cursor {
            special: specials.partition_point(|s| (s.col as usize) < col),
            style: self
                .chunk
                .styles
                .partition_point(|r| r.end as usize <= self.index * self.len() + col),
        };
        self.get_next(col, &mut cursor)
    }

    fn specials(self) -> &'a [Special] {
        let row = &self.chunk.rows[self.index];
        &self.chunk.special[row.special_start as usize..row.special_end as usize]
    }

    pub fn get_next(self, col: usize, cursor: &mut Cursor) -> CellRef<'a> {
        let row = &self.chunk.rows[self.index];
        let specials = self.specials();
        while cursor.special < specials.len() && (specials[cursor.special].col as usize) < col {
            cursor.special += 1;
        }
        let column = self.index * self.len() + col;
        while self.chunk.styles[cursor.style].end as usize <= column {
            cursor.style += 1;
        }
        let style = self.chunk.styles[cursor.style].style;
        if col >= row.text_cols as usize {
            return CellRef {
                ch: ' ',
                extra: None,
                width: 1,
                style,
            };
        }
        if let Some(s) = specials
            .get(cursor.special)
            .filter(|s| s.col as usize == col)
        {
            let text = &self.chunk.text[s.start as usize..s.end as usize];
            let ch = text.chars().next().expect("stored cluster lead");
            return CellRef {
                ch,
                extra: s.extra.then(|| &text[ch.len_utf8()..]),
                width: s.width,
                style,
            };
        }
        let offset = cursor
            .special
            .checked_sub(1)
            .map_or(row.text_start as usize + col, |i| {
                let previous = &specials[i];
                previous.end as usize + col - previous.col as usize - 1
            });
        CellRef {
            ch: self.chunk.text.as_bytes()[offset] as char,
            extra: None,
            width: 1,
            style,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{Cell, UnderlineStyle};
    use crate::theme::Rgb;

    #[test]
    fn preflight_styles_cover_the_exact_fast_path_boundary_and_styled_blank_suffixes() {
        for runs in [
            1,
            2,
            PREFLIGHT_STYLES - 1,
            PREFLIGHT_STYLES,
            PREFLIGHT_STYLES + 1,
        ] {
            let rows: VecDeque<_> = (0..2)
                .map(|row| {
                    Row::new(
                        (0..128)
                            .map(|col| Cell {
                                ch: if col < 8 { 'x' } else { ' ' },
                                bg: Rgb::new(((row * 128 + col).min(runs - 1) % 2) as u8, 7, 9),
                                ..Cell::default()
                            })
                            .collect(),
                    )
                })
                .collect();
            let estimate = PackedChunk::estimate(&rows).unwrap();
            let mut packed = PackedChunk::default();
            packed.pack(&rows, &estimate);
            assert_eq!(packed.bytes(), estimate.bytes());
            for (i, row) in rows.iter().enumerate() {
                for (col, cell) in row.iter().enumerate() {
                    assert_eq!(packed.row(i).get(col), CellRef::from(cell), "runs={runs}");
                }
            }
        }
    }

    #[test]
    fn ascii_and_blank_tails_share_chunk_storage_and_styles() {
        let rows: VecDeque<_> = (0..32)
            .map(|_| {
                let mut cells = vec![Cell::default(); 200];
                for (c, ch) in cells.iter_mut().zip("hello".chars()) {
                    c.ch = ch;
                }
                Row::new(cells)
            })
            .collect();
        let estimate = PackedChunk::estimate(&rows).unwrap();
        let mut packed = PackedChunk::default();
        packed.pack(&rows, &estimate);
        assert_eq!(packed.text.len(), 32 * 5);
        assert!(packed.special.is_empty());
        assert_eq!(
            packed.styles.len(),
            1,
            "styles coalesce across row boundaries"
        );
        assert!(packed.bytes() < 32 * 200 * size_of::<Cell>() / 100);
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(packed.row(i).identity(), (row.id, row.revision));
            for (col, cell) in row.iter().enumerate() {
                assert_eq!(packed.row(i).get(col), CellRef::from(cell));
            }
        }
    }

    #[test]
    fn arbitrary_clusters_continuations_and_all_attributes_round_trip() {
        let chars = ['a', ' ', '中', 'é', '👨', '\0', 'İ'];
        let extras = [
            None,
            Some(""),
            Some("\u{301}"),
            Some("\u{200d}👩\u{200d}👧"),
            Some("x y"),
        ];
        let underlines = [
            UnderlineStyle::None,
            UnderlineStyle::Single,
            UnderlineStyle::Double,
            UnderlineStyle::Curly,
            UnderlineStyle::Dotted,
            UnderlineStyle::Dashed,
        ];
        let mut seed = 7_u64;
        let mut rows = VecDeque::new();
        for _ in 0..32 {
            let cells = (0..79)
                .map(|_| {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let n = (seed >> 32) as usize;
                    Cell {
                        ch: chars[n % chars.len()],
                        extra: extras[(n / 7) % extras.len()].map(Into::into),
                        width: ((n / 35) % 3) as u8,
                        fg: Rgb::new(17, (n >> 8) as u8, 199),
                        bg: Rgb::new((n >> 16) as u8, 83, 5),
                        bold: n & 1 != 0,
                        dim: n & 2 != 0,
                        italic: n & 4 != 0,
                        inverse: n & 8 != 0,
                        strikethrough: n & 16 != 0,
                        overline: n & 32 != 0,
                        underline: underlines[(n / 105) % underlines.len()],
                        underline_color: (n & 64 != 0).then_some(Rgb::new(1, 2, 3)),
                        hyperlink: (n & 128 != 0).then_some(n as u16),
                    }
                })
                .collect();
            rows.push_back(Row::new(cells));
        }
        let estimate = PackedChunk::estimate(&rows).unwrap();
        let mut packed = PackedChunk::default();
        packed.pack(&rows, &estimate);
        for (i, original) in rows.iter().enumerate() {
            let row = packed.row(i);
            let mut cursor = row.cursor();
            for (col, cell) in original.iter().enumerate() {
                assert_eq!(row.get(col).to_cell(), *cell, "random row {i}, col {col}");
                assert_eq!(
                    row.get_next(col, &mut cursor).to_cell(),
                    *cell,
                    "scan row {i}, col {col}"
                );
            }
        }
        assert_eq!(packed.bytes(), estimate.bytes());
    }
}
