//! Settings panel widgets (egui overlay).
//!
//! Drawn inside the overlay's single egui pass: the entry point lives in
//! the native OS menu bar (`Cometty > Settings`, plus `Ctrl+,` / `Cmd+,`),
//! so this module only draws the sidebar + detail window when open. All
//! edits mutate [`Config`] in place and report back through `changed`; the
//! app live-applies the diff and auto-saves afterwards.

use crate::app::settings::{
    CURSOR_SHAPES, FONT_FAMILIES, KNOWN_THEMES, LOG_LEVELS, SettingsPanel, SettingsSection,
    is_known_theme, normalize_min_max, repair_finite_f32, repair_min_u32, repair_min_u64,
    repair_min_usize, repair_positive_f32, repair_positive_f64, reset_all, reset_section,
    sanitize_cursor_shape, sanitize_key_label,
};
use crate::config::{Config, MAX_LOG_BUFFER_LINES, MIN_LOG_BUFFER_LINES};

/// Paint the settings window when open (nothing when closed — the native
/// menu bar owns the entry point).
/// Returns true when a widget mutated `config`.
pub(crate) fn show_settings(
    ctx: &egui::Context,
    settings: &mut SettingsPanel,
    config: &mut Config,
) -> bool {
    if !settings.open {
        return false;
    }

    let mut changed = false;
    // Fixed centered dialog: 80% of the main frame (= 10% margin on every
    // side), never movable/resizable so switching sections can't shift it.
    // `open` binding gives the title bar its `x`; `collapsible(false)`
    // removes the collapse triangle. The bottom `Close` button stays as a
    // redundant affordance alongside `Esc`.
    let mut open = true;
    let area = ctx.viewport_rect();
    // Safety minimum so the sidebar + detail columns stay usable; if the
    // main frame is smaller than that, shrink to fit with a tiny padding
    // instead of overflowing.
    const MIN_SIZE: egui::Vec2 = egui::vec2(560.0, 400.0);
    const EDGE_PAD: f32 = 8.0;
    let max_size = egui::vec2(
        (area.width() - EDGE_PAD * 2.0).max(1.0),
        (area.height() - EDGE_PAD * 2.0).max(1.0),
    );
    let size = egui::vec2(area.width() * 0.8, area.height() * 0.8)
        .max(MIN_SIZE)
        .min(max_size);
    let pos = area.center() - size * 0.5;
    egui::Window::new("Settings")
        .id(egui::Id::new("settings-window"))
        .fixed_pos(pos)
        .fixed_size(size)
        .resizable(false)
        .movable(false)
        .collapsible(false)
        .open(&mut open)
        .show(ctx, |ui| {
            if let Some(banner) = settings.cli_banner() {
                ui.colored_label(egui::Color32::YELLOW, banner);
            }
            if settings.corrupt_at_startup {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "Config file was unreadable at startup; showing defaults. \
                     Edits will overwrite it on save.",
                );
            }
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_min_width(104.0);
                    for s in SettingsSection::ALL {
                        if ui
                            .selectable_label(settings.section == s, s.title())
                            .clicked()
                        {
                            settings.section = s;
                            settings.notice = None;
                        }
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            let section = settings.section;
                            ui.horizontal(|ui| {
                                ui.heading(section.title());
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .small_button("Reset section")
                                            .on_hover_text(format!(
                                                "Reset {} to defaults",
                                                section.title()
                                            ))
                                            .clicked()
                                        {
                                            reset_section(config, section);
                                            settings.notice = Some(format!(
                                                "{} reset to defaults",
                                                section.title()
                                            ));
                                            changed = true;
                                        }
                                    },
                                );
                            });
                            ui.separator();
                            let cli_override = settings.cli_theme_override.is_some();
                            {
                                let mut ed = Edit {
                                    changed: &mut changed,
                                    notice: &mut settings.notice,
                                };
                                show_section(ui, section, cli_override, config, &mut ed);
                            }
                        });
                    });
            });
            ui.separator();
            if let Some(notice) = settings.notice.clone() {
                ui.colored_label(egui::Color32::YELLOW, notice);
            }
            if let Some(err) = settings.last_error.clone() {
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::LIGHT_RED, err);
                    if ui.small_button("Dismiss").clicked() {
                        settings.last_error = None;
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("Saving to {}", settings.config_path_label()))
                        .small()
                        .weak(),
                );
            });
            ui.horizontal(|ui| {
                if ui
                    .button("Reset all to defaults")
                    .on_hover_text("Reset every section and save immediately")
                    .clicked()
                {
                    reset_all(config);
                    settings.notice = Some("All settings reset to defaults".to_string());
                    changed = true;
                }
            });
        });
    // Title-bar `x` was clicked: mirror it into panel state.
    if !open {
        settings.close();
    }
    changed
}

