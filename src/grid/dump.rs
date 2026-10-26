//! Primary-screen dump as replayable VT text, for session scrollback.
//!
//! The format is what a program would have printed: clusters, SGR, OSC 8
//! and CRLF. It stays independent of the compact history encoding, and
//! replaying it through `Terminal::feed` re-applies every history cap.

use std::io::Write as _;

use super::cell::CellStyle;
use super::{CellRef, Grid, RowView, UnderlineStyle};

impl Grid {
    /// History (oldest first) plus the primary screen, never the alternate
    /// one: a quit inside vim/less keeps the shell's rows. Trailing blank
    /// screen rows and trailing default-blank cells are dropped. Rows end in
    /// CRLF, so the cursor of a replaying terminal lands on a fresh line.
    pub fn dump_primary(&self) -> Vec<u8> {
        let live = if self.in_alt {
            self.saved_main_cells.as_deref().unwrap_or_default()
        } else {
            &self.cells
        };
        let default = CellStyle::from(&self.blank_cell());
        let used = live
            .iter()
            .rposition(|row| {
                Self::live_row_view(row)
                    .iter()
                    .any(|c| !is_default_blank(&c, &default))
            })
            .map_or(0, |last| last + 1);
        let mut dump = Dump {
            grid: self,
            out: Vec::new(),
            default,
            style: default,
            link: None,
        };
        for i in 0..self.scrollback.len() {
            dump.row(self.history_row_view(i).expect("history row"));
        }
        for row in &live[..used] {
            dump.row(Self::live_row_view(row));
        }
        dump.set_link(None);
        dump.out
    }
}

fn is_default_blank(cell: &CellRef<'_>, default: &CellStyle) -> bool {
    cell.ch == ' ' && cell.extra.is_none() && cell.width == 1 && cell.style == *default
}

struct Dump<'a> {
    grid: &'a Grid,
    out: Vec<u8>,
    /// The grid theme's default style, emitted as `SGR 0` so restored
    /// default-colored text follows the theme of the restoring terminal.
    default: CellStyle,
    style: CellStyle,
    link: Option<u16>,
}

