//! Content mutation: cursor motion, write, erase, insert/delete.

use crate::grid::Cell;

use super::Grid;

impl Grid {
    pub fn put_char(&mut self, ch: char) {
        if ch == '\0' {
            return;
        }
        self.stick_to_bottom();
        if self.cursor.y >= self.rows {
            self.scroll_up(1);
            self.cursor.y = self.rows.saturating_sub(1);
        }
        if self.cursor.x >= self.cols {
            self.newline();
        }
        if self.cursor.y < self.rows && self.cursor.x < self.cols {
            self.cells[self.cursor.y][self.cursor.x] = Cell {
                ch,
                fg: self.pen.fg,
                bg: self.pen.bg,
                bold: self.pen.bold,
                underline: self.pen.underline,
            };
            self.cursor.x += 1;
            if self.cursor.x >= self.cols {
                // Defer wrap until next printable or explicit newline handling
                // in Terminal: keep x at cols to signal pending wrap.
            }
            self.bump();
        }
    }

    pub fn handle_wrap_if_needed(&mut self) {
        if self.cursor.x >= self.cols {
            self.newline();
        }
    }

    pub fn newline(&mut self) {
        self.stick_to_bottom();
        self.cursor.x = 0;
        if self.cursor.y + 1 >= self.rows {
            self.scroll_up(1);
        } else {
            self.cursor.y += 1;
        }
        self.bump();
    }

    pub fn carriage_return(&mut self) {
        self.cursor.x = 0;
        self.bump();
    }

    pub fn backspace(&mut self) {
        if self.cursor.x > 0 {
            self.cursor.x -= 1;
            self.bump();
        }
    }

    pub fn tab(&mut self) {
        let next = ((self.cursor.x / 8) + 1) * 8;
        self.cursor.x = next.min(self.cols.saturating_sub(1));
        self.bump();
    }

    pub fn move_cursor(&mut self, dx: isize, dy: isize) {
        let nx = (self.cursor.x as isize + dx).clamp(0, self.cols.saturating_sub(1) as isize);
        let ny = (self.cursor.y as isize + dy).clamp(0, self.rows.saturating_sub(1) as isize);
        self.cursor.x = nx as usize;
        self.cursor.y = ny as usize;
        self.bump();
    }

    pub fn set_cursor(&mut self, x: usize, y: usize) {
        self.cursor.x = x.min(self.cols.saturating_sub(1));
        self.cursor.y = y.min(self.rows.saturating_sub(1));
        self.bump();
    }

    pub fn erase_in_display(&mut self, mode: u16) {
        self.stick_to_bottom();
        match mode {
            0 => {
                // cursor to end
                let (cx, cy) = (self.cursor.x, self.cursor.y);
                for x in cx..self.cols {
                    // Erase uses current bg but blank fg? Use theme fg with current bg.
                    self.cells[cy][x] = Cell {
                        ch: ' ',
                        fg: self.theme.foreground,
                        bg: self.pen.bg,
                        bold: false,
                        underline: false,
                    };
                }
                for y in (cy + 1)..self.rows {
                    for x in 0..self.cols {
                        self.cells[y][x] = Cell {
                            ch: ' ',
                            fg: self.theme.foreground,
                            bg: self.pen.bg,
                            bold: false,
                            underline: false,
                        };
                    }
                }
            }
            1 => {
                let (cx, cy) = (self.cursor.x, self.cursor.y);
                for y in 0..cy {
                    for x in 0..self.cols {
                        self.cells[y][x] = self.blank_cell();
                    }
                }
                for x in 0..=cx.min(self.cols.saturating_sub(1)) {
                    self.cells[cy][x] = self.blank_cell();
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
        self.bump();
    }

    pub fn erase_in_line(&mut self, mode: u16) {
        self.stick_to_bottom();
        let y = self.cursor.y.min(self.rows.saturating_sub(1));
        match mode {
            0 => {
                for x in self.cursor.x..self.cols {
                    self.cells[y][x] = self.blank_cell();
                }
            }
            1 => {
                for x in 0..=self.cursor.x.min(self.cols.saturating_sub(1)) {
                    self.cells[y][x] = self.blank_cell();
                }
            }
            2 => {
                for x in 0..self.cols {
                    self.cells[y][x] = self.blank_cell();
                }
            }
            _ => {}
        }
        self.bump();
    }

    pub fn clear_all(&mut self) {
        self.stick_to_bottom();
        let blank = self.blank_cell();
        for row in &mut self.cells {
            for c in row.iter_mut() {
                *c = blank;
            }
        }
        self.bump();
    }

    pub fn insert_lines(&mut self, n: usize) {
        self.stick_to_bottom();
        let y = self.cursor.y.min(self.rows);
        for _ in 0..n {
            if y < self.rows {
                self.cells.insert(y, self.blank_row());
                self.cells.pop();
            }
        }
        self.bump();
    }

    pub fn delete_lines(&mut self, n: usize) {
        self.stick_to_bottom();
        let y = self.cursor.y.min(self.rows);
        for _ in 0..n {
            if y < self.rows {
                self.cells.remove(y);
                self.cells.push(self.blank_row());
            }
        }
        self.bump();
    }
}
