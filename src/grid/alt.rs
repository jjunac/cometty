//! Alt-screen buffer + mode flags (cursor visibility, bracketed paste).

use super::Grid;
use super::cell::{Cursor, CursorStyle};

impl Grid {
    pub fn save_cursor(&mut self) {
        self.saved_cursor = Some(self.cursor);
        self.saved_pen = Some(self.pen);
        self.saved_style = Some(self.cursor_style);
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
        if let Some(s) = self.saved_style
            && self.cursor_style != s
        {
            self.cursor_style = s;
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

    pub fn cursor_style(&self) -> CursorStyle {
        self.cursor_style
    }

    pub fn set_cursor_style(&mut self, style: CursorStyle) {
        if self.cursor_style != style {
            self.cursor_style = style;
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
        self.saved_main_saved_style = self.saved_style;
        self.cells = vec![self.blank_row(); self.rows];
        self.cursor = Cursor { x: 0, y: 0 };
        self.saved_cursor = None;
        self.saved_pen = None;
        self.saved_style = None;
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
        self.saved_style = self.saved_main_saved_style.take();
        self.in_alt = false;
        self.scroll_offset = 0;
        self.bump();
    }
}
