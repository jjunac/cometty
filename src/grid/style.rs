//! SGR pen + extended palette.

use crate::theme::Rgb;

use super::Grid;
use super::cell::UnderlineStyle;

impl Grid {
    // Pen / SGR (flat wrapper for tests + `ESC c` reset).
    pub fn sgr(&mut self, params: &[u16]) {
        if params.is_empty() {
            self.pen = self.default_pen();
            self.bump();
            return;
        }
        let groups: Vec<Vec<u16>> = params.iter().map(|&p| vec![p]).collect();
        self.sgr_groups(&groups);
    }

    /// SGR with subparam grouping preserved (`:` stays in one group).
    /// Each group is one `;`-separated param; `CSI 4:3 m` arrives as
    /// `[[4, 3]]` (curly) while `CSI 4;3 m` arrives as `[[4], [3]]`
    /// (underline + italic).
    pub fn sgr_groups(&mut self, groups: &[Vec<u16>]) {
        if groups.is_empty() {
            self.pen = self.default_pen();
            self.bump();
            return;
        }
        let mut idx = 0;
        while idx < groups.len() {
            let g = &groups[idx];
            if g.is_empty() {
                self.pen = self.default_pen();
                idx += 1;
                continue;
            }
            let p = g[0];
            // `4` with subparams selects the underline style.
            if p == 4 && g.len() > 1 {
                self.pen.underline = match g[1] {
                    0 => UnderlineStyle::None,
                    2 => UnderlineStyle::Double,
                    3 => UnderlineStyle::Curly,
                    4 => UnderlineStyle::Dotted,
                    5 => UnderlineStyle::Dashed,
                    _ => UnderlineStyle::Single,
                };
                idx += 1;
                continue;
            }
            // `38/48/58` with subparams: colon color form.
            if (p == 38 || p == 48 || p == 58) && g.len() > 1 {
                if let Some(col) = Self::colon_color(&g[1..], &self.theme) {
                    Self::apply_extended(&mut self.pen, p, col);
                    idx += 1;
                    continue;
                }
                // Incomplete colon form: fall back to following `;` groups.
                // `38:5;n` split as [[38,5],[n]] or `38:2;r;g;b` as
                // [[38,2],[r],[g],[b]].
                if g[1] == 5
                    && g.len() == 2
                    && idx + 1 < groups.len()
                    && let Some(&n) = groups[idx + 1].first()
                {
                    let col = self.extended_palette(n.min(255) as u8);
                    Self::apply_extended(&mut self.pen, p, col);
                    idx += 2;
                    continue;
                }
                if g[1] == 2 && idx + 1 < groups.len() {
                    let mut vals: Vec<u16> = Vec::new();
                    let mut look = idx + 1;
                    while look < groups.len() && vals.len() < 3 {
                        if let Some(&v) = groups[look].first() {
                            vals.push(v);
                        } else {
                            break;
                        }
                        look += 1;
                    }
                    if vals.len() == 3 {
                        let col = Rgb::new(
                            vals[0].min(255) as u8,
                            vals[1].min(255) as u8,
                            vals[2].min(255) as u8,
                        );
                        Self::apply_extended(&mut self.pen, p, col);
                        idx = look;
                        continue;
                    }
                }
                idx += 1;
                continue;
            }
            match p {
                0 => self.pen = self.default_pen(),
                1 => self.pen.bold = true,
                2 => self.pen.dim = true,
                3 => self.pen.italic = true,
                4 => self.pen.underline = UnderlineStyle::Single,
                7 => self.pen.inverse = true,
                9 => self.pen.strikethrough = true,
                // 21 is CancelBold (matches vte reference); double
                // underline is `4:2`, not `21`.
                21 => self.pen.bold = false,
                22 => {
                    self.pen.bold = false;
                    self.pen.dim = false;
                }
                23 => self.pen.italic = false,
                24 => self.pen.underline = UnderlineStyle::None,
                27 => self.pen.inverse = false,
                29 => self.pen.strikethrough = false,
                53 => self.pen.overline = true,
                55 => self.pen.overline = false,
                30..=37 => self.pen.fg = self.theme.ansi((p - 30) as u8),
                39 => self.pen.fg = self.theme.foreground,
                40..=47 => self.pen.bg = self.theme.ansi((p - 40) as u8),
                49 => self.pen.bg = self.theme.background,
                59 => self.pen.underline_color = None,
                90..=97 => self.pen.fg = self.theme.ansi((p - 90 + 8) as u8),
                100..=107 => self.pen.bg = self.theme.ansi((p - 100 + 8) as u8),
                // 38 / 48 / 58 semicolon form: consume mode on truncated
                // failure (matching legacy flat behavior) so `38;2;10`
                // swallows `2` instead of turning on dim; leftover values
                // that are real codes (e.g. `1` in `38;2;1`) still apply.
                38 | 48 | 58 => {
                    if !self.consume_semicolon_color(groups, &mut idx) {
                        let swallow_mode = matches!(
                            groups.get(idx + 1).and_then(|g| g.first()),
                            Some(2) | Some(5)
                        ) && groups.get(idx + 1).is_some_and(|g| g.len() == 1);
                        idx += if swallow_mode { 2 } else { 1 };
                    }
                    continue;
                }
                _ => {}
            }
            idx += 1;
        }
        self.bump();
    }

