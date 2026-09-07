//! Centralized user configuration (TOML file + defaults).
//!
//! All user-tunable values live here so a future settings UI only edits
//! [`Config`]. Defaults preserve the historical hardcoded behavior.
//! Precedence: `defaults < file < CLI flags` (`--theme`, `--config`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// --- default fns (used by `Default` impls and serde) ---

fn d_theme_name() -> String {
    "tokyo-night".to_string()
}
fn d_font_family() -> String {
    "monospace".to_string()
}
fn d_font_size() -> f32 {
    14.0
}
fn d_line_factor() -> f32 {
    1.25
}
fn d_cell_factor() -> f32 {
    0.602
}
fn d_underline_factor() -> f32 {
    0.07
}
fn d_window_title() -> String {
    "cometty".to_string()
}
fn d_window_width() -> u32 {
    800
}
fn d_window_height() -> u32 {
    600
}
fn d_scrollback() -> usize {
    10_000
}
fn d_min_dim() -> usize {
    1
}
fn d_max_dim() -> usize {
    1024
}
fn d_tab_stop() -> usize {
    8
}
fn d_max_title_chars() -> usize {
    256
}
fn d_shell() -> String {
    String::new()
}
fn d_term() -> String {
    "xterm-256color".to_string()
}
fn d_cwd() -> String {
    String::new()
}
fn d_blink_ms() -> u64 {
    530
}
fn d_cursor_shape() -> String {
    "block".to_string()
}
fn d_true() -> bool {
    true
}
fn d_cursor_underline_factor() -> f32 {
    0.14
}
fn d_bar_factor() -> f32 {
    0.3
}
fn d_track_width() -> f32 {
    10.0
}
fn d_min_thumb() -> f32 {
    20.0
}
fn d_pad() -> f32 {
    2.0
}
fn d_fade_delay_ms() -> u64 {
    800
}
fn d_fade_speed() -> f32 {
    5.0
}
fn d_tabbar_height() -> f32 {
    38.0
}
fn d_min_tab_width() -> f32 {
    100.0
}
fn d_corner_tab() -> u8 {
    8
}
fn d_corner_bar() -> u8 {
    10
}
fn d_inset_x() -> f32 {
    8.0
}
fn d_inset_y() -> f32 {
    5.0
}
fn d_close_reserve() -> f32 {
    24.0
}
fn d_shortcut_reserve() -> f32 {
    34.0
}
fn d_gap() -> f32 {
    4.0
}
fn d_plus_d() -> f32 {
    28.0
}
fn d_plus_gap() -> f32 {
    8.0
}
fn d_max_label_chars() -> usize {
    32
}
fn d_double_click_ms() -> u64 {
    400
}
fn d_word_extra() -> String {
    "_".to_string()
}
fn d_key_c() -> String {
    "c".to_string()
}
fn d_key_v() -> String {
    "v".to_string()
}
fn d_key_t() -> String {
    "t".to_string()
}
fn d_lines_per_tick() -> f64 {
    7.0
}
fn d_pixel_fallback() -> f32 {
    20.0
}

