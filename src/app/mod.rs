//! Application state + event dispatch (split from `main.rs` god function).
//!
//! `App` owns the window, renderer, terminal, and PTY session.
//! Per-event bodies live in submodules; this file only holds state,
//! construction, and re-exports.

pub mod clipboard;
pub mod geometry;
pub mod keyboard;
pub mod links;
pub mod logs;
pub mod mouse;
pub mod persistence;
pub mod pty_io;
pub mod redraw;
pub mod scheduling;
pub mod search;
pub mod selection_view;
pub mod session;
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
    ConfigSaveAvailable,
}

pub struct App {
    pub(crate) window: Option<Arc<Window>>,
    pub(crate) renderer: Option<Renderer>,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active: usize,
    pub(crate) next_tab_id: u64,
    pub(crate) pty_schedule: scheduling::DrainSchedule,
    pub(crate) last_slow_drain: Option<Instant>,
    pub(crate) proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>>,
    pub(crate) theme: Theme,
    pub(crate) config: Config,
    pub(crate) modifiers: ModifiersState,
    pub(crate) cursor_visible: bool,
    pub(crate) last_blink: Instant,
    /// Next egui frame or transient surface retry, if any; independent of
    /// whether the cursor needs a blink timer.
    pub(crate) egui_repaint_at: Option<Instant>,
    pub(crate) occluded: bool,
    pub(crate) zero_sized: bool,
    /// Work left after a bounded drain, independent of presentation.
    pub(crate) pty_pending: bool,
    pub(crate) pending_resize: Option<(u32, u32)>,
    pub(crate) pending_scale: Option<f32>,
    pub(crate) wheel_accum: f64,
    pub(crate) cursor_pos: Option<(f32, f32)>,
    /// Multi-click chain for word/line select: last press time, view cell,
    /// and count (1 single, 2 word, 3 line). Separate from
    /// `last_titlebar_click` (macOS zoom).
    pub(crate) last_click: Option<mouse::ClickState>,
    /// Last single press on empty macOS titlebar chrome, for double-click
    /// zoom. Separate from grid `last_click` (cell-based word select).
    pub(crate) last_titlebar_click: Option<Instant>,
    pub(crate) mouse_pressed: u8,
    pub(crate) last_mouse_report: Option<mouse::MotionState>,
    /// Last cursor icon pushed to the window (link hover changes it).
    pub(crate) cursor_icon: winit::window::CursorIcon,
    /// A left press opened a link: swallow the matching release so no
    /// spurious release reaches a mouse-reporting app.
    pub(crate) link_press: bool,
    pub(crate) clipboard: Option<arboard::Clipboard>,
    pub(crate) window_title: String,
    pub(crate) settings: SettingsPanel,
    pub(crate) config_saver: persistence::ConfigSaver,
    /// Session snapshot ownership + writer (inert unless `main` opens it).
    pub(crate) session: session::SessionState,
    /// A tab was spawned this run: an empty tab list at exit then means
    /// "every tab was closed", not "startup failed".
    pub(crate) spawned_any: bool,
    pub(crate) logs: LogsPanel,
    /// Ring buffer for the log panel; the global logger also holds a clone.
    pub(crate) log_buffer: Arc<Mutex<LogBuffer>>,
    pub(crate) menu: Option<crate::menu::NativeMenu>,
    /// Version-gated grid snapshot: idle blink frames reuse the last clone
    /// instead of re-cloning `rows x cols` Cells every frame.
    pub(crate) snapshot_rows: Vec<Vec<crate::grid::Cell>>,
    pub(crate) snapshot_version: u64,
    pub(crate) snapshot_tab: Option<u64>,
    pub(crate) snapshot_keys: Vec<(u64, u64, u64)>,
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
            next_tab_id: 0,
            pty_schedule: scheduling::DrainSchedule::default(),
            last_slow_drain: None,
            proxy,
            theme,
            config,
            modifiers: ModifiersState::empty(),
            cursor_visible: true,
            last_blink: Instant::now(),
            egui_repaint_at: None,
            occluded: false,
            zero_sized: false,
            pty_pending: false,
            pending_resize: None,
            pending_scale: None,
            wheel_accum: 0.0,
            cursor_pos: None,
            last_click: None,
            last_titlebar_click: None,
            mouse_pressed: 0,
            last_mouse_report: None,
            cursor_icon: winit::window::CursorIcon::Default,
            link_press: false,
            clipboard: None,
            window_title,
            settings: SettingsPanel::new(startup),
            config_saver: persistence::ConfigSaver::default(),
            session: session::SessionState::default(),
            spawned_any: false,
            logs: LogsPanel::default(),
            log_buffer,
            menu: None,
            snapshot_rows: Vec::new(),
            snapshot_version: u64::MAX,
            snapshot_tab: None,
            snapshot_keys: Vec::new(),
        }
    }

    pub(crate) fn visuals_suspended(&self) -> bool {
        self.occluded
            || self.zero_sized
            || self
                .window
                .as_ref()
                .is_some_and(|w| w.is_minimized() == Some(true))
    }

    /// Persist the live config after a panel edit. Failures stay in the
    /// session with a panel toast; the next change retries.
    pub(crate) fn save_config_from_settings(&mut self) {
        self.config_saver.changed(Instant::now());
    }

    /// Successful input clears a stale backpressure notice. Only a key gets
    /// a latency stamp; paste is intentionally not treated as an echo probe.
    pub(crate) fn accept_input_feedback(&mut self, key: bool) {
        let cleared = self.active_tab_mut().is_some_and(|tab| {
            if key {
                tab.last_key_write = Some(Instant::now());
            }
            tab.input_notice.take().is_some()
        });
        if cleared && let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    pub(crate) fn service_config_saves(&mut self, now: Instant, force: bool) {
        let proxy = &self.proxy;
        self.config_saver.dispatch(
            now,
            force,
            &self.config,
            self.settings.effective_save_path(),
            || {
                let proxy = proxy.clone();
                move || {
                    if let Some(p) = &proxy {
                        let _ = p.send_event(UserEvent::ConfigSaveAvailable);
                    }
                }
            },
        );
        if let Some(result) = self.config_saver.take_result() {
            self.report_config_save(result);
        }
    }

    pub(crate) fn finish_config_saves(&mut self) {
        self.service_config_saves(Instant::now(), true);
        if let Some(result) = self.config_saver.finish() {
            self.report_config_save(result);
        }
    }

    fn report_config_save(&mut self, result: Result<(), String>) {
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
        if old.session != self.config.session {
            self.session_changed();
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
                let dropped = tab.terminal.grid().history_dropped();
                tab.terminal.apply_config(theme, &self.config);
                tab.rebase_selection(
                    tab.terminal
                        .grid()
                        .history_dropped()
                        .saturating_sub(dropped) as usize,
                );
                if actions.terminal {
                    tab.pty.apply_terminal_config(&self.config.terminal);
                }
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
        if actions.font || actions.terminal || old.tabbar.height != self.config.tabbar.height {
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
