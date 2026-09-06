use vte::{Params, Parser};

use crate::grid::{CursorShape, CursorStyle, Grid};
use crate::theme::Theme;

pub struct Terminal {
    grid: Grid,
    parser: Parser,
    // Pending wrap: true when cursor is past right margin and next printable should wrap first.
    pending_wrap: bool,
    // Last OSC 0/1/2 title set by the shell; empty when none seen.
    // Drives tab-bar labels (falls back to `Tab N`).
    title: String,
}

/// Maximum stored OSC title length (chars) so a runaway escape can't
/// grow memory without bound; the tab bar truncates further anyway.
const MAX_TITLE_CHARS: usize = 256;

/// Extract an `OSC 0/1/2` window-title update from vte's split params.
///
/// vte splits OSC content on `;`, so `ESC ] 0 ; foo BEL` arrives as
/// `[b"0", b"foo"]` and a title containing `;` arrives as extra pieces
/// (`[b"0", b"a", b"b"]` for `a;b`). A single unsplit param (`[b"0;foo"]`)
/// is handled too for robustness.
fn osc_title(params: &[&[u8]]) -> Option<String> {
    let (num, rest) = match params {
        [] => return None,
        [single] => match single.iter().position(|&b| b == b';') {
            Some(i) => (&single[..i], vec![&single[i + 1..]]),
            None => return None,
        },
        [first, rest @ ..] => (*first, rest.to_vec()),
    };
    if num != b"0" && num != b"1" && num != b"2" {
        return None;
    }
    if rest.is_empty() {
        return Some(String::new());
    }
    // Rejoin multi-`;` titles split by the parser.
    let mut bytes = Vec::new();
    for (i, piece) in rest.iter().enumerate() {
        if i > 0 {
            bytes.push(b';');
        }
        bytes.extend_from_slice(piece);
    }
    let s = String::from_utf8_lossy(&bytes).trim().to_string();
    let truncated: String = s.chars().take(MAX_TITLE_CHARS).collect();
    Some(truncated)
}

impl Terminal {
    pub fn new(cols: usize, rows: usize, theme: Theme) -> Self {
        Self {
            grid: Grid::new(cols, rows, theme),
            parser: Parser::new(),
            pending_wrap: false,
            title: String::new(),
        }
    }

