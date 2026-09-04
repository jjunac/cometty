use vte::{Params, Parser};

use crate::grid::Grid;
use crate::theme::Theme;

pub struct Terminal {
    grid: Grid,
    parser: Parser,
    // Pending wrap: true when cursor is past right margin and next printable should wrap first.
    pending_wrap: bool,
}

impl Terminal {
    pub fn new(cols: usize, rows: usize, theme: Theme) -> Self {
        Self {
            grid: Grid::new(cols, rows, theme),
            parser: Parser::new(),
            pending_wrap: false,
        }
    }

    #[allow(dead_code)]
    pub fn theme(&self) -> Theme {
        self.grid.theme()
    }

    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    pub fn cols(&self) -> usize {
        self.grid.cols()
    }

    pub fn rows(&self) -> usize {
        self.grid.rows()
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.grid.resize(cols, rows);
        self.pending_wrap = false;
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        // vte::Parser::advance borrows both parser and performer; swap parser
        // out to satisfy the borrow checker while preserving state.
        let mut parser = std::mem::replace(&mut self.parser, Parser::new());
        parser.advance(self, bytes);
        self.parser = parser;
    }

    fn param_or(params: &Params, idx: usize, default: u16) -> u16 {
        let flat: Vec<u16> = params.iter().flat_map(|p| p.iter().copied()).collect();
        if idx < flat.len() {
            let v = flat[idx];
            if v == 0 { default } else { v }
        } else {
            default
        }
    }

    fn params_flat(params: &Params) -> Vec<u16> {
        params.iter().flat_map(|p| p.iter().copied()).collect()
    }
}

impl vte::Perform for Terminal {
    fn print(&mut self, c: char) {
        if self.pending_wrap {
            self.grid.newline();
            self.pending_wrap = false;
        } else {
            self.grid.handle_wrap_if_needed();
        }
        // After newline, cursor.x is 0.
        self.grid.put_char(c);
        if self.grid.cursor().x >= self.grid.cols() {
            self.pending_wrap = true;
            // Clamp visual cursor to last column until wrap resolves.
            let y = self.grid.cursor().y;
            self.grid.set_cursor(self.grid.cols().saturating_sub(1), y);
            // put_char already advanced x to cols; set_cursor above fixes it.
            // Re-advance pending state: next print will newline.
            // But set_cursor cleared the >=cols condition; keep pending_wrap.
        }
    }

