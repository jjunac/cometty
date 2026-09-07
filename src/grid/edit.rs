//! Content mutation: cursor motion, write, erase, insert/delete.

use crate::grid::unicode::{char_width, continues_cluster, is_joining_modifier};

use super::Grid;

impl Grid {
    /// Previous cluster lead for a potential append at the cursor.
    /// Returns `(x, y)` of the lead cell when the cursor directly follows a
    /// cluster that `c` continues (combining / ZWJ / VS / skin tone / flag).
    fn append_target(&self, c: char) -> Option<(usize, usize)> {
        if c == '\0' {
            return None;
        }
        // Fast path: explicit joining modifiers always attach when there is
        // a previous cell.
        let joining = is_joining_modifier(c);
        // Locate the previous cell (same row, or end of previous row when at
        // column zero, mirroring xterm's combining-at-margin behavior).
        let (px, py) = if self.cursor.x > 0 {
            (self.cursor.x - 1, self.cursor.y)
        } else if self.cursor.y > 0 && !self.cells.is_empty() {
            (self.cols.saturating_sub(1), self.cursor.y - 1)
        } else {
            return None;
        };
        if py >= self.rows || px >= self.cols {
            return None;
        }
        // Resolve to the lead when the previous column is a continuation.
        let mut lx = px;
        if self.cells[py][px].width == 0 {
            if px == 0 {
                return None;
            }
            lx = px - 1;
            if self.cells[py][lx].width != 2 {
                return None;
            }
        } else if self.cells[py][px].width != 1 && self.cells[py][px].width != 2 {
            return None;
        }
        // Blank filler cells never take combining marks from a fresh line
        // start: e.g. a lone combining char at column zero starts its own
        // cell instead of attaching across lines to unrelated content.
        // (A real base such as `e` or `👨` always appends, including the
        // ZWJ-continuation case where `c` itself is wide.)
        let lead = &self.cells[py][lx];
        let prev_text = lead.cluster();
        let is_blank_filler = lead.ch == ' ' && lead.extra.is_none();
        if joining {
            if is_blank_filler {
                // Attaching a combining mark to a blank still renders better
                // than dropping it: keep the blank base, record the mark.
                // But don't reach across lines for blanks.
                if py != self.cursor.y {
                    return None;
                }
                return Some((lx, py));
            }
            return Some((lx, py));
        }
        if is_blank_filler {
            return None;
        }
        if continues_cluster(&prev_text, c) {
            return Some((lx, py));
        }
        None
    }

    /// True when `c` would append to the previous cluster instead of
    /// starting a new cell. `Terminal` checks this before pending-wrap
    /// handling so combining marks never trigger a wrap.
    pub fn is_append_continuation(&self, c: char) -> bool {
        self.append_target(c).is_some()
    }

    /// Append `c` to the cluster at `(x, y)` (resolving a continuation to
    /// its lead). Used for combining marks arriving while `pending_wrap`
    /// clamps the cursor onto the last cell. Never moves the cursor.
    pub fn append_to_cell(&mut self, x: usize, y: usize, c: char) -> bool {
        if c == '\0' || y >= self.rows || x >= self.cols {
            return false;
        }
        let mut lx = x;
        if self.cells[y][x].width == 0 {
            if x == 0 {
                return false;
            }
            lx = x - 1;
            if self.cells[y][lx].width != 2 {
                return false;
            }
        }
        let lead = &self.cells[y][lx];
        if lead.width != 1 && lead.width != 2 {
            return false;
        }
        let prev_text = lead.cluster();
        let is_blank = lead.ch == ' ' && lead.extra.is_none();
        if !is_joining_modifier(c) && (is_blank || !continues_cluster(&prev_text, c)) {
            return false;
        }
        // Joining modifiers attach even to blanks (lone mark fallback).
        let lead = &mut self.cells[y][lx];
        let mut s = lead.cluster();
        s.push(c);
        let mut chars = s.chars();
        if let Some(first) = chars.next() {
            let rest: String = chars.collect();
            lead.ch = first;
            lead.extra = if rest.is_empty() {
                None
            } else {
                Some(rest.into_boxed_str())
            };
        }
        self.bump();
        true
    }