impl Dump<'_> {
    fn row(&mut self, row: RowView<'_>) {
        // Default blanks are only written once something follows them.
        let mut blanks = 0usize;
        for cell in row.iter() {
            if cell.width == 0 {
                continue;
            }
            if is_default_blank(&cell, &self.default) {
                blanks += 1;
                continue;
            }
            if blanks > 0 {
                self.set_style(self.default);
                self.set_link(None);
                self.out.resize(self.out.len() + blanks, b' ');
                blanks = 0;
            }
            self.set_style(cell.style);
            self.set_link(cell.hyperlink);
            let mut buf = [0u8; 4];
            let ch = if cell.ch.is_control() { ' ' } else { cell.ch };
            self.out
                .extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            if let Some(extra) = cell.extra {
                for ch in extra.chars().filter(|c| !c.is_control()) {
                    self.out
                        .extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
        // A colored pen would paint the next row's scroll fill (BCE).
        self.set_style(self.default);
        self.out.extend_from_slice(b"\r\n");
    }

    fn set_style(&mut self, style: CellStyle) {
        let style = CellStyle {
            hyperlink: None,
            ..style
        };
        if style == self.style {
            return;
        }
        self.style = style;
        let out = &mut self.out;
        out.extend_from_slice(b"\x1b[0");
        for (on, code) in [
            (style.bold, "1"),
            (style.dim, "2"),
            (style.italic, "3"),
            (style.inverse, "7"),
            (style.strikethrough, "9"),
            (style.overline, "53"),
        ] {
            if on {
                out.push(b';');
                out.extend_from_slice(code.as_bytes());
            }
        }
        match style.underline {
            UnderlineStyle::None => {}
            UnderlineStyle::Single => out.extend_from_slice(b";4"),
            UnderlineStyle::Double => out.extend_from_slice(b";4:2"),
            UnderlineStyle::Curly => out.extend_from_slice(b";4:3"),
            UnderlineStyle::Dotted => out.extend_from_slice(b";4:4"),
            UnderlineStyle::Dashed => out.extend_from_slice(b";4:5"),
        }
        if style.fg != self.default.fg {
            let c = style.fg;
            let _ = write!(out, ";38;2;{};{};{}", c.r, c.g, c.b);
        }
        if style.bg != self.default.bg {
            let c = style.bg;
            let _ = write!(out, ";48;2;{};{};{}", c.r, c.g, c.b);
        }
        if let Some(c) = style.underline_color {
            let _ = write!(out, ";58:2:{}:{}:{}", c.r, c.g, c.b);
        }
        out.push(b'm');
    }

    fn set_link(&mut self, link: Option<u16>) {
        // A URI that could end the OSC early is dropped, not escaped.
        let link = link.filter(|&id| {
            self.grid
                .hyperlink_uri(id)
                .is_some_and(|uri| !uri.chars().any(char::is_control))
        });
        if link == self.link {
            return;
        }
        self.link = link;
        let uri = link
            .and_then(|id| self.grid.hyperlink_uri(id))
            .unwrap_or("");
        let _ = write!(self.out, "\x1b]8;;{uri}\x1b\\");
    }
}

#[cfg(test)]
mod tests {
    use crate::term::Terminal;
    use crate::theme::Theme;

    fn lines(t: &Terminal, count: usize) -> Vec<Vec<super::super::Cell>> {
        (0..count)
            .map(|i| t.grid().global_line_cells(i).expect("line"))
            .collect()
    }

    #[test]
    fn replay_reproduces_history_and_screen() {
        let mut a = Terminal::new(20, 5, Theme::default());
        for i in 0..30 {
            a.feed(format!("line {i}\r\n").as_bytes());
        }
        a.feed(b"\x1b[1;31mbold red\x1b[0m \x1b[4:3;58:2:1:2:3mcurly\x1b[0m\r\n");
        a.feed("wide 漢字 e\u{301} 👍🏽\r\n".as_bytes());
        a.feed(b"\x1b]8;;https://example.com\x1b\\link me\x1b]8;;\x1b\\ after\r\n");
        a.feed(b"\x1b[38;2;10;20;30;48;5;196m true \x1b[0m\x1b[44m\x1b[K\x1b[0m\r\n");
        a.feed(b"\x1b[2;7;9;53mattrs\x1b[0m\r\n");
        a.feed(format!("{}\r\n{}\r\n", "x".repeat(20), "y".repeat(25)).as_bytes());
        let count = a.grid().scrollback_len() + a.grid().cursor().y;

        let dump = a.dump_scrollback();
        let mut b = Terminal::new(20, 5, Theme::default());
        b.replay(&dump);
        assert_eq!(lines(&b, count), lines(&a, count));
        let link = b.grid().hyperlink_id_at(count - 6, 0).expect("link kept");
        assert_eq!(b.grid().hyperlink_uri(link), Some("https://example.com"));
        assert_eq!(b.grid().cursor().x, 0, "replay ends on a fresh line");
    }

    #[test]
    fn default_text_is_plain_and_blank_tail_is_trimmed() {
        let mut t = Terminal::new(20, 5, Theme::default());
        t.feed(b"plain   \r\n\r\nend");
        assert_eq!(t.dump_scrollback(), b"plain\r\n\r\nend\r\n");
        assert!(
            Terminal::new(20, 5, Theme::default())
                .dump_scrollback()
                .is_empty()
        );
    }

    #[test]
    fn alternate_screen_is_never_dumped() {
        let mut t = Terminal::new(20, 5, Theme::default());
        t.feed(b"shell$ vim\r\n\x1b[?1049h\x1b[HVIM BUFFER");
        assert!(t.is_alt());
        assert_eq!(t.dump_scrollback(), b"shell$ vim\r\n");
    }

    #[test]
    fn replay_drops_replies_clipboard_and_open_state() {
        let hostile = b"\x1b[6n\x1b]52;c;aGk=\x07\x1b]8;;https://x.test\x1b\\\x1b[31mopen";
        let mut fed = Terminal::new(20, 5, Theme::default());
        fed.feed(b"\x1b[6n");
        assert!(!fed.take_response().is_empty(), "a live feed does reply");

        let mut t = Terminal::new(20, 5, Theme::default());
        t.replay(hostile);
        assert!(t.take_response().is_empty());
        assert!(t.take_clipboard().is_empty());
        assert_eq!(t.grid().current_hyperlink(), None);
        assert_eq!(t.grid().pen(), t.grid().default_pen());
        t.replay(b"\x1b]0;unterminated");
        t.feed(b"ok");
        assert_eq!(t.grid().cell(4, 0).unwrap().ch, 'o', "printing resumes");
    }
}
