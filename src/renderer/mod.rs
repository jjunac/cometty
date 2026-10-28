mod background;
mod blocks;
mod frame;
#[cfg(test)]
mod gpu_tests;
mod log_ui;
mod overlay;
mod search_ui;
mod settings_ui;
pub(crate) mod text;

use background::{BG_SHADER, BgVertex};

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

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

/// Rate-limit slow-frame debug logs: at most one every 2s, so a
/// sustained slowdown is visible without flooding the log panel.
/// Free function so it is unit-testable without a `Renderer` (which
/// needs a window + GPU).
fn slow_log_due(last: &mut Option<Instant>, now: Instant) -> bool {
    if last.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(2)) {
        *last = Some(now);
        true
    } else {
        false
    }
}

/// Surface recovery must not depend on a cursor blink. Back off persistent
/// failures so an unavailable surface cannot create another repaint loop.
fn surface_retry_delay(failures: &mut u8) -> Duration {
    let delay = Duration::from_millis((16_u64 << (*failures).min(6)).min(1000));
    *failures = failures.saturating_add(1);
    delay
}

/// Pick the lowest-latency vsync present mode the surface supports:
/// `Mailbox` never blocks the CPU on acquire and never queues stale
/// frames (no tearing); elsewhere fall back to `AutoVsync`.
/// Free function so it is unit-testable without a GPU surface.
fn pick_present_mode(supported: &[wgpu::PresentMode]) -> wgpu::PresentMode {
    if supported.contains(&wgpu::PresentMode::Mailbox) {
        wgpu::PresentMode::Mailbox
    } else {
        wgpu::PresentMode::AutoVsync
    }
}

/// Next `(force_enable, cached)` state for [`Renderer::ensure_terminal_ime`].
///
/// egui owns window IME while one of its text fields is focused; when that
/// field loses focus, `egui-winit` calls `set_ime_allowed(false)`. The
/// terminal must take IME back on the next frame, or dead keys (macOS
/// US-Intl `' " ` ~`) stop producing any text for the rest of the session.
/// The cached state keeps a steady terminal from re-sending
/// `set_ime_allowed(true)` every frame, which could interrupt an in-progress
/// composition.
fn terminal_ime_step(
    settings_open: bool,
    egui_text_focused: bool,
    terminal_ime_on: bool,
) -> (bool, bool) {
    if settings_open || egui_text_focused {
        // egui drives IME (or the panel owns the keyboard); re-arm the
        // terminal hand-off for the frame after control comes back.
        (false, false)
    } else if terminal_ime_on {
        (false, true)
    } else {
        (true, true)
    }
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
    /// Per-tab find bar (query/matches state; drawn by `search_ui`).
    pub search: &'a mut crate::app::search::SearchPanel,
    pub input_notice: Option<&'a str>,
}

/// Highlight inputs for [`Renderer::render`]: text selection + find
/// matches + link hint underlines. Grouped so `render` stays under the
/// clippy arg limit.
pub struct HighlightCtx<'a> {
    pub selection: Option<((usize, usize), (usize, usize))>,
    pub search: Option<&'a crate::search::SearchView>,
    /// Link/URL underline runs while the hint modifier is held.
    pub links: Option<&'a crate::links::HintView>,
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
    /// True only after a frame was actually submitted and presented.
    pub presented: bool,
    pub scroll_to: Option<usize>,
    pub selected_tab: Option<usize>,
    pub close_tab: Option<usize>,
    pub new_tab: bool,
    /// Find-bar buttons clicked this frame (next/prev/close).
    pub search: search_ui::SearchUiOutput,
    /// Earliest delay egui wants the next frame after (`Duration::MAX` =
    /// idle); see [`OverlayOutput::egui_repaint_delay`].
    pub egui_repaint_delay: std::time::Duration,
    /// Transient surface recovery, independent of cursor/egui animation.
    pub retry_after: Option<Duration>,
}

