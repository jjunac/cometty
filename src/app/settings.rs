//! Settings panel state + validation/diff helpers.
//!
//! `SettingsPanel` is owned by [`crate::app::App`] (App-owned panel state).
//! The egui widgets live in [`crate::renderer::settings_ui`]; everything in
//! this file is headless-testable and must stay free of GPU/window handles.

use std::path::PathBuf;

use winit::keyboard::{Key, ModifiersState};

use crate::config::Config;

/// Sidebar sections, in display order. Basics are shown inline per section;
/// fine-grained knobs sit under an `Advanced` collapsing header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsSection {
    Theme,
    Font,
    Window,
    Terminal,
    Shell,
    Cursor,
    Selection,
    Scrollbar,
    Tabbar,
    Input,
    Logs,
}

impl SettingsSection {
    pub const ALL: [Self; 11] = [
        Self::Theme,
        Self::Font,
        Self::Window,
        Self::Terminal,
        Self::Shell,
        Self::Cursor,
        Self::Selection,
        Self::Scrollbar,
        Self::Tabbar,
        Self::Input,
        Self::Logs,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Theme => "Theme",
            Self::Font => "Font",
            Self::Window => "Window",
            Self::Terminal => "Terminal",
            Self::Shell => "Shell",
            Self::Cursor => "Cursor",
            Self::Selection => "Selection",
            Self::Scrollbar => "Scrollbar",
            Self::Tabbar => "Tab bar",
            Self::Input => "Input",
            Self::Logs => "Logs",
        }
    }
}

/// Startup context captured in `main` before `App` takes over.
pub struct AppStartup {
    pub config_path_override: Option<PathBuf>,
    pub cli_theme: Option<String>,
    pub config_corrupt: bool,
}

/// In-app settings overlay state.
pub struct SettingsPanel {
    pub open: bool,
    pub section: SettingsSection,
    pub cli_theme_override: Option<String>,
    pub custom_config_path: Option<PathBuf>,
    pub corrupt_at_startup: bool,
    pub last_error: Option<String>,
    /// Transient inline hint (e.g. "font size reset to default").
    /// Set on clamp/repair, cleared on section switch or close.
    pub notice: Option<String>,
}

impl SettingsPanel {
    pub fn new(startup: AppStartup) -> Self {
        Self {
            open: false,
            section: SettingsSection::Theme,
            cli_theme_override: startup.cli_theme,
            custom_config_path: startup.config_path_override,
            corrupt_at_startup: startup.config_corrupt,
            last_error: None,
            notice: None,
        }
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.notice = None;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.notice = None;
    }

    /// Session-only override notice (`defaults < file < CLI flags`).
    /// While set, theme edits persist to the file but don't fight the live
    /// session until relaunch without the flag.
    pub fn cli_banner(&self) -> Option<String> {
        self.cli_theme_override.as_ref().map(|t| {
            format!("CLI override active: --theme {t} (session only; file edits apply on relaunch)")
        })
    }

    /// Where auto-save writes: `--config PATH` when given, else the default.
    pub fn effective_save_path(&self) -> Option<PathBuf> {
        if let Some(p) = self.custom_config_path.clone() {
            return Some(p);
        }
        Config::default_path()
    }

    /// Human-readable save target for the panel footer.
    pub fn config_path_label(&self) -> String {
        match self.effective_save_path() {
            Some(p) => p.display().to_string(),
            None => "<no HOME; cannot save>".to_string(),
        }
    }

    pub fn report_saved(&mut self) {
        self.last_error = None;
    }

    pub fn report_error(&mut self, err: impl std::fmt::Display) {
        self.last_error = Some(err.to_string());
    }
}

/// Live-apply routing: which subsystems a config change touches.
/// Shell/cwd/term are deliberately absent — they only affect new tabs.
#[derive(Debug, Default)]
pub struct ApplyActions {
    pub theme: bool,
    pub font: bool,
    pub window_size: bool,
    pub terminal: bool,
    pub chrome: bool,
    pub log: bool,
    pub any: bool,
}

