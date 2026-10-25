//! Keyboard handling (copy/paste shortcuts, new tab, local scroll, PTY send).

use std::time::Instant;

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

/// Pure policy behind [`App::terminal_owns_keyboard`]: keystrokes belong
/// to the terminal unless the settings panel is open (it swallows all),
/// the find bar is waiting for focus, or an egui text field has focus.
/// Free function so it is unit-testable without an `App`.
pub(crate) fn terminal_owns_keyboard_for(
    settings_open: bool,
    search_focus_pending: bool,
    text_edit_focused: bool,
) -> bool {
    !settings_open && !search_focus_pending && !text_edit_focused
}

impl App {
    /// True when keystrokes belong to the terminal, not egui chrome.
    pub(crate) fn terminal_owns_keyboard(&self) -> bool {
        terminal_owns_keyboard_for(
            self.settings.open,
            self.search_focus_pending(),
            self.renderer
                .as_ref()
                .is_some_and(|r| r.text_edit_focused()),
        )
    }

    /// Dead-key / IME commits (e.g. `~ ' " \`` on macOS US-Intl) arrive
    /// as `WindowEvent::Ime::Commit`, not `KeyboardInput`. egui also sees
    /// the event, so only forward to the PTY when the terminal owns the
    /// keyboard; otherwise the focused text field keeps it.
    pub(crate) fn on_ime(&mut self, ime: &winit::event::Ime) {
        let winit::event::Ime::Commit(text) = ime else {
            return;
        };
        if text.is_empty() || !self.terminal_owns_keyboard() {
            return;
        }
        if let Some(tab) = self.active_tab() {
            tab.pty.write(text.as_bytes().to_vec());
        }
    }

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
        // Find bar: the configured search shortcut (Cmd+F on macOS, Ctrl+F
        // elsewhere) opens it (or re-focuses an open bar). Placed
        // after the settings early-return so the dialog keeps the combo.
        if event.state == ElementState::Pressed
            && crate::app::search::is_search_toggle(
                &event.logical_key,
                &self.modifiers,
                &self.config.input,
            )
        {
            self.focus_search();
            return true;
        }
        // While the bar is open: Esc closes; Enter / Shift+Enter and
        // F3 / Shift+F3 walk the matches. All other keys go to the focused
        // egui field (egui's `consumed` flag below) — or to the shell if the
        // pointer moved back to the grid, matching the log panel. Text
        // typed before the field has focus (same frame as the open) is
        // swallowed, not sent to the shell.
        if self.search_open() && event.state == ElementState::Pressed {
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.close_search();
                    return true;
                }
                Key::Named(NamedKey::Enter) | Key::Named(NamedKey::F3) => {
                    self.step_search(!self.modifiers.shift_key());
                    return true;
                }
                Key::Character(_)
                | Key::Named(NamedKey::Backspace | NamedKey::Delete | NamedKey::Space)
                    if self.search_focus_pending() =>
                {
                    return true;
                }
                _ => {}
            }
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
        let wrote_to_pty = if event.state == ElementState::Pressed
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
                true
            } else {
                false
            }
        } else {
            false
        };
        if wrote_to_pty {
            // Latency probe: `drain_pty` turns this into a key-to-echo
            // measurement, `on_redraw` into echo-to-paint (debug logs).
            self.last_key_write = Some(Instant::now());
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::terminal_owns_keyboard_for;

    #[test]
    fn keyboard_ownership_follows_panels_and_text_focus() {
        // Pure terminal: Tab/IME belong to the PTY.
        assert!(terminal_owns_keyboard_for(false, false, false));
        // Settings panel swallows everything, even unfocused.
        assert!(!terminal_owns_keyboard_for(true, false, false));
        assert!(!terminal_owns_keyboard_for(true, false, true));
        // Find bar waiting for focus, or any text field focused.
        assert!(!terminal_owns_keyboard_for(false, true, false));
        assert!(!terminal_owns_keyboard_for(false, false, true));
    }
}