/// All inputs baked into glyphon's prepared instances. The revision belongs
/// to the shared shaped buffer, not a tab's (possibly equal) grid counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PreparedTextKey {
    revision: u64,
    width: u32,
    height: u32,
    top: u32,
    default_color: glyphon::Color,
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    font_system: FontSystem,
    resolved_font: String,
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
    /// Painted find-bar rect (logical points), for press hit-testing.
    search_bar_rect: Option<egui::Rect>,
    last_grid_version: u64,
    /// Cached "terminal forced window IME on" state (see
    /// [`Self::ensure_terminal_ime`]). Cleared while egui owns a text field
    /// or the settings panel, so the hand-off re-asserts when control
    /// returns to the terminal.
    ime_terminal_enabled: bool,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    /// Cached tab-title widths for the current egui font: titles rarely
    /// change but were shaped every frame via `layout_no_wrap`.
    title_width_cache: std::collections::HashMap<String, f32>,
    title_width_font: Option<egui::FontId>,
    /// Natural-advance cache for fixed-cell normalization (see
    /// `renderer::text::AdvanceCache`); cleared when family/size changes.
    advance_cache: text::AdvanceCache,
    /// Per-row text hashes for the last shaped frame (see
    /// `renderer::text::diff_rows`): typing re-shapes only changed lines.
    /// Cleared with `last_grid_version` whenever metrics/theme/size change
    /// (a hash hit is only valid under identical shaping inputs); kept
    /// across `invalidate()` so content-identical frames skip layout.
    shaped_hashes: Vec<u64>,
    text_revision: u64,
    prepared_text: Option<PreparedTextKey>,
    /// Last slow-frame log (see `slow_log_due`): rate-limits the
    /// debug probes so a sustained slowdown can't flood the log panel.
    last_slow_log: Option<Instant>,
    surface_failures: u8,
    /// Surface loss can be retried; confirmed device loss cannot. Stop new
    /// uploads until restart rather than growing work against a dead queue.
    device_lost: Arc<AtomicBool>,
}

impl frame::Backend for Renderer {
    type CommandBuffer = wgpu::CommandBuffer;

    fn is_device_lost(&self) -> bool {
        self.device_lost.load(Ordering::Relaxed)
    }

    fn submit(&mut self, commands: impl IntoIterator<Item = wgpu::CommandBuffer>) {
        self.queue.submit(commands);
    }

    fn update_texture(&mut self, id: egui::TextureId, delta: &egui::epaint::ImageDelta) {
        self.egui_renderer
            .update_texture(&self.device, &self.queue, id, delta);
    }

    fn free_texture(&mut self, id: egui::TextureId) {
        self.egui_renderer.free_texture(&id);
    }
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
        let device_lost = Arc::new(AtomicBool::new(false));
        let lost = Arc::clone(&device_lost);
        device.set_device_lost_callback(move |reason, message| {
            if !lost.swap(true, Ordering::Relaxed) && reason == wgpu::DeviceLostReason::Unknown {
                log::error!("graphics device lost: {message}; rendering disabled until restart");
            }
        });
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
        config.present_mode = pick_present_mode(&surface.get_capabilities(&adapter).present_modes);
        log::debug!("present mode -> {:?}", config.present_mode);
        config.width = clamped_w;
        config.height = clamped_h;
        surface.configure(&device, &config);
        let format = config.format;