/// Diff two `Config` snapshots into [`ApplyActions`].
pub fn diff_actions(old: &Config, new: &Config) -> ApplyActions {
    let theme = old.theme != new.theme;
    let font = old.font != new.font;
    let window_size =
        old.window.width != new.window.width || old.window.height != new.window.height;
    let terminal = old.terminal != new.terminal;
    let chrome = old.scrollbar != new.scrollbar
        || old.tabbar != new.tabbar
        || old.cursor != new.cursor
        || old.selection != new.selection
        || old.input != new.input;
    let log = old.log != new.log;
    let any = old != new;
    ApplyActions {
        theme,
        font,
        window_size,
        terminal,
        chrome,
        log,
        any,
    }
}

/// Reset one sidebar section to compiled defaults.
pub fn reset_section(config: &mut Config, section: SettingsSection) {
    let defaults = Config::default();
    match section {
        SettingsSection::Theme => config.theme = defaults.theme,
        SettingsSection::Font => config.font = defaults.font,
        SettingsSection::Window => config.window = defaults.window,
        SettingsSection::Terminal => config.terminal = defaults.terminal,
        SettingsSection::Shell => config.shell = defaults.shell,
        SettingsSection::Cursor => config.cursor = defaults.cursor,
        SettingsSection::Selection => config.selection = defaults.selection,
        SettingsSection::Scrollbar => config.scrollbar = defaults.scrollbar,
        SettingsSection::Tabbar => config.tabbar = defaults.tabbar,
        SettingsSection::Input => config.input = defaults.input,
        SettingsSection::Logs => config.log = defaults.log,
    }
}

/// Global reset: the whole file back to compiled defaults.
pub fn reset_all(config: &mut Config) {
    *config = Config::default();
}

pub const KNOWN_THEMES: [&str; 3] = ["tokyo-night", "vscode", "tomorrow-night"];
pub const FONT_FAMILIES: [&str; 5] = ["monospace", "sans", "serif", "cursive", "fantasy"];
pub const CURSOR_SHAPES: [&str; 3] = ["block", "underline", "bar"];
/// Values offered for `[log] level` (least to most verbose). Parsing is
/// `log`'s own, so these names are just the picker's list.
pub const LOG_LEVELS: [&str; 6] = ["off", "error", "warn", "info", "debug", "trace"];

pub fn is_known_theme(name: &str) -> bool {
    KNOWN_THEMES.contains(&name)
}

/// `Ctrl+,` / `Cmd+,` toggles the panel. Shift is allowed (some layouts
/// report `<` for Shift+comma); Alt is not.
pub fn is_settings_toggle(logical_key: &Key, modifiers: &ModifiersState) -> bool {
    let Key::Character(s) = logical_key else {
        return false;
    };
    if s.as_str() != "," && s.as_str() != "<" {
        return false;
    }
    if modifiers.alt_key() {
        return false;
    }
    let ctrl = modifiers.control_key() && !modifiers.super_key();
    let sup = modifiers.super_key() && !modifiers.control_key();
    ctrl != sup
}

/// Clamp a positive float, falling back to the compiled default.
/// Returns `(value, was_repaired)` so the UI can show an inline hint.
pub fn repair_positive_f32(value: f32, fallback: f32) -> (f32, bool) {
    if value.is_finite() && value > 0.0 {
        (value, false)
    } else {
        (fallback, true)
    }
}

/// Keep any finite float (zero allowed), else `fallback`.
/// Returns `(value, was_repaired)`.
pub fn repair_finite_f32(value: f32, fallback: f32) -> (f32, bool) {
    if value.is_finite() {
        (value, false)
    } else {
        (fallback, true)
    }
}

