mod background;
mod blocks;
mod log_ui;
mod overlay;
mod settings_ui;
mod text;

use background::{BG_SHADER, BgVertex};

use cosmic_text::{Buffer, Metrics, Wrap};
use glyphon::{
    Cache, FontSystem, Resolution, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer,
    Viewport,
};
use wgpu::MultisampleState;

use crate::config::{Config, FontConfig};
use crate::grid::Cell;
use crate::theme::Theme;

pub(crate) fn scaled_metrics(base: f32, scale: f32, font: &FontConfig) -> (f32, f32, f32) {
    let s = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let font_size = base * s;
    (
        font_size,
        font_size * font.line_height_factor,
        font_size * font.cell_width_factor,
    )
}

pub(crate) fn clamp_surface_size(width: u32, height: u32, max_dim: u32) -> (u32, u32) {
    let max_dim = max_dim.max(1);
    (width.max(1).min(max_dim), height.max(1).min(max_dim))
}

/// macOS/Metal fullscreen stripe workaround.
///
/// On Intel Iris Plus GPUs (MacBookPro16,x and relatives), a drawable that
/// is exactly the size of the display — i.e. native fullscreen — makes the
/// Metal driver paint the *cleared* background as dense vertical stripes
/// (`wgpu#3415`, `pixels#394`). Everything drawn as geometry (text, our bg
/// quads, the egui chrome) stays correct, which is why only the terminal
/// background is corrupted. A drawable one pixel narrower is unaffected;
/// the cost is a single cropped column at the right edge.
///
/// No-op on windowed windows and on every other platform.
pub(crate) fn shrink_fullscreen_surface(width: u32, height: u32, fullscreen: bool) -> (u32, u32) {
    // The Metal backend is the only one affected; the parameter is unused
    // (but still part of the cross-platform signature) elsewhere.
    let _ = fullscreen;
    #[cfg(target_os = "macos")]
    if fullscreen && width > 1 {
        return (width - 1, height);
    }
    (width, height)
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
    pub settings: &'a mut crate::app::settings::SettingsPanel,
    pub config: &'a mut Config,
    pub logs: &'a mut crate::app::logs::LogsPanel,
    /// Shared with the global logger; only locked while the panel is open.
    pub log_buffer: &'a std::sync::Arc<std::sync::Mutex<crate::logbuf::LogBuffer>>,
}

