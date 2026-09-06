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
pub(crate) fn underline_thickness(line_height: f32) -> f32 {
    (line_height * 0.07).max(1.0)
}

/// Thicker strip used for the underline cursor shape (DECSCUSR 3/4).
pub(crate) fn cursor_underline_thickness(line_height: f32) -> f32 {
    (line_height * 0.14).max(1.0)
}

/// Width of the bar cursor shape (DECSCUSR 5/6).
pub(crate) fn bar_cursor_width(cell_width: f32) -> f32 {
    (cell_width * 0.3).max(1.0)
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
    /// differ from the theme background (plus cursor + selection + underline),
    /// ensure GPU capacity, and upload. Returns the vertex count to draw.
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
        let underline_h = underline_thickness(self.line_height);
        let cursor_underline_h = cursor_underline_thickness(self.line_height);
        let bar_w = bar_cursor_width(self.cell_width);
        let pad = self.scale_factor.max(1.0);
        for (y, row) in grid_rows.iter().enumerate() {
            let py = y_off + y as f32 * self.line_height;
            for (x, cell) in row.iter().enumerate() {
                let is_cursor = cursor_visible && cursor.0 == x && cursor.1 == y;
                let is_selected = in_view_selection(selection, x, y);
                let px = x as f32 * self.cell_width;
                // Base background (selection overrides cell bg).
                if is_selected {
                    self.push_quad(
                        &mut verts,
                        px,
                        py,
                        self.cell_width,
                        self.line_height,
                        self.theme.selection.as_linear_f32_array(),
                    );
                } else if cell.bg != self.theme.background {
                    self.push_quad(
                        &mut verts,
                        px,
                        py,
                        self.cell_width,
                        self.line_height,
                        cell.bg.as_linear_f32_array(),
                    );
                }
                // Text underline: thin strip in the cell's fg color.
                if cell.underline {
                    let uy = py + self.line_height - underline_h - pad;
                    self.push_quad(
                        &mut verts,
                        px,
                        uy,
                        self.cell_width,
                        underline_h,
                        cell.fg.as_linear_f32_array(),
                    );
                }
                // Cursor: shape-dependent, drawn last so it covers bg/underline.
                if is_cursor {
                    let cursor_col = self.theme.foreground.as_linear_f32_array();
                    match cursor_shape {
                        crate::grid::CursorShape::Block => {
                            self.push_quad(
                                &mut verts,
                                px,
                                py,
                                self.cell_width,
                                self.line_height,
                                cursor_col,
                            );
                        }
                        crate::grid::CursorShape::Underline => {
                            let uy = py + self.line_height - cursor_underline_h - pad;
                            self.push_quad(
                                &mut verts,
                                px,
                                uy,
                                self.cell_width,
                                cursor_underline_h,
                                cursor_col,
                            );
                        }
                        crate::grid::CursorShape::Bar => {
                            self.push_quad(&mut verts, px, py, bar_w, self.line_height, cursor_col);
                        }
                    }
                }
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

    #[test]
    fn underline_and_cursor_metrics_scale() {
        // 1x metrics (14pt base): line ~17.5, cell ~8.4.
        let h1 = underline_thickness(17.5);
        let ch1 = cursor_underline_thickness(17.5);
        let b1 = bar_cursor_width(8.4);
        assert!(h1 >= 1.0 && h1 < 3.0, "h1={h1}");
        assert!(ch1 >= h1, "cursor underline thicker than text");
        assert!(b1 >= 1.0 && b1 < 8.4, "b1={b1}");
        // 2x scales proportionally.
        let h2 = underline_thickness(35.0);
        let b2 = bar_cursor_width(16.8);
        assert!((h2 - h1 * 2.0).abs() < 0.01, "h2={h2}");
        assert!((b2 - b1 * 2.0).abs() < 0.01, "b2={b2}");
    }
}
