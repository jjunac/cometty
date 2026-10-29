//! Background quads (custom WGSL pipeline + scratch vertex reuse).

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct BgVertex {
    pos: [f32; 2],
    color: [f32; 3],
}

impl BgVertex {
    pub(crate) fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<BgVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: 8,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x3,
                },
            ],
        }
    }
}

pub(crate) const BG_SHADER: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec3<f32>,
};
@vertex
fn vs_main(@location(0) ndc: vec2<f32>, @location(1) color: vec3<f32>) -> VsOut {
    var out: VsOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.color = color;
    return out;
}
@fragment
fn fs_main(@location(0) color: vec3<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(color, 1.0);
}
"#;

/// Grow/shrink policy for the bg vertex buffer: grow on overflow, shrink
/// only when usage drops below a quarter (hysteresis avoids thrash on
/// resize flicker). Returns the new capacity when a reallocation is due.
pub(crate) fn next_bg_capacity(capacity: usize, vert_count: usize) -> Option<usize> {
    if vert_count > capacity {
        return Some(vert_count.next_power_of_two().max(1024));
    }
    if capacity > 1024 && vert_count * 4 < capacity {
        return Some(vert_count.next_power_of_two().max(1024));
    }
    None
}

/// Thickness of the text-underline strip, derived from the (already
/// DPI-scaled) line height so it stays ~1px at 1x and scales on HiDPI.
pub(crate) fn underline_thickness(line_height: f32, font: &crate::config::FontConfig) -> f32 {
    (line_height * font.underline_factor).max(1.0)
}

/// Thicker strip used for the underline cursor shape (DECSCUSR 3/4).
pub(crate) fn cursor_underline_thickness(
    line_height: f32,
    cursor: &crate::config::CursorConfig,
) -> f32 {
    (line_height * cursor.underline_factor).max(1.0)
}

/// Width of the bar cursor shape (DECSCUSR 5/6).
pub(crate) fn bar_cursor_width(cell_width: f32, cursor: &crate::config::CursorConfig) -> f32 {
    (cell_width * cursor.bar_width_factor).max(1.0)
}

/// Edges of a hollow rectangle (the unfocused block cursor), each `thickness`
/// device pixels wide and inside the rect. Top and bottom span the width and
/// the sides fill between them, so no pixel is painted twice. `None` when the
/// rect is too small to show a hole: the caller fills it instead.
pub(crate) fn outline_rects(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    thickness: f32,
) -> Option<[(f32, f32, f32, f32); 4]> {
    let t = thickness.max(1.0);
    if w <= 2.0 * t || h <= 2.0 * t {
        return None;
    }
    Some([
        (x, y, w, t),
        (x, y + h - t, w, t),
        (x, y + t, t, h - 2.0 * t),
        (x + w - t, y + t, t, h - 2.0 * t),
    ])
}

/// Snap a column span to device pixels: both edges are rounded from the
/// grid origin so adjacent cells share the exact same edge value.
/// Returns `(x0, x1)` with `x1 > x0` (at least 1px).
pub(crate) fn snap_col_edges(col: usize, span: usize, cell_width: f32) -> (f32, f32) {
    let cw = if cell_width.is_finite() && cell_width > 0.0 {
        cell_width
    } else {
        1.0
    };
    let x0 = (col as f32 * cw).round();
    let mut x1 = ((col + span.max(1)) as f32 * cw).round();
    if x1 <= x0 {
        x1 = x0 + 1.0;
    }
    (x0, x1)
}

/// Snap a row to device pixels, including the (possibly fractional)
/// tab-bar offset. Adjacent rows share the exact same edge value.
/// Returns `(y0, y1)` with `y1 > y0` (at least 1px).
pub(crate) fn snap_row_edges(row: usize, y_off: f32, line_height: f32) -> (f32, f32) {
    let lh = if line_height.is_finite() && line_height > 0.0 {
        line_height
    } else {
        1.0
    };
    let off = if y_off.is_finite() { y_off } else { 0.0 };
    let y0 = (off + row as f32 * lh).round();
    let mut y1 = (off + (row + 1) as f32 * lh).round();
    if y1 <= y0 {
        y1 = y0 + 1.0;
    }
    (y0, y1)
}