/// Cursor input for [`Renderer::render`]: position + visibility + shape.
/// Grouped so `render` stays under the clippy arg limit.
pub struct CursorCtx {
    pub pos: (usize, usize),
    pub visible: bool,
    pub shape: crate::grid::CursorShape,
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
    user_config: Config,
    base_font_size: f32,
    scale_factor: f32,
    max_surface_dim: u32,
    pub font_size: f32,
    pub line_height: f32,
    pub cell_width: f32,
    width: u32,
    height: u32,
    /// True while the surface is configured for a display-sized (fullscreen)
    /// drawable: feeding [`shrink_fullscreen_surface`].
    fullscreen: bool,
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
        user_config: &Config,
    ) -> anyhow::Result<Self> {
        let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        let base_font_size = user_config.font.size;
        let (font_size, line_height, cell_width) =
            scaled_metrics(base_font_size, scale_factor, &user_config.font);

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
            user_config: user_config.clone(),
            base_font_size,
            scale_factor,
            max_surface_dim,
            font_size,
            line_height,
            cell_width,
            width: clamped_w,
            height: clamped_h,
            // The startup window is never created fullscreen; a later
            // fullscreen resize goes through `resize`.
            fullscreen: false,
            last_grid_version: u64::MAX,
            egui_ctx,
            egui_state,
            egui_renderer,
        };
        r.update_viewport(clamped_w, clamped_h);
        log::debug!(
            "renderer ready: surface {clamped_w}x{clamped_h} @ {scale_factor}x, cell {cell_width:.2}x{line_height:.2}, theme {}",
            r.theme.name
        );
        Ok(r)
    }

    #[allow(dead_code)]
    pub fn theme(&self) -> Theme {
        self.theme
    }

    /// Swap the active theme. Forces a text rebuild on the next render so
    /// future theme switching (CLI flag / config / keybind) just calls this.
    pub fn set_theme(&mut self, theme: Theme) {
        if self.theme != theme {
            log::debug!("theme -> {}", theme.name);
            self.theme = theme;
            self.egui_ctx.set_visuals(theme.to_egui_visuals());
            self.last_grid_version = u64::MAX;
        }
    }

    /// Live-apply settings edits: refresh the cached user config, re-derive
    /// font metrics from the (possibly new) base size, and force a rebuild.
    /// Theme itself flows through [`Self::set_theme`].
    pub fn apply_config(&mut self, config: &Config) {
        self.user_config = config.clone();
        self.base_font_size = config.font.size;
        let (font_size, line_height, cell_width) =
            scaled_metrics(self.base_font_size, self.scale_factor, &config.font);
        self.font_size = font_size;
        self.line_height = line_height;
        self.cell_width = cell_width;
        self.last_grid_version = u64::MAX;
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
        log::debug!("scale factor -> {scale}x");
        self.scale_factor = scale;
        let (font_size, line_height, cell_width) = scaled_metrics(
            self.base_font_size,
            self.scale_factor,
            &self.user_config.font,
        );
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

    /// Resize the surface. `fullscreen` marks a drawable that covers the
    /// display exactly (native fullscreen / screen-sized window): on macOS
    /// the surface is then shrunk by one pixel to dodge the Metal stripe
    /// bug, see [`shrink_fullscreen_surface`]. The flag is part of the
    /// cache key, so entering/leaving fullscreen without a size change
    /// still reconfigures.
    pub fn resize(&mut self, width: u32, height: u32, fullscreen: bool) {
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
        let (surf_w, surf_h) = shrink_fullscreen_surface(clamped_w, clamped_h, fullscreen);
        if surf_w == self.width && surf_h == self.height && fullscreen == self.fullscreen {
            return;
        }
        if surf_w != clamped_w || surf_h != clamped_h {
            log::debug!(
                "macos fullscreen stripe workaround: surface {surf_w}x{surf_h} (window {width}x{height})"
            );
        }
        self.fullscreen = fullscreen;
        self.config.width = surf_w;
        self.config.height = surf_h;
        self.surface.configure(&self.device, &self.config);
        self.update_viewport(surf_w, surf_h);
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
    /// Hidden (0.0) for a single tab; callers pass `tab_count`.
    pub fn tab_bar_px(&self, tab_count: usize) -> f32 {
        crate::app::tab::tab_bar_px(self.scale_factor, tab_count, &self.user_config.tabbar)
    }

    /// Forget the cached grid version so the next `render` reshapes text.
    /// Required on tab switch: two tabs can share a version counter while
    /// holding different content.
    pub fn invalidate(&mut self) {
        self.last_grid_version = u64::MAX;
    }

    /// Rows available to the terminal grid (window minus tab bar).
    pub fn term_rows(&self, tab_count: usize) -> usize {
        let h = (self.height as f32 - self.tab_bar_px(tab_count)).max(1.0) as u32;
        self.rows_for_height(h)
    }

    /// Map physical pixels to a visible `(col, row)` cell in the terminal
    /// area. Returns `None` on the tab bar or outside the grid.
    pub fn cell_at_pos(&self, x: f32, y: f32, tab_count: usize) -> Option<(usize, usize)> {
        let tab_bar = self.tab_bar_px(tab_count);
        if y < tab_bar {
            return None;
        }
        let cols = self.cols_for_width(self.width);
        let rows = self.term_rows(tab_count);
        crate::selection::cell_at_pos(
            x,
            y - tab_bar,
            self.cell_width,
            self.line_height,
            cols,
            rows,
        )
    }

    /// True when physical `y` falls on the tab-bar chrome (never when the
    /// bar is hidden for a single tab).
    pub fn over_tab_bar(&self, y_phys: f32, tab_count: usize) -> bool {
        y_phys.is_finite() && y_phys < self.tab_bar_px(tab_count)
    }

    /// True when physical `x` falls on the overlay scrollbar strip.
    /// Callers use this (not egui's `consumed` flag) to decide whether a
    /// press belongs to scrollbar chrome or terminal selection.
    pub fn over_scrollbar(&self, x_phys: f32) -> bool {
        let scale = self.scale_factor.max(1.0);
        let screen_w_pts = self.width as f32 / scale;
        crate::scrollbar::hit_test(x_phys / scale, screen_w_pts, &self.user_config.scrollbar)
    }

    /// Thin orchestrator: rebuild text on version change, paint bg +
    /// overlay, then submit the frame. Heavy lifting lives in
    /// `text` / `background` / `overlay`.
    pub fn render(
        &mut self,
        grid_rows: &[Vec<Cell>],
        cursor: CursorCtx,
        grid_version: u64,
        selection: Option<((usize, usize), (usize, usize))>,
        scroll: ScrollCtx<'_>,
    ) -> anyhow::Result<RenderOutput> {
        if grid_version != self.last_grid_version {
            self.rebuild_buffer(grid_rows);
            self.last_grid_version = grid_version;
        }

        let tab_count = scroll.tab_titles.len();
        let vert_count = self.paint_bg(
            grid_rows,
            cursor.pos,
            cursor.visible,
            cursor.shape,
            selection,
            tab_count,
        );
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
            top: self.tab_bar_px(tab_count),
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
        let font = crate::config::FontConfig::default();
        buffer.lines.clear();
        for line in super::text::build_buffer_lines(&rows, &theme, &font) {
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
    fn buffer_lines_skip_wide_continuations() {
        let theme = Theme::default();
        let font = crate::config::FontConfig::default();
        let lead = Cell {
            ch: '中',
            extra: None,
            width: 2,
            ..Default::default()
        };
        let cont = Cell {
            ch: ' ',
            extra: None,
            width: 0,
            ..Default::default()
        };
        let a = Cell {
            ch: 'a',
            ..Default::default()
        };
        let rows = vec![vec![lead, cont, a]];
        let lines = super::text::build_buffer_lines(&rows, &theme, &font);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "中a");
    }

    #[test]
    fn buffer_lines_shape_block_elements_as_spaces() {
        // Block elements are painted by the renderer as cell-sized rects, so
        // their glyphs must not be shaped (font glyphs leave row gaps).
        let theme = Theme::default();
        let font = crate::config::FontConfig::default();
        let block = Cell {
            ch: '█',
            ..Default::default()
        };
        let half = Cell {
            ch: '▀',
            ..Default::default()
        };
        let a = Cell {
            ch: 'a',
            ..Default::default()
        };
        let shade = Cell {
            ch: '░',
            ..Default::default()
        };
        let rows = vec![vec![block, half, a, shade]];
        let lines = super::text::build_buffer_lines(&rows, &theme, &font);
        // `█` and `▀` become spaces (drawn as rects); the shade keeps its
        // glyph (a dither pattern, nothing solid to draw).
        assert_eq!(lines[0].text(), "  a░");
    }

    #[test]
    fn buffer_lines_include_zwj_cluster_once() {
        let theme = Theme::default();
        let font = crate::config::FontConfig::default();
        let cluster = "👨\u{200D}👩\u{200D}👧";
        let mut chars = cluster.chars();
        let first = chars.next().unwrap();
        let rest: String = chars.collect();
        let lead = Cell {
            ch: first,
            extra: Some(rest.into_boxed_str()),
            width: 2,
            ..Default::default()
        };
        let cont = Cell {
            ch: ' ',
            extra: None,
            width: 0,
            ..Default::default()
        };
        let rows = vec![vec![lead, cont]];
        let lines = super::text::build_buffer_lines(&rows, &theme, &font);
        assert_eq!(lines[0].text(), cluster);
    }

    #[test]
    fn scaled_metrics_follow_scale_factor() {
        use crate::config::FontConfig;
        let font = FontConfig::default();
        let (font_1x, line_1x, cell_1x) = scaled_metrics(16.0, 1.0, &font);
        let (font_2x, line_2x, cell_2x) = scaled_metrics(16.0, 2.0, &font);
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

    #[test]
    fn fullscreen_surface_shrinks_one_pixel_on_macos() {
        // Windowed drawables are never display-sized: untouched everywhere.
        assert_eq!(shrink_fullscreen_surface(2560, 1600, false), (2560, 1600));
        // Fullscreen: one column cropped on macOS, untouched elsewhere.
        let (w, h) = shrink_fullscreen_surface(2560, 1600, true);
        #[cfg(target_os = "macos")]
        assert_eq!((w, h), (2559, 1600));
        #[cfg(not(target_os = "macos"))]
        assert_eq!((w, h), (2560, 1600));
        // Never shrink below a usable surface.
        assert_eq!(shrink_fullscreen_surface(1, 1, true), (1, 1));
    }
}
