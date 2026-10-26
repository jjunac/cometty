mod app;
mod config;
mod grid;
mod icon;
mod input;
mod links;
mod logbuf;
mod logging;
mod menu;
mod perf;
#[cfg(test)]
mod perf_bench;
mod procinfo;
mod pty;
mod renderer;
mod scrollbar;
mod search;
mod selection;
mod session;
mod tabbar;
mod term;
mod theme;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use app::{App, AppStartup, UserEvent};
use config::Config;
use logbuf::LogBuffer;
use renderer::Renderer;
use theme::Theme;

/// CLI overrides layered on top of the TOML file.
/// Returns `(theme, config_path_override)` or `None` for `--help`/`--list-themes`.
fn resolve_cli() -> anyhow::Result<Option<(Option<Theme>, Option<String>)>> {
    match parse_cli_args(&std::env::args().skip(1).collect::<Vec<_>>())? {
        CliParsed::Run {
            theme, config_path, ..
        } => Ok(Some((theme, config_path))),
        CliParsed::ListThemes => {
            for t in Theme::all() {
                println!("{}", t.name);
            }
            Ok(None)
        }
        CliParsed::Help => {
            println!("cometty [--theme NAME] [--config PATH] [--list-themes]");
            println!("Config file: $HOME/.config/cometty/config.toml");
            println!("Settings panel: Ctrl+, / Cmd+, (or Cometty > Settings in the menu bar)");
            Ok(None)
        }
    }
}

/// Parsed CLI form: pure seam over `std::env::args` so argument handling
/// is unit-testable without spawning the GUI. Printing stays in
/// `resolve_cli`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CliParsed {
    Run {
        theme: Option<Theme>,
        config_path: Option<String>,
    },
    ListThemes,
    Help,
}

pub(crate) fn parse_cli_args(args: &[String]) -> anyhow::Result<CliParsed> {
    let mut theme_override: Option<Theme> = None;
    let mut config_path: Option<String> = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--list-themes" {
            return Ok(CliParsed::ListThemes);
        } else if arg == "--theme" {
            let name = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--theme requires a value (try --list-themes)"))?;
            theme_override =
                Some(Theme::from_name(name).ok_or_else(|| {
                    anyhow::anyhow!("unknown theme {name:?} (try --list-themes)")
                })?);
        } else if let Some(name) = arg.strip_prefix("--theme=") {
            theme_override =
                Some(Theme::from_name(name).ok_or_else(|| {
                    anyhow::anyhow!("unknown theme {name:?} (try --list-themes)")
                })?);
        } else if arg == "--config" {
            let path = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--config requires a value"))?;
            config_path = Some(path.clone());
        } else if let Some(path) = arg.strip_prefix("--config=") {
            config_path = Some(path.to_string());
        } else if arg == "-h" || arg == "--help" {
            return Ok(CliParsed::Help);
        } else {
            return Err(anyhow::anyhow!("unknown argument {arg:?} (try --help)"));
        }
    }
    Ok(CliParsed::Run {
        theme: theme_override,
        config_path,
    })
}

