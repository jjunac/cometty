//! Application state + event dispatch (split from `main.rs` god function).
//!
//! `App` owns the window, renderer, terminal, and PTY session.
//! Per-event bodies live in submodules; this file only holds state,
//! construction, and re-exports.

pub mod clipboard;
pub mod geometry;
pub mod keyboard;
pub mod logs;
pub mod mouse;
pub mod pty_io;
pub mod redraw;
pub mod search;
pub mod selection_view;
pub mod settings;
pub mod tab;

pub use geometry::compute_grid_size;
pub use geometry::is_fullscreen_like;
pub use logs::LogsPanel;
pub use settings::{AppStartup, SettingsPanel};
pub use tab::Tab;

use std::sync::{Arc, Mutex};
use std::time::Instant;

use winit::keyboard::ModifiersState;
use winit::window::Window;

use crate::config::Config;
use crate::logbuf::LogBuffer;
use crate::renderer::Renderer;
use crate::theme::Theme;

#[derive(Debug)]
pub enum UserEvent {
    PtyAvailable,
    MenuEvent(muda::MenuEvent),
    /// A record landed in the in-app log ring (only sent while the log
    /// panel is open, see [`crate::logging::set_wake_enabled`]).
    LogAvailable,
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
    /// Next frame egui asked for (hover fade / animation / tooltip), if
    /// any; `about_to_wait` wakes for it instead of waiting for a blink.
    pub(crate) egui_repaint_at: Option<Instant>,
    pub(crate) pending_resize: Option<(u32, u32)>,
    pub(crate) pending_scale: Option<f32>,
    pub(crate) wheel_accum: f64,
    pub(crate) cursor_pos: Option<(f32, f32)>,
    pub(crate) last_click: Option<(Instant, (usize, usize))>,
    pub(crate) mouse_pressed: u8,
    pub(crate) clipboard: Option<arboard::Clipboard>,
    pub(crate) window_title: String,
    pub(crate) settings: SettingsPanel,
    pub(crate) logs: LogsPanel,
    /// Ring buffer for the log panel; the global logger also holds a clone.
    pub(crate) log_buffer: Arc<Mutex<LogBuffer>>,
    pub(crate) menu: Option<crate::menu::NativeMenu>,
}

impl App {
    pub fn new(
        proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>>,
        theme: Theme,
        config: Config,
        startup: AppStartup,
        log_buffer: Arc<Mutex<LogBuffer>>,
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
            egui_repaint_at: None,
            pending_resize: None,
            pending_scale: None,
            wheel_accum: 0.0,
            cursor_pos: None,
            last_click: None,
            mouse_pressed: 0,
            clipboard: None,
            window_title,
            settings: SettingsPanel::new(startup),
            logs: LogsPanel::default(),
            log_buffer,
            menu: None,
        }
    }

    /// Persist the live config after a panel edit. Failures stay in the
    /// session with a panel toast; the next change retries.
    pub(crate) fn save_config_from_settings(&mut self) {
        let result = match self.settings.custom_config_path.clone() {
            Some(path) => self.config.save_to_path(&path),
            None => self.config.save(),
        };
        match result {
            Ok(()) => {
                log::debug!("saved config to {}", self.settings.config_path_label());
                self.settings.report_saved()
            }
            Err(e) => {
                let target = self.settings.config_path_label();
                log::warn!("failed to save config to {target}: {e:#}");
                self.settings.report_error(format!("save failed: {e:#}"));
            }
        }
    }

    /// Route a settings diff to the live session (theme/font/window/
    /// terminal/chrome). Shell/cwd/term intentionally only affect new tabs.
    pub(crate) fn apply_settings_changes(&mut self, old: &Config) {
        let actions = settings::diff_actions(old, &self.config);
        if !actions.any {
            return;
        }
        // Theme (skipped live while a CLI --theme owns the session) and
        // terminal tuning both flow through the per-tab session config.
        let live_theme = actions.theme && self.settings.cli_theme_override.is_none();
        if live_theme || actions.terminal {
            let theme = if live_theme {
                crate::theme::Theme::from_name(&self.config.theme.name).unwrap_or(self.theme)
            } else {
                self.theme
            };
            if live_theme {
                self.theme = theme;
            }
            for tab in &mut self.tabs {
                tab.terminal.apply_config(theme, &self.config);
            }
        }
        if (actions.theme || actions.font || actions.chrome)
            && let Some(r) = self.renderer.as_mut()
        {
            if live_theme {
                r.set_theme(self.theme);
            }
            r.apply_config(&self.config);
        }
        if actions.font || actions.terminal {
            self.sync_tab_sizes();
        }
        if actions.window_size {
            let (w, h) = (self.config.window.width, self.config.window.height);
            if let Some(window) = self.window.as_ref() {
                use winit::dpi::LogicalSize;
                let _ = window.request_inner_size(LogicalSize::new(w, h));
            }
        }
        // Log filter + ring size apply to subsequent records immediately;
        // the redraw at the end of this function repaints an open panel.
        // Rejected filters (mid-typing) keep the previous one and are
        // surfaced in the panel header, not logged per keystroke.
        if actions.log {
            let _ = crate::logging::set_record_filter(&self.config.log.filter_string());
            self.log_buffer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .set_capacity(self.config.log.buffer_lines);
        }
        self.sync_window_title();
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
}
