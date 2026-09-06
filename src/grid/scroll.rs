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
    pub fn global_line_chars(&self, global: usize) -> Option<Vec<char>> {
        let sb = self.scrollback.len();
        if global < sb {
            Some(self.scrollback[global].iter().map(|c| c.ch).collect())
        } else {
            self.cells
                .get(global - sb)
                .map(|row| row.iter().map(|c| c.ch).collect())
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
        self.bump();
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
        self.bump();
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
        self.bump();
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

    /// New cell output sticks the view to the live bottom.
    /// Called at the top of content-mutating ops (their trailing `bump`
    /// covers the version change, so this deliberately doesn't bump).
    pub(crate) fn stick_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.stick_to_bottom();
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