impl ApplicationHandler<UserEvent> for App {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, _cause: winit::event::StartCause) {
        self.pty_schedule.begin_turn();
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // Native menu: installed here (not in `main`) because winit sets up
        // its own default menu at event-loop startup, which would clobber
        // anything installed earlier. This override sticks: winit only
        // installs its default once. Main thread here, as required by muda.
        // Non-fatal: without the menu the Ctrl+, / Cmd+, shortcut still
        // opens settings.
        if self.menu.is_none() {
            match menu::NativeMenu::new() {
                Ok(native) => {
                    native.install();
                    self.menu = Some(native);
                }
                Err(e) => {
                    log::warn!("native menu unavailable ({e}); settings shortcut still works");
                }
            }
        }
        // macOS ignores `with_window_icon`; set the Dock tile directly so
        // unbundled runs (`cargo run`) still show the logo. Main thread
        // here (same requirement as the native menu above).
        #[cfg(target_os = "macos")]
        icon::set_dock_icon();
        let restored = self.session.take_restored();
        let saved_window = restored.as_ref().and_then(|s| s.window.as_ref());
        let (width, height) = saved_window.map_or(
            (
                self.config.window.width as f64,
                self.config.window.height as f64,
            ),
            |w| (w.width, w.height),
        );
        let mut attrs = Window::default_attributes()
            .with_title(self.config.window.title.clone())
            .with_window_icon(icon::load_window_icon())
            .with_inner_size(winit::dpi::LogicalSize::new(width, height));
        // A saved position on a since-disconnected display would strand the
        // window off-screen: only reuse it when a monitor still shows it.
        if let Some(position) = saved_window.and_then(|w| w.position) {
            let monitors: Vec<session::MonitorRect> = event_loop
                .available_monitors()
                .map(|m| {
                    let (p, s) = (m.position(), m.size());
                    (p.x, p.y, s.width, s.height)
                })
                .collect();
            if session::position_on_monitor(position, &monitors) {
                attrs =
                    attrs.with_position(winit::dpi::PhysicalPosition::new(position.0, position.1));
            }
        }
        // Brave-style merged titlebar on macOS: transparent titlebar with
        // fullsize content view. The tab strip paints into the titlebar
        // area; the OS keeps drawing the traffic lights on top. The title
        // text itself stays hidden (tabs already show OSC titles).
        #[cfg(target_os = "macos")]
        let attrs = {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attrs
                .with_title_hidden(true)
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true)
        };
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("failed to create window: {e:#}");
                event_loop.exit();
                return;
            }
        };
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        let scale = window.scale_factor() as f32;
        let renderer = match Renderer::new(window.clone(), w, h, scale, self.theme, &self.config) {
            Ok(r) => r,
            Err(e) => {
                log::error!("failed to init renderer: {e:#}");
                event_loop.exit();
                return;
            }
        };
        // Size shells for the final tab count (a single tab has no bar),
        // so restored tabs don't each start with a resize.
        let tab_count = restored.as_ref().map_or(1, |s| s.tabs.len());
        let term_h = app::tab::term_height_px(h, scale, tab_count, &self.config.tabbar);
        let (cols, rows) = app::compute_grid_size(
            w,
            term_h,
            renderer.cell_width,
            renderer.line_height,
            &self.config.terminal,
        );

        self.window = Some(window);
        self.renderer = Some(renderer);
        self.last_blink = Instant::now();
        if !self.spawn_initial_tabs(cols, rows, restored) {
            event_loop.exit();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::PtyAvailable => {
                perf::add(perf::Metric::Wake, 1);
                if self.drain_pty() {
                    event_loop.exit();
                }
            }
            // A record landed in the log ring: repaint only if the panel
            // is on screen (the logger doesn't wake us otherwise).
            UserEvent::LogAvailable => {
                logging::acknowledge_wake();
                if self.logs.open
                    && let Some(w) = self.window.as_ref()
                {
                    w.request_redraw();
                }
            }
            UserEvent::ConfigSaveAvailable => {
                self.service_config_saves(Instant::now(), false);
                if self.settings.open
                    && let Some(w) = self.window.as_ref()
                {
                    w.request_redraw();
                }
            }
            // Native menu bar: Settings and Logs open their panel
            // (open-only, not a toggle, matching platform convention;
            // Esc / x still close). Both accelerators are consumed by the
            // OS menu on macOS, so they don't double-fire with the
            // keyboard toggles.
            UserEvent::MenuEvent(event) => {
                let Some(menu) = self.menu.as_ref() else {
                    return;
                };
                let opens_settings = event.id == *menu.settings_id();
                let opens_logs = event.id == *menu.logs_id();
                if !opens_settings && !opens_logs {
                    return;
                }
                if opens_settings {
                    self.settings.open = true;
                    self.settings.notice = None;
                } else {
                    self.logs.show();
                }
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let win_ok = self.window.as_ref().is_some_and(|w| w.id() == window_id);
        if !win_ok {
            return;
        }
        // egui chrome first: scrollbar hover/drag consumes pointer events.
        // Exception: Tab while the terminal owns the keyboard.
        // `egui-winit` marks every Tab consumed (focus navigation), so the
        // first Tab would strand focus in the tab strip and starve the PTY
        // of Tab/arrows/Enter until the next grid click. A panel text
        // field keeps Tab (focus navigation inside the panel).
        let skip_egui_for_tab = matches!(
            &event,
            WindowEvent::KeyboardInput { event, .. }
            if event.logical_key == Key::Named(NamedKey::Tab)
        ) && self.terminal_owns_keyboard();
        let response = match (self.renderer.as_mut(), self.window.as_ref()) {
            (Some(r), Some(w)) if !skip_egui_for_tab => r.on_window_event(w, &event),
            _ => egui_winit::EventResponse::default(),
        };
        let egui_consumed = response.consumed;
        // egui's own repaint request for this event (hover, wheel, typing,
        // IME, …): the loop is otherwise reactive (PTY output / blink), so
        // without it egui only stepped forward on the next blink tick.
        // `RedrawRequested` always asks for a repaint — honoring it here
        // would loop forever.
        if response.repaint
            && !matches!(event, WindowEvent::RedrawRequested)
            && let Some(w) = self.window.as_ref()
        {
            w.request_redraw();
        }
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::Moved(_) => {
                self.session_changed();
            }
            WindowEvent::Resized(size) => {
                self.session_changed();
                self.zero_sized = size.width == 0 || size.height == 0;
                let scale = self
                    .window
                    .as_ref()
                    .map(|w| w.scale_factor() as f32)
                    .unwrap_or(1.0);
                self.pending_resize = Some((size.width, size.height));
                self.pending_scale = Some(scale);
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(w) = self.window.as_ref() {
                    let s = w.inner_size();
                    let scale = w.scale_factor() as f32;
                    self.pending_resize = Some((s.width, s.height));
                    self.pending_scale = Some(scale);
                    w.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                let hints_were = self.hints_active();
                self.modifiers = m.state();
                // Pressing/releasing the link modifier toggles hint
                // underlines and the hover cursor.
                if self.hints_active() != hints_were {
                    self.update_link_cursor();
                    if let Some(w) = self.window.as_ref() {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.on_keyboard(&event, egui_consumed);
            }
            // Dead-key / IME commits (macOS US-Intl `~ ' " \``) never
            // appear as `KeyboardInput`; egui already saw the event above,
            // `on_ime` forwards to the PTY only when no text field owns it.
            WindowEvent::Ime(ime) => {
                self.on_ime(&ime);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.on_cursor_moved(position.x as f32, position.y as f32);
            }
            WindowEvent::CursorLeft { .. } => {
                self.on_cursor_left();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                // NB: egui's `consumed` flag claims left-presses across the
                // whole window, so selection uses an explicit scrollbar
                // hit-test instead; releases always finalize a drag.
                // Exception: with the settings panel open, presses over egui
                // chrome belong to the panel (see `on_mouse_input`).
                self.on_mouse_input(button, state, egui_consumed);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Wheel over the egui scrollbar still scrolls the same view.
                // Exception: with the settings panel open, wheel over the
                // panel scrolls the panel (see `on_wheel`).
                self.on_wheel(delta, egui_consumed);
            }
            WindowEvent::RedrawRequested => {
                self.on_redraw();
                if self.tabs.is_empty() {
                    event_loop.exit();
                }
            }
            WindowEvent::Focused(focused) => {
                self.on_focused(focused);
            }
            WindowEvent::Occluded(occluded) => {
                self.occluded = occluded;
                if !occluded && let Some(w) = self.window.as_ref() {
                    let size = w.inner_size();
                    self.zero_sized = size.width == 0 || size.height == 0;
                    self.pending_resize = Some((size.width, size.height));
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Fallback drain in case a wake was coalesced. When the last
        // tab's shell has exited there is nothing left to show.
        if self.drain_pty() {
            event_loop.exit();
            return;
        }

        let now = Instant::now();
        self.service_config_saves(now, !self.settings.open);
        self.service_session_saves(now);
        let suspended = self.visuals_suspended();
        let (sync_repaint, sync_at) = self.active_tab_mut().map_or((false, None), |tab| {
            let repaint = tab.sync.release_due(now);
            (repaint, tab.sync.deadline())
        });
        if sync_repaint
            && !suspended
            && let Some(w) = self.window.as_ref()
        {
            w.request_redraw();
        }
        let blinking = self.active_tab().is_some_and(|tab| {
            app::scheduling::cursor_blinks(
                suspended || sync_at.is_some(),
                tab.terminal.cursor_visible(),
                tab.terminal.cursor_style().blinking,
                tab.terminal.scroll_offset(),
            )
        });
        let (repaint, mut next) = app::scheduling::blink_step(
            now,
            &mut self.last_blink,
            &mut self.cursor_visible,
            blinking,
            Duration::from_millis(self.config.cursor.blink_ms.max(1)),
        );
        if repaint && let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        if !suspended {
            next = app::scheduling::earlier(next, sync_at);
        }
        if !suspended && sync_at.is_none() {
            let search_at = self
                .active_tab()
                .and_then(|t| t.search.rescan_at(t.terminal.grid(), now));
            for deadline in [self.egui_repaint_at, search_at].into_iter().flatten() {
                if deadline <= now {
                    if self.egui_repaint_at == Some(deadline) {
                        self.egui_repaint_at = None;
                    }
                    if let Some(w) = self.window.as_ref() {
                        w.request_redraw();
                    }
                } else {
                    next = app::scheduling::earlier(next, Some(deadline));
                }
            }
        }
        // Parsing progress must not depend on visual frames, selection
        // state, cursor blinks, or whether the window is visible.
        if self.pty_pending {
            next = app::scheduling::earlier(next, Some(now));
        }
        next = app::scheduling::earlier(next, self.config_saver.deadline());
        next = app::scheduling::earlier(next, self.session.deadline());
        perf::report_due(now);
        event_loop.set_control_flow(next.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }

    /// Every exit path ends here, including macOS Cmd+Q, where AppKit
    /// terminates the process right after this callback so `run_app` never
    /// returns: the session decision and shutdown must not wait for
    /// `main`'s tail (which stays as an idempotent fallback).
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Before the tabs go: they are the snapshot.
        self.finish_session();
        // Request all shutdowns before waiting, so the deadline is shared
        // rather than one grace period per tab.
        self.tabs.clear();
        pty::finish_shutdown();
        self.finish_config_saves();
    }
}

fn main() -> anyhow::Result<()> {
    // CLI first: `--help` / `--list-themes` return before any GUI setup.
    let Some((theme_override, config_path)) = resolve_cli()? else {
        return Ok(());
    };
    // Logging is installed before the config is read, so load warnings
    // land in the in-app panel. Stderr keeps env_logger's behavior
    // (`RUST_LOG`, default error); the panel records by directives
    // (`[log] filter` / `level`) instead of by bare level, so dependency
    // debug/trace spam stays out (see `logging`).
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let log_proxy = event_loop.create_proxy();
    let log_defaults = config::LogConfig::default();
    let log_buffer = Arc::new(Mutex::new(LogBuffer::new(log_defaults.buffer_lines)));
    // Non-fatal: a logger that can't install (already set) must not stop
    // the terminal from running.
    if let Err(e) = logging::install(
        log_buffer.clone(),
        Some(log_proxy),
        &log_defaults.filter_string(),
    ) {
        eprintln!("cometty: logging setup incomplete ({e:#}); continuing");
    }
    // Missing file = silent defaults (first run); present-but-unreadable =
    // corrupt flag surfaced as a warning badge in the settings panel.
    // The GUI keeps session values and wins over later external edits.
    let config_path_override = config_path.map(std::path::PathBuf::from);
    let load_candidate: Option<std::path::PathBuf> =
        config_path_override.clone().or_else(Config::default_path);
    let (config, config_corrupt) = match load_candidate {
        Some(p) if p.exists() => match Config::load_from_path(&p) {
            Ok(c) => (c, false),
            Err(e) => {
                log::warn!("unreadable config {} ({e:#}); using defaults", p.display());
                (Config::default(), true)
            }
        },
        _ => (Config::load(), false),
    };
    // The ring's record filter and capacity come from the file. A typo in
    // `[log] filter` keeps the default filter and says so (in the panel:
    // the logger is already installed here).
    if let Err(e) = logging::set_record_filter(&config.log.filter_string()) {
        log::warn!("{e}");
    }
    log_buffer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .set_capacity(config.log.buffer_lines);
    let cli_theme = theme_override.map(|t| t.name.to_string());
    let theme = theme_override
        .or_else(|| Theme::from_name(&config.theme.name))
        .unwrap_or_default();
    // Forward native-menu activations into the event loop so the menu works
    // while the loop sleeps waiting for PTY output.
    let menu_proxy = event_loop.create_proxy();
    muda::MenuEvent::set_event_handler(Some(move |event| {
        let _ = menu_proxy.send_event(UserEvent::MenuEvent(event));
    }));
    let proxy = event_loop.create_proxy();
    let mut app = App::new(
        Some(proxy),
        theme,
        config,
        AppStartup {
            config_path_override,
            cli_theme,
            config_corrupt,
        },
        log_buffer,
    );
    app.session = app::session::SessionState::open(&app.config);
    let result = event_loop.run_app(&mut app);
    // Normally already done by `exiting`; repeated here in case the loop
    // returned without it. Also covers earlier closed tabs.
    app.tabs.clear();
    pty::finish_shutdown();
    app.finish_config_saves();
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn cli_empty_runs_with_defaults() {
        assert_eq!(
            parse_cli_args(&args(&[])).unwrap(),
            CliParsed::Run {
                theme: None,
                config_path: None,
            }
        );
    }

    #[test]
    fn cli_theme_and_config_forms() {
        let vscode = Theme::from_name("vscode").unwrap();
        assert_eq!(
            parse_cli_args(&args(&["--theme", "vscode"])).unwrap(),
            CliParsed::Run {
                theme: Some(vscode),
                config_path: None,
            }
        );
        assert_eq!(
            parse_cli_args(&args(&["--theme=vscode", "--config=/tmp/c.toml"])).unwrap(),
            CliParsed::Run {
                theme: Some(Theme::from_name("vscode").unwrap()),
                config_path: Some("/tmp/c.toml".to_string()),
            }
        );
        assert_eq!(
            parse_cli_args(&args(&["--config", "/tmp/c.toml"])).unwrap(),
            CliParsed::Run {
                theme: None,
                config_path: Some("/tmp/c.toml".to_string()),
            }
        );
    }

    #[test]
    fn cli_help_and_list_themes() {
        assert_eq!(parse_cli_args(&args(&["--help"])).unwrap(), CliParsed::Help);
        assert_eq!(parse_cli_args(&args(&["-h"])).unwrap(), CliParsed::Help);
        assert_eq!(
            parse_cli_args(&args(&["--list-themes"])).unwrap(),
            CliParsed::ListThemes
        );
    }

    #[test]
    fn cli_rejects_bad_input() {
        assert!(parse_cli_args(&args(&["--theme"])).is_err());
        assert!(parse_cli_args(&args(&["--theme", "nope"])).is_err());
        assert!(parse_cli_args(&args(&["--config"])).is_err());
        assert!(parse_cli_args(&args(&["--frobnicate"])).is_err());
        assert!(parse_cli_args(&args(&["extra-positional"])).is_err());
    }
}
