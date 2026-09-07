//! Text buffer shaping (cosmic-text lines from grid cells).

use cosmic_text::{
    Attrs, AttrsList, BufferLine, Family, LineEnding, Metrics, Shaping, Weight, Wrap,
};

use crate::grid::Cell;

use super::Renderer;

pub(crate) fn font_family(name: &str) -> Family<'static> {
    match name.trim().to_ascii_lowercase().as_str() {
        "sans" | "sans-serif" => Family::SansSerif,
        "serif" => Family::Serif,
        "cursive" => Family::Cursive,
        "fantasy" => Family::Fantasy,
        _ => Family::Monospace,
    }
}

pub(crate) fn build_buffer_lines(
    rows: &[Vec<Cell>],
    theme: &crate::theme::Theme,
    font: &crate::config::FontConfig,
) -> Vec<BufferLine> {
    let mut lines = Vec::with_capacity(rows.len());
    for row_cells in rows {
        // Build the shaped string from cluster leads only; wide
        // continuations occupy a grid column but no glyphs.
        let mut clusters: Vec<(usize, String)> = Vec::new();
        for (col, cell) in row_cells.iter().enumerate() {
            if cell.width == 0 {
                continue;
            }
            clusters.push((col, cell.cluster()));
        }
        // Trim trailing blank cells (single-space narrow cells only; wide
        // clusters and ZWJ sequences are never whitespace-trimmed).
        while clusters.last().is_some_and(|(_, s)| s == " ") {
            clusters.pop();
        }
        let line_string: String = clusters.iter().map(|(_, s)| s.as_str()).collect();
        let (line_text, visible_clusters) = if line_string.trim().is_empty() {
            (" ".to_string(), Vec::new())
        } else {
            (line_string, clusters)
        };
        let family = font_family(&font.family);
        let mut line = BufferLine::new(
            line_text.clone(),
            LineEnding::None,
            AttrsList::new(&Attrs::new().family(family)),
            Shaping::Advanced,
        );
        let mut attrs_list = AttrsList::new(
            &Attrs::new()
                .family(family)
                .color(theme.foreground.as_glyphon_color()),
        );
        // Attr spans keyed by grid column so bold/color follow cells, not
        // characters: a ZWJ cluster is many chars but one cell.
        let mut byte_idx = 0usize;
        let mut i = 0usize;
        while i < visible_clusters.len() {
            let (col, _) = visible_clusters[i];
            let cell = &row_cells[col];
            let color = cell.fg.as_glyphon_color();
            let weight = if cell.bold {
                Weight::BOLD
            } else {
                Weight::NORMAL
            };
            let attrs = Attrs::new().family(family).color(color).weight(weight);
            let start_byte = byte_idx;
            let mut j = i;
            while j < visible_clusters.len() {
                let (col2, s2) = &visible_clusters[j];
                let c2 = &row_cells[*col2];
                let same = c2.fg == cell.fg && c2.bold == cell.bold;
                if !same {
                    break;
                }
                byte_idx += s2.len();
                j += 1;
            }
            if j > i {
                attrs_list.add_span(start_byte..byte_idx, &attrs);
            }
            i = j;
        }
        line.set_attrs_list(attrs_list);
        lines.push(line);
    }
    lines
}

impl Renderer {
    pub(crate) fn rebuild_buffer(&mut self, rows: &[Vec<Cell>]) {
        let metrics = Metrics::new(self.font_size, self.line_height);
        self.buffer.set_metrics(metrics);
        self.buffer.lines.clear();
        for line in build_buffer_lines(rows, &self.theme, &self.user_config.font) {
            self.buffer.lines.push(line);
        }
        self.buffer
            .set_size(Some(self.width as f32), Some(self.height as f32));
        // `lines` was mutated directly, bypassing `Buffer`'s dirty flags, so
        // `shape_until_scroll` alone would early-return via `resolve_dirty`.
        // Lay out each line explicitly so `TextRenderer::prepare` finds glyphs.
        let n = self.buffer.lines.len();
        for i in 0..n {
            self.buffer.line_layout(&mut self.font_system, i);
        }
        // Keep wrap mode pinned (Buffer::new defaults could change).
        self.buffer.set_wrap(Wrap::None);
    }
}
