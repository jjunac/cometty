//! Alt-screen buffer + mode flags (cursor visibility, bracketed paste).

use super::Grid;
use super::cell::{Cursor, CursorStyle};

impl Grid {
    pub fn save_cursor(&mut self) {
        self.saved_cursor = Some(self.cursor);
        self.saved_pen = Some(self.pen);
        self.saved_style = Some(self.cursor_style);
        self.saved_origin = Some(self.origin_mode);
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
        if let Some(o) = self.saved_origin {
            self.origin_mode = o;
        }
        self.snap_cursor_to_lead();
        self.clamp_cursor_to_margins();
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

    pub fn cursor_app_mode(&self) -> bool {
        self.cursor_app
    }

    pub fn set_cursor_app_mode(&mut self, enabled: bool) {
        if self.cursor_app != enabled {
            self.cursor_app = enabled;
            self.bump();
        }
    }

    pub fn keypad_app_mode(&self) -> bool {
        self.keypad_app
    }

    pub fn set_keypad_app_mode(&mut self, enabled: bool) {
        if self.keypad_app != enabled {
            self.keypad_app = enabled;
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

    pub fn origin_mode(&self) -> bool {
        self.origin_mode
    }

    pub fn set_origin_mode(&mut self, enabled: bool) {
        if self.origin_mode != enabled {
            self.origin_mode = enabled;
            // DECOM homes the cursor (origin-aware).
            self.home_cursor();
            self.bump();
        }
    }

    #[allow(dead_code)]
    pub fn insert_mode(&self) -> bool {
        self.insert_mode
    }

    pub fn set_insert_mode(&mut self, enabled: bool) {
        if self.insert_mode != enabled {
            self.insert_mode = enabled;
            self.bump();
        }
    }

    pub fn auto_wrap(&self) -> bool {
        self.auto_wrap
    }

    pub fn set_auto_wrap(&mut self, enabled: bool) {
        if self.auto_wrap != enabled {
            self.auto_wrap = enabled;
            self.bump();
        }
    }

    /// Full DEC reset for `ESC c`: margins + origin/insert/wrap back to
    /// defaults (cursor homing is done by the caller).
    pub fn reset_margins_and_modes(&mut self) {
        self.reset_scroll_region();
        self.origin_mode = false;
        self.insert_mode = false;
        self.auto_wrap = true;
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
        self.saved_main_saved_origin = self.saved_origin;
        self.saved_main_scroll_top = Some(self.scroll_top);
        self.saved_main_scroll_bottom = Some(self.scroll_bottom);
        self.saved_main_origin = Some(self.origin_mode);
        self.saved_main_insert = Some(self.insert_mode);
        self.saved_main_wrap = Some(self.auto_wrap);
        self.cells = vec![self.blank_row(); self.rows];
        self.cursor = Cursor { x: 0, y: 0 };
        self.saved_cursor = None;
        self.saved_pen = None;
        self.saved_style = None;
        self.saved_origin = None;
        self.reset_margins_and_modes();
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
        self.saved_origin = self.saved_main_saved_origin.take();
        if let (Some(top), Some(bottom)) = (
            self.saved_main_scroll_top.take(),
            self.saved_main_scroll_bottom.take(),
        ) {
            let max = self.rows.saturating_sub(1);
            self.scroll_top = top.min(max);
            self.scroll_bottom = bottom.min(max);
            if self.scroll_top >= self.scroll_bottom && self.rows > 1 {
                self.reset_scroll_region();
            }
        }
        if let Some(o) = self.saved_main_origin.take() {
            self.origin_mode = o;
        }
        if let Some(i) = self.saved_main_insert.take() {
            self.insert_mode = i;
        }
        if let Some(w) = self.saved_main_wrap.take() {
            self.auto_wrap = w;
        }
        self.in_alt = false;
        self.scroll_offset = 0;
        self.clamp_cursor_to_margins();
        self.bump();
    }
}