/// Hit-test a normalized view-space selection `((x0, y0), (x1, y1))`.
pub(crate) fn in_view_selection(
    sel: Option<((usize, usize), (usize, usize))>,
    x: usize,
    y: usize,
) -> bool {
    let Some(((ax, ay), (bx, by))) = sel else {
        return false;
    };
    let (s, e) = if (by, bx) < (ay, ax) {
        ((bx, by), (ax, ay))
    } else {
        ((ax, ay), (bx, by))
    };
    if y < s.1 || y > e.1 {
        return false;
    }
    if s.1 == e.1 {
        return x >= s.0 && x <= e.0;
    }
    if y == s.1 {
        return x >= s.0;
    }
    if y == e.1 {
        return x <= e.0;
    }
    true
}

/// Everything cell geometry depends on besides the cells and highlights. Part
/// of the retained geometry's key: a size, tab-bar, metric, scale, or theme
/// change rebuilds the cells.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BgLayout {
    /// Surface size in device pixels, for the NDC conversion.
    pub(crate) width: f32,
    pub(crate) height: f32,
    /// Tab-bar height in device pixels; it may be fractional.
    pub(crate) y_off: f32,
    pub(crate) cell_width: f32,
    pub(crate) line_height: f32,
    pub(crate) underline_h: f32,
    /// One logical point in device pixels: decoration and outline insets.
    pub(crate) pad: f32,
    pub(crate) theme: crate::theme::Theme,
}

/// Cursor-shape metrics, read every frame: the cursor is never retained.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CursorMetrics {
    pub(crate) underline_h: f32,
    pub(crate) bar_w: f32,
}

struct Quads<'a> {
    layout: &'a BgLayout,
    verts: &'a mut Vec<BgVertex>,
}

impl Quads<'_> {
    fn push(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 3]) {
        let (sw, sh) = (self.layout.width, self.layout.height);
        let x0 = (x / sw) * 2.0 - 1.0;
        let y0 = 1.0 - (y / sh) * 2.0;
        let x1 = ((x + w) / sw) * 2.0 - 1.0;
        let y1 = 1.0 - ((y + h) / sh) * 2.0;
        for pos in [[x0, y0], [x1, y0], [x0, y1], [x1, y0], [x1, y1], [x0, y1]] {
            self.verts.push(BgVertex { pos, color });
        }
    }
}

