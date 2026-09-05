//! Text selection model.
//!
//! Pure, toolkit-free state so it stays unit-testable: the GUI (`main.rs`)
//! owns a [`Selection`] in global line space and the renderer only paints
//! what it is given.

/// Position in global line space: `y` counts from the oldest scrollback
/// line (`0`) through the visible cells. `x` is a cell column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellPos {
    pub x: usize,
    pub y: usize,
}

/// Character-wise selection from `anchor` (press point) to `active`
/// (current drag point). Normalized on read so drag direction doesn't matter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: CellPos,
    pub active: CellPos,
}

impl Selection {
    pub fn new(anchor: CellPos) -> Self {
        Self {
            anchor,
            active: anchor,
        }
    }

    pub fn update(&mut self, active: CellPos) {
        self.active = active;
    }

    /// Ordered `(start, end)` with `start <= end` in reading order.
    pub fn normalized(&self) -> (CellPos, CellPos) {
        if (self.active.y, self.active.x) < (self.anchor.y, self.anchor.x) {
            (self.active, self.anchor)
        } else {
            (self.anchor, self.active)
        }
    }

    /// True when the selection covers more than a single cursor point.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.active
    }

    /// Hit-test a global cell.
    #[allow(dead_code)]
    pub fn contains(&self, x: usize, y: usize) -> bool {
        let (s, e) = self.normalized();
        if y < s.y || y > e.y {
            return false;
        }
        if s.y == e.y {
            return x >= s.x && x <= e.x;
        }
        if y == s.y {
            return x >= s.x;
        }
        if y == e.y {
            return x <= e.x;
        }
        true
    }
}

/// Convert a view row (0 = top of screen) to a global line index.
pub fn view_to_global(view_row: usize, scrollback_len: usize, scroll_offset: usize) -> usize {
    scrollback_len.saturating_sub(scroll_offset) + view_row
}

/// Convert a global line index to a view row, or `None` when scrolled out.
#[allow(dead_code)]
pub fn global_to_view(global: usize, scrollback_len: usize, scroll_offset: usize) -> Option<usize> {
    let base = scrollback_len.saturating_sub(scroll_offset);
    global.checked_sub(base)
}

/// Map a view-space selection to view rows for the renderer.
/// Returns `None` when the selection is empty or entirely off-screen.
pub fn selection_to_view(
    sel: &Selection,
    scrollback_len: usize,
    scroll_offset: usize,
    rows: usize,
) -> Option<((usize, usize), (usize, usize))> {
    if sel.is_empty() || rows == 0 {
        return None;
    }
    let (s, e) = sel.normalized();
    let base = scrollback_len.saturating_sub(scroll_offset);
    let end_base = base + rows;
    if e.y < base || s.y >= end_base {
        return None;
    }
    let clamp_view = |p: CellPos| -> (usize, usize) {
        let y = p.y.clamp(base, end_base.saturating_sub(1)) - base;
        (p.x, y)
    };
    Some((clamp_view(s), clamp_view(e)))
}

/// Expand a cell to its word on a single visual row.
/// Word chars are `[A-Za-z0-9_]`; anything else (including spaces) breaks.
pub fn expand_word(row_text: &[char], x: usize) -> (usize, usize) {
    if row_text.is_empty() {
        return (0, 0);
    }
    let x = x.min(row_text.len().saturating_sub(1));
    if !is_word_char(row_text[x]) {
        return (x, x);
    }
    let mut start = x;
    while start > 0 && is_word_char(row_text[start - 1]) {
        start -= 1;
    }
    let mut end = x;
    while end + 1 < row_text.len() && is_word_char(row_text[end + 1]) {
        end += 1;
    }
    (start, end)
}

fn is_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// Extract selected text from lines resolved by `line_at`.
///
/// `line_at(global)` returns the full row's chars (untrimmed); missing lines
/// are treated as blank. Each line is trimmed of trailing spaces and lines
/// are joined with `\n`.
pub fn extract_text(
    sel: &Selection,
    line_at: impl Fn(usize) -> Option<Vec<char>>,
) -> Option<String> {
    if sel.is_empty() {
        return None;
    }
    let (s, e) = sel.normalized();
    let mut out = String::new();
    for y in s.y..=e.y {
        let line = line_at(y).unwrap_or_default();
        let trimmed_end = line
            .iter()
            .rposition(|c| *c != ' ')
            .map(|i| i + 1)
            .unwrap_or(0);
        let from = if y == s.y { s.x.min(trimmed_end) } else { 0 };
        let to = if y == e.y {
            e.x.saturating_add(1).min(trimmed_end)
        } else {
            trimmed_end
        };
        if y > s.y {
            out.push('\n');
        }
        if from < to {
            out.extend(line[from..to].iter());
        }
    }
    Some(out)
}