    /// Append `c` to the previous cluster (combining / ZWJ / flag / VS).
    /// Never advances the cursor and never changes display width, so wrap
    /// state is untouched. Returns false when there was no target.
    pub fn append_to_prev(&mut self, c: char) -> bool {
        let Some((lx, ly)) = self.append_target(c) else {
            return false;
        };
        let lead = &mut self.cells[ly][lx];
        let mut s = lead.cluster();
        s.push(c);
        // Keep width stable (no 1->2 promotion): wide ZWJ sequences stay 2,
        // narrow bases with VS16 stay 1 and let the shaper ligate. This
        // avoids retroactive placeholder growth at the margin.
        let mut chars = s.chars();
        if let Some(first) = chars.next() {
            let rest: String = chars.collect();
            lead.ch = first;
            lead.extra = if rest.is_empty() {
                None
            } else {
                Some(rest.into_boxed_str())
            };
        }
        self.bump();
        true
    }

    /// Clear wide halves that a write of width `w` at `(x, y)` would split.
    fn clear_split_wide(&mut self, x: usize, y: usize, w: usize) {
        if y >= self.rows {
            return;
        }
        let blank = self.erase_cell();
        // Target is the continuation of a wide char: clear its lead.
        if x < self.cols && self.cells[y][x].width == 0 && x > 0 {
            self.cells[y][x - 1] = blank.clone();
        }
        // Narrow write over a wide lead: clear its continuation.
        if w == 1 && x < self.cols && self.cells[y][x].width == 2 && x + 1 < self.cols {
            self.cells[y][x + 1] = blank.clone();
        }
        // Wide write whose second half lands on a wide lead: clear that
        // char's continuation so it doesn't dangle.
        if w == 2 && x + 1 < self.cols && self.cells[y][x + 1].width == 2 && x + 2 < self.cols {
            self.cells[y][x + 2] = blank.clone();
        }
    }

    pub fn put_char(&mut self, ch: char) {
        if ch == '\0' {
            return;
        }
        // Combining / ZWJ / flag continuations attach to the previous
        // cluster without moving the cursor.
        if self.append_to_prev(ch) {
            return;
        }
        let mut w = char_width(ch).clamp(1, 2);
        self.stick_to_bottom();
        if self.cursor.y >= self.rows {
            self.scroll_up(1);
            self.cursor.y = self.rows.saturating_sub(1);
        }
        if self.cursor.x >= self.cols {
            if self.auto_wrap {
                self.newline();
            } else {
                self.cursor.x = self.cols.saturating_sub(1);
            }
        }
        // Wide char with only one column left: pad with a space and wrap,
        // matching xterm's `auto-wrap` behavior. With DECAWM off the wide
        // char degrades to narrow so the cursor stays on the last column.
        if w == 2 && self.cols >= 2 && self.cursor.x + 1 >= self.cols {
            if self.auto_wrap {
                let (cx, cy) = (self.cursor.x, self.cursor.y);
                if cx < self.cols && cy < self.rows {
                    self.cells[cy][cx] = self.erase_cell();
                }
                self.newline();
            } else {
                w = 1;
            }
        }
        // Insert mode (IRM): shift the line right before writing.
        if self.insert_mode
            && self.cursor.y < self.rows
            && self.cursor.x < self.cols
            && self.cursor.x + w <= self.cols
        {
            let (cx, cy) = (self.cursor.x, self.cursor.y);
            let shift = w;
            for i in (cx..self.cols.saturating_sub(shift)).rev() {
                let v = self.cells[cy][i].clone();
                self.cells[cy][i + shift] = v;
            }
            let fill = self.erase_cell();
            for i in cx..(cx + shift).min(self.cols) {
                self.cells[cy][i] = fill.clone();
            }
            // A shifted wide lead at the edge loses its continuation.
            let edge_fix = self.erase_cell();
            Self::fix_row_wide(&mut self.cells[cy], &edge_fix);
        }
        if self.cursor.y < self.rows && self.cursor.x < self.cols {
            // 1-column grid can't fit a wide char: degrade to narrow.
            let w = if w == 2 && self.cols == 1 { 1 } else { w };
            let (cx, cy) = (self.cursor.x, self.cursor.y);
            if cy < self.rows && cx < self.cols {
                self.clear_split_wide(cx, cy, w);
                self.cells[cy][cx] = super::cell::Cell {
                    ch,
                    extra: None,
                    width: w as u8,
                    fg: self.pen.fg,
                    bg: self.pen.bg,
                    bold: self.pen.bold,
                    underline: self.pen.underline,
                };
                if w == 2 && cx + 1 < self.cols {
                    self.cells[cy][cx + 1] = self.placeholder_cell();
                }
                self.cursor.x += w;
                if !self.auto_wrap && self.cursor.x >= self.cols {
                    // DECAWM off: stay on the last column, overwriting it.
                    self.cursor.x = self.cols.saturating_sub(1);
                }
            }
            if self.cursor.x >= self.cols {
                // Defer wrap until next printable or explicit newline handling
                // in Terminal: keep x at cols to signal pending wrap.
            }
            self.bump();
        }
    }