/// Quads for every cell that differs from the theme background, then its
/// block elements, decorations, and link hints. Background priority is
/// active search > selection > other search > cell background. Every rect
/// snaps to whole device pixels with shared edges, so neighbouring cells tile
/// without hairline seams, and no quad leaves its own cell.
pub(crate) fn cell_quads(
    layout: &BgLayout,
    rows: &[Vec<crate::grid::Cell>],
    highlights: &super::HighlightCtx<'_>,
    verts: &mut Vec<BgVertex>,
) {
    let mut quads = Quads { layout, verts };
    let theme = &layout.theme;
    let (underline_h, pad) = (layout.underline_h, layout.pad);
    let search_match_col = theme.search_match.as_linear_f32_array();
    let search_active_col = theme.search_active.as_linear_f32_array();
    let selection_col = theme.selection.as_linear_f32_array();
    let hint_col = theme.link.as_linear_f32_array();
    for (y, row) in rows.iter().enumerate() {
        let (py, row_bottom) = snap_row_edges(y, layout.y_off, layout.line_height);
        let row_h = row_bottom - py;
        let mut x = 0usize;
        while x < row.len() {
            let cell = &row[x];
            // Wide continuations are painted as part of their lead.
            if cell.width == 0 {
                x += 1;
                continue;
            }
            let span = if cell.width == 2 { 2 } else { 1 };
            let (px, cell_right) = snap_col_edges(x, span, layout.cell_width);
            let w_px = cell_right - px;
            // A wide cluster is selected when either half is selected.
            let is_selected = (0..span).any(|d| in_view_selection(highlights.selection, x + d, y));
            let search_hit = highlights
                .search
                .map_or(crate::search::Hit::None, |view| view.hit(y, x, span));
            // Base background. The current search match wins over the
            // selection so `Enter` navigation always shows where it landed;
            // other matches sit underneath a selection.
            let eff_bg = cell.effective_bg();
            if matches!(search_hit, crate::search::Hit::Active) {
                quads.push(px, py, w_px, row_h, search_active_col);
            } else if is_selected {
                quads.push(px, py, w_px, row_h, selection_col);
            } else if matches!(search_hit, crate::search::Hit::Match) {
                quads.push(px, py, w_px, row_h, search_match_col);
            } else if eff_bg != theme.background {
                quads.push(px, py, w_px, row_h, eff_bg.as_linear_f32_array());
            }
            // Foreground-colored geometry is rare: convert the effective
            // foreground only for a cell that draws some.
            let fg_col = || cell.effective_fg().as_linear_f32_array();
            // Block elements are rects, not font glyphs: fonts fill only
            // their line box, which is shorter than the cell, so stacked
            // blocks (`btop`, `chafa`, …) would leave gaps between rows.
            // The matching glyph is skipped in `text` (shaped as a space).
            if cell.width == 1
                && cell.extra.is_none()
                && let Some(rects) = super::blocks::block_rects(cell.ch)
            {
                let cell_rect = (px, py, cell_right, row_bottom);
                let col = fg_col();
                for rect in rects {
                    let (rx0, ry0, rx1, ry1) = super::blocks::snap_rect(cell_rect, *rect);
                    quads.push(rx0, ry0, rx1 - rx0, ry1 - ry0, col);
                }
            }
            // Text decorations: strips in effective fg unless an explicit
            // underline color (`SGR 58`) overrides. Curly / dotted / dashed
            // render as single for now (stored distinctly in the cell for
            // future shaping).
            let deco_col = || match cell.underline_color {
                Some(color) => color.as_linear_f32_array(),
                None => fg_col(),
            };
            match cell.underline {
                crate::grid::UnderlineStyle::None => {}
                crate::grid::UnderlineStyle::Double => {
                    let lower = row_bottom - underline_h - pad;
                    let upper = lower - underline_h - pad;
                    let upper = upper.max(py);
                    let col = deco_col();
                    quads.push(px, lower, w_px, underline_h, col);
                    quads.push(px, upper, w_px, underline_h, col);
                }
                _ if cell.underline.is_active() => {
                    let uy = row_bottom - underline_h - pad;
                    quads.push(px, uy, w_px, underline_h, deco_col());
                }
                _ => {}
            }
            if cell.strikethrough {
                let sy = py + row_h * 0.5 - underline_h * 0.5;
                quads.push(px, sy, w_px, underline_h, fg_col());
            }
            if cell.overline {
                let oy = py + pad;
                quads.push(px, oy, w_px, underline_h, fg_col());
            }
            // Link hints (OSC 8 / detected URLs): same strip as a text
            // underline, tinted with the theme's link color. Cells that
            // already underline keep the app's own styling.
            if cell.underline == crate::grid::UnderlineStyle::None
                && highlights.links.is_some_and(|view| view.hit(y, x, span))
            {
                let uy = row_bottom - underline_h - pad;
                quads.push(px, uy, w_px, underline_h, hint_col);
            }
            x += span;
        }
    }
}

/// The lead column and span a cursor at `col` covers, visiting cells like
/// [`cell_quads`]: a cursor on either half of a wide cluster covers it whole.
pub(crate) fn cursor_cell(row: &[crate::grid::Cell], col: usize) -> Option<(usize, usize)> {
    let mut x = 0;
    while x < row.len() && x <= col {
        if row[x].width == 0 {
            x += 1;
            continue;
        }
        let span = if row[x].width == 2 { 2 } else { 1 };
        if col == x || (span == 2 && col == x + 1) {
            return Some((x, span));
        }
        x += span;
    }
    None
}