/// Map physical pixels to a cell `(col, row)`.
/// Pure helper so it stays unit-testable without a `Renderer`.
pub fn cell_at_pos(
    x: f32,
    y: f32,
    cell_width: f32,
    line_height: f32,
    cols: usize,
    rows: usize,
) -> Option<(usize, usize)> {
    if !x.is_finite()
        || !y.is_finite()
        || cell_width <= 0.0
        || line_height <= 0.0
        || cols == 0
        || rows == 0
    {
        return None;
    }
    if x < 0.0 || y < 0.0 {
        return None;
    }
    let col = (x / cell_width).floor() as usize;
    let row = (y / line_height).floor() as usize;
    if col >= cols || row >= rows {
        return None;
    }
    Some((col, row))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(ax: usize, ay: usize, bx: usize, by: usize) -> Selection {
        Selection {
            anchor: CellPos { x: ax, y: ay },
            active: CellPos { x: bx, y: by },
        }
    }

    #[test]
    fn normalizes_drag_direction() {
        let s = sel(5, 2, 1, 0);
        let (a, b) = s.normalized();
        assert_eq!((a.x, a.y), (1, 0));
        assert_eq!((b.x, b.y), (5, 2));
    }

    #[test]
    fn empty_single_click() {
        let s = sel(2, 1, 2, 1);
        assert!(s.is_empty());
        assert!(s.contains(2, 1));
        assert!(!s.contains(3, 1));
        assert_eq!(extract_text(&s, |_| None), None);
    }

    #[test]
    fn contains_spans_lines() {
        let s = sel(3, 0, 1, 2);
        assert!(s.contains(5, 0));
        assert!(!s.contains(2, 0));
        assert!(s.contains(0, 1));
        assert!(s.contains(0, 2));
        assert!(s.contains(1, 2));
        assert!(!s.contains(2, 2));
        assert!(!s.contains(0, 3));
    }

    #[test]
    fn view_global_roundtrip() {
        // 10 scrollback lines, viewing 3 up: base = 7.
        assert_eq!(view_to_global(0, 10, 3), 7);
        assert_eq!(global_to_view(7, 10, 3), Some(0));
        assert_eq!(global_to_view(6, 10, 3), None);
    }

    #[test]
    fn selection_to_view_clamps() {
        let s = sel(1, 5, 3, 50);
        // scrollback 10, live view of 4 rows: visible globals 10..14.
        let view = selection_to_view(&s, 10, 0, 4).unwrap();
        assert_eq!(view, ((1, 0), (3, 3)));
        // Fully off-screen above.
        let s2 = sel(0, 0, 2, 3);
        assert_eq!(selection_to_view(&s2, 10, 0, 4), None);
    }

    #[test]
    fn extract_trims_and_joins() {
        let lines: Vec<Vec<char>> =
            vec!["hello   ".chars().collect(), "  hi    ".chars().collect()];
        let s = sel(1, 0, 3, 1);
        let text = extract_text(&s, |y| lines.get(y).cloned()).unwrap();
        assert_eq!(text, "ello\n  hi");
    }

    #[test]
    fn expand_word_stops_at_delimiters() {
        let row: Vec<char> = "foo bar_baz-qux".chars().collect();
        assert_eq!(expand_word(&row, 1), (0, 2));
        assert_eq!(expand_word(&row, 5), (4, 10));
        // On a delimiter selects just that cell.
        assert_eq!(expand_word(&row, 3), (3, 3));
        assert_eq!(expand_word(&row, 11), (11, 11));
    }

    #[test]
    fn cell_at_pos_floors_and_clamps() {
        assert_eq!(cell_at_pos(5.0, 5.0, 10.0, 20.0, 80, 24), Some((0, 0)));
        assert_eq!(cell_at_pos(25.0, 45.0, 10.0, 20.0, 80, 24), Some((2, 2)));
        assert_eq!(cell_at_pos(-1.0, 5.0, 10.0, 20.0, 80, 24), None);
        assert_eq!(cell_at_pos(800.0, 5.0, 10.0, 20.0, 80, 24), None);
        assert_eq!(cell_at_pos(5.0, 5.0, 0.0, 20.0, 80, 24), None);
    }
}