    /// Last `OSC 0/1/2` title reported by the shell, or empty.
    pub fn title(&self) -> &str {
        &self.title
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

    pub fn cursor_visible(&self) -> bool {
        self.grid.cursor_enabled()
    }

    pub fn cursor_style(&self) -> CursorStyle {
        self.grid.cursor_style()
    }

    #[allow(dead_code)]
    pub fn bracketed_paste(&self) -> bool {
        self.grid.bracketed_paste()
    }

    #[allow(dead_code)]
    pub fn is_alt(&self) -> bool {
        self.grid.is_alt()
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.grid.resize(cols, rows);
        self.pending_wrap = false;
    }

    pub fn scroll_offset(&self) -> usize {
        self.grid.scroll_offset()
    }

    pub fn scroll_by(&mut self, delta: isize) -> bool {
        let changed = self.grid.scroll_by(delta);
        if changed {
            self.pending_wrap = false;
        }
        changed
    }

    pub fn scroll_to_top(&mut self) -> bool {
        self.grid.scroll_to_top()
    }

    pub fn scroll_to_bottom(&mut self) -> bool {
        self.grid.scroll_to_bottom()
    }

    pub fn scroll_to_offset(&mut self, offset: usize) -> bool {
        let changed = self.grid.scroll_to_offset(offset);
        if changed {
            self.pending_wrap = false;
        }
        changed
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

    fn set_private_mode(&mut self, mode: u16, set: bool) {
        match mode {
            25 => self.grid.set_cursor_enabled(set),
            2004 => self.grid.set_bracketed_paste(set),
            47 | 1047 => {
                if set {
                    self.grid.enter_alt(false);
                } else {
                    self.grid.exit_alt();
                }
                self.pending_wrap = false;
            }
            1048 => {
                if set {
                    self.grid.save_cursor();
                } else {
                    self.grid.restore_cursor();
                }
            }
            1049 => {
                if set {
                    self.grid.save_cursor();
                    self.grid.enter_alt(true);
                } else {
                    self.grid.exit_alt();
                    self.grid.restore_cursor();
                }
                self.pending_wrap = false;
            }
            _ => {}
        }
    }

    fn set_cursor_style(&mut self, ps: u16) {
        let style = match ps {
            0 | 1 => CursorStyle {
                shape: CursorShape::Block,
                blinking: true,
            },
            2 => CursorStyle {
                shape: CursorShape::Block,
                blinking: false,
            },
            3 => CursorStyle {
                shape: CursorShape::Underline,
                blinking: true,
            },
            4 => CursorStyle {
                shape: CursorShape::Underline,
                blinking: false,
            },
            5 => CursorStyle {
                shape: CursorShape::Bar,
                blinking: true,
            },
            6 => CursorStyle {
                shape: CursorShape::Bar,
                blinking: false,
            },
            _ => return,
        };
        self.grid.set_cursor_style(style);
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
            if intermediates == [b'?'] && (action == 'h' || action == 'l') {
                let set = action == 'h';
                self.pending_wrap = false;
                for mode in Self::params_flat(params) {
                    self.set_private_mode(mode, set);
                }
            } else if intermediates == [b' '] && action == 'q' {
                // DECSCUSR: `CSI Ps SP q` cursor shape + blink.
                self.pending_wrap = false;
                self.set_cursor_style(Self::param_or(params, 0, 0));
            }
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
                let y = self.grid.cursor().y as isize + n;
                let y = y.clamp(0, self.grid.rows().saturating_sub(1) as isize) as usize;
                self.grid.set_cursor(0, y);
            }
            'F' => {
                let n = Self::param_or(params, 0, 1) as isize;
                let y = self.grid.cursor().y as isize - n;
                let y = y.clamp(0, self.grid.rows().saturating_sub(1) as isize) as usize;
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
                    // Full reset: back to main buffer with default modes.
                    self.grid.exit_alt();
                    self.grid.set_cursor_enabled(true);
                    self.grid.set_bracketed_paste(false);
                    self.grid.set_cursor_style(CursorStyle::default());
                    // Reset pen before clearing so BCE erase uses default bg.
                    self.grid.sgr(&[0]);
                    self.grid.clear_all();
                    self.grid.set_cursor(0, 0);
                    self.title.clear();
                }
                _ => {}
            }
        }
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        // Only window/icon titles (0/1/2) are tracked; they feed tab labels.
        if let Some(title) = osc_title(params) {
            self.title = title;
        }
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

