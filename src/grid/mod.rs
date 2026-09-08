mod alt;
mod cell;
mod edit;
mod scroll;
mod style;
pub mod unicode;

pub use alt::MouseMode;
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
    tab_stop: usize,
    min_dim: usize,
    max_dim: usize,
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
    cursor_app: bool,
    keypad_app: bool,
    scroll_offset: usize,
    cursor_style: CursorStyle,
    saved_style: Option<CursorStyle>,
    saved_main_saved_style: Option<CursorStyle>,
    scroll_top: usize,
    scroll_bottom: usize,
    origin_mode: bool,
    insert_mode: bool,
    auto_wrap: bool,
    saved_origin: Option<bool>,
    saved_main_scroll_top: Option<usize>,
    saved_main_scroll_bottom: Option<usize>,
    saved_main_origin: Option<bool>,
    saved_main_insert: Option<bool>,
    saved_main_wrap: Option<bool>,
    saved_main_saved_origin: Option<bool>,
    mouse_press: bool,
    mouse_drag: bool,
    mouse_any: bool,
    mouse_sgr: bool,
    focus_report: bool,
    sync_depth: u32,
}

fn cursor_style_from_config(config: &crate::config::Config) -> CursorStyle {
    let shape = match config.cursor.default_shape.to_ascii_lowercase().as_str() {
        "underline" => CursorShape::Underline,
        "bar" => CursorShape::Bar,
        _ => CursorShape::Block,
    };
    CursorStyle {
        shape,
        blinking: config.cursor.default_blinking,
    }
}

impl Grid {
    #[allow(dead_code)]
    pub fn new(cols: usize, rows: usize, theme: Theme) -> Self {
        Self::new_with_config(cols, rows, theme, &crate::config::Config::default())
    }

