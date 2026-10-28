//! Logical row identity is independent of screen position and buffer storage.
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};

use super::Cell;

static NEXT_ROW: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub id: u64,
    pub revision: u64,
    cells: Vec<Cell>,
}

impl Clone for Row {
    fn clone(&self) -> Self {
        // A copied row is new storage: it must never masquerade as the row
        // whose identity caches (search/snapshot) captured.
        Self {
            id: NEXT_ROW.fetch_add(1, Ordering::Relaxed),
            revision: 0,
            cells: self.cells.clone(),
        }
    }
}

impl Row {
    pub fn new(cells: Vec<Cell>) -> Self {
        Self {
            id: NEXT_ROW.fetch_add(1, Ordering::Relaxed),
            revision: 0,
            cells,
        }
    }

    pub fn recycle(&mut self, cols: usize, fill: &Cell) {
        self.id = NEXT_ROW.fetch_add(1, Ordering::Relaxed);
        self.revision = 0;
        self.cells.clear();
        // A formerly very wide row must not become a permanently oversized
        // spare. There is no free list: only one directly recycled row.
        if self.cells.capacity() > cols.saturating_mul(2) {
            self.cells = Vec::new();
        }
        self.cells.resize(cols, fill.clone());
    }

    pub fn bytes(&self) -> usize {
        self.cells.capacity() * size_of::<Cell>()
            + self
                .cells
                .iter()
                .map(|c| c.extra.as_ref().map_or(0, String::capacity))
                .sum::<usize>()
    }
}

impl Deref for Row {
    type Target = Vec<Cell>;
    fn deref(&self) -> &Self::Target {
        &self.cells
    }
}

impl DerefMut for Row {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.revision = self.revision.wrapping_add(1);
        &mut self.cells
    }
}

impl AsRef<[Cell]> for Row {
    fn as_ref(&self) -> &[Cell] {
        &self.cells
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_tail_capacity_not_just_length_is_charged_and_released() {
        let mut tail = String::with_capacity(8192);
        tail.push('\u{301}');
        let mut row = Row::new(vec![Cell {
            extra: Some(tail),
            ..Cell::default()
        }]);
        assert_eq!(row.bytes(), row.capacity() * size_of::<Cell>() + 8192);
        row.recycle(1, &Cell::default());
        assert_eq!(row.bytes(), row.capacity() * size_of::<Cell>());
    }
}