/// Edit accumulator: threads `changed` + inline `notice` through widgets.
struct Edit<'a> {
    changed: &'a mut bool,
    notice: &'a mut Option<String>,
}

/// Fixed-width slot left of every setting label: empty at default,
/// `•` when modified, `⟲` reset button on row hover. Reserving the
/// width up front keeps rows from shifting when the button appears.
const RESET_SLOT_WIDTH: f32 = 18.0;

fn begin_reset_slot(ui: &mut egui::Ui) -> egui::Rect {
    let height = ui.spacing().interact_size.y;
    ui.allocate_exact_size(egui::vec2(RESET_SLOT_WIDTH, height), egui::Sense::hover())
        .0
}

fn end_reset_slot(ui: &mut egui::Ui, slot: egui::Rect, label: &str, modified: bool) -> bool {
    if !modified {
        return false;
    }
    let hovered = ui.rect_contains_pointer(ui.min_rect());
    if hovered {
        ui.put(slot, egui::Button::new("⟲").small())
            .on_hover_text(format!("Reset {label} to default"))
            .clicked()
    } else {
        ui.put(
            slot,
            egui::Label::new(
                egui::RichText::new("•")
                    .small()
                    .color(egui::Color32::YELLOW),
            ),
        )
        .on_hover_text("Modified from default — hover to reset");
        false
    }
}

