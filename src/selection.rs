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

/// Drag-extend granularity: single-click chars, double-click words,
/// triple-click lines. Stored per tab while `selecting` is true.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectMode {
    #[default]
    Char,
    Word,
    Line,
}

/// One grid column for selection math: `text` is the full grapheme cluster
/// for a lead/narrow cell, `width` is 0 (continuation), 1 or 2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelCell {
    pub text: String,
    pub width: u8,
}

impl SelCell {
    pub fn narrow(text: &str) -> Self {
        Self {
            text: text.to_string(),
            width: 1,
        }
    }

    /// Test/helper constructor for a wide lead cell.
    #[allow(dead_code)]
    pub fn wide(text: &str) -> Self {
        Self {
            text: text.to_string(),
            width: 2,
        }
    }

    /// Test/helper constructor for a wide continuation placeholder.
    #[allow(dead_code)]
    pub fn continuation() -> Self {
        Self {
            text: String::new(),
            width: 0,
        }
    }

    pub fn is_continuation(&self) -> bool {
        self.width == 0
    }

    pub fn is_blank(&self) -> bool {
        self.width == 1 && self.text == " "
    }
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

/// Resolve a column to its cluster lead: a continuation maps back to the
/// wide lead so double-click / copy never splits a wide char.
fn resolve_lead(cells: &[SelCell], x: usize) -> usize {
    if cells.is_empty() {
        return 0;
    }
    let mut x = x.min(cells.len().saturating_sub(1));
    if cells[x].is_continuation() && x > 0 {
        x -= 1;
    }
    x
}

/// Expand a cell to its word on a single visual row (match-words regex).
/// The pattern (`config.word_regex`, default `"[A-Za-z0-9_]+"`) is run with
/// `find_iter`; the match containing `x` wins. A cell in no match (space,
/// delimiter) selects just itself. Wide / emoji clusters select as a single
/// unit (lead + continuation) without consulting the regex.
pub fn expand_word(
    row: &[SelCell],
    x: usize,
    config: &crate::config::SelectionConfig,
) -> (usize, usize) {
    if row.is_empty() {
        return (0, 0);
    }
    let x = resolve_lead(row, x);
    // Wide clusters (CJK / emoji / ZWJ) select the whole cell pair.
    if row[x].width == 2 {
        let end = (x + 1).min(row.len().saturating_sub(1));
        return (x, end);
    }
    // Linearized row text plus cluster-boundary table: `starts[i]` is the
    // byte offset of lead column `lead_cols[i]` in `text`.
    let mut text = String::new();
    let mut lead_cols: Vec<usize> = Vec::with_capacity(row.len());
    let mut starts: Vec<usize> = Vec::with_capacity(row.len());
    for (col, cell) in row.iter().enumerate() {
        if cell.is_continuation() {
            continue;
        }
        starts.push(text.len());
        lead_cols.push(col);
        text.push_str(&cell.text);
    }
    if lead_cols.is_empty() {
        return (0, 0);
    }
    let pos = lead_cols.iter().position(|&c| c == x).unwrap_or(0);
    let x_byte = starts[pos];
    let re = config.word_regex_compiled();
    for mat in re.find_iter(&text) {
        let (s, e) = (mat.start(), mat.end());
        if e <= s || x_byte < s || x_byte >= e {
            continue;
        }
        // Boundaries must align to clusters so a match can never split a
        // multi-byte grapheme (e.g. `e` + combining mark).
        if !starts.contains(&s) || (e != text.len() && !starts.contains(&e)) {
            continue;
        }
        let si = starts.iter().position(|&b| b == s).unwrap_or(0);
        let ei = if e == text.len() {
            lead_cols.len().saturating_sub(1)
        } else {
            starts
                .iter()
                .position(|&b| b == e)
                .map(|i| i.saturating_sub(1))
                .unwrap_or(0)
        };
        let mut start_col = lead_cols[si.min(lead_cols.len() - 1)];
        let mut end_col = lead_cols[ei.min(lead_cols.len() - 1)];
        if start_col > end_col {
            std::mem::swap(&mut start_col, &mut end_col);
        }
        // A trailing wide lead inside the match takes its continuation.
        if row[end_col].width == 2 {
            end_col = (end_col + 1).min(row.len().saturating_sub(1));
        }
        return (start_col, end_col);
    }
    (x, x)
}

/// Expand a visual row to its full line for triple-click: column `0`
/// through the last non-blank cell (trailing spaces excluded so copy
/// doesn't drag whitespace). A trailing wide cluster keeps its
/// continuation. All-blank rows collapse to `(0, 0)`.
pub fn expand_line(row: &[SelCell]) -> (usize, usize) {
    if row.is_empty() {
        return (0, 0);
    }
    let mut end = row.len();
    while end > 0 {
        let c = &row[end - 1];
        if c.is_continuation() {
            break;
        }
        if c.is_blank() {
            end -= 1;
        } else {
            break;
        }
    }
    if end == 0 {
        return (0, 0);
    }
    (0, end.saturating_sub(1))
}

/// Backwards-compatible word expansion over plain chars (tests / callers
/// without width info). Wide handling lives in the `SelCell` overload.
#[allow(dead_code)]
pub fn expand_word_chars(row_text: &[char], x: usize) -> (usize, usize) {
    let cells: Vec<SelCell> = row_text
        .iter()
        .map(|c| SelCell::narrow(&c.to_string()))
        .collect();
    expand_word(&cells, x, &crate::config::SelectionConfig::default())
}

/// Extract selected text from lines resolved by `line_at`.
///
/// `line_at(global)` returns the row's per-column cells (lead text +
/// continuation markers); missing lines are blank. Wide leads emit their
/// cluster once, continuations emit nothing (but a selection starting on a
/// continuation still includes its lead). Each line is trimmed of trailing
/// spaces and lines are joined with `\n`.
pub fn extract_text(
    sel: &Selection,
    line_at: impl Fn(usize) -> Option<Vec<SelCell>>,
) -> Option<String> {
    if sel.is_empty() {
        return None;
    }
    let (s, e) = sel.normalized();
    let mut out = String::new();
    for y in s.y..=e.y {
        let line = line_at(y).unwrap_or_default();
        // Trim trailing blanks (spaces + missing tails), but keep a trailing
        // wide cluster (lead + continuation).
        let mut end = line.len();
        while end > 0 {
            let c = &line[end - 1];
            if c.is_continuation() {
                break;
            }
            if c.is_blank() {
                end -= 1;
            } else {
                break;
            }
        }
        let mut from = if y == s.y { s.x } else { 0 };
        let mut to = if y == e.y { e.x.saturating_add(1) } else { end };
        from = from.min(end);
        to = to.min(end);
        // Selection starting on a continuation includes its lead.
        if from < end && line[from].is_continuation() && from > 0 {
            from -= 1;
        }
        // Selection ending on a wide lead includes its continuation.
        if to > 0 && to < line.len() && line[to - 1].width == 2 && line[to].is_continuation() {
            to += 1;
        }
        // Selection ending just before a continuation whose lead is
        // included: also include the continuation (covered above). When `to`
        // points at a lone continuation (started inside it), the `from`
        // back-step already handled it.
        if y > s.y {
            out.push('\n');
        }
        for c in line.iter().take(to).skip(from) {
            if c.is_continuation() {
                continue;
            }
            // Skip trailing filler already trimmed; inner blanks kept.
            out.push_str(&c.text);
        }
        // Trim trailing spaces that came from inner blanks at line end
        // (already handled by `end`, but a partial-line copy can end on a
        // blank: strip it to match the `Vec<char>` legacy behavior).
        let trimmed_len = out.trim_end_matches(' ').len();
        // Only trim the current line's tail, not earlier newlines.
        if let Some(last_nl) = out.rfind('\n') {
            let tail = &out[last_nl + 1..];
            let tail_trimmed = tail.trim_end_matches(' ');
            out.truncate(last_nl + 1 + tail_trimmed.len());
        } else {
            out.truncate(trimmed_len);
        }
    }
    Some(out)
}

/// Legacy `Vec<char>` extraction (kept for tests): treats each char as a
/// narrow cell.
#[allow(dead_code)]
pub fn extract_text_chars(
    sel: &Selection,
    line_at: impl Fn(usize) -> Option<Vec<char>>,
) -> Option<String> {
    extract_text(sel, |y| {
        line_at(y).map(|v| {
            v.into_iter()
                .map(|c| SelCell::narrow(&c.to_string()))
                .collect()
        })
    })
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

    fn narrow_row(s: &str) -> Vec<SelCell> {
        s.chars().map(|c| SelCell::narrow(&c.to_string())).collect()
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
        let lines: Vec<Vec<SelCell>> = vec![narrow_row("hello   "), narrow_row("  hi    ")];
        let s = sel(1, 0, 3, 1);
        let text = extract_text(&s, |y| lines.get(y).cloned()).unwrap();
        assert_eq!(text, "ello\n  hi");
    }

    #[test]
    fn expand_word_stops_at_delimiters() {
        use crate::config::SelectionConfig;
        let cfg = SelectionConfig::default();
        let row = narrow_row("foo bar_baz-qux");
        assert_eq!(expand_word(&row, 1, &cfg), (0, 2));
        assert_eq!(expand_word(&row, 5, &cfg), (4, 10));
        // On a delimiter selects just that cell.
        assert_eq!(expand_word(&row, 3, &cfg), (3, 3));
        assert_eq!(expand_word(&row, 11, &cfg), (11, 11));
    }

    #[test]
    fn expand_word_selects_wide_as_unit() {
        use crate::config::SelectionConfig;
        let cfg = SelectionConfig::default();
        let row = vec![
            SelCell::wide("中"),
            SelCell::continuation(),
            SelCell::narrow("a"),
        ];
        assert_eq!(expand_word(&row, 0, &cfg), (0, 1));
        assert_eq!(expand_word(&row, 1, &cfg), (0, 1));
        assert_eq!(expand_word(&row, 2, &cfg), (2, 2));
    }

    #[test]
    fn expand_word_follows_custom_regex() {
        use crate::config::SelectionConfig;
        let cfg = SelectionConfig {
            word_regex: "[A-Za-z0-9_\\-./:]+".to_string(),
            ..SelectionConfig::default()
        };
        // Path-ish runs stay whole with a permissive pattern.
        let row = narrow_row("/tmp/foo-bar.txt baz");
        assert_eq!(expand_word(&row, 6, &cfg), (0, 15));
        // Default pattern would split the same row at `-` and `.`.
        let def = SelectionConfig::default();
        assert_eq!(expand_word(&row, 6, &def), (5, 7));
    }

    #[test]
    fn expand_word_falls_back_on_invalid_regex() {
        use crate::config::SelectionConfig;
        let cfg = SelectionConfig {
            word_regex: "([a-z".to_string(),
            ..SelectionConfig::default()
        };
        let row = narrow_row("foo bar");
        assert_eq!(expand_word(&row, 1, &cfg), (0, 2));
        assert_eq!(expand_word(&row, 3, &cfg), (3, 3));
    }

    #[test]
    fn expand_line_trims_trailing_blanks() {
        assert_eq!(expand_line(&narrow_row("hello   ")), (0, 4));
        assert_eq!(expand_line(&narrow_row("  hi  ")), (0, 3));
        assert_eq!(expand_line(&narrow_row("      ")), (0, 0));
        assert_eq!(expand_line(&[]), (0, 0));
        let row = vec![
            SelCell::wide("中"),
            SelCell::continuation(),
            SelCell::narrow(" "),
            SelCell::narrow(" "),
        ];
        assert_eq!(expand_line(&row), (0, 1));
    }

    #[test]
    fn extract_skips_continuations() {
        let row = vec![
            SelCell::wide("中"),
            SelCell::continuation(),
            SelCell::narrow("a"),
            SelCell::narrow(" "),
        ];
        let s = sel(0, 0, 2, 0);
        let text = extract_text(&s, |_| Some(row.clone())).unwrap();
        assert_eq!(text, "中a");
        // Starting on the continuation still copies the cluster.
        let s2 = sel(1, 0, 2, 0);
        let text2 = extract_text(&s2, |_| Some(row.clone())).unwrap();
        assert_eq!(text2, "中a");
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