        let mut font_system = FontSystem::new();
        text::configure_shaping_cache(&mut font_system);
        let resolved_font = text::resolved_font_name(&font_system, &user_config.font.family);
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
        // Fixed-cell advances come from per-span `letter_spacing` (see
        // `renderer::text`); `monospace_width` rounding stays off.

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
            resolved_font,
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
            search_bar_rect: None,
            last_grid_version: u64::MAX,
            ime_terminal_enabled: false,
            egui_ctx,
            egui_state,
            egui_renderer,
            title_width_cache: std::collections::HashMap::new(),
            title_width_font: None,
            advance_cache: text::AdvanceCache::new(),
            // Empty: the first render takes the full path below, which
            // fills it; every config change clears it for the same reason.
            shaped_hashes: Vec::new(),
            text_revision: 0,
            prepared_text: None,
            last_slow_log: None,
            surface_failures: 0,
            device_lost,
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
            // Span colors changed: hashes would falsely hit.
            self.shaped_hashes.clear();
        }
    }

    /// Live-apply settings edits: refresh the cached user config, re-derive
    /// font metrics from the (possibly new) base size, and force a rebuild.
    /// Theme itself flows through [`Self::set_theme`].
    pub fn apply_config(&mut self, config: &Config) {
        let metrics_changed = self.user_config.font != config.font;
        self.user_config = config.clone();
        if !metrics_changed {
            return;
        }
        self.base_font_size = config.font.size;
        self.resolved_font = text::resolved_font_name(&self.font_system, &config.font.family);
        let (font_size, line_height, cell_width) =
            scaled_metrics(self.base_font_size, self.scale_factor, &config.font);
        self.font_size = font_size;
        self.line_height = line_height;
        self.cell_width = cell_width;
        // Family/size may have changed: drop cached natural advances.
        self.advance_cache.clear();
        self.font_system.shape_run_cache.clear();
        // Metrics feed every span's advance: hashes would falsely hit.
        self.shaped_hashes.clear();
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

    /// True while an egui text field holds keyboard focus (find-bar query,
    /// log filter, settings inputs). Anything else — unfocused, or focus
    /// on chrome — means the terminal owns keystrokes.
    pub fn text_edit_focused(&self) -> bool {
        self.egui_ctx.text_edit_focused()
    }

    /// Drop whatever egui keyboard focus exists (stale chrome/text focus
    /// after the pointer moved back to the grid). No-op when unfocused.
    pub fn surrender_egui_focus(&mut self) {
        if let Some(id) = self.egui_ctx.memory(|m| m.focused()) {
            self.egui_ctx.memory_mut(|m| m.surrender_focus(id));
        }
    }

    /// Keep window IME on while the terminal owns the keyboard (settings
    /// closed, no egui text field focused). egui enables IME only for its
    /// own text fields; without this, macOS dead keys (`" ' ` ~` on
    /// US-Intl) produce neither text nor `Ime` events and vanish before
    /// any handler runs. Normal keys are unaffected: without marked text
    /// winit still emits plain `KeyboardInput`.
    ///
    /// egui disables IME again when its last text field loses focus (find
    /// bar, log filter, settings input), so this cannot be a one-shot
    /// force: the cached flag is reset whenever egui owns a field and
    /// re-asserted on the frame after it lets go (see
    /// [`terminal_ime_step`]). A steady terminal never re-sends, so an
    /// in-progress composition is not disturbed.
    fn ensure_terminal_ime(&mut self, window: &winit::window::Window, settings_open: bool) {
        let (force, cached) = terminal_ime_step(
            settings_open,
            self.text_edit_focused(),
            self.ime_terminal_enabled,
        );
        self.ime_terminal_enabled = cached;
        if force {
            window.set_ime_allowed(true);
        }
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
        // Scaled size changed: drop cached natural advances.
        self.advance_cache.clear();
        self.font_system.shape_run_cache.clear();
        // Metrics feed every span's advance: hashes would falsely hit.
        self.shaped_hashes.clear();
        self.last_grid_version = u64::MAX;
        true
    }

    fn update_viewport(&mut self, width: u32, height: u32) {
        self.width = width.max(1);
        self.height = height.max(1);
        // CPU layout changes immediately. The glyphon uniform upload waits
        // until acquisition succeeds, so resize/restore failures cannot queue
        // unsubmitted writes outside the frame cleanup boundary.
        self.buffer
            .set_size(Some(self.width as f32), Some(self.height as f32));
        // A new size can reflow layout: hashes would falsely hit.
        self.shaped_hashes.clear();
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
        if !self.device_lost.load(Ordering::Relaxed) {
            self.surface.configure(&self.device, &self.config);
        }
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

    /// True when physical `(x, y)` falls on the painted find bar (only
    /// meaningful while it is open; the app gates on its own state).
    pub fn over_search_bar(&self, x_phys: f32, y_phys: f32) -> bool {
        let Some(rect) = self.search_bar_rect else {
            return false;
        };
        let scale = self.scale_factor.max(1.0);
        rect.contains(egui::pos2(x_phys / scale, y_phys / scale))
    }

    /// Thin orchestrator: rebuild text on version change, paint bg +
    /// overlay, then submit the frame. Heavy lifting lives in
    /// `text` / `background` / `overlay`.
    pub fn render(
        &mut self,
        grid_rows: &[Vec<Cell>],
        cursor: CursorCtx,
        grid_version: u64,
        highlights: HighlightCtx<'_>,
        scroll: ScrollCtx<'_>,
    ) -> anyhow::Result<RenderOutput> {
        let mut result = frame::run(self, |renderer, frame| {
            let result =
                renderer.render_frame(grid_rows, cursor, grid_version, highlights, scroll, frame);
            if result.is_err() {
                // An error can leave atlas/instance state partially replaced.
                renderer.prepared_text = None;
            }
            result
        });
        // Loss may also be reported by the upload-only cleanup submission.
        if self.device_lost.load(Ordering::Relaxed) {
            self.prepared_text = None;
            if let Ok(out) = &mut result {
                out.retry_after = None;
                out.egui_repaint_delay = Duration::MAX;
            }
        }
        result
    }

    fn render_frame(
        &mut self,
        grid_rows: &[Vec<Cell>],
        cursor: CursorCtx,
        grid_version: u64,
        highlights: HighlightCtx<'_>,
        scroll: ScrollCtx<'_>,
        frame_uploads: &mut frame::Frame,
    ) -> anyhow::Result<RenderOutput> {
        // Latency probe: per-keystroke reshape + submit cost. Slow-only,
        // rate-limited debug logs; the fast path stays silent.
        let render_start = Instant::now();
        let rebuild_elapsed = if grid_version != self.last_grid_version {
            let _timing = crate::perf::measure(crate::perf::Metric::Rebuild);
            let rebuild_start = Instant::now();
            if self.rebuild_buffer(grid_rows) {
                self.text_revision = self.text_revision.wrapping_add(1);
            }
            let rebuild_elapsed = rebuild_start.elapsed();
            if rebuild_elapsed >= Duration::from_millis(8)
                && slow_log_due(&mut self.last_slow_log, Instant::now())
            {
                log::debug!("slow text rebuild {rebuild_elapsed:?}");
            }
            self.last_grid_version = grid_version;
            rebuild_elapsed
        } else {
            Duration::ZERO
        };

        let tab_count = scroll.tab_titles.len();
        let overlay_start = Instant::now();
        let overlay_timing = crate::perf::measure(crate::perf::Metric::Overlay);
        let overlay::OverlayOutput {
            scroll_to,
            selected_tab,
            close_tab,
            new_tab,
            search,
            paint_jobs,
            screen_descriptor,
            textures_delta,
            egui_repaint_delay,
        } = self.paint_overlay(scroll);
        drop(overlay_timing);
        let overlay_elapsed = overlay_start.elapsed();

        let mut out = RenderOutput {
            presented: false,
            retry_after: None,
            scroll_to,
            selected_tab,
            close_tab,
            new_tab,
            search,
            egui_repaint_delay,
        };

        // egui input/actions/platform output still progress on skipped frames.
        // Apply deltas now; the common boundary flushes them on every returned
        // failure and frees textures only after that submission.
        if !frame_uploads.update_textures(self, textures_delta) {
            out.egui_repaint_delay = Duration::MAX;
            return Ok(out);
        }

        let acquire_start = Instant::now();
        let acquire_timing = crate::perf::measure(crate::perf::Metric::Acquire);
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Timeout => {
                out.retry_after = Some(surface_retry_delay(&mut self.surface_failures));
                crate::perf::add(crate::perf::Metric::Skipped, 1);
                return Ok(out);
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                // Visibility/restoration events wake us; do not poll an
                // explicitly occluded surface with an animation timer.
                crate::perf::add(crate::perf::Metric::Skipped, 1);
                return Ok(out);
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.prepared_text = None;
                if !self.device_lost.load(Ordering::Relaxed) {
                    self.surface.configure(&self.device, &self.config);
                    out.retry_after = Some(surface_retry_delay(&mut self.surface_failures));
                } else {
                    out.egui_repaint_delay = Duration::MAX;
                }
                crate::perf::add(crate::perf::Metric::Skipped, 1);
                return Ok(out);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                crate::perf::add(crate::perf::Metric::Skipped, 1);
                return Ok(out);
            }
        };
        self.surface_failures = 0;
        drop(acquire_timing);
        // Acquire is timed separately: with queued vsync it blocks for the
        // previous frame's blank, which dwarfs real CPU work.
        let acquire_elapsed = acquire_start.elapsed();
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let bg_start = Instant::now();
        let bg_timing = crate::perf::measure(crate::perf::Metric::Background);
        let Some(vert_count) = frame_uploads.upload(self, |renderer| {
            renderer.viewport.update(
                &renderer.queue,
                Resolution {
                    width: renderer.width,
                    height: renderer.height,
                },
            );
            renderer.paint_bg(grid_rows, &cursor, &highlights, tab_count)
        }) else {
            out.egui_repaint_delay = Duration::MAX;
            return Ok(out);
        };
        drop(bg_timing);
        let bg_elapsed = bg_start.elapsed();

        let top = self.tab_bar_px(tab_count);
        let key = PreparedTextKey {
            revision: self.text_revision,
            width: self.width,
            height: self.height,
            top: top.to_bits(),
            default_color: self.theme.foreground.as_glyphon_color(),
        };
        let prepare_elapsed = if self.prepared_text != Some(key) {
            // This atlas is private to this one TextRenderer. Keep the last
            // preparation's glyphs pinned for every reuse frame; only unpin
            // immediately before replacing ALL instances. No atlas writes or
            // allocation may be introduced on the reuse path. glyphon 0.12's
            // trim clears in-use tracking, it does not free texture storage.
            // A failed prepare may have mutated both atlas and instances:
            // never fall back to the previously prepared key after an error.
            self.prepared_text = None;
            self.atlas.trim();
            let prepare_start = Instant::now();
            let _timing = crate::perf::measure(crate::perf::Metric::Prepare);
            let Some(prepared) = frame_uploads.upload(self, |renderer| {
                renderer.text_renderer.prepare(
                    &renderer.device,
                    &renderer.queue,
                    &mut renderer.font_system,
                    &mut renderer.atlas,
                    &renderer.viewport,
                    [TextArea {
                        buffer: &renderer.buffer,
                        left: 0.0,
                        top,
                        scale: 1.0,
                        bounds: TextBounds {
                            left: 0,
                            top: 0,
                            right: renderer.width as i32,
                            bottom: renderer.height as i32,
                        },
                        default_color: key.default_color,
                        custom_glyphs: &[],
                    }],
                    &mut renderer.swash_cache,
                )
            }) else {
                out.egui_repaint_delay = Duration::MAX;
                return Ok(out);
            };
            prepared?;
            self.prepared_text = Some(key);
            prepare_start.elapsed()
        } else {
            crate::perf::add(crate::perf::Metric::PrepareReuse, 1);
            Duration::ZERO
        };

        let encode_start = Instant::now();
        let encode_timing = crate::perf::measure(crate::perf::Metric::EncodeSubmit);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("cometty encoder"),
            });
        let Some(callback_commands) = frame_uploads.upload(self, |renderer| {
            renderer.egui_renderer.update_buffers(
                &renderer.device,
                &renderer.queue,
                &mut encoder,
                &paint_jobs,
                &screen_descriptor,
            )
        }) else {
            out.egui_repaint_delay = Duration::MAX;
            return Ok(out);
        };
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
        if !frame_uploads.submit(
            self,
            callback_commands.into_iter().chain([encoder.finish()]),
        ) {
            out.egui_repaint_delay = Duration::MAX;
            return Ok(out);
        }
        drop(encode_timing);
        let encode_elapsed = encode_start.elapsed();
        // Timed before `present`: presenting blocks on vsync by design
        // (~16ms on 60Hz), so including it would flag every healthy frame.
        let submit_elapsed = render_start.elapsed();
        if submit_elapsed >= Duration::from_millis(12)
            && slow_log_due(&mut self.last_slow_log, Instant::now())
        {
            log::debug!(
                "slow frame submit={submit_elapsed:?} rebuild={rebuild_elapsed:?} \
                 bg={bg_elapsed:?} overlay={overlay_elapsed:?} \
                 prepare={prepare_elapsed:?} encode={encode_elapsed:?} \
                 acquire={acquire_elapsed:?}"
            );
        }
        {
            let _timing = crate::perf::measure(crate::perf::Metric::Present);
            self.queue.present(frame);
        }
        out.presented = true;
        crate::perf::add(crate::perf::Metric::Presented, 1);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Cell;

    #[test]
    fn prepared_text_key_ignores_grid_counters_and_tracks_baked_inputs() {
        let key = PreparedTextKey {
            revision: 1,
            width: 800,
            height: 600,
            top: 38.0_f32.to_bits(),
            default_color: glyphon::Color::rgb(255, 255, 255),
        };
        let prepared = Some(key);
        // Cursor/selection/search/link/chrome changes alter none of these
        // inputs. Even an invalidated grid can keep all row text hashes.
        let mut rows = vec![test_cells("same shaped text")];
        let mut hashes = vec![text::row_text_hash(&rows[0])];
        rows[0][0].underline = crate::grid::UnderlineStyle::Double;
        rows[0][1].hyperlink = Some(3);
        assert!(text::diff_rows(&mut hashes, &rows).unwrap().is_empty());
        assert_eq!(prepared, Some(key));
        for changed in [
            PreparedTextKey { revision: 2, ..key },
            PreparedTextKey { width: 801, ..key },
            PreparedTextKey { height: 601, ..key },
            PreparedTextKey {
                top: 0.0_f32.to_bits(),
                ..key
            },
            PreparedTextKey {
                default_color: glyphon::Color::rgb(1, 2, 3),
                ..key
            },
        ] {
            assert_ne!(prepared, Some(changed));
        }
        // A new tab with the same numeric grid counter but different text
        // must replace lines before key construction, advancing the revision.
        rows[0][0].ch = 'X';
        assert_eq!(text::diff_rows(&mut hashes, &rows), Some(vec![0]));
    }

    #[test]
    fn surface_retries_back_off_without_a_blink_timer() {
        let mut failures = 0;
        for ms in [16, 32, 64, 128, 256, 512, 1000, 1000] {
            assert_eq!(
                surface_retry_delay(&mut failures),
                Duration::from_millis(ms)
            );
        }
        for _ in 0..300 {
            assert_eq!(surface_retry_delay(&mut failures), Duration::from_secs(1));
        }
        failures = 0; // successful acquisition re-arms the fast first retry
        assert_eq!(
            surface_retry_delay(&mut failures),
            Duration::from_millis(16)
        );
    }

    fn test_cells(text: &str) -> Vec<Cell> {
        text.chars()
            .map(|ch| Cell {
                ch,
                ..Default::default()
            })
            .collect()
    }

    fn production_width_for_rows(rows: &[Vec<Cell>]) -> (f32, f32) {
        let font = crate::config::FontConfig::default();
        let (font_size, line_height, cell_width) = scaled_metrics(16.0, 1.0, &font);
        let mut font_system = FontSystem::new();
        let mut cache = super::text::AdvanceCache::new();
        let theme = Theme::default();
        let family = super::text::font_family(&font.family);
        let features = super::text::terminal_font_features();
        let mut ctx = super::text::NormCtx {
            cell_width,
            font_size,
            line_height,
            family,
            features,
            font_system: &mut font_system,
            cache: &mut cache,
        };
        let lines = super::text::build_buffer_lines(rows, &theme, &mut ctx);
        let metrics = Metrics::new(font_size, line_height);
        let mut buffer = Buffer::new(&mut font_system, metrics);
        buffer.set_wrap(Wrap::None);
        buffer.set_size(Some(800.0), Some(600.0));
        buffer.set_monospace_width(None);
        buffer.lines.clear();
        for line in lines {
            buffer.lines.push(line);
        }
        let n = buffer.lines.len();
        for i in 0..n {
            buffer.line_layout(&mut font_system, i);
        }
        let w = buffer
            .line_layout(&mut font_system, 0)
            .and_then(|ls| ls.first().map(|l| l.w))
            .unwrap_or(0.0);
        (w, cell_width)
    }

    fn production_width_for_text(text: &str) -> (f32, f32) {
        production_width_for_rows(&[test_cells(text)])
    }

    fn build_lines_for_text(
        font_system: &mut FontSystem,
        cache: &mut super::text::AdvanceCache,
        rows: &[Vec<Cell>],
    ) -> Vec<cosmic_text::BufferLine> {
        let font = crate::config::FontConfig::default();
        let (font_size, line_height, cell_width) = scaled_metrics(16.0, 1.0, &font);
        let theme = Theme::default();
        let family = super::text::font_family(&font.family);
        let features = super::text::terminal_font_features();
        let mut ctx = super::text::NormCtx {
            cell_width,
            font_size,
            line_height,
            family,
            features,
            font_system,
            cache,
        };
        super::text::build_buffer_lines(rows, &theme, &mut ctx)
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
        let mut cache = super::text::AdvanceCache::new();
        buffer.lines.clear();
        for line in build_lines_for_text(&mut font_system, &mut cache, &rows) {
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
        let mut font_system = FontSystem::new();
        let mut cache = super::text::AdvanceCache::new();
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
        let lines = build_lines_for_text(&mut font_system, &mut cache, &rows);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "中a");
    }

    #[test]
    fn buffer_lines_shape_block_elements_as_spaces() {
        // Block elements are painted by the renderer as cell-sized rects, so
        // their glyphs must not be shaped (font glyphs leave row gaps).
        let mut font_system = FontSystem::new();
        let mut cache = super::text::AdvanceCache::new();
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
        let lines = build_lines_for_text(&mut font_system, &mut cache, &rows);
        // `█` and `▀` become spaces (drawn as rects); the shade keeps its
        // glyph (a dither pattern, nothing solid to draw).
        assert_eq!(lines[0].text(), "  a░");
    }

    #[test]
    fn buffer_lines_include_zwj_cluster_once() {
        let mut font_system = FontSystem::new();
        let mut cache = super::text::AdvanceCache::new();
        let cluster = "👨\u{200D}👩\u{200D}👧";
        let mut chars = cluster.chars();
        let first = chars.next().unwrap();
        let rest: String = chars.collect();
        let lead = Cell {
            ch: first,
            extra: Some(rest),
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
        let lines = build_lines_for_text(&mut font_system, &mut cache, &rows);
        assert_eq!(lines[0].text(), cluster);
    }

    #[test]
    fn row_text_hash_tracks_shaping_inputs_only() {
        use super::text::row_text_hash;
        let base = test_cells("hello");
        let h = row_text_hash(&base);
        // Text, weight, style, and effective-fg changes invalidate.
        assert_ne!(row_text_hash(&test_cells("hellp")), h);
        let mut bold = base.clone();
        bold[0].bold = true;
        assert_ne!(row_text_hash(&bold), h);
        let mut italic = base.clone();
        italic[0].italic = true;
        assert_ne!(row_text_hash(&italic), h);
        let mut fg = base.clone();
        fg[0].fg = crate::theme::Rgb { r: 1, g: 2, b: 3 };
        assert_ne!(row_text_hash(&fg), h);
        // Inverse swaps the effective foreground.
        let mut inverse = base.clone();
        inverse[0].inverse = true;
        assert_ne!(row_text_hash(&inverse), h);
        // Decoration-only changes paint via bg quads from the live grid,
        // never text spans: the layout stays valid.
        let mut deco = base.clone();
        deco[1].underline = crate::grid::UnderlineStyle::Single;
        deco[2].hyperlink = Some(7);
        deco[3].bg = crate::theme::Rgb { r: 1, g: 2, b: 3 };
        assert_eq!(row_text_hash(&deco), h);
        // Wide continuations are skipped by shaping: mutating one must
        // not invalidate the line.
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
        let row = vec![lead, cont];
        let mut row2 = row.clone();
        row2[1].ch = 'X';
        row2[1].bold = true;
        assert_eq!(row_text_hash(&row), row_text_hash(&row2));
    }

    #[test]
    fn diff_rows_reports_changed_indices_once() {
        use super::text::{diff_rows, row_text_hash};
        let rows_a = vec![test_cells("a"), test_cells("b")];
        let mut hashes: Vec<u64> = rows_a.iter().map(|row| row_text_hash(row)).collect();
        // Identical rows: no work.
        assert_eq!(diff_rows(&mut hashes, &rows_a), Some(vec![]));
        // One edit: index reported, hashes updated in place.
        let mut rows_b = rows_a.clone();
        rows_b[1] = test_cells("c");
        assert_eq!(diff_rows(&mut hashes, &rows_b), Some(vec![1]));
        assert_eq!(diff_rows(&mut hashes, &rows_b), Some(vec![]));
        // Row-count change: full rebuild required.
        let mut rows_c = rows_b.clone();
        rows_c.push(test_cells("d"));
        assert_eq!(diff_rows(&mut hashes, &rows_c), None);
    }

    /// Shape `rows` into a laid-out buffer (production path, headless).
    fn shape_buffer(
        font_system: &mut FontSystem,
        cache: &mut super::text::AdvanceCache,
        rows: &[Vec<Cell>],
    ) -> Buffer {
        let font = crate::config::FontConfig::default();
        let (font_size, line_height, _) = scaled_metrics(16.0, 1.0, &font);
        let metrics = Metrics::new(font_size, line_height);
        let mut buffer = Buffer::new(font_system, metrics);
        buffer.set_wrap(Wrap::None);
        buffer.set_size(Some(800.0), Some(600.0));
        buffer.lines.clear();
        for line in build_lines_for_text(font_system, cache, rows) {
            buffer.lines.push(line);
        }
        let n = buffer.lines.len();
        for i in 0..n {
            buffer.line_layout(font_system, i);
        }
        buffer
    }

    fn layout_fingerprint(buffer: &mut Buffer, font_system: &mut FontSystem) -> Vec<String> {
        (0..buffer.lines.len())
            .map(|i| format!("{:?}", buffer.line_layout(font_system, i)))
            .collect()
    }

    #[test]
    fn partial_relayout_matches_full_rebuild() {
        // Two consecutive frames where typing edited line 2 and toggled
        // bold on line 4: replacing + laying out only the diffed lines
        // must shape identically to a full rebuild.
        let rows_a: Vec<Vec<Cell>> = (0..6)
            .map(|r| test_cells(&format!("scrollback line {r} 0123456789")))
            .collect();
        let mut rows_b = rows_a.clone();
        rows_b[2] = test_cells("scrollback line 2 0123456789!");
        rows_b[4] = rows_a[4]
            .iter()
            .map(|c| Cell {
                bold: true,
                ..c.clone()
            })
            .collect();
        let mut font_system = FontSystem::new();
        let mut cache = super::text::AdvanceCache::new();
        // Partial path, mirroring `rebuild_buffer`: full build of A, then
        // single-line rebuilds for the diff to B.
        let mut partial = shape_buffer(&mut font_system, &mut cache, &rows_a);
        let mut hashes: Vec<u64> = rows_a
            .iter()
            .map(|row| super::text::row_text_hash(row))
            .collect();
        let changed = super::text::diff_rows(&mut hashes, &rows_b).expect("same row count");
        assert_eq!(changed, vec![2, 4]);
        let font = crate::config::FontConfig::default();
        let (font_size, line_height, cell_width) = scaled_metrics(16.0, 1.0, &font);
        let theme = Theme::default();
        let family = super::text::font_family(&font.family);
        let features = super::text::terminal_font_features();
        for i in changed {
            let line = {
                let mut ctx = super::text::NormCtx {
                    cell_width,
                    font_size,
                    line_height,
                    family,
                    features: features.clone(),
                    font_system: &mut font_system,
                    cache: &mut cache,
                };
                super::text::build_buffer_line(&rows_b[i], &theme, &mut ctx)
            };
            partial.lines[i] = line;
            partial.line_layout(&mut font_system, i);
        }
        // Fresh full build of B.
        let mut full = shape_buffer(&mut font_system, &mut cache, &rows_b);
        assert_eq!(
            layout_fingerprint(&mut partial, &mut font_system),
            layout_fingerprint(&mut full, &mut font_system),
        );
    }

    #[test]
    fn slow_log_cooldown_suppresses_repeats() {
        use std::time::{Duration, Instant};
        let mut last = None;
        let t0 = Instant::now();
        // First slow frame logs.
        assert!(slow_log_due(&mut last, t0));
        // Immediate repeats are suppressed.
        assert!(!slow_log_due(&mut last, t0));
        assert!(!slow_log_due(&mut last, t0 + Duration::from_secs(1)));
        // After the cooldown a slow frame logs again.
        assert!(slow_log_due(&mut last, t0 + Duration::from_secs(2)));
    }

    #[test]
    fn present_mode_prefers_mailbox_with_fallback() {
        assert_eq!(
            pick_present_mode(&[wgpu::PresentMode::Fifo, wgpu::PresentMode::Mailbox]),
            wgpu::PresentMode::Mailbox
        );
        assert_eq!(
            pick_present_mode(&[wgpu::PresentMode::Fifo]),
            wgpu::PresentMode::AutoVsync
        );
        assert_eq!(pick_present_mode(&[]), wgpu::PresentMode::AutoVsync);
    }

    #[test]
    fn terminal_ime_reasserted_after_egui_text_focus_closes() {
        // Startup: the terminal takes IME.
        assert_eq!(terminal_ime_step(false, false, false), (true, true));
        // Steady state: no repeated `set_ime_allowed(true)` calls.
        assert_eq!(terminal_ime_step(false, false, true), (false, true));
        // Find bar / settings field focused: egui owns IME; re-arm the
        // terminal hand-off.
        assert_eq!(terminal_ime_step(false, true, true), (false, false));
        // Field closed: egui has just disabled IME. The terminal must take
        // it back or dead-key quotes stop working for the rest of the
        // session (the reported Ctrl+F -> close -> broken quotes bug).
        assert_eq!(terminal_ime_step(false, false, false), (true, true));
        // The settings panel does the same hand-off, even with no focused
        // field, and closing it re-asserts terminal IME.
        assert_eq!(terminal_ime_step(true, false, true), (false, false));
        assert_eq!(terminal_ime_step(false, false, false), (true, true));
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

    #[test]
    fn fixed_cell_normalizes_spinner_frames() {
        // OpenCode-style loading bar: dots vs full squares must occupy the
        // same columns so trailing text doesn't jitter left/right.
        // Note: `\u{2588}` (full block) is rect-painted + shaped as a space (see
        // `blocks.rs`), and trailing spaces trim, so lone blocks collapse;
        // they are covered via the suffixed case below instead.
        let frames = [
            "...",
            "\u{00b7}\u{00b7}\u{00b7}",
            "\u{25a0}\u{25a0}\u{25a0}",
            "\u{25cf}\u{25cf}\u{25cf}",
            "\u{280b}\u{280b}\u{280b}",
        ];
        let mut widths = Vec::new();
        let mut cells = 0.0;
        for f in frames {
            let (w, cell) = production_width_for_text(f);
            widths.push(w);
            cells = cell;
        }
        for w in &widths {
            assert!(
                (*w - 3.0 * cells).abs() < 0.5,
                "frame width {w} != 3 cells ({cells})"
            );
        }
        let first = widths[0];
        for w in &widths[1..] {
            assert!(
                (*w - first).abs() < 0.5,
                "spinner frames differ: {first} vs {w}"
            );
        }
        // Suffix stability: the same trailing text after different prefixes
        // must start at the same x (i.e. total widths match).
        let (a, cell) = production_width_for_text("\u{00b7}\u{00b7}\u{00b7}hi");
        let (b, _) = production_width_for_text("\u{25a0}\u{25a0}\u{25a0}hi");
        let (c, _) = production_width_for_text("\u{2588}\u{2588}\u{2588}hi");
        assert!(
            (a - b).abs() < 0.5,
            "suffix shifts with spinner frame: {a} vs {b}"
        );
        assert!(
            (a - c).abs() < 0.5,
            "suffix shifts with block frame: {a} vs {c}"
        );
        assert!(
            (a - 5.0 * cell).abs() < 0.5,
            "row width {a} != 5 cells ({cell})"
        );
    }

    #[test]
    fn fixed_cell_keeps_wide_cells_double() {
        // `\u{3042}` (Hiragana, wide) uses a vector fallback with finite advances.
        // (`\u{4e2d}` hits macOS `GB18030Bitmap`, whose advances shape to inf --
        // a pre-existing font issue outside this fix.)
        let lead = Cell {
            ch: '\u{3042}',
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
        let rows = vec![vec![lead, cont]];
        let (w, cell) = production_width_for_rows(&rows);
        assert!(
            (w - 2.0 * cell).abs() < 0.5,
            "wide cell width {w} != 2 cells ({cell})"
        );
    }

    #[test]
    fn fixed_cell_disables_ligatures_across_columns() {
        // `...` must stay three cells, never ligate to a single ellipsis.
        let (w, cell) = production_width_for_text("...");
        assert!(
            (w - 3.0 * cell).abs() < 0.5,
            "ligature collapsed columns: {w} != 3 cells ({cell})"
        );
        let mut font_system = FontSystem::new();
        let mut cache = super::text::AdvanceCache::new();
        let rows = vec![test_cells("...")];
        let lines = build_lines_for_text(&mut font_system, &mut cache, &rows);
        assert_eq!(lines[0].text(), "...");
    }
}