    /// Colon color tail after `38/48/58` (e.g. `[5, n]` or `[2, r, g, b]`
    /// or `[2, cs, r, g, b]` with colorspace skipped).
    fn colon_color(tail: &[u16], theme: &crate::theme::Theme) -> Option<Rgb> {
        if tail.is_empty() {
            return None;
        }
        match tail[0] {
            5 => {
                let n = *tail.get(1)? as u8;
                Some(Self::palette_for(theme, n))
            }
            2 => {
                if tail.len() == 4 {
                    // [2, r, g, b]
                    Some(Rgb::new(
                        tail[1].min(255) as u8,
                        tail[2].min(255) as u8,
                        tail[3].min(255) as u8,
                    ))
                } else if tail.len() >= 5 {
                    // [2, cs, r, g, b]: skip colorspace.
                    Some(Rgb::new(
                        tail[2].min(255) as u8,
                        tail[3].min(255) as u8,
                        tail[4].min(255) as u8,
                    ))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn palette_for(theme: &crate::theme::Theme, idx: u8) -> Rgb {
        if idx < 16 {
            return theme.ansi(idx);
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

    fn apply_extended(pen: &mut super::cell::Pen, target: u16, col: Rgb) {
        match target {
            38 => pen.fg = col,
            48 => pen.bg = col,
            58 => pen.underline_color = Some(col),
            _ => {}
        }
    }

    /// Try `38;5;n` / `38;2;r;g;b` (and `48`/`58`) from following groups.
    /// Returns true when a color was applied (advancing `idx` past the
    /// consumed groups); false leaves `idx` untouched for normal handling.
    fn consume_semicolon_color(&mut self, groups: &[Vec<u16>], idx: &mut usize) -> bool {
        let target = groups[*idx][0];
        let mode = match groups.get(*idx + 1).and_then(|g| g.first()) {
            Some(&m) => m,
            None => return false,
        };
        // Require singleton mode group to avoid eating a colon group like
        // `[4, 3]` as a color mode.
        if groups[*idx + 1].len() != 1 {
            // Allow `[2, r, g, b]` colon tail after `;` (e.g. `38;2:10:20:30`).
            let ng = &groups[*idx + 1];
            if ng.len() >= 4 && ng[0] == 2 {
                let col = if ng.len() >= 5 {
                    Rgb::new(
                        ng[2].min(255) as u8,
                        ng[3].min(255) as u8,
                        ng[4].min(255) as u8,
                    )
                } else {
                    Rgb::new(
                        ng[1].min(255) as u8,
                        ng[2].min(255) as u8,
                        ng[3].min(255) as u8,
                    )
                };
                Self::apply_extended(&mut self.pen, target, col);
                *idx += 2;
                return true;
            }
            if ng.len() >= 2 && ng[0] == 5 {
                let col = self.extended_palette(ng[1].min(255) as u8);
                Self::apply_extended(&mut self.pen, target, col);
                *idx += 2;
                return true;
            }
            return false;
        }
        if mode == 5 {
            let n = match groups.get(*idx + 2).and_then(|g| g.first()) {
                Some(&n) if groups[*idx + 2].len() == 1 => n,
                _ => return false,
            };
            let col = self.extended_palette(n.min(255) as u8);
            Self::apply_extended(&mut self.pen, target, col);
            *idx += 3;
            true
        } else if mode == 2 {
            let (r, g, b) = match (
                groups.get(*idx + 2),
                groups.get(*idx + 3),
                groups.get(*idx + 4),
            ) {
                (Some(r), Some(g), Some(b)) if r.len() == 1 && g.len() == 1 && b.len() == 1 => {
                    (r[0], g[0], b[0])
                }
                _ => return false,
            };
            let col = Rgb::new(r.min(255) as u8, g.min(255) as u8, b.min(255) as u8);
            Self::apply_extended(&mut self.pen, target, col);
            *idx += 5;
            true
        } else {
            false
        }
    }

    pub(crate) fn extended_palette(&self, idx: u8) -> Rgb {
        Self::palette_for(&self.theme, idx)
    }
}