// --- sections ---

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThemeConfig {
    #[serde(default = "d_theme_name")]
    pub name: String,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            name: d_theme_name(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FontConfig {
    #[serde(default = "d_font_family")]
    pub family: String,
    #[serde(default = "d_font_size")]
    pub size: f32,
    #[serde(default = "d_line_factor")]
    pub line_height_factor: f32,
    #[serde(default = "d_cell_factor")]
    pub cell_width_factor: f32,
    #[serde(default = "d_underline_factor")]
    pub underline_factor: f32,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: d_font_family(),
            size: d_font_size(),
            line_height_factor: d_line_factor(),
            cell_width_factor: d_cell_factor(),
            underline_factor: d_underline_factor(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowConfig {
    #[serde(default = "d_window_title")]
    pub title: String,
    #[serde(default = "d_window_width")]
    pub width: u32,
    #[serde(default = "d_window_height")]
    pub height: u32,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: d_window_title(),
            width: d_window_width(),
            height: d_window_height(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TerminalConfig {
    #[serde(default = "d_scrollback")]
    pub scrollback_lines: usize,
    #[serde(default = "d_min_dim")]
    pub min_dim: usize,
    #[serde(default = "d_max_dim")]
    pub max_dim: usize,
    #[serde(default = "d_tab_stop")]
    pub tab_stop: usize,
    #[serde(default = "d_max_title_chars")]
    pub max_title_chars: usize,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            scrollback_lines: d_scrollback(),
            min_dim: d_min_dim(),
            max_dim: d_max_dim(),
            tab_stop: d_tab_stop(),
            max_title_chars: d_max_title_chars(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShellConfig {
    /// Empty = auto (`$SHELL` or `/bin/bash`, `COMSPEC` on Windows).
    #[serde(default = "d_shell")]
    pub shell: String,
    #[serde(default = "d_term")]
    pub term: String,
    /// Empty = `$HOME` or `/`.
    #[serde(default = "d_cwd")]
    pub cwd: String,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            shell: d_shell(),
            term: d_term(),
            cwd: d_cwd(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CursorConfig {
    #[serde(default = "d_blink_ms")]
    pub blink_ms: u64,
    /// `"block" | "underline" | "bar"` — initial DECSCUSR shape.
    #[serde(default = "d_cursor_shape")]
    pub default_shape: String,
    #[serde(default = "d_true")]
    pub default_blinking: bool,
    #[serde(default = "d_cursor_underline_factor")]
    pub underline_factor: f32,
    #[serde(default = "d_bar_factor")]
    pub bar_width_factor: f32,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            blink_ms: d_blink_ms(),
            default_shape: d_cursor_shape(),
            default_blinking: true,
            underline_factor: d_cursor_underline_factor(),
            bar_width_factor: d_bar_factor(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScrollbarConfig {
    #[serde(default = "d_track_width")]
    pub track_width: f32,
    #[serde(default = "d_min_thumb")]
    pub min_thumb: f32,
    #[serde(default = "d_pad")]
    pub pad: f32,
    #[serde(default = "d_fade_delay_ms")]
    pub fade_delay_ms: u64,
    #[serde(default = "d_fade_speed")]
    pub fade_speed: f32,
}

impl Default for ScrollbarConfig {
    fn default() -> Self {
        Self {
            track_width: d_track_width(),
            min_thumb: d_min_thumb(),
            pad: d_pad(),
            fade_delay_ms: d_fade_delay_ms(),
            fade_speed: d_fade_speed(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TabbarConfig {
    #[serde(default = "d_tabbar_height")]
    pub height: f32,
    #[serde(default = "d_min_tab_width")]
    pub min_tab_width: f32,
    #[serde(default = "d_corner_tab")]
    pub corner_radius_tab: u8,
    #[serde(default = "d_corner_bar")]
    pub corner_radius_bar: u8,
    #[serde(default = "d_inset_x")]
    pub inset_x: f32,
    #[serde(default = "d_inset_y")]
    pub inset_y: f32,
    #[serde(default = "d_close_reserve")]
    pub close_reserve: f32,
    #[serde(default = "d_shortcut_reserve")]
    pub shortcut_reserve: f32,
    #[serde(default = "d_gap")]
    pub gap: f32,
    #[serde(default = "d_plus_d")]
    pub plus_diameter: f32,
    #[serde(default = "d_plus_gap")]
    pub plus_gap: f32,
    #[serde(default = "d_max_label_chars")]
    pub max_label_chars: usize,
}

impl Default for TabbarConfig {
    fn default() -> Self {
        Self {
            height: d_tabbar_height(),
            min_tab_width: d_min_tab_width(),
            corner_radius_tab: d_corner_tab(),
            corner_radius_bar: d_corner_bar(),
            inset_x: d_inset_x(),
            inset_y: d_inset_y(),
            close_reserve: d_close_reserve(),
            shortcut_reserve: d_shortcut_reserve(),
            gap: d_gap(),
            plus_diameter: d_plus_d(),
            plus_gap: d_plus_gap(),
            max_label_chars: d_max_label_chars(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SelectionConfig {
    #[serde(default = "d_double_click_ms")]
    pub double_click_ms: u64,
    /// Extra word chars beyond ASCII alphanumeric (default `"_"`).
    #[serde(default = "d_word_extra")]
    pub word_extra_chars: String,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self {
            double_click_ms: d_double_click_ms(),
            word_extra_chars: d_word_extra(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputConfig {
    #[serde(default = "d_key_c")]
    pub copy_key: String,
    #[serde(default = "d_key_v")]
    pub paste_key: String,
    #[serde(default = "d_key_t")]
    pub new_tab_key: String,
    #[serde(default = "d_true")]
    pub copy_ctrl_shift: bool,
    #[serde(default = "d_true")]
    pub copy_super: bool,
    #[serde(default = "d_true")]
    pub paste_ctrl_shift: bool,
    #[serde(default = "d_true")]
    pub paste_super: bool,
    #[serde(default = "d_true")]
    pub new_tab_ctrl: bool,
    #[serde(default = "d_true")]
    pub new_tab_super: bool,
    #[serde(default = "d_true")]
    pub tab_switch_ctrl: bool,
    #[serde(default = "d_true")]
    pub tab_switch_super: bool,
    #[serde(default = "d_true")]
    pub shift_page_scroll: bool,
    #[serde(default = "d_lines_per_tick")]
    pub lines_per_tick: f64,
    #[serde(default = "d_pixel_fallback")]
    pub pixel_fallback_line_height: f32,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            copy_key: d_key_c(),
            paste_key: d_key_v(),
            new_tab_key: d_key_t(),
            copy_ctrl_shift: true,
            copy_super: true,
            paste_ctrl_shift: true,
            paste_super: true,
            new_tab_ctrl: true,
            new_tab_super: true,
            tab_switch_ctrl: true,
            tab_switch_super: true,
            shift_page_scroll: true,
            lines_per_tick: d_lines_per_tick(),
            pixel_fallback_line_height: d_pixel_fallback(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub theme: ThemeConfig,
    #[serde(default)]
    pub font: FontConfig,
    #[serde(default)]
    pub window: WindowConfig,
    #[serde(default)]
    pub terminal: TerminalConfig,
    #[serde(default)]
    pub shell: ShellConfig,
    #[serde(default)]
    pub cursor: CursorConfig,
    #[serde(default)]
    pub scrollbar: ScrollbarConfig,
    #[serde(default)]
    pub tabbar: TabbarConfig,
    #[serde(default)]
    pub selection: SelectionConfig,
    #[serde(default)]
    pub input: InputConfig,
}

impl Config {
    /// Default config file location: `$HOME/.config/cometty/config.toml`.
    pub fn default_path() -> Option<PathBuf> {
        std::env::var("HOME")
            .ok()
            .map(|h| PathBuf::from(h).join(".config/cometty/config.toml"))
    }

    /// Load from the default path. Missing/unreadable file = defaults.
    pub fn load() -> Self {
        Self::default_path()
            .and_then(|p| Self::load_from_path(&p).ok())
            .unwrap_or_default()
    }

    pub fn load_from_path(path: &std::path::Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let cfg: Self = toml::from_str(&text)?;
        Ok(cfg.sanitized())
    }

    /// Clamp obviously degenerate values so a bad file can't break layout.
    fn sanitized(mut self) -> Self {
        if !self.font.size.is_finite() || self.font.size <= 0.0 {
            self.font.size = d_font_size();
        }
        if !self.font.line_height_factor.is_finite() || self.font.line_height_factor <= 0.0 {
            self.font.line_height_factor = d_line_factor();
        }
        if !self.font.cell_width_factor.is_finite() || self.font.cell_width_factor <= 0.0 {
            self.font.cell_width_factor = d_cell_factor();
        }
        if self.terminal.max_dim == 0 {
            self.terminal.max_dim = d_max_dim();
        }
        if self.terminal.min_dim == 0 {
            self.terminal.min_dim = 1;
        }
        if self.terminal.min_dim > self.terminal.max_dim {
            std::mem::swap(&mut self.terminal.min_dim, &mut self.terminal.max_dim);
        }
        if self.terminal.tab_stop == 0 {
            self.terminal.tab_stop = d_tab_stop();
        }
        if self.window.width == 0 {
            self.window.width = d_window_width();
        }
        if self.window.height == 0 {
            self.window.height = d_window_height();
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_historical_hardcodes() {
        let c = Config::default();
        assert_eq!(c.theme.name, "tokyo-night");
        assert_eq!(c.font.size, 14.0);
        assert_eq!(c.font.line_height_factor, 1.25);
        assert_eq!(c.font.cell_width_factor, 0.602);
        assert_eq!(c.window.title, "cometty");
        assert_eq!((c.window.width, c.window.height), (800, 600));
        assert_eq!(c.terminal.scrollback_lines, 10_000);
        assert_eq!((c.terminal.min_dim, c.terminal.max_dim), (1, 1024));
        assert_eq!(c.terminal.tab_stop, 8);
        assert_eq!(c.shell.term, "xterm-256color");
        assert_eq!(c.cursor.blink_ms, 530);
        assert_eq!(c.scrollbar.track_width, 10.0);
        assert_eq!(c.scrollbar.min_thumb, 20.0);
        assert_eq!(c.scrollbar.pad, 2.0);
        assert_eq!(c.scrollbar.fade_delay_ms, 800);
        assert_eq!(c.scrollbar.fade_speed, 5.0);
        assert_eq!(c.tabbar.height, 38.0);
        assert_eq!(c.tabbar.min_tab_width, 100.0);
        assert_eq!(c.selection.double_click_ms, 400);
        assert_eq!(c.selection.word_extra_chars, "_");
        assert_eq!(c.input.lines_per_tick, 7.0);
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let c: Config = toml::from_str(
            "[theme]\nname=\"vscode\"\n[font]\nsize=16.0\n[input]\ncopy_key=\"c\"\n",
        )
        .unwrap();
        assert_eq!(c.theme.name, "vscode");
        assert_eq!(c.font.size, 16.0);
        assert_eq!(c.font.line_height_factor, 1.25);
        assert_eq!(c.window.title, "cometty");
        assert!(c.input.copy_ctrl_shift);
    }

    #[test]
    fn sanitized_repairs_degenerate() {
        let mut c = Config::default();
        c.font.size = 0.0;
        c.terminal.min_dim = 9999;
        c.terminal.max_dim = 10;
        let s = c.sanitized();
        assert_eq!(s.font.size, 14.0);
        assert!(s.terminal.min_dim <= s.terminal.max_dim);
    }
}
