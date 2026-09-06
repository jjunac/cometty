mod background;
mod overlay;
mod text;

use background::{BG_SHADER, BgVertex};

use cosmic_text::{Buffer, Metrics, Wrap};
use glyphon::{
    Cache, FontSystem, Resolution, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer,
    Viewport,
};
use wgpu::MultisampleState;

use crate::grid::Cell;
use crate::theme::Theme;

/// Logical font size in points. Scaled by the window scale factor to get
/// physical pixels, so text looks the same size on 1x and 2x displays.
const BASE_FONT_SIZE_LOGICAL: f32 = 14.0;

/// Tab-bar height in logical points. The terminal grid is laid out in the
/// window area below it; all glyphon/GL coordinates add the scaled offset.
pub const TAB_BAR_HEIGHT_POINTS: f32 = 32.0;

pub(crate) fn scaled_metrics(base: f32, scale: f32) -> (f32, f32, f32) {
    let s = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let font_size = base * s;
    (font_size, font_size * 1.25, font_size * 0.602)
}

pub(crate) fn clamp_surface_size(width: u32, height: u32, max_dim: u32) -> (u32, u32) {
    let max_dim = max_dim.max(1);
    (width.max(1).min(max_dim), height.max(1).min(max_dim))
}

/// egui chrome input for [`Renderer::render`]: tab strip + overlay scrollbar.
/// Grouped so `render` stays under the clippy arg limit.
pub struct ScrollCtx<'a> {
    pub window: &'a winit::window::Window,
    pub ui: &'a mut crate::scrollbar::ScrollbarUi,
    pub total: usize,
    pub visible: usize,
    pub offset: usize,
    pub is_alt: bool,
    pub tab_titles: &'a [String],
    pub active_tab: usize,
}

/// [`Renderer::render`] output: scrollbar scrolling plus tab-strip actions
/// the app applies after the frame (switch/close/new).
pub struct RenderOutput {
    pub scroll_to: Option<usize>,
    pub selected_tab: Option<usize>,
    pub close_tab: Option<usize>,
    pub new_tab: bool,
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    buffer: Buffer,
    bg_pipeline: wgpu::RenderPipeline,
    bg_vertex_buf: wgpu::Buffer,
    bg_vertex_capacity: usize,
    bg_scratch: Vec<BgVertex>,
    theme: Theme,
    base_font_size: f32,
    scale_factor: f32,
    max_surface_dim: u32,
    pub font_size: f32,
    pub line_height: f32,
    pub cell_width: f32,
    width: u32,
    height: u32,
    last_grid_version: u64,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
}