    pub fn handle_wrap_if_needed(&mut self) {
        if self.cursor.x >= self.cols {
            if self.auto_wrap {
                self.newline();
            } else {
                self.cursor.x = self.cols.saturating_sub(1);
            }
        }
    }

    pub fn newline(&mut self) {
        self.stick_to_bottom();
        self.cursor.x = 0;
        let max = self.rows.saturating_sub(1);
        let top = self.scroll_top.min(max);
        let bottom = self.scroll_bottom.min(max);
        let in_region = self.rows > 0 && self.cursor.y >= top && self.cursor.y <= bottom;
        if in_region {
            if self.cursor.y == bottom {
                if top == 0 && bottom == max {
                    // Full-screen scroll keeps history.
                    let cur_top = self.cells.remove(0);
                    if !self.in_alt {
                        if self.scrollback.len() >= self.max_scrollback {
                            self.scrollback.pop_front();
                        }
                        self.scrollback.push_back(cur_top);
                    }
                    self.cells.push(self.erase_row());
                } else {
                    self.scroll_region_up_inner(1);
                }
            } else {
                self.cursor.y += 1;
            }
        } else if self.rows > 0 {
            // Outside the margins: move down without scrolling.
            if self.cursor.y + 1 < self.rows {
                self.cursor.y += 1;
            }
        }
        self.bump();
    }

    /// Reverse index (`ESC M`): up one, scrolling the margin region down
    /// when at its top. No scroll when outside the region.
    pub fn reverse_index(&mut self) {
        self.stick_to_bottom();
        if self.rows == 0 {
            return;
        }
        let max = self.rows - 1;
        let top = self.scroll_top.min(max);
        let bottom = self.scroll_bottom.min(max);
        if self.cursor.y >= top && self.cursor.y <= bottom {
            if self.cursor.y == top {
                self.scroll_region_down_inner(1);
            } else {
                self.cursor.y -= 1;
            }
        } else if self.cursor.y > 0 {
            self.cursor.y -= 1;
        }
        self.snap_cursor_to_lead();
        self.bump();
    }

    pub fn carriage_return(&mut self) {
        self.cursor.x = 0;
        self.bump();
    }

    pub fn backspace(&mut self) {
        if self.cursor.x > 0 {
            // Step over the whole previous cluster: 2 for a wide char
            // (landing on its lead), 1 otherwise.
            let prev = self.cursor.x - 1;
            let step = if self.cursor.y < self.rows
                && prev < self.cols
                && self.cells[self.cursor.y][prev].width == 0
            {
                2
            } else if self.cursor.y < self.rows && prev < self.cols {
                self.cells[self.cursor.y][prev].width.max(1) as usize
            } else {
                1
            };
            self.cursor.x = self.cursor.x.saturating_sub(step);
            self.snap_cursor_to_lead();
            self.bump();
        }
    }

    pub fn tab(&mut self) {
        let next = ((self.cursor.x / 8) + 1) * 8;
        self.cursor.x = next.min(self.cols.saturating_sub(1));
        self.snap_cursor_to_lead();
        self.bump();
    }

    pub fn move_cursor(&mut self, dx: isize, dy: isize) {
        let nx = (self.cursor.x as isize + dx).clamp(0, self.cols.saturating_sub(1) as isize);
        let max_y = self.rows.saturating_sub(1) as isize;
        let ny = (self.cursor.y as isize + dy).clamp(0, max_y);
        self.cursor.x = nx as usize;
        self.cursor.y = ny as usize;
        self.snap_cursor_to_lead();
        self.clamp_cursor_to_margins();
        self.bump();
    }

