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
fn d_scrollback_bytes() -> usize {
    128 * 1024 * 1024
}
fn d_max_cells() -> usize {
    1024 * 1024
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
fn d_title_format() -> String {
    "$command | $cwd".to_string()
}
fn d_double_click_ms() -> u64 {
    400
}
fn d_word_regex() -> String {
    DEFAULT_WORD_REGEX.to_string()
}
fn d_log_level() -> String {
    "debug".to_string()
}
fn d_log_filter() -> String {
    String::new()
}
fn d_log_buffer_lines() -> usize {
    1000
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
fn d_key_f() -> String {
    "f".to_string()
}
/// Ctrl+F opens the find bar only off macOS; there Cmd+F is the terminal
/// convention and Ctrl+F belongs to the shell (readline forward-char).
fn d_search_ctrl() -> bool {
    !cfg!(target_os = "macos")
}
fn d_lines_per_tick() -> f64 {
    7.0
}
fn d_pixel_fallback() -> f32 {
    20.0
}
fn d_open_modifier() -> String {
    "auto".to_string()
}
fn d_link_underline() -> String {
    "hover".to_string()
}
fn d_osc52() -> String {
    "write".to_string()
}
fn d_session_restore() -> String {
    "scrollback".to_string()
}

// --- sections ---

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerminalConfig {
    #[serde(default = "d_scrollback")]
    pub scrollback_lines: usize,
    /// Per-tab retained history storage, including capacities and cluster tails.
    #[serde(default = "d_scrollback_bytes")]
    pub scrollback_bytes: usize,
    /// Live cells per buffer, in addition to the per-axis and PTY limits.
    #[serde(default = "d_max_cells")]
    pub max_cells: usize,
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
            scrollback_bytes: d_scrollback_bytes(),
            max_cells: d_max_cells(),
            min_dim: d_min_dim(),
            max_dim: d_max_dim(),
            tab_stop: d_tab_stop(),
            max_title_chars: d_max_title_chars(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// Template for tab labels, interpolated per tab. Variables:
    /// `$title` (OSC 0/1/2), `$command` (foreground process), `$cwd`
    /// (`~`-shortened working directory), `$tab` (1-based index).
    /// `${name}` delimits a variable before more text; `$$` is a literal
    /// `$`; unknown `$name` stays literal. When every referenced variable
    /// is empty the label falls back to `Tab N`.
    #[serde(default = "d_title_format")]
    pub title_format: String,
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
            title_format: d_title_format(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SelectionConfig {
    #[serde(default = "d_double_click_ms")]
    pub double_click_ms: u64,
    /// Match-words regex for double-click selection (default
    /// `"[A-Za-z0-9_]+"`, the old alphanumeric-plus-`word_extra_chars`
    /// behavior). Invalid or empty patterns fall back to the default at
    /// runtime and are repaired on load.
    #[serde(default = "d_word_regex")]
    pub word_regex: String,
    /// Deprecated: pre-regex extra word chars (default `"_"`). Loaded for
    /// migration but never saved (`skip_serializing`); a non-default value
    /// with a default `word_regex` is converted to `[A-Za-z0-9…]+` on load.
    #[serde(default, skip_serializing)]
    pub word_extra_chars: String,
}

/// Default match-words pattern (also the pre-regex alphanumeric + `_` behavior).
pub const DEFAULT_WORD_REGEX: &str = "[A-Za-z0-9_]+";

struct CachedWordRegex {
    pattern: String,
    compiled: Option<regex::Regex>,
}

thread_local! {
    // One entry per calling thread, not an ever-growing cache of edits.
    // Shared by word selection and the settings validation indicator.
    static WORD_REGEX: std::cell::RefCell<Option<CachedWordRegex>> = const { std::cell::RefCell::new(None) };
}

static DEFAULT_COMPILED_WORD_REGEX: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| {
        regex::Regex::new(DEFAULT_WORD_REGEX).expect("default word regex compiles")
    });

impl SelectionConfig {
    /// Effective pattern: `word_regex` when non-empty, else the default.
    pub fn word_pattern(&self) -> &str {
        if self.word_regex.is_empty() {
            DEFAULT_WORD_REGEX
        } else {
            &self.word_regex
        }
    }

    /// Borrow the compiled effective pattern and its validation result.
    /// Failed in-progress edits are cached too, and use the default matcher.
    pub fn with_word_regex<T>(&self, use_regex: impl FnOnce(&regex::Regex, bool) -> T) -> T {
        WORD_REGEX.with(|slot| {
            let mut cache = slot.borrow_mut();
            let pattern = self.word_pattern();
            if cache.as_ref().is_none_or(|entry| entry.pattern != pattern) {
                *cache = Some(CachedWordRegex {
                    pattern: pattern.to_owned(),
                    compiled: regex::Regex::new(pattern).ok(),
                });
            }
            let entry = cache.as_ref().unwrap();
            use_regex(
                entry
                    .compiled
                    .as_ref()
                    .unwrap_or(&DEFAULT_COMPILED_WORD_REGEX),
                entry.compiled.is_some(),
            )
        })
    }

    pub fn word_regex_valid(&self) -> bool {
        self.with_word_regex(|_, valid| valid)
    }
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self {
            double_click_ms: d_double_click_ms(),
            word_regex: d_word_regex(),
            word_extra_chars: String::new(),
        }
    }
}

/// Accepted range for `[log] buffer_lines` (also the settings panel's
/// drag range). The in-app buffer only ever holds this many entries.
pub const MIN_LOG_BUFFER_LINES: usize = 100;
pub const MAX_LOG_BUFFER_LINES: usize = 100_000;
/// Level other crates get in the default in-app filter: their warnings and
/// errors still reach the panel, their debug/trace noise does not.
pub const LOG_DEPS_LEVEL: &str = "warn";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogConfig {
    /// Most verbose level recorded for cometty's own logs (`off`..`trace`).
    /// Stderr output is independent (`RUST_LOG`).
    #[serde(default = "d_log_level")]
    pub level: String,
    /// Raw `RUST_LOG`-style directives for the in-app buffer. Empty =
    /// `cometty=<level>,<LOG_DEPS_LEVEL>`, i.e. app logs at `level` plus
    /// dependency warnings. Set e.g. `cometty=debug,wgpu=debug,warn` to
    /// pull a dependency's detail into the panel.
    #[serde(default = "d_log_filter")]
    pub filter: String,
    /// In-app buffer capacity in entries; oldest entries drop first.
    #[serde(default = "d_log_buffer_lines")]
    pub buffer_lines: usize,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: d_log_level(),
            filter: d_log_filter(),
            buffer_lines: d_log_buffer_lines(),
        }
    }
}

impl LogConfig {
    /// Directive string for the in-app buffer: [`Self::filter`] when set,
    /// else our crate at [`Self::level`] plus dependency warnings.
    pub fn filter_string(&self) -> String {
        let filter = self.filter.trim();
        if filter.is_empty() {
            format!("cometty={},{}", self.level, LOG_DEPS_LEVEL)
        } else {
            filter.to_string()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputConfig {
    #[serde(default = "d_key_c")]
    pub copy_key: String,
    #[serde(default = "d_key_v")]
    pub paste_key: String,
    #[serde(default = "d_key_t")]
    pub new_tab_key: String,
    #[serde(default = "d_key_f")]
    pub search_key: String,
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
    #[serde(default = "d_search_ctrl")]
    pub search_ctrl: bool,
    #[serde(default = "d_true")]
    pub search_super: bool,
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
            search_key: d_key_f(),
            copy_ctrl_shift: true,
            copy_super: true,
            paste_ctrl_shift: true,
            paste_super: true,
            new_tab_ctrl: true,
            new_tab_super: true,
            search_ctrl: d_search_ctrl(),
            search_super: true,
            tab_switch_ctrl: true,
            tab_switch_super: true,
            shift_page_scroll: true,
            lines_per_tick: d_lines_per_tick(),
            pixel_fallback_line_height: d_pixel_fallback(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinksConfig {
    /// Which modifier reveals link hints and opens them on click:
    /// `"auto"` (Cmd on macOS, Ctrl elsewhere), `"cmd"`, `"ctrl"`, `"alt"`,
    /// or `"none"` (hints and click-to-open disabled).
    #[serde(default = "d_open_modifier")]
    pub open_modifier: String,
    /// When link underlines are painted: `"always"`, `"hover"`
    /// (only while the open modifier is held), or `"never"`.
    #[serde(default = "d_link_underline")]
    pub underline: String,
    /// Detect plain-text URLs (`https://…`, `www.…`, `mailto:…`) in
    /// addition to `OSC 8` hyperlinks.
    #[serde(default = "d_true")]
    pub detect_urls: bool,
}

impl Default for LinksConfig {
    fn default() -> Self {
        Self {
            open_modifier: d_open_modifier(),
            underline: d_link_underline(),
            detect_urls: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipboardConfig {
    /// `OSC 52` access: `"off"`, `"write"` (apps may set the clipboard),
    /// or `"read-write"` (apps may also read it back — privacy risk).
    #[serde(default = "d_osc52")]
    pub osc52: String,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self { osc52: d_osc52() }
    }
}

impl ClipboardConfig {
    pub fn allows_write(&self) -> bool {
        matches!(self.osc52.as_str(), "write" | "read-write")
    }

    pub fn allows_read(&self) -> bool {
        self.osc52 == "read-write"
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionConfig {
    /// What quitting remembers for the next launch: `"scrollback"` (tabs
    /// plus each tab's history and screen), `"tabs"` (tab order, working
    /// directories, active tab, window geometry), or `"off"`.
    #[serde(default = "d_session_restore")]
    pub restore: String,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            restore: d_session_restore(),
        }
    }
}

impl SessionConfig {
    pub fn restores_tabs(&self) -> bool {
        matches!(self.restore.as_str(), "scrollback" | "tabs")
    }

    pub fn restores_scrollback(&self) -> bool {
        self.restore == "scrollback"
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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
    #[serde(default)]
    pub links: LinksConfig,
    #[serde(default)]
    pub clipboard: ClipboardConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub log: LogConfig,
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

    /// Atomically persist to `path` (temp file + rename). Creates parent
    /// directories as needed. Never writes invalid TOML: serialization of
    /// `Config` always succeeds, and callers sanitize before saving.
    pub fn save_to_path(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Persist to the default location (`$HOME/.config/cometty/config.toml`).
    #[allow(dead_code)]
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::default_path()
            .ok_or_else(|| anyhow::anyhow!("no HOME set; cannot resolve config path"))?;
        self.save_to_path(&path)
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
        self.terminal.max_cells = self.terminal.max_cells.clamp(1, d_max_cells());
        self.terminal.max_dim = self.terminal.max_dim.min(u16::MAX as usize);
        self.terminal.min_dim = self.terminal.min_dim.min(self.terminal.max_dim);
        if self.terminal.tab_stop == 0 {
            self.terminal.tab_stop = d_tab_stop();
        }
        if self.window.width == 0 {
            self.window.width = d_window_width();
        }
        if self.window.height == 0 {
            self.window.height = d_window_height();
        }
        // Canonical (lowercase) level name so the picker matches the file.
        self.log.level = match self.log.level.parse::<log::LevelFilter>() {
            Ok(level) => level.as_str().to_ascii_lowercase(),
            Err(_) => d_log_level(),
        };
        self.tabbar.title_format = self.tabbar.title_format.trim().to_string();
        // Enum-ish strings: unknown values fall back to the compiled
        // defaults so the picker and the runtime agree.
        self.links.open_modifier = match self
            .links
            .open_modifier
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            known @ ("auto" | "cmd" | "ctrl" | "alt" | "none") => known.to_string(),
            _ => d_open_modifier(),
        };
        self.links.underline = match self.links.underline.trim().to_ascii_lowercase().as_str() {
            known @ ("always" | "hover" | "never") => known.to_string(),
            _ => d_link_underline(),
        };
        self.clipboard.osc52 = match self.clipboard.osc52.trim().to_ascii_lowercase().as_str() {
            known @ ("off" | "write" | "read-write") => known.to_string(),
            _ => d_osc52(),
        };
        self.session.restore = match self.session.restore.trim().to_ascii_lowercase().as_str() {
            known @ ("scrollback" | "tabs" | "off") => known.to_string(),
            _ => d_session_restore(),
        };
        self.log.buffer_lines = self
            .log
            .buffer_lines
            .clamp(MIN_LOG_BUFFER_LINES, MAX_LOG_BUFFER_LINES);
        // Deprecated `word_extra_chars` migration: a non-empty legacy value
        // with a default `word_regex` becomes `[A-Za-z0-9…]+`. Runs before
        // regex validation so a migrated pattern is itself validated.
        if !self.selection.word_extra_chars.is_empty()
            && self.selection.word_regex == DEFAULT_WORD_REGEX
        {
            let legacy = self.selection.word_extra_chars.clone();
            if legacy != "_" {
                let mut class = String::from("A-Za-z0-9");
                for ch in legacy.chars() {
                    if matches!(ch, ']' | '\\' | '^' | '-') {
                        class.push('\\');
                    }
                    class.push(ch);
                }
                self.selection.word_regex = format!("[{class}]+");
            }
        }
        self.selection.word_extra_chars.clear();
        if self.selection.word_regex.is_empty()
            || regex::Regex::new(&self.selection.word_regex).is_err()
        {
            self.selection.word_regex = DEFAULT_WORD_REGEX.to_string();
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
        assert_eq!(c.tabbar.title_format, "$command | $cwd");
        assert_eq!(c.selection.double_click_ms, 400);
        assert_eq!(c.selection.word_regex, "[A-Za-z0-9_]+");
        assert!(c.selection.word_extra_chars.is_empty());
        assert_eq!(c.input.lines_per_tick, 7.0);
        assert_eq!(c.input.search_key, "f");
        assert_eq!(c.input.search_ctrl, !cfg!(target_os = "macos"));
        assert!(c.input.search_super);
        assert_eq!(c.links.open_modifier, "auto");
        assert_eq!(c.links.underline, "hover");
        assert!(c.links.detect_urls);
        assert_eq!(c.clipboard.osc52, "write");
        assert!(c.clipboard.allows_write() && !c.clipboard.allows_read());
        assert_eq!(c.session.restore, "scrollback");
        assert!(c.session.restores_tabs() && c.session.restores_scrollback());
        assert_eq!(c.log.level, "debug");
        assert_eq!(c.log.filter, "");
        assert_eq!(c.log.buffer_lines, 1000);
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
    fn tabbar_title_format_parses_and_trims() {
        let c: Config = toml::from_str("[tabbar]\ntitle_format=\"  $command [$cwd]  \"\n").unwrap();
        assert_eq!(c.tabbar.max_label_chars, d_max_label_chars());
        assert_eq!(c.sanitized().tabbar.title_format, "$command [$cwd]");
        // Missing key keeps the default; an empty template is a valid
        // (if degenerate) choice that falls back to `Tab N`.
        assert_eq!(Config::default().tabbar.title_format, "$command | $cwd");
        let c: Config = toml::from_str("[tabbar]\ntitle_format=\"\"\n").unwrap();
        assert_eq!(c.sanitized().tabbar.title_format, "");
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

    #[test]
    fn corrupt_toml_fails_to_load() {
        assert!(toml::from_str::<Config>("not [valid").is_err());
        assert!(toml::from_str::<Config>("font = 42").is_err());
    }

    #[test]
    fn log_section_sanitizes_level_and_clamps_lines() {
        let c: Config = toml::from_str("[log]\nlevel=\"LOUD\"\nbuffer_lines=7\n").unwrap();
        assert_eq!(
            c.log.level, "LOUD",
            "unknown names survive parsing for repair"
        );
        assert_eq!(c.log.buffer_lines, 7);
        let s = c.sanitized();
        assert_eq!(s.log.level, "debug");
        assert_eq!(s.log.buffer_lines, MIN_LOG_BUFFER_LINES);

        // Known names are canonicalized for the picker, whatever the case.
        let c: Config = toml::from_str("[log]\nlevel=\"WARN\"\n").unwrap();
        assert_eq!(c.sanitized().log.level, "warn");

        let mut huge = Config::default();
        huge.log.buffer_lines = usize::MAX;
        assert_eq!(huge.sanitized().log.buffer_lines, MAX_LOG_BUFFER_LINES);
    }

    #[test]
    fn log_filter_string_derives_from_level_until_overridden() {
        let mut c = Config::default();
        assert_eq!(c.log.filter_string(), "cometty=debug,warn");
        c.log.level = "trace".to_string();
        assert_eq!(c.log.filter_string(), "cometty=trace,warn");
        c.log.filter = "  cometty=trace,wgpu=info  ".to_string();
        assert_eq!(c.log.filter_string(), "cometty=trace,wgpu=info");
    }

    #[test]
    fn unknown_theme_name_loads_for_panel_fallback() {
        let c: Config = toml::from_str("[theme]\nname=\"solarized\"\n").unwrap();
        assert_eq!(c.theme.name, "solarized");
        assert_eq!(c.font.size, 14.0);
    }

    #[test]
    fn links_and_clipboard_sanitize_enums() {
        let c: Config = toml::from_str(
            "[links]\nopen_modifier=\"CMD\"\nunderline=\"weird\"\ndetect_urls=false\n[clipboard]\nosc52=\"read-write\"\n",
        )
        .unwrap();
        assert_eq!(c.links.open_modifier, "CMD", "raw value kept for repair");
        assert_eq!(c.links.underline, "weird");
        assert!(!c.links.detect_urls);
        let s = c.sanitized();
        assert_eq!(s.links.open_modifier, "cmd");
        assert_eq!(s.links.underline, "hover", "unknown value repaired");
        assert!(!s.links.detect_urls, "boolean survives sanitize");
        assert_eq!(s.clipboard.osc52, "read-write");
        assert!(s.clipboard.allows_read());
        // Unknown osc52 mode falls back to write-only.
        let bad: Config = toml::from_str("[clipboard]\nosc52=\"loud\"\n").unwrap();
        assert_eq!(bad.sanitized().clipboard.osc52, "write");
        let off: Config = toml::from_str("[session]\nrestore=\"OFF\"\n").unwrap();
        assert!(!off.sanitized().session.restores_tabs());
        let tabs: Config = toml::from_str("[session]\nrestore=\"Tabs\"\n").unwrap();
        let tabs = tabs.sanitized().session;
        assert!(tabs.restores_tabs() && !tabs.restores_scrollback());
        let bad: Config = toml::from_str("[session]\nrestore=\"all\"\n").unwrap();
        assert_eq!(bad.sanitized().session.restore, "scrollback");
    }

    #[test]
    fn selection_word_regex_sanitizes_and_migrates() {
        // Invalid and empty patterns repair to the default.
        let bad: Config = toml::from_str("[selection]\nword_regex=\"([a-z\"\n").unwrap();
        assert_eq!(bad.sanitized().selection.word_regex, DEFAULT_WORD_REGEX);
        let mut empty = Config::default();
        empty.selection.word_regex.clear();
        assert_eq!(empty.sanitized().selection.word_regex, DEFAULT_WORD_REGEX);
        // Custom patterns survive.
        let custom: Config = toml::from_str("[selection]\nword_regex=\"[a-z]+\"\n").unwrap();
        assert_eq!(custom.sanitized().selection.word_regex, "[a-z]+");
        // Legacy `word_extra_chars` migrates to a char class.
        let legacy: Config = toml::from_str("[selection]\nword_extra_chars=\"_-\"\n").unwrap();
        let s = legacy.sanitized();
        assert!(s.selection.word_extra_chars.is_empty());
        let re = regex::Regex::new(&s.selection.word_regex).unwrap();
        assert!(re.is_match("_"));
        assert!(re.is_match("-"));
        assert!(re.is_match("a"));
        assert!(!re.is_match(" "));
        // A set `word_regex` wins over the legacy field.
        let both: Config =
            toml::from_str("[selection]\nword_regex=\"[a-z]+\"\nword_extra_chars=\"_-\"\n")
                .unwrap();
        assert_eq!(both.sanitized().selection.word_regex, "[a-z]+");
    }

    #[test]
    fn save_roundtrips_all_sections() {
        let mut c = Config::default();
        c.theme.name = "vscode".to_string();
        c.font.size = 16.0;
        c.window.title = "test".to_string();
        c.shell.shell = "/bin/zsh".to_string();
        let dir = std::env::temp_dir().join(format!("cometty-test-{}", std::process::id()));
        let path = dir.join("config.toml");
        c.save_to_path(&path).unwrap();
        assert!(!path.with_extension("toml.tmp").exists());
        let loaded = Config::load_from_path(&path).unwrap();
        assert_eq!(loaded.theme.name, "vscode");
        assert_eq!(loaded.font.size, 16.0);
        assert_eq!(loaded.window.title, "test");
        assert_eq!(loaded.shell.shell, "/bin/zsh");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_creates_parent_dirs() {
        let c = Config::default();
        let dir = std::env::temp_dir().join(format!(
            "cometty-test-nested-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let path = dir.join("a").join("b").join("config.toml");
        c.save_to_path(&path).unwrap();
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