impl Edit<'_> {
    fn mark(&mut self) {
        *self.changed = true;
    }

    fn note(&mut self, msg: String) {
        *self.notice = Some(msg);
    }

    /// Positive-float row. The `DragValue` rejects non-numeric keystrokes;
    /// out-of-range input snaps back to the compiled default with a hint.
    fn f32(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        value: &mut f32,
        range: std::ops::RangeInclusive<f32>,
        default: f32,
    ) {
        ui.horizontal(|ui| {
            let slot = begin_reset_slot(ui);
            ui.label(label);
            if ui
                .add(egui::DragValue::new(value).range(range).speed(0.1))
                .changed()
            {
                let (fixed, repaired) = repair_positive_f32(*value, default);
                *value = fixed;
                if repaired {
                    self.note(format!("{label} must be positive; reset to {default}"));
                }
                self.mark();
            }
            if end_reset_slot(ui, slot, label, *value != default) {
                *value = default;
                self.mark();
            }
        });
    }

    /// Float row where zero is legal (only non-finite input is repaired).
    fn f32_or_zero(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        value: &mut f32,
        range: std::ops::RangeInclusive<f32>,
        default: f32,
    ) {
        ui.horizontal(|ui| {
            let slot = begin_reset_slot(ui);
            ui.label(label);
            if ui
                .add(egui::DragValue::new(value).range(range).speed(0.1))
                .changed()
            {
                let (fixed, repaired) = repair_finite_f32(*value, default);
                *value = fixed;
                if repaired {
                    self.note(format!("{label} must be a number; reset to {default}"));
                }
                self.mark();
            }
            if end_reset_slot(ui, slot, label, *value != default) {
                *value = default;
                self.mark();
            }
        });
    }

    fn usize(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        value: &mut usize,
        range: std::ops::RangeInclusive<usize>,
        min: usize,
        default: usize,
    ) {
        ui.horizontal(|ui| {
            let slot = begin_reset_slot(ui);
            ui.label(label);
            if ui.add(egui::DragValue::new(value).range(range)).changed() {
                let (fixed, repaired) = repair_min_usize(*value, min, default);
                *value = fixed;
                if repaired {
                    self.note(format!("{label} must be ≥ {min}; reset to {default}"));
                }
                self.mark();
            }
            if end_reset_slot(ui, slot, label, *value != default) {
                *value = default;
                self.mark();
            }
        });
    }

    fn u32(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        value: &mut u32,
        range: std::ops::RangeInclusive<u32>,
        min: u32,
        default: u32,
    ) {
        ui.horizontal(|ui| {
            let slot = begin_reset_slot(ui);
            ui.label(label);
            if ui.add(egui::DragValue::new(value).range(range)).changed() {
                let (fixed, repaired) = repair_min_u32(*value, min, default);
                *value = fixed;
                if repaired {
                    self.note(format!("{label} must be ≥ {min}; reset to {default}"));
                }
                self.mark();
            }
            if end_reset_slot(ui, slot, label, *value != default) {
                *value = default;
                self.mark();
            }
        });
    }

    fn u64(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        value: &mut u64,
        range: std::ops::RangeInclusive<u64>,
        min: u64,
        default: u64,
    ) {
        ui.horizontal(|ui| {
            let slot = begin_reset_slot(ui);
            ui.label(label);
            if ui.add(egui::DragValue::new(value).range(range)).changed() {
                let (fixed, repaired) = repair_min_u64(*value, min, default);
                *value = fixed;
                if repaired {
                    self.note(format!("{label} must be ≥ {min}; reset to {default}"));
                }
                self.mark();
            }
            if end_reset_slot(ui, slot, label, *value != default) {
                *value = default;
                self.mark();
            }
        });
    }

    fn u8(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        value: &mut u8,
        range: std::ops::RangeInclusive<u8>,
        default: u8,
    ) {
        ui.horizontal(|ui| {
            let slot = begin_reset_slot(ui);
            ui.label(label);
            if ui.add(egui::DragValue::new(value).range(range)).changed() {
                self.mark();
            }
            if end_reset_slot(ui, slot, label, *value != default) {
                *value = default;
                self.mark();
            }
        });
    }

    fn f64(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        value: &mut f64,
        range: std::ops::RangeInclusive<f64>,
        default: f64,
    ) {
        ui.horizontal(|ui| {
            let slot = begin_reset_slot(ui);
            ui.label(label);
            if ui
                .add(egui::DragValue::new(value).range(range).speed(0.1))
                .changed()
            {
                let (fixed, repaired) = repair_positive_f64(*value, default);
                *value = fixed;
                if repaired {
                    self.note(format!("{label} must be positive; reset to {default}"));
                }
                self.mark();
            }
            if end_reset_slot(ui, slot, label, *value != default) {
                *value = default;
                self.mark();
            }
        });
    }

    fn flag(&mut self, ui: &mut egui::Ui, label: &str, value: &mut bool, default: bool) {
        ui.horizontal(|ui| {
            let slot = begin_reset_slot(ui);
            if ui.checkbox(value, label).changed() {
                self.mark();
            }
            if end_reset_slot(ui, slot, label, *value != default) {
                *value = default;
                self.mark();
            }
        });
    }

    fn text(&mut self, ui: &mut egui::Ui, label: &str, value: &mut String, default: &str) {
        ui.horizontal(|ui| {
            let slot = begin_reset_slot(ui);
            ui.label(label);
            if ui.text_edit_singleline(value).changed() {
                self.mark();
            }
            if end_reset_slot(ui, slot, label, value.as_str() != default) {
                *value = default.to_string();
                self.mark();
            }
        });
    }
}