/// Clamp a positive double, falling back to the compiled default.
/// Returns `(value, was_repaired)`.
pub fn repair_positive_f64(value: f64, fallback: f64) -> (f64, bool) {
    if value.is_finite() && value > 0.0 {
        (value, false)
    } else {
        (fallback, true)
    }
}

/// Clamp a `usize` below `min` back to `fallback`.
/// Returns `(value, was_repaired)`.
pub fn repair_min_usize(value: usize, min: usize, fallback: usize) -> (usize, bool) {
    if value >= min {
        (value, false)
    } else {
        (fallback, true)
    }
}

/// Clamp a `u32` below `min` back to `fallback`.
/// Returns `(value, was_repaired)`.
pub fn repair_min_u32(value: u32, min: u32, fallback: u32) -> (u32, bool) {
    if value >= min {
        (value, false)
    } else {
        (fallback, true)
    }
}

/// Clamp a `u64` below `min` back to `fallback`.
/// Returns `(value, was_repaired)`.
pub fn repair_min_u64(value: u64, min: u64, fallback: u64) -> (u64, bool) {
    if value >= min {
        (value, false)
    } else {
        (fallback, true)
    }
}

/// Repair a min/max pair (`0` = degenerate, inverted = swapped).
/// Returns `(min, max, was_repaired)`.
pub fn normalize_min_max(
    min: usize,
    max: usize,
    default_min: usize,
    default_max: usize,
) -> (usize, usize, bool) {
    let mut repaired = false;
    let mut min = if min == 0 {
        repaired = true;
        default_min
    } else {
        min
    };
    let mut max = if max == 0 {
        repaired = true;
        default_max
    } else {
        max
    };
    if min > max {
        std::mem::swap(&mut min, &mut max);
        repaired = true;
    }
    (min, max, repaired)
}

/// Keep the first alphanumeric char of a shortcut label, else `fallback`.
pub fn sanitize_key_label(raw: &str, fallback: &str) -> String {
    if let Some(ch) = raw.chars().find(|c| c.is_ascii_alphanumeric()) {
        ch.to_ascii_lowercase().to_string()
    } else {
        fallback.to_string()
    }
}

