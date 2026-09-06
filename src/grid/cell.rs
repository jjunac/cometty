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

    pub fn blank_row(&self) -> Vec<Cell> {
        vec![self.blank_cell(); self.cols]
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