/// The cursor's quads, drawn after all cell geometry so it covers the cell
/// background and decorations. They stay inside the cursor's own cell, which
/// no other cell's quads enter, so drawing them last paints the same pixels as
/// drawing them right after that cell.
pub(crate) fn cursor_quads(
    layout: &BgLayout,
    rows: &[Vec<crate::grid::Cell>],
    cursor: &super::CursorCtx,
    metrics: &CursorMetrics,
    verts: &mut Vec<BgVertex>,
) {
    if !cursor.visible {
        return;
    }
    let (col, y) = cursor.pos;
    let Some((lead, span)) = rows.get(y).and_then(|row| cursor_cell(row, col)) else {
        return;
    };
    let (px, cell_right) = snap_col_edges(lead, span, layout.cell_width);
    let (py, row_bottom) = snap_row_edges(y, layout.y_off, layout.line_height);
    let (w_px, row_h) = (cell_right - px, row_bottom - py);
    let color = layout.theme.foreground.as_linear_f32_array();
    let mut quads = Quads { layout, verts };
    match cursor.shape {
        crate::grid::CursorShape::Block => {
            // Unfocused: an outline, the usual cue that keys go elsewhere;
            // it also leaves the glyph readable.
            let outline = (!cursor.focused)
                .then(|| outline_rects(px, py, w_px, row_h, layout.pad.round()))
                .flatten();
            match outline {
                Some(edges) => {
                    for (ex, ey, ew, eh) in edges {
                        quads.push(ex, ey, ew, eh, color);
                    }
                }
                None => quads.push(px, py, w_px, row_h, color),
            }
        }
        crate::grid::CursorShape::Underline => {
            let uy = row_bottom - metrics.underline_h - layout.pad;
            quads.push(px, uy, w_px, metrics.underline_h, color);
        }
        crate::grid::CursorShape::Bar => {
            quads.push(px, py, metrics.bar_w, row_h, color);
        }
    }
}

/// Inputs of the retained cell quads.
#[derive(Clone, Debug, PartialEq)]
struct CellsKey {
    /// Snapshot content; a tab switch (equal versions, different rows)
    /// invalidates explicitly.
    version: u64,
    layout: BgLayout,
    selection: Option<((usize, usize), (usize, usize))>,
    search: Option<crate::search::SearchView>,
    links: Option<crate::links::HintView>,
}

impl CellsKey {
    fn matches(
        &self,
        version: u64,
        layout: &BgLayout,
        highlights: &super::HighlightCtx<'_>,
    ) -> bool {
        self.version == version
            && self.layout == *layout
            && self.selection == highlights.selection
            && self.search.as_ref() == highlights.search
            && self.links.as_ref() == highlights.links
    }
}

/// Retained background geometry. The cell quads are rebuilt only when their
/// [`CellsKey`] changes. Cursor-only frames, such as blink frames, rebuild and
/// upload just the cursor's few vertices, which follow the cells in the same
/// vertex buffer.
#[derive(Default)]
pub(crate) struct BgGeometry {
    key: Option<CellsKey>,
    /// `verts[..cells]` are cell quads; the rest are the cursor's.
    cells: usize,
    verts: Vec<BgVertex>,
    /// The GPU buffer holds `verts[..cells]` from the last build.
    uploaded: bool,
    #[cfg(test)]
    rebuilds: usize,
}

impl BgGeometry {
    /// The rows may differ under the same grid version (a tab switch).
    pub(crate) fn invalidate(&mut self) {
        self.key = None;
    }

    /// The GPU buffer was replaced, so it holds none of the cells.
    pub(crate) fn buffer_replaced(&mut self) {
        self.uploaded = false;
    }

    /// Build this frame's vertices. Returns the range of
    /// [`Self::vertices`] the GPU buffer lacks, to be written at the same
    /// offset; call [`Self::mark_uploaded`] once it is written.
    pub(crate) fn prepare(
        &mut self,
        layout: &BgLayout,
        version: u64,
        rows: &[Vec<crate::grid::Cell>],
        highlights: &super::HighlightCtx<'_>,
        cursor: &super::CursorCtx,
        metrics: &CursorMetrics,
    ) -> std::ops::Range<usize> {
        if !self
            .key
            .as_ref()
            .is_some_and(|key| key.matches(version, layout, highlights))
        {
            crate::perf::add(crate::perf::Metric::BgRebuild, 1);
            #[cfg(test)]
            {
                self.rebuilds += 1;
            }
            self.verts.clear();
            cell_quads(layout, rows, highlights, &mut self.verts);
            self.cells = self.verts.len();
            self.uploaded = false;
            self.key = Some(CellsKey {
                version,
                layout: *layout,
                selection: highlights.selection,
                search: highlights.search.cloned(),
                links: highlights.links.cloned(),
            });
        }
        self.verts.truncate(self.cells);
        cursor_quads(layout, rows, cursor, metrics, &mut self.verts);
        let start = if self.uploaded { self.cells } else { 0 };
        start..self.verts.len()
    }

