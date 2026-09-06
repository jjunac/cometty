//! SGR pen + extended palette.

use crate::theme::Rgb;

use super::Grid;

impl Grid {
    // Pen / SGR
    pub fn sgr(&mut self, params: &[u16]) {
        if params.is_empty() {
            self.pen = self.default_pen();
            return;
        }
        let mut i = 0;
        while i < params.len() {
            let p = params[i];
            match p {
                0 => self.pen = self.default_pen(),
                1 => self.pen.bold = true,
                4 => self.pen.underline = true,
                22 => self.pen.bold = false,
                24 => self.pen.underline = false,
                30..=37 => self.pen.fg = self.theme.ansi((p - 30) as u8),
                39 => self.pen.fg = self.theme.foreground,
                40..=47 => self.pen.bg = self.theme.ansi((p - 40) as u8),
                49 => self.pen.bg = self.theme.background,
                90..=97 => self.pen.fg = self.theme.ansi((p - 90 + 8) as u8),
                100..=107 => self.pen.bg = self.theme.ansi((p - 100 + 8) as u8),
                // 38 / 48 extended color: only support 38;5;n and 48;5;n, plus 38;2;r;g;b
                38 | 48 => {
                    let is_fg = p == 38;
                    if i + 1 < params.len() {
                        let mode = params[i + 1];
                        if mode == 5 && i + 2 < params.len() {
                            let idx = params[i + 2].min(255) as u8;
                            // map 256 palette approximately: 0-15 use theme, else grayscale/rgb cube approx
                            let col = self.extended_palette(idx);
                            if is_fg {
                                self.pen.fg = col;
                            } else {
                                self.pen.bg = col;
                            }
                            i += 2;
                        } else if mode == 2 && i + 4 < params.len() {
                            let r = params[i + 2].min(255) as u8;
                            let g = params[i + 3].min(255) as u8;
                            let b = params[i + 4].min(255) as u8;
                            if is_fg {
                                self.pen.fg = Rgb::new(r, g, b);
                            } else {
                                self.pen.bg = Rgb::new(r, g, b);
                            }
                            i += 4;
                        } else {
                            i += 1;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
        self.bump();
    }

    pub(crate) fn extended_palette(&self, idx: u8) -> Rgb {
        if idx < 16 {
            return self.theme.ansi(idx);
        }
        if (16..232).contains(&idx) {
            let i = idx - 16;
            let r = (i / 36) % 6;
            let g = (i / 6) % 6;
            let b = i % 6;
            let conv = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            return Rgb::new(conv(r), conv(g), conv(b));
        }
        let v = 8 + (idx - 232) * 10;
        Rgb::new(v, v, v)
    }
}