impl Renderer {
    pub fn new(
        window: std::sync::Arc<winit::window::Window>,
        width: u32,
        height: u32,
        scale_factor: f32,
        theme: Theme,
    ) -> anyhow::Result<Self> {
        let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        let base_font_size = BASE_FONT_SIZE_LOGICAL;
        let (font_size, line_height, cell_width) = scaled_metrics(base_font_size, scale_factor);

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))?;
        let device_desc_default = wgpu::DeviceDescriptor {
            label: Some("cometty"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            ..Default::default()
        };
        let (device, queue) = match pollster::block_on(adapter.request_device(&device_desc_default))
        {
            Ok(pair) => pair,
            Err(_) => pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("cometty"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                ..Default::default()
            }))?,
        };
        let max_surface_dim = device
            .limits()
            .max_texture_dimension_2d
            .min(adapter.limits().max_texture_dimension_2d)
            .max(1);
        let (clamped_w, clamped_h) = clamp_surface_size(width, height, max_surface_dim);
        if clamped_w != width.max(1) || clamped_h != height.max(1) {
            log::warn!(
                "clamping surface size {width}x{height} to {clamped_w}x{clamped_h} (limit {max_surface_dim})"
            );
        }

        let mut config = surface
            .get_default_config(&adapter, clamped_w, clamped_h)
            .ok_or_else(|| anyhow::anyhow!("surface not supported"))?;
        config.present_mode = wgpu::PresentMode::AutoVsync;
        config.width = clamped_w;
        config.height = clamped_h;
        surface.configure(&device, &config);
        let format = config.format;

        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);
        let metrics = Metrics::new(font_size, line_height);
        let mut buffer = Buffer::new(&mut font_system, metrics);
        buffer.set_wrap(Wrap::None);
        buffer.set_size(Some(clamped_w as f32), Some(clamped_h as f32));

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bg"),
            source: wgpu::ShaderSource::Wgsl(BG_SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bg layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let bg_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("bg pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(BgVertex::desc())],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let bg_vertex_capacity = 1024;
        let bg_vertex_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bg verts"),
            size: (bg_vertex_capacity * std::mem::size_of::<BgVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let egui_ctx = egui::Context::default();
        egui_ctx.set_visuals(theme.to_egui_visuals());
        let mut egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(scale_factor),
            None,
            Some(max_surface_dim as usize),
        );
        egui_state.set_max_texture_side(max_surface_dim as usize);
        let egui_renderer = egui_wgpu::Renderer::new(&device, format, Default::default());

        let mut r = Self {
            device,
            queue,
            surface,
            config,
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            buffer,
            bg_pipeline,
            bg_vertex_buf,
            bg_vertex_capacity,
            bg_scratch: Vec::new(),
            theme,
            base_font_size,
            scale_factor,
            max_surface_dim,
            font_size,
            line_height,
            cell_width,
            width: clamped_w,
            height: clamped_h,
            last_grid_version: u64::MAX,
            egui_ctx,
            egui_state,
            egui_renderer,
        };
        r.update_viewport(clamped_w, clamped_h);
        Ok(r)
    }

    #[allow(dead_code)]
    pub fn theme(&self) -> Theme {
        self.theme
    }

    /// Swap the active theme. Forces a text rebuild on the next render so
    /// future theme switching (CLI flag / config / keybind) just calls this.
    #[allow(dead_code)]
    pub fn set_theme(&mut self, theme: Theme) {
        if self.theme != theme {
            self.theme = theme;
            self.egui_ctx.set_visuals(theme.to_egui_visuals());
            self.last_grid_version = u64::MAX;
        }
    }

    /// Forward a winit window event to egui. Call at the top of the
    /// `WindowEvent` handler; when `consumed` is true the event was over
    /// egui chrome (scrollbar) and terminal handling should be skipped.
    pub fn on_window_event(
        &mut self,
        window: &winit::window::Window,
        event: &winit::event::WindowEvent,
    ) -> egui_winit::EventResponse {
        self.egui_state.on_window_event(window, event)
    }

    /// Update the DPI scale factor. Font metrics are re-derived so logical
    /// text size stays constant across 1x/2x displays.
    /// Returns true if the scale changed.
    pub fn set_scale_factor(&mut self, scale: f32) -> bool {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        if (scale - self.scale_factor).abs() < f32::EPSILON {
            return false;
        }
        self.scale_factor = scale;
        let (font_size, line_height, cell_width) =
            scaled_metrics(self.base_font_size, self.scale_factor);
        self.font_size = font_size;
        self.line_height = line_height;
        self.cell_width = cell_width;
        self.last_grid_version = u64::MAX;
        true
    }

    fn update_viewport(&mut self, width: u32, height: u32) {
        self.width = width.max(1);
        self.height = height.max(1);
        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.width,
                height: self.height,
            },
        );
        self.buffer
            .set_size(Some(self.width as f32), Some(self.height as f32));
        self.last_grid_version = u64::MAX;
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        let (clamped_w, clamped_h) = clamp_surface_size(width, height, self.max_surface_dim);
        if clamped_w != width || clamped_h != height {
            log::warn!(
                "clamping surface size {width}x{height} to {clamped_w}x{clamped_h} (limit {})",
                self.max_surface_dim
            );
        }
        if clamped_w == self.width && clamped_h == self.height {
            return;
        }
        self.config.width = clamped_w;
        self.config.height = clamped_h;
        self.surface.configure(&self.device, &self.config);
        self.update_viewport(clamped_w, clamped_h);
    }

    pub fn cols_for_width(&self, width: u32) -> usize {
        ((width as f32 / self.cell_width).floor() as usize).max(1)
    }

    pub fn rows_for_height(&self, height: u32) -> usize {
        ((height as f32 / self.line_height).floor() as usize).max(1)
    }

    /// Current DPI scale factor (for tab-bar / grid-size math in `App`).
    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    /// Tab-bar height in physical pixels at the current scale.
    pub fn tab_bar_px(&self) -> f32 {
        crate::app::tab::tab_bar_px(self.scale_factor)
    }

    /// Forget the cached grid version so the next `render` reshapes text.
    /// Required on tab switch: two tabs can share a version counter while
    /// holding different content.
    pub fn invalidate(&mut self) {
        self.last_grid_version = u64::MAX;
    }

    /// Rows available to the terminal grid (window minus tab bar).
    pub fn term_rows(&self) -> usize {
        let h = (self.height as f32 - self.tab_bar_px()).max(1.0) as u32;
        self.rows_for_height(h)
    }

    /// Map physical pixels to a visible `(col, row)` cell in the terminal
    /// area. Returns `None` on the tab bar or outside the grid.
    pub fn cell_at_pos(&self, x: f32, y: f32) -> Option<(usize, usize)> {
        let tab_bar = self.tab_bar_px();
        if y < tab_bar {
            return None;
        }
        let cols = self.cols_for_width(self.width);
        let rows = self.term_rows();
        crate::selection::cell_at_pos(
            x,
            y - tab_bar,
            self.cell_width,
            self.line_height,
            cols,
            rows,
        )
    }

    /// True when physical `y` falls on the tab-bar chrome.
    pub fn over_tab_bar(&self, y_phys: f32) -> bool {
        y_phys.is_finite() && y_phys < self.tab_bar_px()
    }

    /// True when physical `x` falls on the overlay scrollbar strip.
    /// Callers use this (not egui's `consumed` flag) to decide whether a
    /// press belongs to scrollbar chrome or terminal selection.
    pub fn over_scrollbar(&self, x_phys: f32) -> bool {
        let scale = self.scale_factor.max(1.0);
        let screen_w_pts = self.width as f32 / scale;
        crate::scrollbar::hit_test(x_phys / scale, screen_w_pts)
    }

    /// Thin orchestrator: rebuild text on version change, paint bg +
    /// overlay, then submit the frame. Heavy lifting lives in
    /// `text` / `background` / `overlay`.
    pub fn render(
        &mut self,
        grid_rows: &[Vec<Cell>],
        cursor: (usize, usize),
        cursor_visible: bool,
        grid_version: u64,
        selection: Option<((usize, usize), (usize, usize))>,
        scroll: ScrollCtx<'_>,
    ) -> anyhow::Result<RenderOutput> {
        if grid_version != self.last_grid_version {
            self.rebuild_buffer(grid_rows);
            self.last_grid_version = grid_version;
        }

        let vert_count = self.paint_bg(grid_rows, cursor, cursor_visible, selection);
        let overlay::OverlayOutput {
            scroll_to,
            selected_tab,
            close_tab,
            new_tab,
            paint_jobs,
            screen_descriptor,
        } = self.paint_overlay(scroll);

        let out = RenderOutput {
            scroll_to,
            selected_tab,
            close_tab,
            new_tab,
        };

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(out);
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(out);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Ok(out);
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let text_areas = [TextArea {
            buffer: &self.buffer,
            left: 0.0,
            top: self.tab_bar_px(),
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: 0,
                right: self.width as i32,
                bottom: self.height as i32,
            },
            default_color: self.theme.foreground.as_glyphon_color(),
            custom_glyphs: &[],
        }];

        self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            text_areas,
            &mut self.swash_cache,
        )?;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("cometty encoder"),
            });
        self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &paint_jobs,
            &screen_descriptor,
        );
        {
            let bg = self.theme.background.as_linear_f32_array();
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("cometty pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: bg[0] as f64,
                            g: bg[1] as f64,
                            b: bg[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if vert_count > 0 {
                pass.set_pipeline(&self.bg_pipeline);
                pass.set_vertex_buffer(0, self.bg_vertex_buf.slice(..));
                pass.draw(0..vert_count as u32, 0..1);
            }
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)?;
            self.egui_renderer
                .render(&mut pass.forget_lifetime(), &paint_jobs, &screen_descriptor);
        }
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        self.atlas.trim();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Cell;

    fn test_cells(text: &str) -> Vec<Cell> {
        text.chars()
            .map(|ch| Cell {
                ch,
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn buffer_lines_shape_to_visible_glyphs() {
        // Headless regression test for invisible text: lines pushed directly
        // into a cosmic-text Buffer bypass dirty flags, so they must be laid
        // out explicitly before glyphon can render them.
        let mut font_system = FontSystem::new();
        let metrics = Metrics::new(16.0, 20.0);
        let mut buffer = Buffer::new(&mut font_system, metrics);
        buffer.set_wrap(Wrap::None);
        buffer.set_size(Some(800.0), Some(600.0));

        let rows = vec![test_cells("hello"), test_cells("hi")];
        let theme = Theme::default();
        buffer.lines.clear();
        for line in super::text::build_buffer_lines(&rows, &theme) {
            buffer.lines.push(line);
        }

        // Without explicit layout there are no visible runs (the bug).
        assert_eq!(buffer.layout_runs().count(), 0);

        let n = buffer.lines.len();
        for i in 0..n {
            buffer.line_layout(&mut font_system, i);
        }

        assert!(buffer.layout_runs().count() > 0);
    }

    #[test]
    fn scaled_metrics_follow_scale_factor() {
        let (font_1x, line_1x, cell_1x) = scaled_metrics(16.0, 1.0);
        let (font_2x, line_2x, cell_2x) = scaled_metrics(16.0, 2.0);
        assert_eq!(font_1x, 16.0);
        assert_eq!(font_2x, 32.0);
        assert_eq!(line_2x, line_1x * 2.0);
        assert_eq!(cell_2x, cell_1x * 2.0);
    }

    #[test]
    fn clamp_surface_size_caps_at_limit() {
        assert_eq!(clamp_surface_size(2204, 1200, 2048), (2048, 1200));
        assert_eq!(clamp_surface_size(800, 600, 2048), (800, 600));
        assert_eq!(clamp_surface_size(0, 0, 2048), (1, 1));
    }
}
