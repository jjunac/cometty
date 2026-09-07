mod app;
mod config;
mod grid;
mod input;
mod pty;
mod renderer;
mod scrollbar;
mod selection;
mod tabbar;
mod term;
mod theme;

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use app::{App, UserEvent};
use config::Config;
use renderer::Renderer;
use theme::Theme;

/// CLI overrides layered on top of the TOML file.
/// Returns `(theme, config_path_override)` or `None` for `--help`/`--list-themes`.
fn resolve_cli() -> anyhow::Result<Option<(Option<Theme>, Option<String>)>> {
    let mut theme_override: Option<Theme> = None;
    let mut config_path: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--list-themes" {
            for t in Theme::all() {
                println!("{}", t.name);
            }
            return Ok(None);
        } else if arg == "--theme" {
            let name = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--theme requires a value (try --list-themes)"))?;
            theme_override =
                Some(Theme::from_name(&name).ok_or_else(|| {
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
            config_path = Some(path);
        } else if let Some(path) = arg.strip_prefix("--config=") {
            config_path = Some(path.to_string());
        } else if arg == "-h" || arg == "--help" {
            println!("cometty [--theme NAME] [--config PATH] [--list-themes]");
            println!("Config file: $HOME/.config/cometty/config.toml");
            return Ok(None);
        } else {
            return Err(anyhow::anyhow!("unknown argument {arg:?} (try --help)"));
        }
    }
    Ok(Some((theme_override, config_path)))
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(self.config.window.title.clone())
            .with_inner_size(winit::dpi::LogicalSize::new(
                self.config.window.width as f64,
                self.config.window.height as f64,
            ));
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
        // Single tab: no bar, so the grid gets the full height.
        let term_h = app::tab::term_height_px(h, scale, 1, &self.config.tabbar);
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
        if !self.spawn_tab(cols, rows) {
            event_loop.exit();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::PtyAvailable => {
                if self.drain_pty() {
                    event_loop.exit();
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
        let egui_consumed = match (self.renderer.as_mut(), self.window.as_ref()) {
            (Some(r), Some(w)) => r.on_window_event(w, &event).consumed,
            _ => false,
        };
        // While fading/dragging, keep frames coming without waiting for PTY.
        if egui_consumed
            && matches!(
                event,
                WindowEvent::CursorMoved { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::CursorLeft { .. }
            )
            && let Some(w) = self.window.as_ref()
        {
            w.request_redraw();
        }
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
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
                self.modifiers = m.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.on_keyboard(&event, egui_consumed);
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
                let _ = egui_consumed;
                self.on_mouse_input(button, state);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Wheel over the egui scrollbar still scrolls the same view.
                let _ = egui_consumed;
                self.on_wheel(delta);
            }
            WindowEvent::RedrawRequested => {
                self.on_redraw();
                if self.tabs.is_empty() {
                    event_loop.exit();
                }
            }
            WindowEvent::Focused(_) => {
                if let Some(w) = self.window.as_ref() {
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

        let blink_ms = self.config.cursor.blink_ms;
        let now = Instant::now();
        if now.duration_since(self.last_blink) >= Duration::from_millis(blink_ms) {
            self.cursor_visible = !self.cursor_visible;
            self.last_blink = now;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
        // Wake up for next blink toggle.
        let next = self.last_blink + Duration::from_millis(blink_ms);
        event_loop.set_control_flow(ControlFlow::WaitUntil(next));
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let Some((theme_override, config_path)) = resolve_cli()? else {
        return Ok(());
    };
    let config = if let Some(path) = config_path {
        Config::load_from_path(std::path::Path::new(&path)).unwrap_or_default()
    } else {
        Config::load()
    };
    let theme = theme_override
        .or_else(|| Theme::from_name(&config.theme.name))
        .unwrap_or_default();
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut app = App::new(Some(proxy), theme, config);
    event_loop.run_app(&mut app)?;
    Ok(())
}
