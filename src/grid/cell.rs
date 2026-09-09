//! Cell + pen types and blank-cell constructors.

use crate::theme::{Rgb, Theme};

use super::Grid;

/// SGR underline style (`CSI 4[:style] m`).
/// `Single` is plain `CSI 4 m`; `Double/Curly/Dotted/Dashed` come from
/// `CSI 4:2-5 m`. Curly/dotted/dashed render as single for now but are
/// stored distinctly for future shaping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UnderlineStyle {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

impl UnderlineStyle {
    pub fn is_active(self) -> bool {
        !matches!(self, UnderlineStyle::None)
    }
}

/// A terminal cell holds one grapheme cluster: `ch` is the first char and
/// `extra` carries the rest (`combining` marks, `ZWJ` sequences, `VS16`,
/// skin tones, flags). `width` is the display width: `0` = wide
/// continuation placeholder, `1` = narrow, `2` = wide lead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub extra: Option<Box<str>>,
    pub width: u8,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub inverse: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub underline: UnderlineStyle,
    pub underline_color: Option<Rgb>,
}

impl Cell {
    /// Effective foreground after inverse + dim (dim applies to text only).
    pub fn effective_fg(&self) -> Rgb {
        let base = if self.inverse { self.bg } else { self.fg };
        if self.dim { base.dimmed() } else { base }
    }

    /// Effective background after inverse (selection overrides at paint).
    pub fn effective_bg(&self) -> Rgb {
        if self.inverse { self.fg } else { self.bg }
    }

    /// Full cluster text for shaping / copy (`ch` + `extra`).
    /// Continuations return a single space (they are skipped by callers).
    pub fn cluster(&self) -> String {
        if self.width == 0 {
            return " ".to_string();
        }
        let mut s = String::with_capacity(
            self.ch.len_utf8() + self.extra.as_ref().map(|e| e.len()).unwrap_or(0),
        );
        s.push(self.ch);
        if let Some(extra) = self.extra.as_ref() {
            s.push_str(extra);
        }
        s
    }

    /// True for the trailing placeholder of a double-width cell.
    #[allow(dead_code)]
    pub fn is_continuation(&self) -> bool {
        self.width == 0
    }

    /// True for the lead half of a double-width cell.
    #[allow(dead_code)]
    pub fn is_wide_lead(&self) -> bool {
        self.width == 2
    }
}

impl Default for Cell {
    fn default() -> Self {
        let theme = Theme::default();
        Self {
            ch: ' ',
            extra: None,
            width: 1,
            fg: theme.foreground,
            bg: theme.background,
            bold: false,
            dim: false,
            italic: false,
            inverse: false,
            strikethrough: false,
            overline: false,
            underline: UnderlineStyle::None,
            underline_color: None,
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
    pub dim: bool,
    pub italic: bool,
    pub inverse: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub underline: UnderlineStyle,
    pub underline_color: Option<Rgb>,
}

impl Default for Pen {
    fn default() -> Self {
        let theme = Theme::default();
        Self {
            fg: theme.foreground,
            bg: theme.background,
            bold: false,
            dim: false,
            italic: false,
            inverse: false,
            strikethrough: false,
            overline: false,
            underline: UnderlineStyle::None,
            underline_color: None,
        }
    }
}

impl Grid {
    pub(crate) fn blank_cell(&self) -> Cell {
        Cell {
            ch: ' ',
            extra: None,
            width: 1,
            fg: self.theme.foreground,
            bg: self.theme.background,
            bold: false,
            dim: false,
            italic: false,
            inverse: false,
            strikethrough: false,
            overline: false,
            underline: UnderlineStyle::None,
            underline_color: None,
        }
    }

    /// BCE erase cell: blank char with current pen bg, default fg/attrs.
    /// Used by ED/EL, clear, and IL/DL/scroll fill. `blank_cell()` stays for
    /// structural fills (resize growth, fresh alt buffer) that must not
    /// inherit the pen bg.
    pub(crate) fn erase_cell(&self) -> Cell {
        Cell {
            ch: ' ',
            extra: None,
            width: 1,
            fg: self.theme.foreground,
            bg: self.pen.bg,
            bold: false,
            dim: false,
            italic: false,
            inverse: false,
            strikethrough: false,
            overline: false,
            underline: UnderlineStyle::None,
            underline_color: None,
        }
    }

    /// Trailing placeholder for a double-width lead. Inherits the lead's
    /// style so cursor/selection painting stays consistent; text callers
    /// skip continuations.
    pub(crate) fn placeholder_cell(&self) -> Cell {
        Cell {
            ch: ' ',
            extra: None,
            width: 0,
            fg: self.pen.fg,
            bg: self.pen.bg,
            bold: self.pen.bold,
            dim: self.pen.dim,
            italic: self.pen.italic,
            inverse: self.pen.inverse,
            strikethrough: self.pen.strikethrough,
            overline: self.pen.overline,
            underline: self.pen.underline,
            underline_color: self.pen.underline_color,
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
            dim: false,
            italic: false,
            inverse: false,
            strikethrough: false,
            overline: false,
            underline: UnderlineStyle::None,
            underline_color: None,
        }
    }
}
