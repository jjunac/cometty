//! Scrollback storage + view-offset math.

use crate::grid::Cell;

use super::Grid;

impl Grid {
    /// Rows actually on screen, accounting for scrollback viewing.
    /// `scroll_offset == 0` is the live view; `> 0` looks that many lines up.
    pub fn view_rows(&self) -> Vec<&Vec<Cell>> {
        let off = self.scroll_offset.min(self.scrollback.len());
        if off == 0 || self.rows == 0 {
            return self.cells.iter().collect();
        }
        let sb = self.scrollback.len();
        (0..self.rows)
            .map(|i| {
                let global = sb - off + i;
                if global < sb {
                    &self.scrollback[global]
                } else {
                    &self.cells[global - sb]
                }
            })
            .collect()
    }

    // Read accessor for tests/inspection; the app itself renders in bulk via
    // `visible_rows`, so silence the binary-crate `dead_code` lint.
    #[allow(dead_code)]
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// Total lines in global space: scrollback + visible.
    #[allow(dead_code)]
    pub fn total_lines(&self) -> usize {
        self.scrollback.len() + self.rows
    }

    /// Chars of a global line (`0` = oldest scrollback). Returns `None`
    /// when out of range. Used by selection text extraction.
    /// Legacy single-char view (first char of each cluster); prefer
    /// [`Self::global_line_cells`] for Unicode-aware copy.
    #[allow(dead_code)]
    pub fn global_line_chars(&self, global: usize) -> Option<Vec<char>> {
        self.global_line_cells(global)
            .map(|row| row.iter().map(|c| c.ch).collect())
    }

    /// Full cells of a global line for Unicode-aware selection copy.
    pub fn global_line_cells(&self, global: usize) -> Option<Vec<Cell>> {
        self.global_line(global).map(|row| row.to_vec())
    }

    /// Borrowed cells of a global line, for hot read-only paths (search)
    /// that must not clone a whole scrollback row per line.
    pub fn global_line(&self, global: usize) -> Option<&[Cell]> {
        let sb = self.scrollback.len();
        if global < sb {
            self.scrollback.get(global).map(|row| row.as_slice())
        } else {
            self.cells.get(global - sb).map(|row| row.as_slice())
        }
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset.min(self.scrollback.len())
    }

    /// Scroll the view by `delta` lines (`> 0` = up into history).
    /// No-op in the alt buffer. Returns true if the view changed.
    pub fn scroll_by(&mut self, delta: isize) -> bool {
        if self.in_alt {
            return false;
        }
        let max = self.scrollback.len() as isize;
        let cur = self.scroll_offset.min(self.scrollback.len()) as isize;
        let next = (cur + delta).clamp(0, max) as usize;
        if next == self.scroll_offset {
            return false;
        }
        self.scroll_offset = next;
        self.bump_view();
        true
    }

    pub fn scroll_to_top(&mut self) -> bool {
        if self.in_alt {
            return false;
        }
        self.scroll_by(self.scrollback.len() as isize)
    }

    pub fn scroll_to_bottom(&mut self) -> bool {
        if self.scroll_offset == 0 {
            return false;
        }
        self.scroll_offset = 0;
        self.bump_view();
        true
    }

    /// Jump directly to an absolute scroll offset (0 = live bottom).
    /// Used by the GUI scrollbar drag. No-op in the alt buffer.
    pub fn scroll_to_offset(&mut self, offset: usize) -> bool {
        if self.in_alt {
            return false;
        }
        let next = offset.min(self.scrollback.len());
        if next == self.scroll_offset {
            return false;
        }
        self.scroll_offset = next;
        self.bump_view();
        true
    }

    pub fn clear_scrollback(&mut self) {
        if self.scrollback.is_empty() && self.scroll_offset == 0 {
            return;
        }
        self.scrollback.clear();
        self.scroll_offset = 0;
        self.bump();
    }

    pub(crate) fn stick_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    /// Scroll margins (DECSTBM), 0-based inclusive. Full screen when
    /// `top == 0 && bottom + 1 == rows`.
    pub fn scroll_region(&self) -> (usize, usize) {
        (self.scroll_top, self.scroll_bottom)
    }

    pub fn is_full_region(&self) -> bool {
        self.rows == 0 || (self.scroll_top == 0 && self.scroll_bottom + 1 >= self.rows)
    }

    fn clamp_region(&self, top: usize, bottom: usize) -> (usize, usize) {
        if self.rows == 0 {
            return (0, 0);
        }
        let max = self.rows - 1;
        (top.min(max), bottom.min(max))
    }