fn show_section(
    ui: &mut egui::Ui,
    section: SettingsSection,
    cli_override: bool,
    config: &mut Config,
    ed: &mut Edit,
) {
    let d = Config::default();
    match section {
        SettingsSection::Theme => {
            if cli_override {
                ui.label(
                    "Theme edits save to the file; the live session keeps \
                    the CLI theme until relaunch without --theme.",
                );
            }
            if !is_known_theme(&config.theme.name) {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    format!(
                        "Unknown theme {:?}; pick one or type a name.",
                        config.theme.name
                    ),
                );
            }
            ui.horizontal(|ui| {
                let slot = begin_reset_slot(ui);
                ui.label("Color theme");
                egui::ComboBox::from_id_salt("theme-name")
                    .selected_text(config.theme.name.as_str())
                    .show_ui(ui, |ui| {
                        for name in KNOWN_THEMES {
                            if ui
                                .selectable_value(&mut config.theme.name, name.to_string(), name)
                                .changed()
                            {
                                ed.mark();
                            }
                        }
                    });
                if end_reset_slot(ui, slot, "theme", config.theme.name != d.theme.name) {
                    config.theme.name = d.theme.name.clone();
                    ed.mark();
                }
            });
            if !is_known_theme(&config.theme.name) {
                ed.text(ui, "Custom name", &mut config.theme.name, &d.theme.name);
                if config.theme.name.trim().is_empty() {
                    config.theme.name = d.theme.name.clone();
                    ed.note("Theme name can't be empty; reset to default".to_string());
                    ed.mark();
                }
            }
        }
        SettingsSection::Font => {
            ui.horizontal(|ui| {
                let slot = begin_reset_slot(ui);
                ui.label("Family");
                egui::ComboBox::from_id_salt("font-family")
                    .selected_text(config.font.family.as_str())
                    .show_ui(ui, |ui| {
                        for fam in FONT_FAMILIES {
                            if ui
                                .selectable_value(&mut config.font.family, fam.to_string(), fam)
                                .changed()
                            {
                                ed.mark();
                            }
                        }
                    });
                if end_reset_slot(ui, slot, "font family", config.font.family != d.font.family) {
                    config.font.family = d.font.family.clone();
                    ed.mark();
                }
            });
            if !FONT_FAMILIES.contains(&config.font.family.as_str()) {
                ui.label("Custom family (not in the preset list):");
                ed.text(ui, "Custom family", &mut config.font.family, &d.font.family);
            }
            ed.f32(ui, "Size", &mut config.font.size, 6.0..=72.0, d.font.size);
            ed.f32(
                ui,
                "Line height factor",
                &mut config.font.line_height_factor,
                0.5..=3.0,
                d.font.line_height_factor,
            );
            egui::CollapsingHeader::new("Advanced").show(ui, |ui| {
                ed.f32(
                    ui,
                    "Cell width factor",
                    &mut config.font.cell_width_factor,
                    0.3..=1.5,
                    d.font.cell_width_factor,
                );
                ed.f32_or_zero(
                    ui,
                    "Underline factor",
                    &mut config.font.underline_factor,
                    0.0..=0.5,
                    d.font.underline_factor,
                );
            });
        }
        SettingsSection::Window => {
            ed.text(ui, "Title", &mut config.window.title, &d.window.title);
            if config.window.title.trim().is_empty() {
                config.window.title = d.window.title.clone();
                ed.note("Title can't be empty; reset to default".to_string());
                ed.mark();
            }
            ui.label("Size resizes the live window.");
            ed.u32(
                ui,
                "Width",
                &mut config.window.width,
                100..=10_000,
                1,
                d.window.width,
            );
            ed.u32(
                ui,
                "Height",
                &mut config.window.height,
                100..=10_000,
                1,
                d.window.height,
            );
        }
        SettingsSection::Terminal => {
            ui.label("Scrollback and dimensions apply live; running shells keep going.");
            ed.usize(
                ui,
                "Scrollback lines",
                &mut config.terminal.scrollback_lines,
                0..=1_000_000,
                0,
                d.terminal.scrollback_lines,
            );
            ui.horizontal(|ui| {
                let slot = begin_reset_slot(ui);
                ui.label("Min dim");
                if ui
                    .add(egui::DragValue::new(&mut config.terminal.min_dim).range(1..=100_000))
                    .changed()
                {
                    let (min, max, repaired) = normalize_min_max(
                        config.terminal.min_dim,
                        config.terminal.max_dim,
                        d.terminal.min_dim,
                        d.terminal.max_dim,
                    );
                    config.terminal.min_dim = min;
                    config.terminal.max_dim = max;
                    if repaired {
                        ed.note("Min/max dims repaired (no zero, min ≤ max)".to_string());
                    }
                    ed.mark();
                }
                ui.label("Max dim");
                if ui
                    .add(egui::DragValue::new(&mut config.terminal.max_dim).range(1..=100_000))
                    .changed()
                {
                    let (min, max, repaired) = normalize_min_max(
                        config.terminal.min_dim,
                        config.terminal.max_dim,
                        d.terminal.min_dim,
                        d.terminal.max_dim,
                    );
                    config.terminal.min_dim = min;
                    config.terminal.max_dim = max;
                    if repaired {
                        ed.note("Min/max dims repaired (no zero, min ≤ max)".to_string());
                    }
                    ed.mark();
                }
                if end_reset_slot(
                    ui,
                    slot,
                    "dims",
                    config.terminal.min_dim != d.terminal.min_dim
                        || config.terminal.max_dim != d.terminal.max_dim,
                ) {
                    config.terminal.min_dim = d.terminal.min_dim;
                    config.terminal.max_dim = d.terminal.max_dim;
                    ed.mark();
                }
            });
            ed.usize(
                ui,
                "Tab stop",
                &mut config.terminal.tab_stop,
                1..=64,
                1,
                d.terminal.tab_stop,
            );
            ed.usize(
                ui,
                "Max title chars",
                &mut config.terminal.max_title_chars,
                1..=4096,
                1,
                d.terminal.max_title_chars,
            );
        }
        SettingsSection::Shell => {
            ui.label("Applies to new tabs; running sessions keep their shell.");
            ed.text(
                ui,
                "Shell (empty = $SHELL)",
                &mut config.shell.shell,
                &d.shell.shell,
            );
            ed.text(ui, "TERM", &mut config.shell.term, &d.shell.term);
            if config.shell.term.trim().is_empty() {
                config.shell.term = d.shell.term.clone();
                ed.note("TERM can't be empty; reset to default".to_string());
                ed.mark();
            }
            ed.text(
                ui,
                "Working dir (empty = $HOME)",
                &mut config.shell.cwd,
                &d.shell.cwd,
            );
        }
        SettingsSection::Cursor => {
            ui.label("Shape defaults apply to new tabs; blink speed is live.");
            ed.u64(
                ui,
                "Blink ms",
                &mut config.cursor.blink_ms,
                1..=10_000,
                1,
                d.cursor.blink_ms,
            );
            ui.horizontal(|ui| {
                let slot = begin_reset_slot(ui);
                ui.label("Default shape");
                egui::ComboBox::from_id_salt("cursor-shape")
                    .selected_text(config.cursor.default_shape.as_str())
                    .show_ui(ui, |ui| {
                        for shape in CURSOR_SHAPES {
                            if ui
                                .selectable_value(
                                    &mut config.cursor.default_shape,
                                    shape.to_string(),
                                    shape,
                                )
                                .changed()
                            {
                                ed.mark();
                            }
                        }
                    });
                if end_reset_slot(
                    ui,
                    slot,
                    "cursor shape",
                    config.cursor.default_shape != d.cursor.default_shape,
                ) {
                    config.cursor.default_shape = d.cursor.default_shape.clone();
                    ed.mark();
                }
            });
            {
                let (fixed, repaired) = sanitize_cursor_shape(&config.cursor.default_shape);
                if repaired {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        format!(
                            "Unknown shape {:?}; using {fixed:?} until fixed.",
                            config.cursor.default_shape
                        ),
                    );
                    config.cursor.default_shape = fixed;
                    ed.note("Cursor shape repaired to block".to_string());
                    ed.mark();
                }
            }
            ed.flag(
                ui,
                "Blinking by default",
                &mut config.cursor.default_blinking,
                true,
            );
            egui::CollapsingHeader::new("Advanced").show(ui, |ui| {
                ed.f32_or_zero(
                    ui,
                    "Underline factor",
                    &mut config.cursor.underline_factor,
                    0.0..=1.0,
                    d.cursor.underline_factor,
                );
                ed.f32(
                    ui,
                    "Bar width factor",
                    &mut config.cursor.bar_width_factor,
                    0.05..=1.0,
                    d.cursor.bar_width_factor,
                );
            });
        }
        SettingsSection::Selection => {
            ed.u64(
                ui,
                "Double-click ms",
                &mut config.selection.double_click_ms,
                0..=5000,
                0,
                d.selection.double_click_ms,
            );
            ed.text(
                ui,
                "Extra word chars",
                &mut config.selection.word_extra_chars,
                &d.selection.word_extra_chars,
            );
        }
        SettingsSection::Scrollbar => {
            ed.f32(
                ui,
                "Track width",
                &mut config.scrollbar.track_width,
                2.0..=64.0,
                d.scrollbar.track_width,
            );
            ed.f32(
                ui,
                "Min thumb",
                &mut config.scrollbar.min_thumb,
                4.0..=200.0,
                d.scrollbar.min_thumb,
            );
            ed.f32_or_zero(
                ui,
                "Pad",
                &mut config.scrollbar.pad,
                0.0..=32.0,
                d.scrollbar.pad,
            );
            ed.u64(
                ui,
                "Fade delay ms",
                &mut config.scrollbar.fade_delay_ms,
                0..=10_000,
                0,
                d.scrollbar.fade_delay_ms,
            );
            ed.f32(
                ui,
                "Fade speed",
                &mut config.scrollbar.fade_speed,
                0.1..=50.0,
                d.scrollbar.fade_speed,
            );
        }
        SettingsSection::Tabbar => {
            ed.f32_or_zero(
                ui,
                "Height",
                &mut config.tabbar.height,
                0.0..=200.0,
                d.tabbar.height,
            );
            ed.f32(
                ui,
                "Min tab width",
                &mut config.tabbar.min_tab_width,
                10.0..=1000.0,
                d.tabbar.min_tab_width,
            );
            ed.u8(
                ui,
                "Tab corner radius",
                &mut config.tabbar.corner_radius_tab,
                0..=24,
                d.tabbar.corner_radius_tab,
            );
            ed.u8(
                ui,
                "Bar corner radius",
                &mut config.tabbar.corner_radius_bar,
                0..=24,
                d.tabbar.corner_radius_bar,
            );
            ed.f32_or_zero(
                ui,
                "Inset X",
                &mut config.tabbar.inset_x,
                0.0..=200.0,
                d.tabbar.inset_x,
            );
            ed.f32_or_zero(
                ui,
                "Inset Y",
                &mut config.tabbar.inset_y,
                0.0..=200.0,
                d.tabbar.inset_y,
            );
            ed.f32_or_zero(
                ui,
                "Close reserve",
                &mut config.tabbar.close_reserve,
                0.0..=200.0,
                d.tabbar.close_reserve,
            );
            ed.f32_or_zero(
                ui,
                "Shortcut reserve",
                &mut config.tabbar.shortcut_reserve,
                0.0..=200.0,
                d.tabbar.shortcut_reserve,
            );
            ed.f32_or_zero(ui, "Gap", &mut config.tabbar.gap, 0.0..=200.0, d.tabbar.gap);
            ed.f32(
                ui,
                "Plus diameter",
                &mut config.tabbar.plus_diameter,
                8.0..=200.0,
                d.tabbar.plus_diameter,
            );
            ed.f32_or_zero(
                ui,
                "Plus gap",
                &mut config.tabbar.plus_gap,
                0.0..=200.0,
                d.tabbar.plus_gap,
            );
            ed.usize(
                ui,
                "Max label chars",
                &mut config.tabbar.max_label_chars,
                1..=256,
                1,
                d.tabbar.max_label_chars,
            );
            ed.text(
                ui,
                "Title format",
                &mut config.tabbar.title_format,
                &d.tabbar.title_format,
            );
            ui.weak(
                "Variables: $title (shell OSC), $command (foreground process), \
                 $cwd (working dir), $tab (index). All empty -> Tab N.",
            );
        }
        SettingsSection::Input => {
            for (label, value, fallback) in [
                (
                    "Copy key",
                    &mut config.input.copy_key,
                    d.input.copy_key.as_str(),
                ),
                (
                    "Paste key",
                    &mut config.input.paste_key,
                    d.input.paste_key.as_str(),
                ),
                (
                    "New tab key",
                    &mut config.input.new_tab_key,
                    d.input.new_tab_key.as_str(),
                ),
            ] {
                ui.horizontal(|ui| {
                    let slot = begin_reset_slot(ui);
                    ui.label(label);
                    let mut buf = value.clone();
                    if ui
                        .add(egui::TextEdit::singleline(&mut buf).desired_width(28.0))
                        .changed()
                    {
                        *value = sanitize_key_label(&buf, fallback);
                        ed.mark();
                    }
                    if end_reset_slot(ui, slot, label, value.as_str() != fallback) {
                        *value = fallback.to_string();
                        ed.mark();
                    }
                });
            }
            ed.flag(
                ui,
                "Copy on Ctrl+Shift",
                &mut config.input.copy_ctrl_shift,
                true,
            );
            ed.flag(ui, "Copy on Cmd/Super", &mut config.input.copy_super, true);
            ed.flag(
                ui,
                "Paste on Ctrl+Shift",
                &mut config.input.paste_ctrl_shift,
                true,
            );
            ed.flag(
                ui,
                "Paste on Cmd/Super",
                &mut config.input.paste_super,
                true,
            );
            ed.flag(ui, "New tab on Ctrl", &mut config.input.new_tab_ctrl, true);
            ed.flag(
                ui,
                "New tab on Cmd/Super",
                &mut config.input.new_tab_super,
                true,
            );
            ed.flag(
                ui,
                "Tab switch on Ctrl",
                &mut config.input.tab_switch_ctrl,
                true,
            );
            ed.flag(
                ui,
                "Tab switch on Cmd/Super",
                &mut config.input.tab_switch_super,
                true,
            );
            ed.flag(
                ui,
                "Shift+PgUp/Dn scrolls",
                &mut config.input.shift_page_scroll,
                true,
            );
            ed.f64(
                ui,
                "Lines per tick",
                &mut config.input.lines_per_tick,
                0.1..=100.0,
                d.input.lines_per_tick,
            );
            ed.f32(
                ui,
                "Pixel fallback line height",
                &mut config.input.pixel_fallback_line_height,
                1.0..=200.0,
                d.input.pixel_fallback_line_height,
            );
        }
        SettingsSection::Logs => {
            ui.label(
                "Recorded in memory for the Logs window (Ctrl/Cmd+Shift+L or \
                 Cometty > Logs…). The level applies to cometty's own logs; \
                 other crates are held to warnings unless the filter below \
                 says otherwise. Terminal output on stderr still follows \
                 RUST_LOG.",
            );
            ui.horizontal(|ui| {
                let slot = begin_reset_slot(ui);
                ui.label("Record level");
                egui::ComboBox::from_id_salt("log-level")
                    .selected_text(config.log.level.as_str())
                    .show_ui(ui, |ui| {
                        for name in LOG_LEVELS {
                            if ui
                                .selectable_value(&mut config.log.level, name.to_string(), name)
                                .changed()
                            {
                                ed.mark();
                            }
                        }
                    });
                if end_reset_slot(ui, slot, "log level", config.log.level != d.log.level) {
                    config.log.level = d.log.level.clone();
                    ed.mark();
                }
            });
            egui::CollapsingHeader::new("Advanced").show(ui, |ui| {
                ed.text(ui, "Filter", &mut config.log.filter, &d.log.filter);
                ui.label(
                    egui::RichText::new(
                        "RUST_LOG syntax, e.g. cometty=debug,wgpu=debug,warn \
                         (empty = cometty=<level>,warn)",
                    )
                    .small()
                    .weak(),
                );
                ed.usize(
                    ui,
                    "Buffer lines",
                    &mut config.log.buffer_lines,
                    MIN_LOG_BUFFER_LINES..=MAX_LOG_BUFFER_LINES,
                    MIN_LOG_BUFFER_LINES,
                    d.log.buffer_lines,
                );
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::settings::AppStartup;

    /// The logs section is the newest widget arm; a headless egui frame
    /// proves it lays out (and that `SettingsSection::ALL` reaches it).
    #[test]
    fn logs_section_lays_out_headless() {
        let mut settings = SettingsPanel::new(AppStartup {
            config_path_override: None,
            cli_theme: None,
            config_corrupt: false,
        });
        settings.open = true;
        settings.section = SettingsSection::Logs;
        let mut config = Config::default();
        let ctx = egui::Context::default();
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1024.0, 768.0),
            )),
            ..Default::default()
        };
        // First frame sizes the window, second paints it.
        let mut output = ctx.run_ui(input(), |ui| {
            let _ = show_settings(ui.ctx(), &mut settings, &mut config);
        });
        output.textures_delta.clear();
        let mut output = ctx.run_ui(input(), |ui| {
            let _ = show_settings(ui.ctx(), &mut settings, &mut config);
        });
        assert!(!output.shapes.is_empty(), "settings panel paints");
        output.textures_delta.clear();
    }
}