    pub(crate) fn mark_uploaded(&mut self) {
        self.uploaded = true;
    }

    pub(crate) fn vertices(&self) -> &[BgVertex] {
        &self.verts
    }
}

impl super::Renderer {
    /// Prepare this frame's background geometry (see [`BgGeometry`]), upload
    /// what the GPU buffer lacks, and return the vertex count to draw.
    pub(crate) fn paint_bg(
        &mut self,
        grid_rows: &[Vec<crate::grid::Cell>],
        grid_version: u64,
        cursor: &super::CursorCtx,
        highlights: &super::HighlightCtx<'_>,
        tab_count: usize,
    ) -> usize {
        let layout = BgLayout {
            width: self.width as f32,
            height: self.height as f32,
            y_off: self.tab_bar_px(tab_count),
            cell_width: self.cell_width,
            line_height: self.line_height,
            underline_h: underline_thickness(self.line_height, &self.user_config.font),
            pad: self.scale_factor.max(1.0),
            theme: self.theme,
        };
        let metrics = CursorMetrics {
            underline_h: cursor_underline_thickness(self.line_height, &self.user_config.cursor),
            bar_w: bar_cursor_width(self.cell_width, &self.user_config.cursor),
        };
        let mut upload = self.bg.prepare(
            &layout,
            grid_version,
            grid_rows,
            highlights,
            cursor,
            &metrics,
        );
        let vert_count = self.bg.vertices().len();
        crate::perf::add(crate::perf::Metric::BgVertices, vert_count);
        crate::perf::peak(crate::perf::Metric::BgVertices, vert_count);

        if let Some(cap) = next_bg_capacity(self.bg_vertex_capacity, vert_count) {
            self.bg_vertex_capacity = cap;
            self.bg_vertex_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bg verts"),
                size: (self.bg_vertex_capacity * std::mem::size_of::<BgVertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.bg.buffer_replaced();
            upload = 0..vert_count;
        }
        if !upload.is_empty() {
            let offset = (upload.start * std::mem::size_of::<BgVertex>()) as u64;
            self.queue.write_buffer(
                &self.bg_vertex_buf,
                offset,
                bytemuck::cast_slice(&self.bg.vertices()[upload]),
            );
        }
        self.bg.mark_uploaded();
        vert_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CursorConfig, FontConfig};
    use crate::grid::{Cell, CursorShape, UnderlineStyle};
    use crate::renderer::{CursorCtx, HighlightCtx};
    use crate::theme::{Rgb, Theme};
    use bytemuck::Zeroable;

    fn layout() -> BgLayout {
        BgLayout {
            width: 1000.0,
            height: 700.0,
            y_off: 76.0,
            cell_width: 16.856,
            line_height: 35.0,
            underline_h: 2.45,
            pad: 2.0,
            theme: Theme::default(),
        }
    }

    const METRICS: CursorMetrics = CursorMetrics {
        underline_h: 4.9,
        bar_w: 5.06,
    };

    fn cell(ch: char, width: u8) -> Cell {
        Cell {
            ch,
            width,
            ..Default::default()
        }
    }

    /// Narrow and wide text, a colored background, a block element and
    /// every decoration kind.
    fn rows() -> Vec<Vec<Cell>> {
        let mut styled = cell('s', 1);
        styled.bg = Rgb::new(10, 20, 30);
        styled.underline = UnderlineStyle::Double;
        let mut struck = cell('t', 1);
        struck.strikethrough = true;
        struck.overline = true;
        struck.underline = UnderlineStyle::Single;
        struck.underline_color = Some(Rgb::new(200, 0, 0));
        vec![
            vec![cell('a', 1), cell('中', 2), cell(' ', 0), cell('b', 1)],
            vec![styled, struck, cell('█', 1), cell('c', 1)],
            vec![cell('d', 1), cell('e', 1), cell('😀', 2), cell(' ', 0)],
        ]
    }

    fn cursor(pos: (usize, usize), shape: CursorShape, focused: bool) -> CursorCtx {
        CursorCtx {
            pos,
            visible: true,
            shape,
            focused,
        }
    }

    fn fresh(layout: &BgLayout, rows: &[Vec<Cell>], hl: &HighlightCtx, c: &CursorCtx) -> Vec<u8> {
        let mut verts = Vec::new();
        cell_quads(layout, rows, hl, &mut verts);
        cursor_quads(layout, rows, c, &METRICS, &mut verts);
        bytemuck::cast_slice(&verts).to_vec()
    }

    #[test]
    fn cursor_cell_matches_the_former_interleaved_cell_walk() {
        // The cursor used to be painted while walking leads, on the lead
        // whose span contains the cursor column.
        fn walk(row: &[Cell], col: usize) -> Option<(usize, usize)> {
            let mut x = 0;
            while x < row.len() {
                if row[x].width == 0 {
                    x += 1;
                    continue;
                }
                let span = if row[x].width == 2 { 2 } else { 1 };
                if col == x || (span == 2 && col == x + 1) {
                    return Some((x, span));
                }
                x += span;
            }
            None
        }
        let malformed = [
            // A wide lead followed by a narrow cell, a leading continuation,
            // and a wide lead in the last column.
            vec![cell('中', 2), cell('a', 1), cell('b', 1)],
            vec![cell(' ', 0), cell('a', 1)],
            vec![cell('a', 1), cell('中', 2)],
        ];
        for row in rows().iter().chain(malformed.iter()) {
            for col in 0..row.len() + 2 {
                assert_eq!(cursor_cell(row, col), walk(row, col), "{row:?} col {col}");
            }
        }
        assert_eq!(cursor_cell(&rows()[0], 2), Some((1, 2)));
    }

    #[test]
    fn cursor_quads_stay_inside_their_cell() {
        let layout = layout();
        let rows = rows();
        let to_px = |v: &BgVertex| {
            (
                (v.pos[0] + 1.0) / 2.0 * layout.width,
                (1.0 - v.pos[1]) / 2.0 * layout.height,
            )
        };
        for (y, row) in rows.iter().enumerate() {
            for x in 0..row.len() {
                let (lead, span) = cursor_cell(row, x).unwrap_or((x, 1));
                let (x0, x1) = snap_col_edges(lead, span, layout.cell_width);
                let (y0, y1) = snap_row_edges(y, layout.y_off, layout.line_height);
                for shape in [CursorShape::Block, CursorShape::Underline, CursorShape::Bar] {
                    for focused in [true, false] {
                        let mut verts = Vec::new();
                        cursor_quads(
                            &layout,
                            &rows,
                            &cursor((x, y), shape, focused),
                            &METRICS,
                            &mut verts,
                        );
                        assert_eq!(verts.is_empty(), cursor_cell(row, x).is_none());
                        for (px, py) in verts.iter().map(to_px) {
                            assert!(
                                px >= x0 - 1e-3 && px <= x1 + 1e-3,
                                "x {px} outside {x0}..{x1}"
                            );
                            assert!(
                                py >= y0 - 1e-3 && py <= y1 + 1e-3,
                                "y {py} outside {y0}..{y1}"
                            );
                        }
                    }
                }
            }
        }
        let mut hidden = Vec::new();
        let mut off = cursor((0, 0), CursorShape::Block, true);
        off.visible = false;
        cursor_quads(&layout, &rows, &off, &METRICS, &mut hidden);
        assert!(hidden.is_empty());
    }

    #[test]
    fn retained_cells_plus_cursor_upload_match_a_fresh_build_every_frame() {
        let mut layout = layout();
        let mut rows = rows();
        let search = crate::search::SearchView {
            rows: vec![vec![(0, 0)], vec![], vec![(1, 2)]],
            active: Some((2, 1, 2)),
        };
        let hints = crate::links::HintView {
            rows: vec![vec![], vec![(2, 3)], vec![]],
        };
        let mut bg = BgGeometry::default();
        let mut gpu: Vec<BgVertex> = Vec::new();
        let mut version = 1;
        /// One frame: prepare, apply the planned upload to a fake GPU
        /// buffer, and compare what would be drawn with a fresh build.
        /// Returns how many vertices were uploaded.
        fn frame(
            gpu: &mut Vec<BgVertex>,
            bg: &mut BgGeometry,
            layout: &BgLayout,
            version: u64,
            rows: &[Vec<Cell>],
            hl: &HighlightCtx,
            c: &CursorCtx,
        ) -> usize {
            let upload = bg.prepare(layout, version, rows, hl, c, &METRICS);
            let len = bg.vertices().len();
            // The real buffer only grows by reallocation, which re-uploads.
            if gpu.len() < len {
                gpu.resize(len, BgVertex::zeroed());
            }
            let uploaded = upload.len();
            gpu[upload.clone()].copy_from_slice(&bg.vertices()[upload]);
            bg.mark_uploaded();
            let drawn: &[u8] = bytemuck::cast_slice(&gpu[..len]);
            assert_eq!(drawn, fresh(layout, rows, hl, c).as_slice());
            uploaded
        }
        let none = HighlightCtx {
            selection: None,
            search: None,
            links: None,
        };
        let block = cursor((0, 0), CursorShape::Block, true);
        assert!(frame(&mut gpu, &mut bg, &layout, version, &rows, &none, &block) > 6);
        assert_eq!(bg.rebuilds, 1);
        // Blink and cursor motion, shape and focus: no cell rebuild, and
        // only the cursor's vertices are written.
        let mut hidden = block;
        hidden.visible = false;
        for c in [
            hidden,
            block,
            cursor((2, 0), CursorShape::Block, true),
            cursor((3, 2), CursorShape::Bar, true),
            cursor((1, 1), CursorShape::Underline, true),
            cursor((1, 0), CursorShape::Block, false),
        ] {
            assert!(frame(&mut gpu, &mut bg, &layout, version, &rows, &none, &c) <= 24);
        }
        assert_eq!(bg.rebuilds, 1);
        // Highlights, content, and layout each rebuild the cells once.
        let selected = HighlightCtx {
            selection: Some(((1, 0), (2, 1))),
            search: None,
            links: None,
        };
        frame(
            &mut gpu, &mut bg, &layout, version, &rows, &selected, &block,
        );
        frame(
            &mut gpu, &mut bg, &layout, version, &rows, &selected, &block,
        );
        assert_eq!(bg.rebuilds, 2);
        let searched = HighlightCtx {
            search: Some(&search),
            ..selected
        };
        frame(
            &mut gpu, &mut bg, &layout, version, &rows, &searched, &block,
        );
        // An equal projection in another allocation is not a change.
        let same = search.clone();
        let searched_again = HighlightCtx {
            search: Some(&same),
            ..selected
        };
        frame(
            &mut gpu,
            &mut bg,
            &layout,
            version,
            &rows,
            &searched_again,
            &block,
        );
        assert_eq!(bg.rebuilds, 3);
        let hinted = HighlightCtx {
            links: Some(&hints),
            ..searched
        };
        frame(&mut gpu, &mut bg, &layout, version, &rows, &hinted, &block);
        assert_eq!(bg.rebuilds, 4);
        rows[2][0].bg = Rgb::new(1, 2, 3);
        version += 1;
        frame(&mut gpu, &mut bg, &layout, version, &rows, &hinted, &block);
        assert_eq!(bg.rebuilds, 5);
        layout.y_off = 0.0;
        frame(&mut gpu, &mut bg, &layout, version, &rows, &hinted, &block);
        layout.theme = Theme::vscode();
        frame(&mut gpu, &mut bg, &layout, version, &rows, &hinted, &block);
        assert_eq!(bg.rebuilds, 7);
        // A replaced GPU buffer re-uploads everything without a rebuild.
        bg.buffer_replaced();
        gpu.iter_mut().for_each(|v| *v = BgVertex::zeroed());
        let all = frame(&mut gpu, &mut bg, &layout, version, &rows, &hinted, &block);
        assert_eq!(all, bg.vertices().len());
        assert_eq!(bg.rebuilds, 7);
        // A tab switch can show different rows under the same version.
        rows[0][0].bg = Rgb::new(9, 9, 9);
        bg.invalidate();
        frame(&mut gpu, &mut bg, &layout, version, &rows, &hinted, &block);
        assert_eq!(bg.rebuilds, 8);
    }

    #[test]
    fn underline_and_cursor_metrics_scale() {
        let font = FontConfig::default();
        let cursor = CursorConfig::default();
        // 1x metrics (14pt base): line ~17.5, cell ~8.4.
        let h1 = underline_thickness(17.5, &font);
        let ch1 = cursor_underline_thickness(17.5, &cursor);
        let b1 = bar_cursor_width(8.4, &cursor);
        assert!((1.0..3.0).contains(&h1), "h1={h1}");
        assert!(ch1 >= h1, "cursor underline thicker than text");
        assert!((1.0..8.4).contains(&b1), "b1={b1}");
        // 2x scales proportionally.
        let h2 = underline_thickness(35.0, &font);
        let b2 = bar_cursor_width(16.8, &cursor);
        assert!((h2 - h1 * 2.0).abs() < 0.01, "h2={h2}");
        assert!((b2 - b1 * 2.0).abs() < 0.01, "b2={b2}");
    }

    #[test]
    fn unfocused_block_outline_tiles_the_cell_border_once() {
        let (x, y, w, h, t) = (10.0, 20.0, 17.0, 35.0, 2.0);
        let edges = outline_rects(x, y, w, h, t).unwrap();
        // Inside the cell, never overlapping, and covering exactly the border.
        let area: f32 = edges.iter().map(|(_, _, ew, eh)| ew * eh).sum();
        assert_eq!(area, w * h - (w - 2.0 * t) * (h - 2.0 * t));
        for (i, a) in edges.iter().enumerate() {
            assert!(a.0 >= x && a.1 >= y && a.0 + a.2 <= x + w && a.1 + a.3 <= y + h);
            assert!(a.2 >= 1.0 && a.3 >= 1.0);
            for b in &edges[i + 1..] {
                let overlap_w = (a.0 + a.2).min(b.0 + b.2) - a.0.max(b.0);
                let overlap_h = (a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1);
                assert!(overlap_w <= 0.0 || overlap_h <= 0.0, "{a:?} overlaps {b:?}");
            }
        }
        // The hole is left unpainted, so the glyph under the cursor shows.
        let hole = (x + w / 2.0, y + h / 2.0);
        assert!(!edges.iter().any(|(ex, ey, ew, eh)| {
            (*ex..ex + ew).contains(&hole.0) && (*ey..ey + eh).contains(&hole.1)
        }));
        // Thickness is at least one device pixel; tiny cells stay filled.
        assert_eq!(outline_rects(0.0, 0.0, 8.0, 8.0, 0.0).unwrap()[0].3, 1.0);
        assert!(outline_rects(0.0, 0.0, 4.0, 30.0, 2.0).is_none());
        assert!(outline_rects(0.0, 0.0, 8.0, 3.0, 2.0).is_none());
    }

    #[test]
    fn bg_capacity_grows_and_shrinks_with_hysteresis() {
        // Grow on overflow.
        assert_eq!(next_bg_capacity(1024, 1025), Some(2048));
        // No churn inside the hysteresis band.
        assert_eq!(next_bg_capacity(4096, 2000), None);
        assert_eq!(next_bg_capacity(4096, 1024), None);
        // Shrink only below a quarter, floored at 1024.
        assert_eq!(next_bg_capacity(4096, 500), Some(1024));
        assert_eq!(next_bg_capacity(8192, 0), Some(1024));
        // Small buffers never shrink below the floor.
        assert_eq!(next_bg_capacity(1024, 0), None);
    }
}
