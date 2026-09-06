mod alt;
mod cell;
mod edit;
mod scroll;
mod style;

pub use cell::{Cell, Cursor, CursorShape, CursorStyle, Pen};

use std::collections::VecDeque;

use crate::theme::Theme;

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
    cursor_style: CursorStyle,
    saved_style: Option<CursorStyle>,
    saved_main_saved_style: Option<CursorStyle>,
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
            cursor_style: CursorStyle::default(),
            saved_style: None,
            saved_main_saved_style: None,
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

    // Read accessor for tests/inspection; the app itself renders in bulk via
    // `visible_rows`, so silence the binary-crate `dead_code` lint.
    #[allow(dead_code)]
    pub fn pen(&self) -> Pen {
        self.pen
    }

    pub(crate) fn bump(&mut self) {
        self.version = self.version.wrapping_add(1);
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

    #[test]
    fn erase_uses_pen_bg_consistently() {
        let theme = test_theme();
        let red = theme.ansi(1);
        let mut g = Grid::new(4, 3, theme);
        g.sgr(&[41, 1]);
        // ED 2 clears everything with pen bg, dropping bold.
        g.erase_in_display(2);
        let c = g.cell(0, 0).unwrap();
        assert_eq!(c.bg, red);
        assert_eq!(c.fg, theme.foreground);
        assert!(!c.bold);

        // EL 2 clears the cursor row with pen bg.
        g.sgr(&[0]);
        g.sgr(&[44, 1]);
        let blue = theme.ansi(4);
        g.set_cursor(1, 1);
        g.erase_in_line(2);
        let c = g.cell(0, 1).unwrap();
        assert_eq!(c.bg, blue);
        assert!(!c.bold);

        // ED 1 (start to cursor) matches ED 0 / EL behavior.
        g.set_cursor(2, 1);
        g.erase_in_display(1);
        assert_eq!(g.cell(0, 0).unwrap().bg, blue);
        assert_eq!(g.cell(2, 1).unwrap().bg, blue);
        // Below-cursor rows are untouched by ED 1.
        assert_eq!(g.cell(0, 2).unwrap().bg, red);

        // IL / DL fill with pen bg.
        g.set_cursor(0, 0);
        g.insert_lines(1);
        assert_eq!(g.cell(0, 0).unwrap().bg, blue);
        g.delete_lines(1);
        assert_eq!(g.cell(0, 2).unwrap().bg, blue);

        // Scroll fill uses pen bg.
        g.scroll_up(1);
        assert_eq!(g.cell(0, 2).unwrap().bg, blue);
        g.scroll_down(1);
        assert_eq!(g.cell(0, 0).unwrap().bg, blue);

        // Structural resize growth stays theme-default, not pen bg.
        g.resize(5, 4);
        assert_eq!(g.cell(4, 0).unwrap().bg, theme.background);
        assert_eq!(g.cell(0, 3).unwrap().bg, theme.background);
    }

    #[test]
    fn cursor_style_save_restore_across_alt() {
        use super::cell::{CursorShape, CursorStyle};
        let mut g = Grid::new(2, 2, test_theme());
        assert_eq!(g.cursor_style(), CursorStyle::default());
        g.set_cursor_style(CursorStyle {
            shape: CursorShape::Bar,
            blinking: false,
        });
        g.save_cursor();
        g.set_cursor_style(CursorStyle {
            shape: CursorShape::Underline,
            blinking: true,
        });
        g.restore_cursor();
        assert_eq!(
            g.cursor_style(),
            CursorStyle {
                shape: CursorShape::Bar,
                blinking: false,
            }
        );
        // Style persists across alt; saved state is isolated per buffer.
        g.save_cursor();
        g.enter_alt(true);
        assert_eq!(
            g.cursor_style(),
            CursorStyle {
                shape: CursorShape::Bar,
                blinking: false,
            }
        );
        g.set_cursor_style(CursorStyle {
            shape: CursorShape::Block,
            blinking: true,
        });
        g.exit_alt();
        // Exiting restores the main saved state, not the alt edit.
        g.restore_cursor();
        assert_eq!(
            g.cursor_style(),
            CursorStyle {
                shape: CursorShape::Bar,
                blinking: false,
            }
        );
    }
}
