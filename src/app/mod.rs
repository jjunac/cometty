//! Application state + event dispatch (split from `main.rs` god function).
//!
//! `App` owns the window, renderer, terminal, and PTY session.
//! Per-event bodies live in submodules; this file only holds state,
//! construction, and re-exports.

pub mod clipboard;
pub mod geometry;
pub mod keyboard;
pub mod mouse;
pub mod pty_io;
pub mod redraw;
pub mod selection_view;
pub mod tab;

pub use geometry::compute_grid_size;
pub use tab::Tab;

use std::sync::Arc;
use std::time::Instant;

use winit::keyboard::ModifiersState;
use winit::window::Window;

use crate::config::Config;
use crate::renderer::Renderer;
use crate::theme::Theme;

#[derive(Debug)]
pub enum UserEvent {
    PtyAvailable,
}

pub struct App {
    pub(crate) window: Option<Arc<Window>>,
    pub(crate) renderer: Option<Renderer>,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active: usize,
    pub(crate) proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>>,
    pub(crate) theme: Theme,
    pub(crate) config: Config,
    pub(crate) modifiers: ModifiersState,
    pub(crate) cursor_visible: bool,
    pub(crate) last_blink: Instant,
    pub(crate) pending_resize: Option<(u32, u32)>,
    pub(crate) pending_scale: Option<f32>,
    pub(crate) wheel_accum: f64,
    pub(crate) cursor_pos: Option<(f32, f32)>,
    pub(crate) last_click: Option<(Instant, (usize, usize))>,
    pub(crate) clipboard: Option<arboard::Clipboard>,
    pub(crate) window_title: String,
}

impl App {
    pub fn new(
        proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>>,
        theme: Theme,
        config: Config,
    ) -> Self {
        let window_title = config.window.title.clone();
        Self {
            window: None,
            renderer: None,
            tabs: Vec::new(),
            active: 0,
            proxy,
            theme,
            config,
            modifiers: ModifiersState::empty(),
            cursor_visible: true,
            last_blink: Instant::now(),
            pending_resize: None,
            pending_scale: None,
            wheel_accum: 0.0,
            cursor_pos: None,
            last_click: None,
            clipboard: None,
            window_title,
        }
    }
}