    /// Set scroll margins (0-based inclusive). Returns false when the
    /// request is invalid (`top >= bottom`): the region is left unchanged
    /// and the caller should ignore the sequence.
    pub fn set_scroll_region(&mut self, top: usize, bottom: usize) -> bool {
        let (top, bottom) = self.clamp_region(top, bottom);
        if top >= bottom {
            return false;
        }
        self.scroll_top = top;
        self.scroll_bottom = bottom;
        // Cursor homes on DECSTBM (origin-aware).
        self.home_cursor();
        self.bump();
        true
    }

    /// Reset margins to the full screen without moving the cursor.
    /// Used by resize / alt-buffer switches / full reset.
    pub fn reset_scroll_region(&mut self) {
        self.scroll_top = 0;
        self.scroll_bottom = self.rows.saturating_sub(1);
    }

    /// Origin-aware cursor home: `(0, top)` in origin mode, else `(0, 0)`.
    pub fn home_cursor(&mut self) {
        let y = if self.origin_mode {
            self.scroll_top.min(self.rows.saturating_sub(1))
        } else {
            0
        };
        self.cursor.x = 0;
        self.cursor.y = y;
        self.snap_cursor_to_lead();
    }

    /// Clamp the cursor into the margins. Used after absolute moves while
    /// origin mode is on.
    pub fn clamp_cursor_to_margins(&mut self) {
        if self.origin_mode && self.rows > 0 {
            let max = self.rows.saturating_sub(1);
            let top = self.scroll_top.min(max);
            let bottom = self.scroll_bottom.min(max);
            if self.cursor.y < top {
                self.cursor.y = top;
            } else if self.cursor.y > bottom {
                self.cursor.y = bottom;
            }
        }
    }

    pub(crate) fn scroll_region_up_inner(&mut self, n: usize) {
        if self.rows == 0 {
            return;
        }
        let max = self.rows - 1;
        let top = self.scroll_top.min(max);
        let bottom = self.scroll_bottom.min(max);
        if top >= bottom && n > 0 {
            return;
        }
        for _ in 0..n {
            if top >= self.cells.len() || bottom >= self.cells.len() {
                break;
            }
            self.cells.remove(top);
            // After removal the region shrank by one; re-insert fill at
            // `bottom` so only `[top..=bottom]` rotates.
            let fill = self.erase_row();
            if bottom <= self.cells.len() {
                self.cells.insert(bottom, fill);
            } else {
                self.cells.push(fill);
            }
        }
    }

    pub(crate) fn scroll_region_down_inner(&mut self, n: usize) {
        if self.rows == 0 {
            return;
        }
        let max = self.rows - 1;
        let top = self.scroll_top.min(max);
        let bottom = self.scroll_bottom.min(max);
        if top >= bottom && n > 0 {
            return;
        }
        for _ in 0..n {
            if top >= self.cells.len() || bottom >= self.cells.len() {
                break;
            }
            self.cells.remove(bottom);
            let fill = self.erase_row();
            self.cells.insert(top, fill);
        }
    }

    /// Scroll the margin region up (no scrollback). Full-region callers
    /// that want history should use [`Self::scroll_up`].
    #[allow(dead_code)]
    pub fn scroll_region_up(&mut self, n: usize) {
        self.stick_to_bottom();
        self.scroll_region_up_inner(n);
        self.bump();
    }

    /// Scroll the margin region down (no scrollback).
    #[allow(dead_code)]
    pub fn scroll_region_down(&mut self, n: usize) {
        self.stick_to_bottom();
        self.scroll_region_down_inner(n);
        self.bump();
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.stick_to_bottom();
        if !self.is_full_region() {
            self.scroll_region_up_inner(n);
            self.bump();
            return;
        }
        for _ in 0..n {
            if self.rows == 0 {
                break;
            }
            let top = self.cells.remove(0);
            if !self.in_alt {
                if self.scrollback.len() >= self.max_scrollback {
                    self.scrollback.pop_front();
                }
                // Don't store fully blank default lines to save memory? Keep for simplicity.
                self.scrollback.push_back(top);
            }
            self.cells.push(self.erase_row());
        }
        self.bump();
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.stick_to_bottom();
        if !self.is_full_region() {
            self.scroll_region_down_inner(n);
            self.bump();
            return;
        }
        for _ in 0..n {
            if self.cells.is_empty() {
                break;
            }
            self.cells.pop();
            self.cells.insert(0, self.erase_row());
        }
        self.bump();
    }
}
