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

impl super::Renderer {
    fn push_quad(&self, verts: &mut Vec<BgVertex>, x: f32, y: f32, w: f32, h: f32, col: [f32; 3]) {
        let sw = self.width as f32;
        let sh = self.height as f32;
        let x0 = (x / sw) * 2.0 - 1.0;
        let y0 = 1.0 - (y / sh) * 2.0;
        let x1 = ((x + w) / sw) * 2.0 - 1.0;
        let y1 = 1.0 - ((y + h) / sh) * 2.0;
        verts.push(BgVertex {
            pos: [x0, y0],
            color: col,
        });
        verts.push(BgVertex {
            pos: [x1, y0],
            color: col,
        });
        verts.push(BgVertex {
            pos: [x0, y1],
            color: col,
        });
        verts.push(BgVertex {
            pos: [x1, y0],
            color: col,
        });
        verts.push(BgVertex {
            pos: [x1, y1],
            color: col,
        });
        verts.push(BgVertex {
            pos: [x0, y1],
            color: col,
        });
    }

    /// Fill the scratch vertex buffer with background quads for cells that
    /// differ from the theme background (plus cursor + selection + underline/
    /// strike/overline), ensure GPU capacity, and upload. Returns the vertex
    /// count to draw.
    pub(crate) fn paint_bg(
        &mut self,
        grid_rows: &[Vec<crate::grid::Cell>],
        cursor: (usize, usize),
        cursor_visible: bool,
        cursor_shape: crate::grid::CursorShape,
        selection: Option<((usize, usize), (usize, usize))>,
        tab_count: usize,
    ) -> usize {
        // Take the scratch buffer so `push_quad(&self, …)` and the vertex
        // mutation don't alias; capacity is preserved across frames.
        let mut verts = std::mem::take(&mut self.bg_scratch);
        verts.clear();
        let y_off = self.tab_bar_px(tab_count);
        let underline_h = underline_thickness(self.line_height, &self.user_config.font);
        let cursor_underline_h =
            cursor_underline_thickness(self.line_height, &self.user_config.cursor);
        let bar_w = bar_cursor_width(self.cell_width, &self.user_config.cursor);
        let pad = self.scale_factor.max(1.0);
        for (y, row) in grid_rows.iter().enumerate() {
            let py = y_off + y as f32 * self.line_height;
            let mut x = 0usize;
            while x < row.len() {
                let cell = &row[x];
                // Wide continuations are painted as part of their lead.
                if cell.width == 0 {
                    x += 1;
                    continue;
                }
                let span = if cell.width == 2 { 2 } else { 1 };
                let w_px = self.cell_width * span as f32;
                let px = x as f32 * self.cell_width;
                // A wide cluster is selected when either half is selected.
                let is_selected = (0..span).any(|d| in_view_selection(selection, x + d, y));
                // Cursor on either half of a wide cluster highlights the
                // whole cluster for block cursors.
                let is_cursor = cursor_visible
                    && cursor.1 == y
                    && (cursor.0 == x || (span == 2 && cursor.0 == x + 1));
                // Base background (selection overrides cell bg; inverse
                // swaps fg/bg via effective colors).
                let eff_bg = cell.effective_bg();
                let eff_fg = cell.effective_fg();
                if is_selected {
                    self.push_quad(
                        &mut verts,
                        px,
                        py,
                        w_px,
                        self.line_height,
                        self.theme.selection.as_linear_f32_array(),
                    );
                } else if eff_bg != self.theme.background {
                    self.push_quad(
                        &mut verts,
                        px,
                        py,
                        w_px,
                        self.line_height,
                        eff_bg.as_linear_f32_array(),
                    );
                }
                // Text decorations: strips in effective fg unless an
                // explicit underline color (`SGR 58`) overrides. Curly /
                // dotted / dashed render as single for now (stored
                // distinctly in the cell for future shaping).
                let deco_col = cell.underline_color.unwrap_or(eff_fg).as_linear_f32_array();
                let fg_col = eff_fg.as_linear_f32_array();
                match cell.underline {
                    crate::grid::UnderlineStyle::None => {}
                    crate::grid::UnderlineStyle::Double => {
                        let lower = py + self.line_height - underline_h - pad;
                        let upper = lower - underline_h - pad;
                        let upper = upper.max(py);
                        self.push_quad(&mut verts, px, lower, w_px, underline_h, deco_col);
                        self.push_quad(&mut verts, px, upper, w_px, underline_h, deco_col);
                    }
                    _ if cell.underline.is_active() => {
                        let uy = py + self.line_height - underline_h - pad;
                        self.push_quad(&mut verts, px, uy, w_px, underline_h, deco_col);
                    }
                    _ => {}
                }
                if cell.strikethrough {
                    let sy = py + self.line_height * 0.5 - underline_h * 0.5;
                    self.push_quad(&mut verts, px, sy, w_px, underline_h, fg_col);
                }
                if cell.overline {
                    let oy = py + pad;
                    self.push_quad(&mut verts, px, oy, w_px, underline_h, fg_col);
                }
                // Cursor: shape-dependent, drawn last so it covers bg/underline.
                if is_cursor {
                    let cursor_col = self.theme.foreground.as_linear_f32_array();
                    match cursor_shape {
                        crate::grid::CursorShape::Block => {
                            self.push_quad(&mut verts, px, py, w_px, self.line_height, cursor_col);
                        }
                        crate::grid::CursorShape::Underline => {
                            let uy = py + self.line_height - cursor_underline_h - pad;
                            self.push_quad(
                                &mut verts,
                                px,
                                uy,
                                w_px,
                                cursor_underline_h,
                                cursor_col,
                            );
                        }
                        crate::grid::CursorShape::Bar => {
                            self.push_quad(&mut verts, px, py, bar_w, self.line_height, cursor_col);
                        }
                    }
                }
                x += span;
            }
        }
        let vert_count = verts.len();
        self.bg_scratch = verts;

        if vert_count > self.bg_vertex_capacity {
            self.bg_vertex_capacity = vert_count.next_power_of_two().max(1024);
            self.bg_vertex_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bg verts"),
                size: (self.bg_vertex_capacity * std::mem::size_of::<BgVertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if vert_count > 0 {
            self.queue.write_buffer(
                &self.bg_vertex_buf,
                0,
                bytemuck::cast_slice(&self.bg_scratch),
            );
        }
        vert_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CursorConfig, FontConfig};

    #[test]
    fn underline_and_cursor_metrics_scale() {
        let font = FontConfig::default();
        let cursor = CursorConfig::default();
        // 1x metrics (14pt base): line ~17.5, cell ~8.4.
        let h1 = underline_thickness(17.5, &font);
        let ch1 = cursor_underline_thickness(17.5, &cursor);
        let b1 = bar_cursor_width(8.4, &cursor);
        assert!(h1 >= 1.0 && h1 < 3.0, "h1={h1}");
        assert!(ch1 >= h1, "cursor underline thicker than text");
        assert!(b1 >= 1.0 && b1 < 8.4, "b1={b1}");
        // 2x scales proportionally.
        let h2 = underline_thickness(35.0, &font);
        let b2 = bar_cursor_width(16.8, &cursor);
        assert!((h2 - h1 * 2.0).abs() < 0.01, "h2={h2}");
        assert!((b2 - b1 * 2.0).abs() < 0.01, "b2={b2}");
    }
}
