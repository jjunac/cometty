mod app;
mod grid;
mod input;
mod pty;
mod renderer;
mod scrollbar;
mod selection;
mod term;
mod theme;

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use app::{App, UserEvent};
use pty::PtySession;
use renderer::Renderer;
use term::Terminal;
use theme::Theme;

/// Resolve `--theme NAME` / `--list-themes` from argv.
/// Returns `None` when the flag was informational (`--help`,
/// `--list-themes`) and the caller should exit successfully.
fn resolve_theme() -> anyhow::Result<Option<Theme>> {
    let mut theme = Theme::default();
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
            theme = Theme::from_name(&name)
                .ok_or_else(|| anyhow::anyhow!("unknown theme {name:?} (try --list-themes)"))?;
        } else if let Some(name) = arg.strip_prefix("--theme=") {
            theme = Theme::from_name(name)
                .ok_or_else(|| anyhow::anyhow!("unknown theme {name:?} (try --list-themes)"))?;
        } else if arg == "-h" || arg == "--help" {
            println!("cometty [--theme NAME] [--list-themes]");
            return Ok(None);
        } else {
            return Err(anyhow::anyhow!("unknown argument {arg:?} (try --help)"));
        }
    }
    Ok(Some(theme))
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("cometty")
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
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
        let renderer = match Renderer::new(window.clone(), w, h, scale, self.theme) {
            Ok(r) => r,
            Err(e) => {
                log::error!("failed to init renderer: {e:#}");
                event_loop.exit();
                return;
            }
        };
        let (cols, rows) = app::compute_grid_size(w, h, renderer.cell_width, renderer.line_height);
        let terminal = Terminal::new(cols, rows, self.theme);

        let proxy = self.proxy.clone();
        let waker = move || {
            if let Some(p) = proxy.as_ref() {
                let _ = p.send_event(UserEvent::PtyAvailable);
            }
        };
        let pty = match PtySession::spawn_with_size(cols, rows, waker) {
            Ok(p) => p,
            Err(e) => {
                log::error!("failed to spawn pty: {e:#}");
                event_loop.exit();
                return;
            }
        };

        self.window = Some(window);
        self.renderer = Some(renderer);
        self.terminal = Some(terminal);
        self.pty = Some(pty);
        self.last_blink = Instant::now();
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::PtyAvailable => self.drain_pty(),
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
                self.exited = true;
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
            }
            WindowEvent::Focused(_) => {
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Fallback drain in case a wake was coalesced.
        self.drain_pty();

        let now = Instant::now();
        if now.duration_since(self.last_blink) >= Duration::from_millis(530) {
            self.cursor_visible = !self.cursor_visible;
            self.last_blink = now;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
        // Wake up for next blink toggle.
        let next = self.last_blink + Duration::from_millis(530);
        _event_loop.set_control_flow(ControlFlow::WaitUntil(next));
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let Some(theme) = resolve_theme()? else {
        return Ok(());
    };
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut app = App::new(Some(proxy), theme);
    event_loop.run_app(&mut app)?;
    Ok(())
}
