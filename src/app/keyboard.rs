//! Keyboard handling (copy/paste shortcuts, new tab, local scroll, PTY send).

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
            // New tab never reaches the PTY either.
            if crate::input::is_new_tab_shortcut(&event.logical_key, &self.modifiers) {
                self.spawn_tab_for_window();
                return true;
            }
            // Reserved for future tab switching; consume so the PTY never
            // sees Ctrl+Tab / Ctrl+Shift+Tab (TODO: switch_tab).
            if crate::input::is_tab_switch_shortcut(&event.logical_key, &self.modifiers) {
                return true;
            }
        }
        // Shift+PgUp/PgDn/Home/End scrolls locally instead of sending to the PTY.
        // Only plain Shift (no Ctrl/Alt/Super) scrolls; e.g. Ctrl+Shift+PgUp
        // still goes to the PTY as `ESC[5;6~`.
        if event.state == ElementState::Pressed
            && self.modifiers.shift_key()
            && !self.modifiers.control_key()
            && !self.modifiers.alt_key()
            && !self.modifiers.super_key()
            && let Key::Named(named) = &event.logical_key
        {
            let handled = match named {
                NamedKey::PageUp => {
                    let page = self
                        .active_tab()
                        .map(|t| t.terminal.rows().saturating_sub(1).max(1) as isize)
                        .unwrap_or(1);
                    self.scroll_terminal(page);
                    true
                }
                NamedKey::PageDown => {
                    let page = self
                        .active_tab()
                        .map(|t| t.terminal.rows().saturating_sub(1).max(1) as isize)
                        .unwrap_or(1);
                    self.scroll_terminal(-page);
                    true
                }
                NamedKey::Home => {
                    if let Some(t) = self.active_tab_mut()
                        && t.terminal.scroll_to_top()
                        && let Some(w) = self.window.as_ref()
                    {
                        w.request_redraw();
                    }
                    true
                }
                NamedKey::End => {
                    if let Some(t) = self.active_tab_mut()
                        && t.terminal.scroll_to_bottom()
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
        // Regular keys go to the active tab's PTY.
        if event.state == ElementState::Pressed
            && let Some(tab) = self.active_tab()
        {
            let cursor_app = tab.terminal.cursor_app_mode();
            let keypad_app = tab.terminal.keypad_app_mode();
            if let Some(bytes) =
                crate::input::key_to_bytes(event, &self.modifiers, cursor_app, keypad_app)
            {
                tab.pty.write(bytes);
            }
        }
        true
    }
}
