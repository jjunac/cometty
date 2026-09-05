//! Color themes for cometty.
//!
//! All color decisions live here so supporting a new theme later only means
//! adding a `Theme` constructor (and eventually a `--theme` flag / config
//! lookup via [`Theme::from_name`]). The rest of the codebase (`grid`, `term`,
//! `renderer`) only reads colors through `Theme`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn as_f32_array(&self) -> [f32; 3] {
        [
            self.r as f32 / 255.0,
            self.g as f32 / 255.0,
            self.b as f32 / 255.0,
        ]
    }

    /// sRGB bytes as linear-light values for the GPU.
    ///
    /// The wgpu surface is an sRGB format, so fragment output and clear
    /// colors are treated as linear and encoded to sRGB on store. Passing
    /// the naive `c / 255.0` values would double-apply the transfer curve
    /// and render dark colors (e.g. `#1E1E1E`) much too light.
    pub fn as_linear_f32_array(&self) -> [f32; 3] {
        fn to_linear(c: f32) -> f32 {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        let [r, g, b] = self.as_f32_array();
        [to_linear(r), to_linear(g), to_linear(b)]
    }

    pub fn as_glyphon_color(&self) -> glyphon::Color {
        glyphon::Color::rgb(self.r, self.g, self.b)
    }

    pub fn as_egui_color(&self) -> egui::Color32 {
        egui::Color32::from_rgb(self.r, self.g, self.b)
    }
}

/// A full terminal color scheme.
///
/// `palette[0..8]` are the normal ANSI colors, `palette[8..16]` the bright
/// variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub name: &'static str,
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor_bg: Rgb,
    pub cursor_fg: Rgb,
    pub palette: [Rgb; 16],
    pub scrollbar_track: Rgb,
    pub scrollbar_thumb: Rgb,
    pub scrollbar_hover: Rgb,
}

impl Theme {
    /// VS Code Dark+ integrated terminal defaults (`dark` column of
    /// `terminal.ansi*` in `terminalColorRegistry.ts`, plus `terminal.foreground`
    /// `#CCCCCC` and the Dark+ editor background `#1E1E1E`).
    pub const fn vscode() -> Self {
        Self {
            name: "vscode",
            foreground: Rgb::new(0xCC, 0xCC, 0xCC),
            background: Rgb::new(0x1E, 0x1E, 0x1E),
            cursor_bg: Rgb::new(0xCC, 0xCC, 0xCC),
            cursor_fg: Rgb::new(0x1E, 0x1E, 0x1E),
            scrollbar_track: Rgb::new(0x2D, 0x2D, 0x2D),
            scrollbar_thumb: Rgb::new(0x5A, 0x5A, 0x5A),
            scrollbar_hover: Rgb::new(0xCC, 0xCC, 0xCC),
            palette: [
                Rgb::new(0x00, 0x00, 0x00), // black
                Rgb::new(0xCD, 0x31, 0x31), // red
                Rgb::new(0x0D, 0xBC, 0x79), // green
                Rgb::new(0xE5, 0xE5, 0x10), // yellow
                Rgb::new(0x24, 0x72, 0xC8), // blue
                Rgb::new(0xBC, 0x3F, 0xBC), // magenta
                Rgb::new(0x11, 0xA8, 0xCD), // cyan
                Rgb::new(0xE5, 0xE5, 0xE5), // white
                Rgb::new(0x66, 0x66, 0x66), // bright black
                Rgb::new(0xF1, 0x4C, 0x4C), // bright red
                Rgb::new(0x23, 0xD1, 0x8B), // bright green
                Rgb::new(0xF5, 0xF5, 0x43), // bright yellow
                Rgb::new(0x3B, 0x8E, 0xEA), // bright blue
                Rgb::new(0xD6, 0x70, 0xD6), // bright magenta
                Rgb::new(0x29, 0xB8, 0xDB), // bright cyan
                Rgb::new(0xE5, 0xE5, 0xE5), // bright white
            ],
        }
    }

