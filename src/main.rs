mod grid;
mod input;
mod pty;
mod renderer;
mod scrollbar;
mod term;
mod theme;

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
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
    pending_scale: Option<f32>,
    wheel_accum: f64,
    scrollbar: scrollbar::ScrollbarUi,
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
            pending_scale: None,
            wheel_accum: 0.0,
            scrollbar: scrollbar::ScrollbarUi::new(Instant::now()),
            exited: false,
        }
    }

    fn scroll_terminal(&mut self, delta: isize) {
        if delta == 0 {
            return;
        }
        if let Some(t) = self.terminal.as_mut()
            && t.scroll_by(delta)
            && let Some(w) = self.window.as_ref()
        {
            w.request_redraw();
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

    fn sync_scale_factor(&mut self) -> f32 {
        let scale = self
            .window
            .as_ref()
            .map(|w| w.scale_factor() as f32)
            .unwrap_or(1.0);
        if let Some(r) = self.renderer.as_mut() {
            r.set_scale_factor(scale);
        }
        scale
    }

    fn apply_resize(&mut self, width: u32, height: u32, scale: f32) {
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
        renderer.set_scale_factor(scale);
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
        let scale = window.scale_factor() as f32;
        let renderer = match Renderer::new(window.clone(), w, h, scale, self.theme) {
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
                if egui_consumed {
                    return;
                }
                // Shift+PgUp/PgDn/Home/End scrolls locally instead of sending to the PTY.
                if event.state == ElementState::Pressed
                    && self.modifiers.shift_key()
                    && let Key::Named(named) = &event.logical_key
                {
                    let handled = match named {
                        NamedKey::PageUp => {
                            let page = self
                                .terminal
                                .as_ref()
                                .map(|t| t.rows().saturating_sub(1).max(1) as isize)
                                .unwrap_or(1);
                            self.scroll_terminal(page);
                            true
                        }
                        NamedKey::PageDown => {
                            let page = self
                                .terminal
                                .as_ref()
                                .map(|t| t.rows().saturating_sub(1).max(1) as isize)
                                .unwrap_or(1);
                            self.scroll_terminal(-page);
                            true
                        }
                        NamedKey::Home => {
                            if let Some(t) = self.terminal.as_mut()
                                && t.scroll_to_top()
                                && let Some(w) = self.window.as_ref()
                            {
                                w.request_redraw();
                            }
                            true
                        }
                        NamedKey::End => {
                            if let Some(t) = self.terminal.as_mut()
                                && t.scroll_to_bottom()
                                && let Some(w) = self.window.as_ref()
                            {
                                w.request_redraw();
                            }
                            true
                        }
                        _ => false,
                    };
                    if handled {
                        return;
                    }
                }
                // Allow Ctrl+Shift+Q / Cmd+Q style quit? Keep minimal: no custom shortcuts.
                if event.state == ElementState::Pressed
                    && let Some(bytes) = input::key_to_bytes(&event, &self.modifiers)
                    && let Some(p) = self.pty.as_ref()
                {
                    p.write(bytes);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Wheel over the egui scrollbar still scrolls the same view.
                let _ = egui_consumed;
                const LINES_PER_TICK: f64 = 7.0;
                match delta {
                    MouseScrollDelta::LineDelta(_, y) => {
                        self.scroll_terminal((y as f64 * LINES_PER_TICK).round() as isize);
                    }
                    MouseScrollDelta::PixelDelta(pos) => {
                        let line_height = self
                            .renderer
                            .as_ref()
                            .map(|r| f64::from(r.line_height))
                            .unwrap_or(20.0)
                            .max(1.0);
                        self.wheel_accum += pos.y / line_height;
                        let lines = self.wheel_accum.trunc() as isize;
                        if lines != 0 {
                            self.wheel_accum -= lines as f64;
                            self.scroll_terminal(lines);
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                // Scale can change without a Resized event (monitor move), so
                // always sync before applying a pending resize.
                let current_scale = self.sync_scale_factor();
                if let Some((w, h)) = self.pending_resize.take() {
                    let scale = self.pending_scale.take().unwrap_or(current_scale);
                    self.apply_resize(w, h, scale);
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
                let rows: Vec<Vec<grid::Cell>> = grid.view_rows().into_iter().cloned().collect();
                let version = grid.version;
                let effective_cursor = self.cursor_visible
                    && terminal.cursor_visible()
                    && terminal.scroll_offset() == 0;
                let total = grid.scrollback_len() + grid.rows();
                match renderer.render(
                    &rows,
                    (cursor.x, cursor.y),
                    effective_cursor,
                    version,
                    renderer::ScrollCtx {
                        window,
                        ui: &mut self.scrollbar,
                        total,
                        visible: grid.rows(),
                        offset: terminal.scroll_offset(),
                        is_alt: grid.is_alt(),
                    },
                ) {
                    Ok(scroll_to) => {
                        if let Some(target) = scroll_to
                            && let Some(t) = self.terminal.as_mut()
                            && t.scroll_to_offset(target)
                            && let Some(w) = self.window.as_ref()
                        {
                            w.request_redraw();
                        }
                        // Keep animating the fade without PTY traffic.
                        let fading = self.scrollbar.opacity > 0.0 && self.scrollbar.opacity < 1.0;
                        if let Some(w) = self.window.as_ref()
                            && (fading || self.scrollbar.is_dragging())
                        {
                            w.request_redraw();
                        }
                    }
                    Err(e) => {
                        // Surface lost / outdated is recoverable via resize.
                        log::warn!("render failed: {e:#}");
                        let s = window.inner_size();
                        renderer.resize(s.width.max(1), s.height.max(1));
                    }
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