    pub fn set_cursor(&mut self, x: usize, y: usize) {
        self.cursor.x = x.min(self.cols.saturating_sub(1));
        self.cursor.y = y.min(self.rows.saturating_sub(1));
        self.snap_cursor_to_lead();
        self.clamp_cursor_to_margins();
        self.bump();
    }

    /// After a range erase, dangling halves outside the range (lead without
    /// continuation or vice versa) are repaired to spaces.
    fn repair_wide_after_erase(&mut self) {
        let blank = self.erase_cell();
        for row in self.cells.iter_mut() {
            Self::fix_row_wide(row, &blank);
        }
    }

    pub fn erase_in_display(&mut self, mode: u16) {
        self.stick_to_bottom();
        match mode {
            0 => {
                // cursor to end
                let (cx, cy) = (self.cursor.x, self.cursor.y);
                for x in cx..self.cols {
                    self.cells[cy][x] = self.erase_cell();
                }
                for y in (cy + 1)..self.rows {
                    for x in 0..self.cols {
                        self.cells[y][x] = self.erase_cell();
                    }
                }
            }
            1 => {
                let (cx, cy) = (self.cursor.x, self.cursor.y);
                for y in 0..cy {
                    for x in 0..self.cols {
                        self.cells[y][x] = self.erase_cell();
                    }
                }
                for x in 0..=cx.min(self.cols.saturating_sub(1)) {
                    self.cells[cy][x] = self.erase_cell();
                }
            }
            2 => {
                self.clear_all();
            }
            3 => {
                self.clear_all();
                self.clear_scrollback();
            }
            _ => {}
        }
        self.repair_wide_after_erase();
        self.bump();
    }

    pub fn erase_in_line(&mut self, mode: u16) {
        self.stick_to_bottom();
        let y = self.cursor.y.min(self.rows.saturating_sub(1));
        match mode {
            0 => {
                for x in self.cursor.x..self.cols {
                    self.cells[y][x] = self.erase_cell();
                }
            }
            1 => {
                for x in 0..=self.cursor.x.min(self.cols.saturating_sub(1)) {
                    self.cells[y][x] = self.erase_cell();
                }
            }
            2 => {
                for x in 0..self.cols {
                    self.cells[y][x] = self.erase_cell();
                }
            }
            _ => {}
        }
        self.repair_wide_after_erase();
        self.bump();
    }

    pub fn clear_all(&mut self) {
        self.stick_to_bottom();
        let blank = self.erase_cell();
        for row in &mut self.cells {
            for c in row.iter_mut() {
                *c = blank.clone();
            }
        }
        self.bump();
    }

    pub fn insert_lines(&mut self, n: usize) {
        self.stick_to_bottom();
        if self.rows == 0 {
            self.bump();
            return;
        }
        let max = self.rows - 1;
        let top = self.scroll_top.min(max);
        let bottom = self.scroll_bottom.min(max);
        let y = self.cursor.y.min(max);
        // IL is a no-op when the cursor is outside the margins.
        if y < top || y > bottom {
            self.bump();
            return;
        }
        for _ in 0..n {
            if y > bottom || y >= self.cells.len() {
                break;
            }
            self.cells.insert(y, self.erase_row());
            // Drop the overflow inside the region, not at screen bottom.
            if bottom + 1 < self.cells.len() {
                self.cells.remove(bottom + 1);
            } else {
                self.cells.pop();
            }
        }
        self.bump();
    }

    pub fn delete_lines(&mut self, n: usize) {
        self.stick_to_bottom();
        if self.rows == 0 {
            self.bump();
            return;
        }
        let max = self.rows - 1;
        let top = self.scroll_top.min(max);
        let bottom = self.scroll_bottom.min(max);
        let y = self.cursor.y.min(max);
        // DL is a no-op when the cursor is outside the margins.
        if y < top || y > bottom {
            self.bump();
            return;
        }
        for _ in 0..n {
            if y > bottom || y >= self.cells.len() {
                break;
            }
            self.cells.remove(y);
            let fill = self.erase_row();
            if bottom <= self.cells.len() {
                self.cells.insert(bottom, fill);
            } else {
                self.cells.push(fill);
            }
        }
        self.bump();
    }
}