    /// Tokyo Night: dark blue-grey background with high-contrast foreground.
    /// Default theme.
    pub const fn tokyo_night() -> Self {
        Self {
            name: "tokyo-night",
            foreground: Rgb::new(0xC0, 0xCA, 0xF5),
            background: Rgb::new(0x1A, 0x1B, 0x26),
            cursor_bg: Rgb::new(0xC0, 0xCA, 0xF5),
            cursor_fg: Rgb::new(0x1A, 0x1B, 0x26),
            scrollbar_track: Rgb::new(0x1F, 0x23, 0x35),
            scrollbar_thumb: Rgb::new(0x41, 0x48, 0x68),
            scrollbar_hover: Rgb::new(0xC0, 0xCA, 0xF5),
            palette: [
                Rgb::new(0x15, 0x16, 0x1E), // black
                Rgb::new(0xF7, 0x76, 0x8E), // red
                Rgb::new(0x9E, 0xCE, 0x6A), // green
                Rgb::new(0xE0, 0xAF, 0x68), // yellow
                Rgb::new(0x7A, 0xA2, 0xF7), // blue
                Rgb::new(0xBB, 0x9A, 0xF7), // magenta
                Rgb::new(0x7D, 0xCF, 0xFF), // cyan
                Rgb::new(0xA9, 0xB1, 0xD6), // white
                Rgb::new(0x41, 0x48, 0x68), // bright black
                Rgb::new(0xF7, 0x76, 0x8E), // bright red
                Rgb::new(0x9E, 0xCE, 0x6A), // bright green
                Rgb::new(0xE0, 0xAF, 0x68), // bright yellow
                Rgb::new(0x7A, 0xA2, 0xF7), // bright blue
                Rgb::new(0xBB, 0x9A, 0xF7), // bright magenta
                Rgb::new(0x7D, 0xCF, 0xFF), // bright cyan
                Rgb::new(0xC0, 0xCA, 0xF5), // bright white
            ],
        }
    }

    /// Previous built-in colors (Tomorrow Night), kept for fallback/tests.
    #[allow(dead_code)]
    pub const fn tomorrow_night() -> Self {
        Self {
            name: "tomorrow-night",
            foreground: Rgb::new(0xC5, 0xC8, 0xC6),
            background: Rgb::new(0x1D, 0x1F, 0x21),
            cursor_bg: Rgb::new(0xC5, 0xC8, 0xC6),
            cursor_fg: Rgb::new(0x1D, 0x1F, 0x21),
            scrollbar_track: Rgb::new(0x28, 0x2A, 0x2E),
            scrollbar_thumb: Rgb::new(0x96, 0x98, 0x96),
            scrollbar_hover: Rgb::new(0xC5, 0xC8, 0xC6),
            palette: [
                Rgb::new(0x1D, 0x1F, 0x21),
                Rgb::new(0xCC, 0x66, 0x66),
                Rgb::new(0xB5, 0xBD, 0x68),
                Rgb::new(0xF0, 0xC6, 0x74),
                Rgb::new(0x81, 0xA2, 0xBE),
                Rgb::new(0xB2, 0x94, 0xBB),
                Rgb::new(0x8A, 0xBE, 0xB7),
                Rgb::new(0xC5, 0xC8, 0xC6),
                Rgb::new(0x96, 0x98, 0x96),
                Rgb::new(0xCC, 0x66, 0x66),
                Rgb::new(0xB5, 0xBD, 0x68),
                Rgb::new(0xF0, 0xC6, 0x74),
                Rgb::new(0x81, 0xA2, 0xBE),
                Rgb::new(0xB2, 0x94, 0xBB),
                Rgb::new(0x8A, 0xBE, 0xB7),
                Rgb::new(0xFF, 0xFF, 0xFF),
            ],
        }
    }

