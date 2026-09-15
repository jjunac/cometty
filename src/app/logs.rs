//! In-app log viewer state ([`crate::renderer::log_ui`] draws it).
//!
//! Owned by [`crate::app::App`], mirroring [`crate::app::settings`]: this
//! file is headless-testable state plus the shortcut matcher; all egui
//! widgets live in the renderer module.

use log::LevelFilter;
use winit::keyboard::{Key, ModifiersState};

/// Log window state. The level/filter fields are display-only: what gets
/// recorded is `[log] level` plus `RUST_LOG` ([`crate::logging`]).
pub struct LogsPanel {
    pub open: bool,
    /// Most verbose level shown (log-filter semantics, `off` = nothing).
    pub level: LevelFilter,
    /// Case-insensitive substring filter over target + message.
    pub filter_text: String,
    /// Stick to the newest entry as records arrive.
    pub following: bool,
}

impl Default for LogsPanel {
    fn default() -> Self {
        Self {
            open: false,
            level: LevelFilter::Debug,
            filter_text: String::new(),
            following: true,
        }
    }
}

impl LogsPanel {
    pub fn toggle(&mut self) {
        self.set_open(!self.open);
    }

    /// Open from the native menu bar (open-only, like Settings).
    pub fn show(&mut self) {
        self.set_open(true);
    }

    pub fn close(&mut self) {
        self.set_open(false);
    }

    fn set_open(&mut self, open: bool) {
        self.open = open;
        // Only wake the event loop for records that change what's on
        // screen; with the panel closed, logging stays wake-free.
        crate::logging::set_wake_enabled(open);
    }
}

/// `Ctrl+Shift+L` / `Cmd+Shift+L` toggles the panel. Shift is required so
/// a bare `Ctrl+L` (shell "clear screen") still reaches the PTY, and so
/// the terminal keeps its usual Ctrl+letter bindings; Alt is not allowed.
pub fn is_logs_toggle(logical_key: &Key, modifiers: &ModifiersState) -> bool {
    let Key::Character(s) = logical_key else {
        return false;
    };
    if !s.as_str().eq_ignore_ascii_case("l") {
        return false;
    }
    if modifiers.alt_key() || !modifiers.shift_key() {
        return false;
    }
    let ctrl = modifiers.control_key() && !modifiers.super_key();
    let sup = modifiers.super_key() && !modifiers.control_key();
    ctrl != sup
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::NamedKey;

    #[test]
    fn toggle_shortcut() {
        let lower: Key = Key::Character("l".into());
        let upper: Key = Key::Character("L".into());
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        let sup_shift = ModifiersState::SUPER | ModifiersState::SHIFT;
        // Both cases: Shift makes the logical key layout-dependent.
        assert!(is_logs_toggle(&lower, &ctrl_shift));
        assert!(is_logs_toggle(&upper, &ctrl_shift));
        assert!(is_logs_toggle(&lower, &sup_shift));
        assert!(is_logs_toggle(&upper, &sup_shift));
        // Shift is mandatory: Ctrl+L belongs to the shell.
        assert!(!is_logs_toggle(&lower, &ModifiersState::CONTROL));
        assert!(!is_logs_toggle(&lower, &ModifiersState::SHIFT));
        assert!(!is_logs_toggle(&lower, &ModifiersState::empty()));
        // One of Ctrl/Super, never both, and never with Alt.
        assert!(!is_logs_toggle(
            &lower,
            &(ctrl_shift | ModifiersState::SUPER)
        ));
        assert!(!is_logs_toggle(&lower, &(ctrl_shift | ModifiersState::ALT)));
        // Other keys and non-character keys stay out.
        assert!(!is_logs_toggle(&Key::Character("k".into()), &ctrl_shift));
        assert!(!is_logs_toggle(&Key::Named(NamedKey::Escape), &ctrl_shift));
    }

    #[test]
    fn panel_open_state_controls_record_wakeups() {
        // Only test in this binary that flips the wake gate: keeps the
        // wake assertions deterministic under parallel test threads.
        let mut panel = LogsPanel::default();
        assert!(!panel.open);
        assert!(!crate::logging::wake_enabled(), "closed: logs never wake");
        panel.toggle();
        assert!(panel.open);
        assert!(crate::logging::wake_enabled(), "open: records repaint");
        panel.toggle();
        assert!(!panel.open);
        assert!(!crate::logging::wake_enabled());
        panel.show();
        assert!(panel.open);
        panel.close();
        assert!(!panel.open);
        assert!(!crate::logging::wake_enabled());
    }
}
