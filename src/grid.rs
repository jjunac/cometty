use std::collections::VecDeque;

use crate::theme::{Rgb, Theme};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub underline: bool,
}

impl Default for Cell {
    fn default() -> Self {
        let theme = Theme::default();
        Self {
            ch: ' ',
            fg: theme.foreground,
            bg: theme.background,
            bold: false,
            underline: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub x: usize,
    pub y: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pen {
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub underline: bool,
}

impl Default for Pen {
    fn default() -> Self {
        let theme = Theme::default();
        Self {
            fg: theme.foreground,
            bg: theme.background,
            bold: false,
            underline: false,
        }
    }
}

pub struct Grid {
    cols: usize,
    rows: usize,
    theme: Theme,
    cells: Vec<Vec<Cell>>,
    scrollback: VecDeque<Vec<Cell>>,
    max_scrollback: usize,
    cursor: Cursor,
    pen: Pen,
    saved_cursor: Option<Cursor>,
    saved_pen: Option<Pen>,
    pub version: u64,
    in_alt: bool,
    saved_main_cells: Option<Vec<Vec<Cell>>>,
    saved_main_cursor: Option<Cursor>,
    saved_main_saved_cursor: Option<Cursor>,
    saved_main_saved_pen: Option<Pen>,
    cursor_enabled: bool,
    bracketed_paste: bool,
    scroll_offset: usize,
}

impl Grid {
    pub fn new(cols: usize, rows: usize, theme: Theme) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let blank = Cell {
            ch: ' ',
            fg: theme.foreground,
            bg: theme.background,
            bold: false,
            underline: false,
        };
        Self {
            cols,
            rows,
            theme,
            cells: vec![vec![blank; cols]; rows],
            scrollback: VecDeque::new(),
            max_scrollback: 10_000,
            cursor: Cursor { x: 0, y: 0 },
            pen: Pen {
                fg: theme.foreground,
                bg: theme.background,
                bold: false,
                underline: false,
            },
            saved_cursor: None,
            saved_pen: None,
            version: 0,
            in_alt: false,
            saved_main_cells: None,
            saved_main_cursor: None,
            saved_main_saved_cursor: None,
            saved_main_saved_pen: None,
            cursor_enabled: true,
            bracketed_paste: false,
            scroll_offset: 0,
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cursor(&self) -> Cursor {
        self.cursor
    }

    #[allow(dead_code)]
    pub fn theme(&self) -> Theme {
        self.theme
    }

    fn blank_cell(&self) -> Cell {
        Cell {
            ch: ' ',
            fg: self.theme.foreground,
            bg: self.theme.background,
            bold: false,
            underline: false,
        }
    }

    // Read accessor for tests/inspection; the app itself renders in bulk via
    // `visible_rows`, so silence the binary-crate `dead_code` lint.
    #[allow(dead_code)]
    pub fn pen(&self) -> Pen {
        self.pen
    }

    fn bump(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    pub fn blank_row(&self) -> Vec<Cell> {
        vec![self.blank_cell(); self.cols]
    }

    // See `pen`.
    #[allow(dead_code)]
    pub fn cell(&self, x: usize, y: usize) -> Option<Cell> {
        self.cells.get(y)?.get(x).copied()
    }

    #[allow(dead_code)]
    pub fn visible_rows(&self) -> &[Vec<Cell>] {
        &self.cells
    }

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

    // See `pen`.
    #[allow(dead_code)]
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
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
    fn stick_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.clamp(1, 1024);
        let rows = rows.clamp(1, 1024);
        if cols == self.cols && rows == self.rows {
            return;
        }
        let blank = self.blank_cell();
        let mut new_cells = vec![vec![blank; cols]; rows];
        let copy_rows = self.rows.min(rows);
        let copy_cols = self.cols.min(cols);
        for (y, new_row) in new_cells.iter_mut().enumerate().take(copy_rows) {
            for (x, new_cell) in new_row.iter_mut().enumerate().take(copy_cols) {
                *new_cell = self.cells[y][x];
            }
        }
        self.cells = new_cells;
        if let Some(main) = self.saved_main_cells.take() {
            let mut new_main = vec![vec![blank; cols]; rows];
            let main_rows = main.len();
            let main_cols = main.first().map(|r| r.len()).unwrap_or(0);
            for (y, new_row) in new_main.iter_mut().enumerate().take(main_rows.min(rows)) {
                for (x, new_cell) in new_row.iter_mut().enumerate().take(main_cols.min(cols)) {
                    *new_cell = main[y][x];
                }
            }
            self.saved_main_cells = Some(new_main);
        }
        self.cols = cols;
        self.rows = rows;
        if cols != self.scrollback.front().map(|r| r.len()).unwrap_or(cols) {
            for row in self.scrollback.iter_mut() {
                row.resize(cols, blank);
            }
        }
        self.scroll_offset = self.scroll_offset.min(self.scrollback.len());
        self.cursor.x = self.cursor.x.min(cols.saturating_sub(1));
        self.cursor.y = self.cursor.y.min(rows.saturating_sub(1));
        if let Some(c) = self.saved_main_cursor.as_mut() {
            c.x = c.x.min(cols.saturating_sub(1));
            c.y = c.y.min(rows.saturating_sub(1));
        }
        self.bump();
    }

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
            self.cells.push(self.blank_row());
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
            self.cells.insert(0, self.blank_row());
        }
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

    // Pen / SGR
    pub fn sgr(&mut self, params: &[u16]) {
        if params.is_empty() {
            self.pen = self.default_pen();
            return;
        }
        let mut i = 0;
        while i < params.len() {
            let p = params[i];
            match p {
                0 => self.pen = self.default_pen(),
                1 => self.pen.bold = true,
                4 => self.pen.underline = true,
                22 => self.pen.bold = false,
                24 => self.pen.underline = false,
                30..=37 => self.pen.fg = self.theme.ansi((p - 30) as u8),
                39 => self.pen.fg = self.theme.foreground,
                40..=47 => self.pen.bg = self.theme.ansi((p - 40) as u8),
                49 => self.pen.bg = self.theme.background,
                90..=97 => self.pen.fg = self.theme.ansi((p - 90 + 8) as u8),
                100..=107 => self.pen.bg = self.theme.ansi((p - 100 + 8) as u8),
                // 38 / 48 extended color: only support 38;5;n and 48;5;n, plus 38;2;r;g;b
                38 | 48 => {
                    let is_fg = p == 38;
                    if i + 1 < params.len() {
                        let mode = params[i + 1];
                        if mode == 5 && i + 2 < params.len() {
                            let idx = params[i + 2].min(255) as u8;
                            // map 256 palette approximately: 0-15 use theme, else grayscale/rgb cube approx
                            let col = self.extended_palette(idx);
                            if is_fg {
                                self.pen.fg = col;
                            } else {
                                self.pen.bg = col;
                            }
                            i += 2;
                        } else if mode == 2 && i + 4 < params.len() {
                            let r = params[i + 2].min(255) as u8;
                            let g = params[i + 3].min(255) as u8;
                            let b = params[i + 4].min(255) as u8;
                            if is_fg {
                                self.pen.fg = Rgb::new(r, g, b);
                            } else {
                                self.pen.bg = Rgb::new(r, g, b);
                            }
                            i += 4;
                        } else {
                            i += 1;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
        self.bump();
    }

    pub fn save_cursor(&mut self) {
        self.saved_cursor = Some(self.cursor);
        self.saved_pen = Some(self.pen);
    }

    pub fn restore_cursor(&mut self) {
        if let Some(c) = self.saved_cursor {
            self.cursor = Cursor {
                x: c.x.min(self.cols.saturating_sub(1)),
                y: c.y.min(self.rows.saturating_sub(1)),
            };
        }
        if let Some(p) = self.saved_pen {
            self.pen = p;
        }
        self.bump();
    }

    #[allow(dead_code)]
    pub fn is_alt(&self) -> bool {
        self.in_alt
    }

    pub fn cursor_enabled(&self) -> bool {
        self.cursor_enabled
    }

    pub fn set_cursor_enabled(&mut self, enabled: bool) {
        if self.cursor_enabled != enabled {
            self.cursor_enabled = enabled;
            self.bump();
        }
    }

    #[allow(dead_code)]
    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    pub fn set_bracketed_paste(&mut self, enabled: bool) {
        if self.bracketed_paste != enabled {
            self.bracketed_paste = enabled;
            self.bump();
        }
    }

    pub fn enter_alt(&mut self, clear: bool) {
        if self.in_alt {
            if clear {
                self.clear_all();
                self.set_cursor(0, 0);
            }
            return;
        }
        self.saved_main_cells = Some(std::mem::take(&mut self.cells));
        self.saved_main_cursor = Some(self.cursor);
        self.saved_main_saved_cursor = self.saved_cursor;
        self.saved_main_saved_pen = self.saved_pen;
        self.cells = vec![self.blank_row(); self.rows];
        self.cursor = Cursor { x: 0, y: 0 };
        self.saved_cursor = None;
        self.saved_pen = None;
        self.in_alt = true;
        self.scroll_offset = 0;
        if !clear {
            // Fresh alt buffer is already blank; keep cursor at home.
        }
        self.bump();
    }

    pub fn exit_alt(&mut self) {
        if !self.in_alt {
            return;
        }
        if let Some(main) = self.saved_main_cells.take() {
            self.cells = main;
        } else {
            self.cells = vec![self.blank_row(); self.rows];
        }
        if let Some(c) = self.saved_main_cursor.take() {
            self.cursor = Cursor {
                x: c.x.min(self.cols.saturating_sub(1)),
                y: c.y.min(self.rows.saturating_sub(1)),
            };
        }
        self.saved_cursor = self.saved_main_saved_cursor.take();
        self.saved_pen = self.saved_main_saved_pen.take();
        self.in_alt = false;
        self.scroll_offset = 0;
        self.bump();
    }

    fn default_pen(&self) -> Pen {
        Pen {
            fg: self.theme.foreground,
            bg: self.theme.background,
            bold: false,
            underline: false,
        }
    }

    fn extended_palette(&self, idx: u8) -> Rgb {
        if idx < 16 {
            return self.theme.ansi(idx);
        }
        if (16..232).contains(&idx) {
            let i = idx - 16;
            let r = (i / 36) % 6;
            let g = (i / 6) % 6;
            let b = i % 6;
            let conv = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            return Rgb::new(conv(r), conv(g), conv(b));
        }
        let v = 8 + (idx - 232) * 10;
        Rgb::new(v, v, v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn test_theme() -> Theme {
        Theme::default()
    }

    #[test]
    fn put_and_wrap() {
        let mut g = Grid::new(3, 2, test_theme());
        g.put_char('a');
        g.put_char('b');
        g.put_char('c');
        assert_eq!(g.cursor(), Cursor { x: 3, y: 0 });
        g.handle_wrap_if_needed();
        assert_eq!(g.cursor(), Cursor { x: 0, y: 1 });
        g.put_char('d');
        assert_eq!(
            g.cell(0, 0),
            Some(Cell {
                ch: 'a',
                ..Default::default()
            })
        );
        assert_eq!(g.cell(0, 1).unwrap().ch, 'd');
    }

    #[test]
    fn newline_scrolls_with_scrollback() {
        let mut g = Grid::new(2, 2, test_theme());
        g.put_char('a');
        g.newline();
        g.put_char('b');
        g.newline();
        // scrolled once (a moved to scrollback)
        assert_eq!(g.scrollback_len(), 1);
        // next newline scrolls again
        g.newline();
        assert_eq!(g.scrollback_len(), 2);
    }

    #[test]
    fn sgr_basic() {
        let theme = test_theme();
        let mut g = Grid::new(5, 2, theme);
        g.sgr(&[31]);
        assert_eq!(g.pen().fg, theme.ansi(1));
        g.sgr(&[0]);
        assert_eq!(g.pen().fg, theme.foreground);
        assert_eq!(g.pen().bg, theme.background);
    }

    #[test]
    fn erase_display() {
        let mut g = Grid::new(3, 2, test_theme());
        g.put_char('x');
        g.erase_in_display(2);
        assert_eq!(g.cell(0, 0).unwrap().ch, ' ');
    }

    #[test]
    fn resize_preserves() {
        let mut g = Grid::new(4, 2, test_theme());
        g.put_char('h');
        g.put_char('i');
        g.resize(2, 2);
        assert_eq!(g.cell(0, 0).unwrap().ch, 'h');
        assert_eq!(g.cell(1, 0).unwrap().ch, 'i');
        g.resize(6, 3);
        assert_eq!(g.cell(0, 0).unwrap().ch, 'h');
    }

    #[test]
    fn alt_buffer_isolates_and_restores() {
        let mut g = Grid::new(4, 2, test_theme());
        g.put_char('a');
        g.enter_alt(true);
        assert!(g.is_alt());
        assert_eq!(g.cell(0, 0).unwrap().ch, ' ');
        g.put_char('b');
        assert_eq!(g.cell(0, 0).unwrap().ch, 'b');
        g.exit_alt();
        assert!(!g.is_alt());
        assert_eq!(g.cell(0, 0).unwrap().ch, 'a');
    }

    #[test]
    fn alt_buffer_suppresses_scrollback() {
        let mut g = Grid::new(2, 2, test_theme());
        g.enter_alt(true);
        g.put_char('x');
        g.newline();
        g.put_char('y');
        g.newline();
        assert_eq!(g.scrollback_len(), 0);
        g.exit_alt();
        assert_eq!(g.scrollback_len(), 0);
    }

    #[test]
    fn cursor_and_bracketed_flags_toggle() {
        let mut g = Grid::new(2, 2, test_theme());
        assert!(g.cursor_enabled());
        assert!(!g.bracketed_paste());
        g.set_cursor_enabled(false);
        g.set_bracketed_paste(true);
        assert!(!g.cursor_enabled());
        assert!(g.bracketed_paste());
    }

    #[test]
    fn scroll_offset_clamps_and_views_history() {
        let mut g = Grid::new(2, 2, test_theme());
        g.put_char('a');
        g.newline();
        g.put_char('b');
        g.newline();
        g.put_char('c');
        assert_eq!(g.scrollback_len(), 1);
        assert_eq!(g.scroll_offset(), 0);
        // Live view: b, c rows
        assert_eq!(g.view_rows()[0][0].ch, 'b');
        assert!(g.scroll_by(5));
        assert_eq!(g.scroll_offset(), 1);
        // Scrolled up one: a, b rows
        let view = g.view_rows();
        assert_eq!(view[0][0].ch, 'a');
        assert_eq!(view[1][0].ch, 'b');
        assert!(!g.scroll_by(5));
        assert!(g.scroll_to_bottom());
        assert_eq!(g.scroll_offset(), 0);
        assert!(!g.scroll_to_bottom());
    }

    #[test]
    fn new_output_sticks_to_bottom() {
        let mut g = Grid::new(2, 2, test_theme());
        g.put_char('a');
        g.newline();
        g.put_char('b');
        g.newline();
        assert!(g.scroll_by(1));
        g.put_char('z');
        assert_eq!(g.scroll_offset(), 0);
    }

    #[test]
    fn scroll_disabled_in_alt() {
        let mut g = Grid::new(2, 2, test_theme());
        g.put_char('a');
        g.newline();
        g.put_char('b');
        g.newline();
        g.enter_alt(true);
        assert!(!g.scroll_by(1));
        assert_eq!(g.scroll_offset(), 0);
        g.exit_alt();
        assert!(g.scroll_by(1));
    }

    #[test]
    fn ed3_clears_scrollback() {
        let mut g = Grid::new(2, 2, test_theme());
        g.put_char('a');
        g.newline();
        g.put_char('b');
        g.newline();
        assert_eq!(g.scrollback_len(), 1);
        g.scroll_by(1);
        g.erase_in_display(3);
        assert_eq!(g.scrollback_len(), 0);
        assert_eq!(g.scroll_offset(), 0);
    }
}