    #[test]
    fn alt_screen_1049_swaps_and_restores() {
        let mut t = test_terminal(5, 3);
        feed_str(&mut t, "hi");
        feed_str(&mut t, "\x1b[?1049h");
        assert!(t.is_alt());
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, ' ');
        feed_str(&mut t, "X");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, 'X');
        assert_eq!(t.grid().scrollback_len(), 0);
        feed_str(&mut t, "\x1b[?1049l");
        assert!(!t.is_alt());
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, 'h');
        assert_eq!(t.grid().cell(1, 0).unwrap().ch, 'i');
    }

    #[test]
    fn alt_screen_1047_and_cursor_save_restore() {
        let mut t = test_terminal(5, 3);
        feed_str(&mut t, "ab");
        feed_str(&mut t, "\x1b[?1048h\x1b[?1047h");
        assert!(t.is_alt());
        feed_str(&mut t, "\x1b[?1047l\x1b[?1048l");
        assert!(!t.is_alt());
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, 'a');
    }

    #[test]
    fn decset_cursor_and_bracketed_modes() {
        let mut t = test_terminal(5, 3);
        assert!(t.cursor_visible());
        assert!(!t.bracketed_paste());
        feed_str(&mut t, "\x1b[?25l");
        assert!(!t.cursor_visible());
        feed_str(&mut t, "\x1b[?25h");
        assert!(t.cursor_visible());
        feed_str(&mut t, "\x1b[?2004h");
        assert!(t.bracketed_paste());
        feed_str(&mut t, "\x1b[?2004l");
        assert!(!t.bracketed_paste());
    }

    #[test]
    fn unknown_private_mode_ignored() {
        let mut t = test_terminal(5, 3);
        feed_str(&mut t, "ok\x1b[?9999h");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, 'o');
        assert!(!t.is_alt());
    }

    #[test]
    fn cnl_and_cpl_move_to_column_zero() {
        let mut t = test_terminal(5, 4);
        feed_str(&mut t, "\x1b[3;3H"); // row 3, col 3 (1-based)
        feed_str(&mut t, "\x1b[1E");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (0, 3));
        feed_str(&mut t, "\x1b[2F");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (0, 1));
        // Clamped at the margins, never panics.
        feed_str(&mut t, "\x1b[99F");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (0, 0));
        feed_str(&mut t, "\x1b[99E");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (0, 3));
    }

    #[test]
    fn insert_and_delete_lines_shift_rows() {
        let mut t = test_terminal(3, 3);
        feed_str(&mut t, "a\r\nb\r\nc");
        feed_str(&mut t, "\x1b[1;1H\x1b[1L");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, ' ');
        assert_eq!(t.grid().cell(0, 1).unwrap().ch, 'a');
        feed_str(&mut t, "\x1b[1;1H\x1b[1M");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, 'a');
    }

    #[test]
    fn ed1_and_el_modes_clear() {
        let mut t = test_terminal(4, 2);
        feed_str(&mut t, "abcd\r\nwxyz\x1b[1;3H");
        feed_str(&mut t, "\x1b[1K");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, ' ');
        assert_eq!(t.grid().cell(2, 0).unwrap().ch, ' ');
        assert_eq!(t.grid().cell(3, 0).unwrap().ch, 'd');
        feed_str(&mut t, "\x1b[1J");
        assert_eq!(t.grid().cell(3, 0).unwrap().ch, 'd');
        assert_eq!(t.grid().cell(0, 1).unwrap().ch, 'w');
    }

    #[test]
    fn malformed_extended_sgr_is_ignored() {
        let mut t = test_terminal(5, 2);
        let before = t.grid().pen();
        // Truncated tails must not panic or change the pen.
        // (Note: leftover values that ARE real codes, e.g. the `1` in
        // `38;2;1`, still apply as usual — only the truncated prefix is skipped.)
        feed_str(&mut t, "\x1b[38;5mX\x1b[38;2;10mY\x1b[48mZ");
        assert_eq!(t.grid().pen(), before);
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, 'X');
    }

    #[test]
    fn osc_title_is_stored_for_tab_labels() {
        let mut t = test_terminal(10, 2);
        assert_eq!(t.title(), "");
        feed_str(&mut t, "\x1b]0;nvim | ~/dev\x07");
        assert_eq!(t.title(), "nvim | ~/dev");
        // Icon title (1) and window title (2) also update.
        feed_str(&mut t, "\x1b]2;second\x07");
        assert_eq!(t.title(), "second");
        // Titles containing ';' survive vte's param splitting.
        feed_str(&mut t, "\x1b]0;a;b\x07");
        assert_eq!(t.title(), "a;b");
    }

    #[test]
    fn osc_non_title_is_ignored() {
        let mut t = test_terminal(10, 2);
        feed_str(&mut t, "\x1b]0;kept\x07");
        // OSC 4 (palette) must not clobber the title.
        feed_str(&mut t, "\x1b]4;1;red\x07");
        assert_eq!(t.title(), "kept");
    }

    #[test]
    fn osc_title_parser_shapes() {
        assert_eq!(osc_title(&[]), None);
        assert_eq!(osc_title(&[b"4", b"1;red"]), None);
        assert_eq!(osc_title(&[b"0", b"hi"]), Some("hi".to_string()));
        assert_eq!(
            osc_title(&[b"2;split;title"]),
            Some("split;title".to_string())
        );
        assert_eq!(osc_title(&[b"0"]), None);
    }

    #[test]
    fn sgr_underline_is_stored_on_cells() {
        use crate::grid::{CursorShape, CursorStyle};
        let mut t = test_terminal(5, 2);
        feed_str(&mut t, "\x1b[4mU\x1b[24mN");
        assert!(t.grid().cell(0, 0).unwrap().underline);
        assert!(!t.grid().cell(1, 0).unwrap().underline);
        // Default cursor style is blinking block.
        assert_eq!(t.cursor_style(), CursorStyle::default());
        assert_eq!(
            t.cursor_style(),
            CursorStyle {
                shape: CursorShape::Block,
                blinking: true,
            }
        );
    }

    #[test]
    fn decscusr_sets_shape_and_blink() {
        use crate::grid::CursorShape;
        let mut t = test_terminal(5, 2);
        feed_str(&mut t, "\x1b[0 q");
        assert_eq!(t.cursor_style().shape, CursorShape::Block);
        assert!(t.cursor_style().blinking);
        feed_str(&mut t, "\x1b[2 q");
        assert_eq!(t.cursor_style().shape, CursorShape::Block);
        assert!(!t.cursor_style().blinking);
        feed_str(&mut t, "\x1b[3 q");
        assert_eq!(t.cursor_style().shape, CursorShape::Underline);
        assert!(t.cursor_style().blinking);
        feed_str(&mut t, "\x1b[4 q");
        assert_eq!(t.cursor_style().shape, CursorShape::Underline);
        assert!(!t.cursor_style().blinking);
        feed_str(&mut t, "\x1b[5 q");
        assert_eq!(t.cursor_style().shape, CursorShape::Bar);
        assert!(t.cursor_style().blinking);
        feed_str(&mut t, "\x1b[6 q");
        assert_eq!(t.cursor_style().shape, CursorShape::Bar);
        assert!(!t.cursor_style().blinking);
        // Unknown Ps is ignored, keeping the previous style.
        feed_str(&mut t, "\x1b[9 q");
        assert_eq!(t.cursor_style().shape, CursorShape::Bar);
        assert!(!t.cursor_style().blinking);
    }

    #[test]
    fn decscusr_save_restore_and_reset() {
        use crate::grid::CursorShape;
        let mut t = test_terminal(5, 2);
        feed_str(&mut t, "\x1b[5 q");
        feed_str(&mut t, "\x1b7");
        feed_str(&mut t, "\x1b[3 q");
        assert_eq!(t.cursor_style().shape, CursorShape::Underline);
        feed_str(&mut t, "\x1b8");
        assert_eq!(t.cursor_style().shape, CursorShape::Bar);
        // Full reset restores blinking block.
        feed_str(&mut t, "\x1bc");
        assert_eq!(t.cursor_style(), crate::grid::CursorStyle::default());
    }

    #[test]
    fn erase_uses_pen_bg_and_reset_clears_to_default() {
        let mut t = test_terminal(4, 2);
        let theme = t.theme();
        feed_str(&mut t, "\x1b[41m\x1b[2J");
        let c = t.grid().cell(0, 0).unwrap();
        assert_eq!(c.bg, theme.ansi(1));
        assert_eq!(c.fg, theme.foreground);
        // Full reset clears pen first, so BCE fill lands on default bg.
        feed_str(&mut t, "\x1bc");
        let c = t.grid().cell(0, 0).unwrap();
        assert_eq!(c.bg, theme.background);
    }
}