    /// Look up a theme by name. Used by future `--theme` / config support.
    #[allow(dead_code)]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "tokyo-night" | "tokyo_night" | "default" => Some(Self::tokyo_night()),
            "vscode" | "dark-plus" | "dark_plus" => Some(Self::vscode()),
            "tomorrow-night" | "tomorrow_night" => Some(Self::tomorrow_night()),
            _ => None,
        }
    }

    /// All built-in themes, for future theme listing/switching UI.
    #[allow(dead_code)]
    pub fn all() -> [Self; 3] {
        [Self::vscode(), Self::tokyo_night(), Self::tomorrow_night()]
    }

    pub fn ansi(&self, index: u8) -> Rgb {
        if (index as usize) < self.palette.len() {
            self.palette[index as usize]
        } else {
            self.foreground
        }
    }

    /// One-way bridge for egui chrome (tab bar, scrollbar, dialogs).
    /// Terminal cells keep using glyphon/cosmic-text; only egui widgets
    /// read through this.
    pub fn to_egui_visuals(self) -> egui::Visuals {
        let mut visuals = egui::Visuals::dark();
        visuals.window_fill = self.background.as_egui_color();
        visuals.panel_fill = self.background.as_egui_color();
        visuals.faint_bg_color = self.scrollbar_track.as_egui_color();
        visuals.extreme_bg_color = self.background.as_egui_color();
        visuals.code_bg_color = self.background.as_egui_color();
        visuals.override_text_color = Some(self.foreground.as_egui_color());
        visuals.selection.bg_fill = self.scrollbar_thumb.as_egui_color();
        visuals.selection.stroke.color = self.foreground.as_egui_color();
        visuals
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::tokyo_night()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_falls_back_to_foreground() {
        let theme = Theme::vscode();
        assert_eq!(theme.ansi(1), theme.palette[1]);
        assert_eq!(theme.ansi(200), theme.foreground);
    }

    #[test]
    fn linear_conversion_matches_srgb_transfer_function() {
        assert_eq!(Rgb::new(0, 0, 0).as_linear_f32_array(), [0.0, 0.0, 0.0]);
        assert_eq!(
            Rgb::new(255, 255, 255).as_linear_f32_array(),
            [1.0, 1.0, 1.0]
        );
        // #1E1E1E: naive sRGB 0.118 would render ~9x too bright without
        // conversion; linear must be ~0.013.
        let [r, g, b] = Rgb::new(0x1E, 0x1E, 0x1E).as_linear_f32_array();
        assert!((r - 0.013).abs() < 0.002, "r={r}");
        assert!((g - 0.013).abs() < 0.002, "g={g}");
        assert!((b - 0.013).abs() < 0.002, "b={b}");
    }

    #[test]
    fn vscode_matches_upstream_dark_defaults() {
        let theme = Theme::vscode();
        assert_eq!(theme.foreground, Rgb::new(0xCC, 0xCC, 0xCC));
        assert_eq!(theme.background, Rgb::new(0x1E, 0x1E, 0x1E));
        assert_eq!(theme.palette[0], Rgb::new(0x00, 0x00, 0x00));
        assert_eq!(theme.palette[1], Rgb::new(0xCD, 0x31, 0x31));
        assert_eq!(theme.palette[2], Rgb::new(0x0D, 0xBC, 0x79));
        assert_eq!(theme.palette[9], Rgb::new(0xF1, 0x4C, 0x4C));
    }

    #[test]
    fn default_is_tokyo_night() {
        assert_eq!(Theme::default(), Theme::tokyo_night());
    }

    #[test]
    fn from_name_resolves_builtins() {
        assert_eq!(Theme::from_name("vscode"), Some(Theme::vscode()));
        assert_eq!(Theme::from_name("default"), Some(Theme::tokyo_night()));
        assert_eq!(Theme::from_name("tokyo-night"), Some(Theme::tokyo_night()));
        assert_eq!(
            Theme::from_name("tomorrow-night"),
            Some(Theme::tomorrow_night())
        );
        assert_eq!(Theme::from_name("nope"), None);
    }
}
