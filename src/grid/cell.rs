//! Cell + pen types and blank-cell constructors.

use crate::theme::{Rgb, Theme};

use super::Grid;

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

/// DECSCUSR cursor shape (`CSI Ps SP q`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

/// DECSCUSR cursor style: shape + blink phase.
/// `blinking == true` follows the app blink timer; `false` is steady.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorStyle {
    pub shape: CursorShape,
    pub blinking: bool,
}

impl Default for CursorStyle {
    fn default() -> Self {
        Self {
            shape: CursorShape::Block,
            blinking: true,
        }
    }
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

impl Grid {
    pub(crate) fn blank_cell(&self) -> Cell {
        Cell {
            ch: ' ',
            fg: self.theme.foreground,
            bg: self.theme.background,
            bold: false,
            underline: false,
        }
    }

    /// BCE erase cell: blank char with current pen bg, default fg/attrs.
    /// Used by ED/EL, clear, and IL/DL/scroll fill. `blank_cell()` stays for
    /// structural fills (resize growth, fresh alt buffer) that must not
    /// inherit the pen bg.
    pub(crate) fn erase_cell(&self) -> Cell {
        Cell {
            ch: ' ',
            fg: self.theme.foreground,
            bg: self.pen.bg,
            bold: false,
            underline: false,
        }
    }

    pub fn blank_row(&self) -> Vec<Cell> {
        vec![self.blank_cell(); self.cols]
    }

    pub(crate) fn erase_row(&self) -> Vec<Cell> {
        vec![self.erase_cell(); self.cols]
    }

    pub(crate) fn default_pen(&self) -> Pen {
        Pen {
            fg: self.theme.foreground,
            bg: self.theme.background,
            bold: false,
            underline: false,
        }
    }
}
