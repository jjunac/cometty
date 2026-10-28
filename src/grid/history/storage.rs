//! A bounded dense tail and immutable compact chunks. Row-level eviction keeps
//! at most one partially expired front chunk, whose complete allocation stays
//! charged. No global interner, worker queue or decompressed-history cache.

use std::collections::VecDeque;

use super::packed::{PackedChunk, PackedRowRef};
use crate::grid::Row;

pub(super) const BATCH_ROWS: usize = 32;
pub(super) const BATCH_BYTES: usize = 256 * 1024;

#[derive(Debug)]
struct Dense {
    rows: VecDeque<Row>,
    cell_bytes: usize,
}

#[derive(Debug)]
enum Storage {
    Dense(Dense),
    Packed { data: PackedChunk, head: usize },
}

#[derive(Debug)]
struct Chunk {
    first: u64,
    cohort: u64,
    sealed: bool,
    storage: Storage,
}

impl Chunk {
    fn len(&self) -> usize {
        match &self.storage {
            Storage::Dense(d) => d.rows.len(),
            Storage::Packed { data, head } => data.len() - head,
        }
    }

    fn bytes(&self) -> usize {
        match &self.storage {
            Storage::Dense(d) => d.cell_bytes + d.rows.capacity() * size_of::<Row>(),
            Storage::Packed { data, .. } => data.bytes(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum StoredRow<'a> {
    Dense(&'a Row),
    Packed(PackedRowRef<'a>),
}

impl StoredRow<'_> {
    pub fn identity(self) -> (u64, u64) {
        match self {
            Self::Dense(row) => (row.id, row.revision),
            Self::Packed(row) => row.identity(),
        }
    }

    pub fn len(self) -> usize {
        match self {
            Self::Dense(row) => row.len(),
            Self::Packed(row) => row.len(),
        }
    }
}

#[derive(Debug, Default)]
pub(in crate::grid) struct History {
    chunks: VecDeque<Chunk>,
    len: usize,
    next: u64,
    chunk_bytes: usize,
    // Dense buffers released together by sealing are consumed by subsequent
    // live rows. Both pools have hard bounds and are charged to history.
    spare_rows: Vec<Row>,
    spare_bytes: usize,
    empty_rows: VecDeque<Row>,
    reusable: Option<PackedChunk>,
}

impl History {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn bytes(&self) -> usize {
        self.chunk_bytes
            + self.chunks.capacity() * size_of::<Chunk>()
            + self.spare_bytes
            + self.spare_rows.capacity() * size_of::<Row>()
            + self.empty_rows.capacity() * size_of::<Row>()
            + self.reusable.as_ref().map_or(0, PackedChunk::bytes)
    }

    pub(super) fn get(&self, index: usize) -> Option<(u64, StoredRow<'_>)> {
        if index >= self.len {
            return None;
        }
        let target = self.next - self.len as u64 + index as u64;
        // Index by stable admission sequence, not a linear walk over chunks.
        let mut low = 0;
        let mut high = self.chunks.len();
        while low < high {
            let mid = low + (high - low) / 2;
            let chunk = &self.chunks[mid];
            if chunk.first + chunk.len() as u64 <= target {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        let chunk = &self.chunks[low];
        let offset = (target - chunk.first) as usize;
        let row = match &chunk.storage {
            Storage::Dense(d) => StoredRow::Dense(&d.rows[offset]),
            Storage::Packed { data, head } => StoredRow::Packed(data.row(head + offset)),
        };
        Some((chunk.cohort, row))
    }

    pub fn push(&mut self, row: Row, cohort: u64) {
        let bytes = row.bytes();
        let fits = self.chunks.back().is_some_and(|chunk| {
            !chunk.sealed && chunk.cohort == cohort && matches!(&chunk.storage,
                Storage::Dense(d) if d.rows.len() < BATCH_ROWS && d.cell_bytes.saturating_add(bytes) <= BATCH_BYTES)
        });
        if !fits {
            self.seal();
            let chunk = Chunk {
                first: self.next,
                cohort,
                sealed: false,
                storage: Storage::Dense(Dense {
                    rows: std::mem::take(&mut self.empty_rows),
                    cell_bytes: 0,
                }),
            };
            self.chunk_bytes += chunk.bytes();
            self.chunks.push_back(chunk);
        }
        let chunk = self.chunks.back_mut().expect("history tail");
        self.chunk_bytes -= chunk.bytes();
        let Storage::Dense(dense) = &mut chunk.storage else {
            unreachable!()
        };
        dense.cell_bytes += bytes;
        dense.rows.push_back(row);
        let seal = dense.rows.len() == BATCH_ROWS || dense.cell_bytes >= BATCH_BYTES;
        self.chunk_bytes += chunk.bytes();
        self.len += 1;
        self.next += 1;
        if seal {
            self.seal();
        }
    }

    pub fn seal(&mut self) {
        let Some(chunk) = self.chunks.back_mut().filter(|c| !c.sealed) else {
            return;
        };
        chunk.sealed = true;
        let Storage::Dense(dense) = &chunk.storage else {
            return;
        };
        let _timing = crate::perf::measure(crate::perf::Metric::HistorySeal);
        // Do not duplicate a giant cluster or exceptionally wide row just to
        // discover it is incompressible. The ordinary batch's source is bounded.
        if dense.cell_bytes > BATCH_BYTES {
            return;
        }
        let Some(estimate) = PackedChunk::estimate(&dense.rows) else {
            return;
        };
        // Marginal savings are not worth slowing high-entropy Unicode/style
        // data. Keep at least one eighth of the original allocation saved.
        if estimate.bytes() > chunk.bytes() - chunk.bytes() / 8 {
            return;
        }
        let mut packed = self.reusable.take().unwrap_or_default();
        if packed.bytes() > estimate.bytes().saturating_mul(2) {
            packed = PackedChunk::default();
        }
        packed.pack(&dense.rows, &estimate);
        if packed.bytes() > chunk.bytes() - chunk.bytes() / 8 {
            // An oversized reusable allocation must never turn a saving into
            // extra retained memory. Keep dense storage in this rare case.
            return;
        }
        let old_bytes = chunk.bytes();
        let Storage::Dense(mut old) = std::mem::replace(
            &mut chunk.storage,
            Storage::Packed {
                data: packed,
                head: 0,
            },
        ) else {
            unreachable!()
        };
        self.chunk_bytes = self.chunk_bytes - old_bytes + chunk.bytes();
        for mut row in old.rows.drain(..) {
            row.clear(); // free tails, keep only reusable cell capacity
            let bytes = row.bytes();
            if self.spare_rows.len() < BATCH_ROWS
                && self.spare_bytes.saturating_add(bytes) <= BATCH_BYTES
            {
                self.spare_rows.push(row);
                self.spare_bytes += bytes;
            }
        }
        self.empty_rows = old.rows;
    }

    pub fn take_spare(&mut self) -> Option<Row> {
        let row = self.spare_rows.pop()?;
        self.spare_bytes -= row.bytes();
        Some(row)
    }

    pub fn pop_front(&mut self) -> Option<(u64, Option<Row>)> {
        let chunk = self.chunks.front_mut()?;
        let old_bytes = chunk.bytes();
        let cohort = chunk.cohort;
        let row = match &mut chunk.storage {
            Storage::Dense(d) => {
                let row = d.rows.pop_front().expect("nonempty history chunk");
                d.cell_bytes -= row.bytes();
                Some(row)
            }
            Storage::Packed { head, .. } => {
                *head += 1;
                None
            }
        };
        chunk.first += 1;
        self.len -= 1;
        if chunk.len() == 0 {
            let chunk = self.chunks.pop_front().unwrap();
            self.chunk_bytes -= old_bytes;
            match chunk.storage {
                Storage::Packed { mut data, .. } => {
                    data.clear();
                    if data.bytes() <= BATCH_BYTES {
                        self.reusable = Some(data);
                    }
                }
                Storage::Dense(d) => self.empty_rows = d.rows,
            }
        } else {
            self.chunk_bytes = self.chunk_bytes - old_bytes + chunk.bytes();
        }
        Some((cohort, row))
    }

    pub fn discard_spares(&mut self) {
        self.spare_rows = Vec::new();
        self.spare_bytes = 0;
        self.empty_rows = VecDeque::new();
        self.reusable = None;
    }

    pub fn release_unused(&mut self) {
        self.discard_spares();
        if self.is_empty() {
            self.chunks = VecDeque::new();
        } else {
            self.chunks.shrink_to_fit();
        }
    }

    #[cfg(test)]
    pub fn storage_addresses(&self) -> Vec<usize> {
        self.chunks
            .iter()
            .map(|c| match &c.storage {
                Storage::Dense(d) => d.rows.front().unwrap().as_ptr() as usize,
                Storage::Packed { data, .. } => data.storage_address(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Cell;

    fn assert_accounting(history: &History) {
        assert_eq!(
            history.chunk_bytes,
            history.chunks.iter().map(Chunk::bytes).sum::<usize>()
        );
        assert_eq!(
            history.len,
            history.chunks.iter().map(Chunk::len).sum::<usize>()
        );
        assert_eq!(
            history.spare_bytes,
            history.spare_rows.iter().map(Row::bytes).sum::<usize>()
        );
        assert!(history.spare_bytes <= BATCH_BYTES);
        assert!(history.spare_rows.len() <= BATCH_ROWS);
        assert!(history.empty_rows.is_empty());
    }

    #[test]
    fn chunks_are_bounded_by_rows_bytes_and_cohorts() {
        let mut history = History::default();
        for _ in 0..100 {
            history.push(Row::new(vec![Cell::default(); 128]), 0);
        }
        assert_eq!(history.chunks.len(), 4);
        for chunk in history.chunks.iter().take(3) {
            assert!(matches!(chunk.storage, Storage::Packed { .. }));
            assert_eq!(chunk.len(), BATCH_ROWS);
        }
        history.push(Row::new(vec![Cell::default(); 128]), 1);
        assert_eq!(history.chunks.len(), 5);
        assert!(history.chunks[3].sealed);
        assert_accounting(&history);

        let mut history = History::default();
        let width = BATCH_BYTES / size_of::<Cell>() / 3;
        for _ in 0..7 {
            history.push(Row::new(vec![Cell::default(); width]), 0);
        }
        assert_eq!(history.chunks.len(), 3);
        for chunk in history.chunks.iter().take(2) {
            assert_eq!(chunk.len(), 3);
        }
        assert_accounting(&history);

        let mut history = History::default();
        for _ in 0..2 {
            history.push(
                Row::new(vec![Cell {
                    extra: Some("x".repeat(BATCH_BYTES / 2 - size_of::<Cell>())),
                    ..Cell::default()
                }]),
                0,
            );
        }
        assert!(
            history.chunks[0].sealed,
            "seal at the exact cumulative source-byte bound"
        );
        assert_eq!(history.chunks[0].len(), 2);
        assert_accounting(&history);
    }

    #[test]
    fn partial_front_eviction_preserves_every_other_row_and_charges_dead_prefix() {
        let mut history = History::default();
        let mut ids = VecDeque::new();
        for n in 0..70 {
            let row = Row::new(vec![
                Cell {
                    ch: char::from(b'a' + n % 26),
                    ..Cell::default()
                };
                160
            ]);
            ids.push_back(row.id);
            history.push(row, 42);
        }
        let first_bytes = history.chunks[0].bytes();
        for n in 0..70 {
            for (index, id) in ids.iter().enumerate() {
                assert_eq!(history.get(index).unwrap().1.identity().0, *id);
            }
            assert!(history.get(ids.len()).is_none());
            assert_eq!(history.pop_front().unwrap().0, 42);
            ids.pop_front();
            if n < BATCH_ROWS - 1 {
                assert_eq!(history.chunks[0].bytes(), first_bytes);
            }
            assert_accounting(&history);
        }
        history.release_unused();
        assert_eq!(history.bytes(), 0);
    }

    #[test]
    fn oversized_rows_and_incompressible_batches_stay_dense_without_giant_copy() {
        let huge = "x".repeat(BATCH_BYTES + 1);
        let row = Row::new(vec![Cell {
            extra: Some(huge),
            ..Cell::default()
        }]);
        let mut history = History::default();
        let counts = crate::perf_bench::allocation_stats(|| history.push(row, 0));
        assert!(
            counts.largest_request < 4096,
            "oversized tail is moved, not copied"
        );
        assert!(matches!(history.get(0).unwrap().1, StoredRow::Dense(_)));
        assert!(history.chunks[0].sealed);

        // Full Unicode/attribute diversity saves too little to justify the
        // extra lookup work. The adaptive path retains the batch losslessly.
        let mut id = 0;
        for i in 0..BATCH_ROWS {
            let row = Row::new(
                (0..80)
                    .map(|col| Cell {
                        ch: '🦀',
                        extra: Some("漢".repeat(32)),
                        width: 2,
                        fg: crate::theme::Rgb::new(col, i as u8, 1),
                        ..Cell::default()
                    })
                    .collect(),
            );
            if i == 0 {
                id = row.id;
            }
            history.push(row, 1);
        }
        assert!(matches!(history.get(1).unwrap().1, StoredRow::Dense(_)));
        assert_eq!(history.get(1).unwrap().1.identity().0, id);
        assert_accounting(&history);
    }
}