/// Accept only `block | underline | bar` (case-insensitive).
/// Returns `(shape, was_repaired)`.
pub fn sanitize_cursor_shape(raw: &str) -> (String, bool) {
    let lower = raw.trim().to_ascii_lowercase();
    if CURSOR_SHAPES.contains(&lower.as_str()) {
        (lower, false)
    } else {
        ("block".to_string(), true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::ModifiersState;

    #[test]
    fn toggle_shortcut() {
        let comma: Key = Key::Character(",".into());
        let lt: Key = Key::Character("<".into());
        let c: Key = Key::Character("c".into());
        assert!(is_settings_toggle(&comma, &ModifiersState::CONTROL));
        assert!(is_settings_toggle(&comma, &ModifiersState::SUPER));
        assert!(is_settings_toggle(&lt, &ModifiersState::CONTROL));
        assert!(!is_settings_toggle(&c, &ModifiersState::CONTROL));
        assert!(!is_settings_toggle(&comma, &ModifiersState::empty()));
        assert!(!is_settings_toggle(
            &comma,
            &(ModifiersState::CONTROL | ModifiersState::ALT)
        ));
        assert!(!is_settings_toggle(
            &comma,
            &(ModifiersState::CONTROL | ModifiersState::SUPER)
        ));
    }

    #[test]
    fn repair_helpers() {
        assert_eq!(repair_positive_f32(14.0, 14.0), (14.0, false));
        assert_eq!(repair_positive_f32(0.0, 14.0), (14.0, true));
        assert_eq!(repair_positive_f32(f32::NAN, 14.0), (14.0, true));
        assert_eq!(repair_positive_f32(f32::INFINITY, 14.0), (14.0, true));
        assert_eq!(repair_finite_f32(0.0, 2.0), (0.0, false));
        assert_eq!(repair_finite_f32(f32::NAN, 2.0), (2.0, true));
        assert_eq!(repair_positive_f64(7.0, 7.0), (7.0, false));
        assert_eq!(repair_positive_f64(0.0, 7.0), (7.0, true));
        assert_eq!(repair_min_usize(5, 1, 8), (5, false));
        assert_eq!(repair_min_usize(0, 1, 8), (8, true));
        assert_eq!(repair_min_u32(800, 1, 800), (800, false));
        assert_eq!(repair_min_u32(0, 1, 800), (800, true));
        assert_eq!(repair_min_u64(530, 1, 530), (530, false));
        assert_eq!(repair_min_u64(0, 1, 530), (530, true));
        assert_eq!(normalize_min_max(1, 1024, 1, 1024), (1, 1024, false));
        assert_eq!(normalize_min_max(0, 0, 1, 1024), (1, 1024, true));
        assert_eq!(normalize_min_max(9999, 10, 1, 1024), (10, 9999, true));
    }

    #[test]
    fn sanitize_helpers() {
        assert_eq!(sanitize_key_label("C", "c"), "c");
        assert_eq!(sanitize_key_label("", "c"), "c");
        assert_eq!(sanitize_key_label(",,t", "c"), "t");
        assert_eq!(sanitize_cursor_shape("Bar"), ("bar".to_string(), false));
        assert_eq!(sanitize_cursor_shape("beam"), ("block".to_string(), true));
        assert!(is_known_theme("vscode"));
        assert!(!is_known_theme("nope"));
    }

    #[test]
    fn diff_routes_sections() {
        let old = Config::default();
        let mut new = old.clone();
        assert!(!diff_actions(&old, &new).any);
        new.font.size = 18.0;
        let a = diff_actions(&old, &new);
        assert!(a.font && a.any);
        assert!(!a.theme && !a.terminal && !a.window_size && !a.chrome);
        new = old.clone();
        new.theme.name = "vscode".to_string();
        assert!(diff_actions(&old, &new).theme);
        new = old.clone();
        new.window.width = 1024;
        assert!(diff_actions(&old, &new).window_size);
        new = old.clone();
        new.terminal.scrollback_lines = 100;
        assert!(diff_actions(&old, &new).terminal);
        new = old.clone();
        new.shell.shell = "/bin/zsh".to_string();
        let a = diff_actions(&old, &new);
        assert!(a.any);
        assert!(!a.theme && !a.font && !a.terminal && !a.chrome && !a.window_size);
        new = old.clone();
        new.tabbar.height = 48.0;
        assert!(diff_actions(&old, &new).chrome);
        new = old.clone();
        new.log.level = "trace".to_string();
        let a = diff_actions(&old, &new);
        assert!(a.log && a.any);
        assert!(!a.chrome && !a.font && !a.theme);
    }

    #[test]
    fn resets_restore_defaults() {
        let mut c = Config::default();
        c.font.size = 24.0;
        c.theme.name = "vscode".to_string();
        reset_section(&mut c, SettingsSection::Font);
        assert_eq!(c.font.size, 14.0);
        assert_eq!(c.theme.name, "vscode");
        reset_all(&mut c);
        assert_eq!(c, Config::default());
    }

    #[test]
    fn banner_and_path_label() {
        let p = SettingsPanel::new(AppStartup {
            config_path_override: None,
            cli_theme: Some("vscode".to_string()),
            config_corrupt: false,
        });
        assert!(p.cli_banner().unwrap().contains("--theme vscode"));
        assert!(p.config_path_label().contains("cometty"));
        let q = SettingsPanel::new(AppStartup {
            config_path_override: Some(PathBuf::from("/tmp/x.toml")),
            cli_theme: None,
            config_corrupt: false,
        });
        assert!(q.cli_banner().is_none());
        assert_eq!(q.config_path_label(), "/tmp/x.toml");
    }
}
