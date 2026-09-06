//! Keyboard handling (copy/paste shortcuts, local scroll, PTY send).

use winit::event::ElementState;
use winit::keyboard::{Key, NamedKey};

use super::App;

impl App {
    /// Returns true when the event was consumed locally (never reaches PTY).
    pub(crate) fn on_keyboard(
        &mut self,
        event: &winit::event::KeyEvent,
        egui_consumed: bool,
    ) -> bool {
        if egui_consumed {
            return true;
        }
        // Explicit copy/paste never reaches the PTY.
        if event.state == ElementState::Pressed {
            if crate::input::is_copy_shortcut(&event.logical_key, &self.modifiers) {
                self.copy_selection();
                return true;
            }
            if crate::input::is_paste_shortcut(&event.logical_key, &self.modifiers) {
                self.paste_from_clipboard();
                return true;
            }
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
                return true;
            }
        }
        // Regular keys go to the PTY.
        if event.state == ElementState::Pressed
            && let Some(bytes) = crate::input::key_to_bytes(event, &self.modifiers)
            && let Some(p) = self.pty.as_ref()
        {
            p.write(bytes);
        }
        true
    }
}
