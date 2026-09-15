//! Keyboard handling (copy/paste shortcuts, new tab, local scroll, PTY send).

use winit::event::ElementState;
use winit::keyboard::{Key, NamedKey};

#[cfg(any(
    target_os = "windows",
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
))]
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

use super::App;

impl App {
    /// Returns true when the event was consumed locally (never reaches PTY).
    pub(crate) fn on_keyboard(
        &mut self,
        event: &winit::event::KeyEvent,
        egui_consumed: bool,
    ) -> bool {
        // Settings toggle works whether or not the panel is open, and takes
        // precedence over egui + terminal so the combo never reaches the PTY.
        if event.state == ElementState::Pressed
            && crate::app::settings::is_settings_toggle(&event.logical_key, &self.modifiers)
        {
            self.settings.toggle();
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
            return true;
        }
        // Log panel toggle: same precedence as Settings, so the combo
        // works while the settings panel is open too.
        if event.state == ElementState::Pressed
            && crate::app::logs::is_logs_toggle(&event.logical_key, &self.modifiers)
        {
            self.logs.toggle();
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
            return true;
        }
        // Panel-first: while open, everything except the toggle (above) and
        // Esc-to-close goes to the egui panel, never the shell. Esc closes.
        if self.settings.open {
            if event.state == ElementState::Pressed
                && event.logical_key == Key::Named(NamedKey::Escape)
            {
                self.settings.close();
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            return true;
        }
        if egui_consumed {
            return true;
        }
        // Explicit copy/paste never reaches the PTY.
        if event.state == ElementState::Pressed {
            if crate::input::is_copy_shortcut(
                &event.logical_key,
                &self.modifiers,
                &self.config.input,
            ) {
                self.copy_selection();
                return true;
            }
            if crate::input::is_paste_shortcut(
                &event.logical_key,
                &self.modifiers,
                &self.config.input,
            ) {
                self.paste_from_clipboard();
                return true;
            }
            // New tab never reaches the PTY either.
            if crate::input::is_new_tab_shortcut(
                &event.logical_key,
                &self.modifiers,
                &self.config.input,
            ) {
                self.spawn_tab_for_window();
                return true;
            }
            // Tab switching never reaches the PTY: Ctrl/Super+Tab cycles
            // (Shift = back), Super+1..9 jumps directly.
            #[cfg(any(
                target_os = "windows",
                target_os = "macos",
                target_os = "linux",
                target_os = "freebsd",
                target_os = "dragonfly",
                target_os = "netbsd",
                target_os = "openbsd"
            ))]
            let without = event.key_without_modifiers();
            #[cfg(not(any(
                target_os = "windows",
                target_os = "macos",
                target_os = "linux",
                target_os = "freebsd",
                target_os = "dragonfly",
                target_os = "netbsd",
                target_os = "openbsd"
            )))]
            let without = event.logical_key.clone();
            if let Some(index) = crate::input::tab_direct_index(
                &event.logical_key,
                &without,
                &self.modifiers,
                &self.config.input,
            ) {
                self.switch_tab(index);
                return true;
            }
            if crate::input::is_tab_switch_shortcut(
                &event.logical_key,
                &self.modifiers,
                &self.config.input,
            ) {
                self.switch_relative(crate::input::tab_switch_delta(&self.modifiers));
                return true;
            }
        }
        // Shift+PgUp/PgDn/Home/End scrolls locally instead of sending to the PTY.
        // Only plain Shift (no Ctrl/Alt/Super) scrolls; e.g. Ctrl+Shift+PgUp
        // still goes to the PTY as `ESC[5;6~`.
        if event.state == ElementState::Pressed
            && self.config.input.shift_page_scroll
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
            if let Some(bytes) = crate::input::key_to_bytes(
                event,
                &self.modifiers,
                cursor_app,
                keypad_app,
                &self.config.input,
            ) {
                tab.pty.write(bytes);
            }
        }
        true
    }
}
