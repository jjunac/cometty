mod grid;
mod input;
mod pty;
mod renderer;
mod term;
mod theme;

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use pty::{PtyEvent, PtySession};
use renderer::Renderer;
use term::Terminal;
use theme::Theme;

#[derive(Debug)]
enum UserEvent {
    PtyAvailable,
}

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    terminal: Option<Terminal>,
    pty: Option<PtySession>,
    proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>>,
    theme: Theme,
    modifiers: ModifiersState,
    cursor_visible: bool,
    last_blink: Instant,
    pending_resize: Option<(u32, u32)>,
    exited: bool,
}

impl App {
    fn new(proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>>) -> Self {
        Self {
            window: None,
            renderer: None,
            terminal: None,
            pty: None,
            proxy,
            theme: Theme::default(),
            modifiers: ModifiersState::empty(),
            cursor_visible: true,
            last_blink: Instant::now(),
            pending_resize: None,
            exited: false,
        }
    }

    fn drain_pty(&mut self) {
        let mut got_data = false;
        loop {
            let ev = self.pty.as_ref().and_then(|p| p.try_recv());
            match ev {
                Some(PtyEvent::Data(bytes)) => {
                    if let Some(t) = self.terminal.as_mut() {
                        t.feed(&bytes);
                    }
                    got_data = true;
                }
                Some(PtyEvent::Exit) => {
                    log::info!("shell exited");
                    got_data = true;
                    break;
                }
                None => break,
            }
        }
        if got_data && let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    fn apply_resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        let (renderer, term, pty) = match (
            self.renderer.as_mut(),
            self.terminal.as_mut(),
            self.pty.as_ref(),
        ) {
            (Some(r), Some(t), Some(p)) => (r, t, p),
            _ => return,
        };
        renderer.resize(width, height);
        let cols = renderer.cols_for_width(width).clamp(1, 1024);
        let rows = renderer.rows_for_height(height).clamp(1, 1024);
        if cols != term.cols() || rows != term.rows() {
            term.resize(cols, rows);
            pty.resize(cols, rows);
        }
    }
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
        let renderer = match Renderer::new(window.clone(), w, h, self.theme) {
            Ok(r) => r,
            Err(e) => {
                log::error!("failed to init renderer: {e:#}");
                event_loop.exit();
                return;
            }
        };
        let cols = renderer.cols_for_width(w).clamp(1, 256);
        let rows = renderer.rows_for_height(h).clamp(1, 256);
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
        match event {
            WindowEvent::CloseRequested => {
                self.exited = true;
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                self.pending_resize = Some((size.width, size.height));
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(w) = self.window.as_ref() {
                    let s = w.inner_size();
                    self.pending_resize = Some((s.width, s.height));
                    w.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // Allow Ctrl+Shift+Q / Cmd+Q style quit? Keep minimal: no custom shortcuts.
                if event.state == ElementState::Pressed
                    && let Some(bytes) = input::key_to_bytes(&event, &self.modifiers)
                    && let Some(p) = self.pty.as_ref()
                {
                    p.write(bytes);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some((w, h)) = self.pending_resize.take() {
                    self.apply_resize(w, h);
                }
                // Drain any pending PTY output that arrived between wake and draw.
                self.drain_pty();

                let (renderer, terminal, window) = match (
                    self.renderer.as_mut(),
                    self.terminal.as_ref(),
                    self.window.as_ref(),
                ) {
                    (Some(r), Some(t), Some(w)) => (r, t, w),
                    _ => return,
                };
                let grid = terminal.grid();
                let cursor = grid.cursor();
                let rows: Vec<Vec<grid::Cell>> = grid.visible_rows().to_vec();
                let version = grid.version;
                if let Err(e) =
                    renderer.render(&rows, (cursor.x, cursor.y), self.cursor_visible, version)
                {
                    // Surface lost / outdated is recoverable via resize.
                    log::warn!("render failed: {e:#}");
                    let s = window.inner_size();
                    renderer.resize(s.width.max(1), s.height.max(1));
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
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut app = App::new(Some(proxy));
    event_loop.run_app(&mut app)?;
    Ok(())
}
