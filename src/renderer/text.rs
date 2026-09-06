//! Text buffer shaping (cosmic-text lines from grid cells).

use cosmic_text::{
    Attrs, AttrsList, BufferLine, Family, LineEnding, Metrics, Shaping, Weight, Wrap,
};

use crate::grid::Cell;

use super::Renderer;

pub(crate) fn build_buffer_lines(
    rows: &[Vec<Cell>],
    theme: &crate::theme::Theme,
) -> Vec<BufferLine> {
    let mut lines = Vec::with_capacity(rows.len());
    for row_cells in rows {
        let text: String = row_cells.iter().map(|c| c.ch).collect();
        let trimmed = text.trim_end();
        let line_text = if trimmed.is_empty() {
            " ".to_string()
        } else {
            trimmed.to_string()
        };
        let mut line = BufferLine::new(
            line_text.clone(),
            LineEnding::None,
            AttrsList::new(&Attrs::new().family(Family::Monospace)),
            Shaping::Advanced,
        );
        let mut attrs_list = AttrsList::new(
            &Attrs::new()
                .family(Family::Monospace)
                .color(theme.foreground.as_glyphon_color()),
        );
        let visible_len = line_text.chars().count();
        let mut byte_idx = 0;
        let chars: Vec<char> = line_text.chars().collect();
        let mut i = 0;
        while i < visible_len {
            let cell = &row_cells[i];
            let color = cell.fg.as_glyphon_color();
            let weight = if cell.bold {
                Weight::BOLD
            } else {
                Weight::NORMAL
            };
            let attrs = Attrs::new()
                .family(Family::Monospace)
                .color(color)
                .weight(weight);
            let start_byte = byte_idx;
            let mut j = i;
            while j < visible_len {
                let c2 = &row_cells[j];
                let same = c2.fg == cell.fg && c2.bold == cell.bold;
                if !same {
                    break;
                }
                byte_idx += chars[j].len_utf8();
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
        for line in build_buffer_lines(rows, &self.theme) {
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