    fn execute(&mut self, byte: u8) {
        self.pending_wrap = false;
        match byte {
            b'\n' | 0x0B | 0x0C => self.grid.newline(),
            b'\r' => self.grid.carriage_return(),
            0x08 => self.grid.backspace(),
            b'\t' => self.grid.tab(),
            0x07 => {} // bell: ignore for v1
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        if !intermediates.is_empty() {
            return;
        }
        self.pending_wrap = false;
        match action {
            'A' => {
                let n = Self::param_or(params, 0, 1) as isize;
                self.grid.move_cursor(0, -n);
            }
            'B' | 'e' => {
                let n = Self::param_or(params, 0, 1) as isize;
                self.grid.move_cursor(0, n);
            }
            'C' | 'a' => {
                let n = Self::param_or(params, 0, 1) as isize;
                self.grid.move_cursor(n, 0);
            }
            'D' => {
                let n = Self::param_or(params, 0, 1) as isize;
                self.grid.move_cursor(-n, 0);
            }
            'E' => {
                let n = Self::param_or(params, 0, 1) as isize;
                self.grid.move_cursor(0, n);
                self.grid.carriage_return();
                let x = 0;
                let y = self.grid.cursor().y;
                self.grid.set_cursor(x, y);
            }
            'F' => {
                let n = Self::param_or(params, 0, 1) as isize;
                self.grid.move_cursor(0, -n);
                let y = self.grid.cursor().y;
                self.grid.set_cursor(0, y);
            }
            'G' | '`' => {
                let col = Self::param_or(params, 0, 1).saturating_sub(1) as usize;
                let y = self.grid.cursor().y;
                self.grid.set_cursor(col, y);
            }
            'd' => {
                let row = Self::param_or(params, 0, 1).saturating_sub(1) as usize;
                let x = self.grid.cursor().x;
                self.grid.set_cursor(x, row);
            }
            'H' | 'f' => {
                let row = Self::param_or(params, 0, 1).saturating_sub(1) as usize;
                let col = Self::param_or(params, 1, 1).saturating_sub(1) as usize;
                self.grid.set_cursor(col, row);
            }
            'J' => {
                let mode = Self::param_or(params, 0, 0);
                self.grid.erase_in_display(mode);
            }
            'K' => {
                let mode = Self::param_or(params, 0, 0);
                self.grid.erase_in_line(mode);
            }
            'm' => {
                let flat = Self::params_flat(params);
                self.grid.sgr(&flat);
            }
            's' => self.grid.save_cursor(),
            'u' => self.grid.restore_cursor(),
            'L' => {
                let n = Self::param_or(params, 0, 1) as usize;
                self.grid.insert_lines(n);
            }
            'M' => {
                let n = Self::param_or(params, 0, 1) as usize;
                self.grid.delete_lines(n);
            }
            'S' => {
                let n = Self::param_or(params, 0, 1) as usize;
                self.grid.scroll_up(n);
            }
            'T' => {
                let n = Self::param_or(params, 0, 1) as usize;
                self.grid.scroll_down(n);
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        self.pending_wrap = false;
        if intermediates.is_empty() {
            match byte {
                b'M' => {
                    // Reverse index
                    let cur = self.grid.cursor();
                    if cur.y == 0 {
                        self.grid.scroll_down(1);
                    } else {
                        self.grid.set_cursor(cur.x, cur.y.saturating_sub(1));
                    }
                }
                b'7' => self.grid.save_cursor(),
                b'8' => self.grid.restore_cursor(),
                b'c' => {
                    self.grid.clear_all();
                    self.grid.set_cursor(0, 0);
                    self.grid.sgr(&[0]);
                }
                _ => {}
            }
        }
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}

    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {
        // Ignore window title etc. for v1.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn feed_str(t: &mut Terminal, s: &str) {
        t.feed(s.as_bytes());
    }

    fn test_terminal(cols: usize, rows: usize) -> Terminal {
        Terminal::new(cols, rows, Theme::default())
    }

    #[test]
    fn prints_simple_text() {
        let mut t = test_terminal(10, 5);
        feed_str(&mut t, "hi");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, 'h');
        assert_eq!(t.grid().cell(1, 0).unwrap().ch, 'i');
    }

    #[test]
    fn handles_crlf() {
        let mut t = test_terminal(10, 5);
        feed_str(&mut t, "a\r\nb");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, 'a');
        assert_eq!(t.grid().cell(0, 1).unwrap().ch, 'b');
    }

    #[test]
    fn handles_sgr_color() {
        let mut t = test_terminal(10, 5);
        feed_str(&mut t, "\x1b[31mR\x1b[0mN");
        let r = t.grid().cell(0, 0).unwrap();
        let n = t.grid().cell(1, 0).unwrap();
        let theme = t.theme();
        assert_eq!(r.ch, 'R');
        assert_eq!(r.fg, theme.ansi(1));
        assert_eq!(n.ch, 'N');
        assert_eq!(n.fg, theme.foreground);
    }

    #[test]
    fn handles_cup_and_ed() {
        let mut t = test_terminal(10, 5);
        feed_str(&mut t, "hello");
        feed_str(&mut t, "\x1b[1;1H");
        assert_eq!(t.grid().cursor().x, 0);
        feed_str(&mut t, "\x1b[2J");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, ' ');
    }

    #[test]
    fn handles_wrap() {
        let mut t = test_terminal(3, 2);
        feed_str(&mut t, "abcd");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, 'a');
        assert_eq!(t.grid().cell(2, 0).unwrap().ch, 'c');
        assert_eq!(t.grid().cell(0, 1).unwrap().ch, 'd');
    }

    #[test]
    fn handles_truecolor() {
        let mut t = test_terminal(10, 2);
        feed_str(&mut t, "\x1b[38;2;10;20;30mX");
        let c = t.grid().cell(0, 0).unwrap();
        assert_eq!((c.fg.r, c.fg.g, c.fg.b), (10, 20, 30));
    }
}