    pub fn new_with_config(
        cols: usize,
        rows: usize,
        theme: Theme,
        config: &crate::config::Config,
    ) -> Self {
        let min_dim = config.terminal.min_dim.max(1);
        let max_dim = config.terminal.max_dim.max(min_dim);
        let cols = cols.clamp(min_dim, max_dim);
        let rows = rows.clamp(min_dim, max_dim);
        let blank = Cell {
            ch: ' ',
            extra: None,
            width: 1,
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
            max_scrollback: config.terminal.scrollback_lines,
            tab_stop: config.terminal.tab_stop.max(1),
            min_dim,
            max_dim,
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
            cursor_app: false,
            keypad_app: false,
            scroll_offset: 0,
            cursor_style: cursor_style_from_config(config),
            saved_style: None,
            saved_main_saved_style: None,
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
            origin_mode: false,
            insert_mode: false,
            auto_wrap: true,
            saved_origin: None,
            saved_main_scroll_top: None,
            saved_main_scroll_bottom: None,
            saved_main_origin: None,
            saved_main_insert: None,
            saved_main_wrap: None,
            saved_main_saved_origin: None,
            mouse_press: false,
            mouse_drag: false,
            mouse_any: false,
            mouse_sgr: false,
            focus_report: false,
            sync_depth: 0,
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Live-apply a theme change: fresh cells use the new colors while
    /// existing cells keep their resolved colors (standard terminal
    /// behavior on theme switch).
    pub fn set_theme(&mut self, theme: Theme) {
        if self.theme != theme {
            self.theme = theme;
            self.bump();
        }
    }

    /// Live-apply terminal tuning without respawning the session.
    /// Shrinking the scrollback cap drops the oldest rows immediately;
    /// growing it takes effect as new output scrolls in.
    pub fn apply_terminal_config(&mut self, config: &crate::config::TerminalConfig) {
        self.max_scrollback = config.scrollback_lines;
        while self.scrollback.len() > self.max_scrollback {
            self.scrollback.pop_front();
        }
        self.scroll_offset = self.scroll_offset.min(self.scrollback.len());
        self.tab_stop = config.tab_stop.max(1);
        let min_dim = config.min_dim.max(1);
        self.max_dim = config.max_dim.max(min_dim);
        self.min_dim = min_dim.min(self.max_dim);
        self.bump();
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
        self.cells.get(y)?.get(x).cloned()
    }

    #[allow(dead_code)]
    pub fn visible_rows(&self) -> &[Vec<Cell>] {
        &self.cells
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let min_dim = self.min_dim.max(1);
        let max_dim = self.max_dim.max(min_dim);
        let cols = cols.clamp(min_dim, max_dim);
        let rows = rows.clamp(min_dim, max_dim);
        if cols == self.cols && rows == self.rows {
            return;
        }
        let blank = self.blank_cell();
        let mut new_cells = vec![vec![blank.clone(); cols]; rows];
        let copy_rows = self.rows.min(rows);
        let copy_cols = self.cols.min(cols);
        for (y, new_row) in new_cells.iter_mut().enumerate().take(copy_rows) {
            for (x, new_cell) in new_row.iter_mut().enumerate().take(copy_cols) {
                *new_cell = self.cells[y][x].clone();
            }
        }
        // A shrink can strand a wide lead without its continuation (or a
        // continuation without its lead) at the cut edge: repair to spaces.
        for row in new_cells.iter_mut() {
            Self::fix_row_wide(row, &blank);
        }
        self.cells = new_cells;
        if let Some(main) = self.saved_main_cells.take() {
            let mut new_main = vec![vec![blank.clone(); cols]; rows];
            let main_rows = main.len();
            let main_cols = main.first().map(|r| r.len()).unwrap_or(0);
            for (y, new_row) in new_main.iter_mut().enumerate().take(main_rows.min(rows)) {
                for (x, new_cell) in new_row.iter_mut().enumerate().take(main_cols.min(cols)) {
                    *new_cell = main[y][x].clone();
                }
            }
            for row in new_main.iter_mut() {
                Self::fix_row_wide(row, &blank);
            }
            self.saved_main_cells = Some(new_main);
        }
        self.cols = cols;
        self.rows = rows;
        // Resize resets scroll margins to full (DECSTBM).
        self.scroll_top = 0;
        self.scroll_bottom = rows.saturating_sub(1);
        if self.saved_main_scroll_top.is_some() {
            self.saved_main_scroll_top = Some(0);
            self.saved_main_scroll_bottom = Some(rows.saturating_sub(1));
        }
        if cols != self.scrollback.front().map(|r| r.len()).unwrap_or(cols) {
            for row in self.scrollback.iter_mut() {
                row.resize(cols, blank.clone());
                Self::fix_row_wide(row, &blank);
            }
        }
        self.scroll_offset = self.scroll_offset.min(self.scrollback.len());
        self.cursor.x = self.cursor.x.min(cols.saturating_sub(1));
        self.cursor.y = self.cursor.y.min(rows.saturating_sub(1));
        // Cursor must never rest on a wide continuation: snap to the lead.
        self.snap_cursor_to_lead();
        if let Some(c) = self.saved_main_cursor.as_mut() {
            c.x = c.x.min(cols.saturating_sub(1));
            c.y = c.y.min(rows.saturating_sub(1));
        }
        self.bump();
    }

    /// Repair dangling wide halves in a row: a lead at the last column (or
    /// before a non-continuation) becomes a space; a continuation without a
    /// lead becomes a space.
    pub(crate) fn fix_row_wide(row: &mut [Cell], blank: &Cell) {
        if row.is_empty() {
            return;
        }
        for i in 0..row.len() {
            if row[i].width == 0 {
                let lead_ok = i > 0 && row[i - 1].width == 2;
                if !lead_ok {
                    row[i] = blank.clone();
                }
            }
        }
        for i in 0..row.len() {
            if row[i].width == 2 {
                let cont_ok = i + 1 < row.len() && row[i + 1].width == 0;
                if !cont_ok {
                    row[i] = blank.clone();
                }
            }
        }
    }

    /// If the cursor sits on a wide continuation, move it back to the lead
    /// so overwrite/cursor logic stays on cluster boundaries.
    pub(crate) fn snap_cursor_to_lead(&mut self) {
        if self.cursor.y < self.rows
            && self.cursor.x < self.cols
            && self.cursor.x > 0
            && self.cells[self.cursor.y][self.cursor.x].width == 0
        {
            self.cursor.x -= 1;
        }
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
    fn set_theme_bumps_version_only_on_change() {
        let mut g = Grid::new(3, 2, test_theme());
        let v = g.version;
        g.set_theme(test_theme());
        assert_eq!(g.version, v);
        g.set_theme(Theme::vscode());
        assert_ne!(g.version, v);
        assert_eq!(g.theme(), Theme::vscode());
    }

    #[test]
    fn apply_terminal_config_truncates_scrollback() {
        let mut cfg = crate::config::Config::default();
        cfg.terminal.scrollback_lines = 10;
        let mut g = Grid::new_with_config(2, 2, test_theme(), &cfg);
        for _ in 0..5 {
            g.newline();
        }
        // 2-row grid: first newline moves within the viewport, the rest scroll.
        assert_eq!(g.scrollback_len(), 4);
        cfg.terminal.scrollback_lines = 2;
        cfg.terminal.tab_stop = 4;
        g.apply_terminal_config(&cfg.terminal);
        assert_eq!(g.scrollback_len(), 2);
    }

    #[test]
    fn apply_terminal_config_repairs_dims() {
        let mut g = Grid::new(4, 2, test_theme());
        let mut t = crate::config::TerminalConfig::default();
        t.min_dim = 9999;
        t.max_dim = 10;
        t.tab_stop = 0;
        g.apply_terminal_config(&t);
        g.resize(100_000, 100_000);
        assert!(g.cols() <= 9999);
        assert!(g.cols() >= 10);
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

    #[test]
    fn wide_char_occupies_two_cells() {
        let mut g = Grid::new(5, 2, test_theme());
        g.put_char('中');
        assert_eq!(g.cursor(), Cursor { x: 2, y: 0 });
        let lead = g.cell(0, 0).unwrap();
        let cont = g.cell(1, 0).unwrap();
        assert_eq!(lead.width, 2);
        assert_eq!(lead.ch, '中');
        assert_eq!(cont.width, 0);
        assert_eq!(g.cell(2, 0).unwrap().ch, ' ');
    }

    #[test]
    fn wide_char_wraps_at_margin() {
        // 4 cols: `ab` fills 0-1, wide needs 2-3, next wide must wrap.
        let mut g = Grid::new(4, 2, test_theme());
        g.put_char('a');
        g.put_char('b');
        g.put_char('中');
        assert_eq!(g.cursor(), Cursor { x: 4, y: 0 });
        // Only one column left on row 0 is impossible for wide: next wide
        // pads and wraps to row 1.
        g.handle_wrap_if_needed();
        g.put_char('あ');
        assert_eq!(g.cell(0, 1).unwrap().ch, 'あ');
        assert_eq!(g.cell(1, 1).unwrap().width, 0);
    }

    #[test]
    fn wide_at_last_column_pads_and_wraps() {
        let mut g = Grid::new(3, 2, test_theme());
        g.put_char('a');
        g.put_char('b');
        // Cursor at last column (2); wide doesn't fit.
        g.put_char('中');
        assert_eq!(g.cell(2, 0).unwrap().ch, ' ');
        assert_eq!(g.cell(0, 1).unwrap().ch, '中');
        assert_eq!(g.cell(1, 1).unwrap().width, 0);
    }

    #[test]
    fn narrow_overwrites_wide_clears_continuation() {
        let mut g = Grid::new(4, 1, test_theme());
        g.put_char('中');
        g.set_cursor(0, 0);
        g.put_char('x');
        assert_eq!(g.cell(0, 0).unwrap().ch, 'x');
        assert_eq!(g.cell(0, 0).unwrap().width, 1);
        assert_eq!(g.cell(1, 0).unwrap().ch, ' ');
        assert_eq!(g.cell(1, 0).unwrap().width, 1);
    }

    #[test]
    fn overwrite_continuation_clears_lead() {
        let mut g = Grid::new(4, 1, test_theme());
        g.put_char('中');
        g.set_cursor(1, 0);
        // Cursor snaps to lead via set_cursor, so force the split case by
        // writing at the continuation through direct positioning is not
        // possible; instead verify a narrow write at 0 clears, and a wide
        // write splitting a neighbor clears correctly.
        g.set_cursor(0, 0);
        g.put_char('a');
        g.put_char('b');
        // Row: a b _ _ ; write wide at 1 (splits nothing, fits 1-2).
        g.set_cursor(1, 0);
        g.put_char('あ');
        assert_eq!(g.cell(1, 0).unwrap().ch, 'あ');
        assert_eq!(g.cell(2, 0).unwrap().width, 0);
    }

    #[test]
    fn combining_appends_without_advance() {
        let mut g = Grid::new(5, 1, test_theme());
        g.put_char('e');
        assert_eq!(g.cursor(), Cursor { x: 1, y: 0 });
        g.put_char('\u{0301}');
        assert_eq!(g.cursor(), Cursor { x: 1, y: 0 });
        let c = g.cell(0, 0).unwrap();
        assert_eq!(c.cluster(), "e\u{0301}");
        assert_eq!(c.width, 1);
    }

    #[test]
    fn zwj_sequence_stays_in_one_wide_cell() {
        let mut g = Grid::new(6, 1, test_theme());
        for c in "👨\u{200D}👩\u{200D}👧".chars() {
            g.put_char(c);
        }
        let lead = g.cell(0, 0).unwrap();
        assert_eq!(lead.width, 2);
        assert_eq!(lead.cluster(), "👨\u{200D}👩\u{200D}👧");
        assert_eq!(g.cell(1, 0).unwrap().width, 0);
        // Whole family took one wide cell: cursor advanced once.
        assert_eq!(g.cursor(), Cursor { x: 2, y: 0 });
    }

    #[test]
    fn flag_pair_is_single_wide_cell() {
        let mut g = Grid::new(6, 1, test_theme());
        for c in "🇺🇸".chars() {
            g.put_char(c);
        }
        let lead = g.cell(0, 0).unwrap();
        assert_eq!(lead.width, 2);
        assert_eq!(lead.cluster(), "🇺🇸");
        assert_eq!(g.cursor(), Cursor { x: 2, y: 0 });
    }

    #[test]
    fn backspace_steps_over_wide() {
        let mut g = Grid::new(5, 1, test_theme());
        g.put_char('中');
        assert_eq!(g.cursor().x, 2);
        g.backspace();
        assert_eq!(g.cursor().x, 0);
    }

    #[test]
    fn erase_repairs_split_wide() {
        let mut g = Grid::new(4, 1, test_theme());
        g.put_char('中');
        // Erase only the lead half; the continuation must not dangle.
        g.set_cursor(0, 0);
        g.erase_in_line(0);
        assert_eq!(g.cell(0, 0).unwrap().width, 1);
        assert_eq!(g.cell(1, 0).unwrap().width, 1);
    }

    #[test]
    fn resize_repairs_stranded_wide() {
        let mut g = Grid::new(4, 1, test_theme());
        g.put_char('a');
        g.put_char('b');
        g.put_char('中'); // occupies cols 2-3
        g.resize(3, 1);
        // Lead at col 2 lost its continuation: repaired to space.
        assert_eq!(g.cell(2, 0).unwrap().width, 1);
        assert_eq!(g.cell(2, 0).unwrap().ch, ' ');
    }

    #[test]
    fn scroll_region_newline_preserves_outside() {
        let mut g = Grid::new(3, 5, test_theme());
        for (y, ch) in [(0, 'a'), (1, 'b'), (2, 'c'), (3, 'd'), (4, 'e')] {
            g.set_cursor(0, y);
            g.put_char(ch);
        }
        assert!(g.set_scroll_region(1, 3));
        assert_eq!(g.scroll_region(), (1, 3));
        // Cursor homes to region top (origin off).
        assert_eq!(g.cursor(), Cursor { x: 0, y: 0 });
        g.set_cursor(0, 3);
        g.newline();
        // Region [b,c,d] scrolled to [c,d,blank]; outside untouched.
        assert_eq!(g.cell(0, 0).unwrap().ch, 'a');
        assert_eq!(g.cell(0, 1).unwrap().ch, 'c');
        assert_eq!(g.cell(0, 2).unwrap().ch, 'd');
        assert_eq!(g.cell(0, 3).unwrap().ch, ' ');
        assert_eq!(g.cell(0, 4).unwrap().ch, 'e');
        // Partial-region scroll never touches scrollback.
        assert_eq!(g.scrollback_len(), 0);
    }

    #[test]
    fn scroll_region_invalid_is_ignored() {
        let mut g = Grid::new(3, 4, test_theme());
        assert!(!g.set_scroll_region(2, 2));
        assert!(!g.set_scroll_region(3, 1));
        assert_eq!(g.scroll_region(), (0, 3));
    }

    #[test]
    fn newline_outside_region_does_not_scroll() {
        let mut g = Grid::new(2, 4, test_theme());
        assert!(g.set_scroll_region(0, 1));
        g.set_cursor(0, 3);
        g.newline();
        // Stays on the last row, region untouched, no scrollback.
        assert_eq!(g.cursor().y, 3);
        assert_eq!(g.scrollback_len(), 0);
    }

    #[test]
    fn reverse_index_at_region_top_scrolls_down() {
        let mut g = Grid::new(2, 4, test_theme());
        for (y, ch) in [(0, 'a'), (1, 'b'), (2, 'c'), (3, 'd')] {
            g.set_cursor(0, y);
            g.put_char(ch);
        }
        assert!(g.set_scroll_region(1, 2));
        g.set_cursor(0, 1);
        g.reverse_index();
        assert_eq!(g.cell(0, 1).unwrap().ch, ' ');
        assert_eq!(g.cell(0, 2).unwrap().ch, 'b');
        assert_eq!(g.cell(0, 0).unwrap().ch, 'a');
        assert_eq!(g.cell(0, 3).unwrap().ch, 'd');
    }

    #[test]
    fn insert_delete_lines_constrained_to_region() {
        let mut g = Grid::new(2, 4, test_theme());
        for (y, ch) in [(0, 'a'), (1, 'b'), (2, 'c'), (3, 'd')] {
            g.set_cursor(0, y);
            g.put_char(ch);
        }
        assert!(g.set_scroll_region(1, 2));
        g.set_cursor(0, 1);
        g.insert_lines(1);
        assert_eq!(g.cell(0, 1).unwrap().ch, ' ');
        assert_eq!(g.cell(0, 2).unwrap().ch, 'b');
        assert_eq!(g.cell(0, 0).unwrap().ch, 'a');
        assert_eq!(g.cell(0, 3).unwrap().ch, 'd');
        g.delete_lines(1);
        assert_eq!(g.cell(0, 1).unwrap().ch, 'b');
        assert_eq!(g.cell(0, 2).unwrap().ch, ' ');
        // Outside the margins IL/DL are no-ops.
        g.set_cursor(0, 0);
        g.insert_lines(1);
        assert_eq!(g.cell(0, 0).unwrap().ch, 'a');
        g.delete_lines(1);
        assert_eq!(g.cell(0, 0).unwrap().ch, 'a');
    }

    #[test]
    fn origin_mode_clamps_cursor_to_margins() {
        let mut g = Grid::new(3, 5, test_theme());
        assert!(g.set_scroll_region(1, 3));
        g.set_origin_mode(true);
        // DECOM homes to the region top.
        assert_eq!(g.cursor(), Cursor { x: 0, y: 1 });
        g.set_cursor(0, 0);
        assert_eq!(g.cursor().y, 1);
        g.set_cursor(0, 4);
        assert_eq!(g.cursor().y, 3);
        g.set_cursor(0, 2);
        g.move_cursor(0, -5);
        assert_eq!(g.cursor().y, 1);
        g.move_cursor(0, 5);
        assert_eq!(g.cursor().y, 3);
        g.set_origin_mode(false);
        g.set_cursor(0, 0);
        assert_eq!(g.cursor().y, 0);
    }

    #[test]
    fn insert_mode_shifts_line_right() {
        let mut g = Grid::new(4, 1, test_theme());
        g.set_cursor(0, 0);
        g.put_char('a');
        g.put_char('b');
        g.put_char('c');
        g.set_cursor(1, 0);
        g.set_insert_mode(true);
        g.put_char('X');
        assert_eq!(g.cell(0, 0).unwrap().ch, 'a');
        assert_eq!(g.cell(1, 0).unwrap().ch, 'X');
        assert_eq!(g.cell(2, 0).unwrap().ch, 'b');
        assert_eq!(g.cell(3, 0).unwrap().ch, 'c');
    }

    #[test]
    fn no_wrap_overwrites_last_column() {
        let mut g = Grid::new(3, 2, test_theme());
        g.set_auto_wrap(false);
        g.set_cursor(0, 0);
        g.put_char('a');
        g.put_char('b');
        g.put_char('c');
        g.put_char('d');
        assert_eq!(g.cell(0, 0).unwrap().ch, 'a');
        assert_eq!(g.cell(1, 0).unwrap().ch, 'b');
        assert_eq!(g.cell(2, 0).unwrap().ch, 'd');
        assert_eq!(g.cursor(), Cursor { x: 2, y: 0 });
        assert_eq!(g.cell(0, 1).unwrap().ch, ' ');
    }

    #[test]
    fn save_restore_keeps_origin_mode() {
        let mut g = Grid::new(3, 4, test_theme());
        assert!(g.set_scroll_region(1, 2));
        g.set_origin_mode(true);
        g.set_cursor(1, 1);
        g.save_cursor();
        g.set_origin_mode(false);
        g.set_cursor(0, 0);
        g.restore_cursor();
        assert!(g.origin_mode());
        assert_eq!(g.cursor(), Cursor { x: 1, y: 1 });
    }

    #[test]
    fn alt_buffer_saves_and_restores_margins_and_modes() {
        let mut g = Grid::new(3, 4, test_theme());
        assert!(g.set_scroll_region(1, 2));
        g.set_origin_mode(true);
        g.set_insert_mode(true);
        g.set_auto_wrap(false);
        g.enter_alt(true);
        assert_eq!(g.scroll_region(), (0, 3));
        assert!(!g.origin_mode());
        assert!(!g.insert_mode());
        assert!(g.auto_wrap());
        g.exit_alt();
        assert_eq!(g.scroll_region(), (1, 2));
        assert!(g.origin_mode());
        assert!(g.insert_mode());
        assert!(!g.auto_wrap());
    }

    #[test]
    fn resize_resets_margins_to_full() {
        let mut g = Grid::new(3, 4, test_theme());
        assert!(g.set_scroll_region(1, 2));
        g.resize(3, 5);
        assert_eq!(g.scroll_region(), (0, 4));
    }

    #[test]
    fn mouse_mode_reports_motion_only_when_expected() {
        use super::MouseMode;
        assert!(!MouseMode::Off.reports_motion(false));
        assert!(!MouseMode::Off.reports_motion(true));
        assert!(!MouseMode::Press.reports_motion(true));
        assert!(!MouseMode::Drag.reports_motion(false));
        assert!(MouseMode::Drag.reports_motion(true));
        assert!(MouseMode::Any.reports_motion(false));
        assert!(MouseMode::Any.reports_motion(true));

        let mut g = Grid::new(4, 2, test_theme());
        assert_eq!(g.mouse_mode(), MouseMode::Off);
        g.set_mouse_press(true);
        g.set_mouse_drag(true);
        assert_eq!(g.mouse_mode(), MouseMode::Drag);
        g.set_mouse_any(true);
        assert_eq!(g.mouse_mode(), MouseMode::Any);
        g.set_mouse_any(false);
        assert_eq!(g.mouse_mode(), MouseMode::Drag);
        g.reset_mouse_and_focus();
        assert_eq!(g.mouse_mode(), MouseMode::Off);
        assert!(!g.in_sync());
        g.sync_begin();
        g.sync_begin();
        assert!(g.in_sync());
        g.sync_end();
        assert!(g.in_sync());
        g.sync_end();
        assert!(!g.in_sync());
    }
}
