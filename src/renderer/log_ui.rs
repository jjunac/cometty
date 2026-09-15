//! In-app log viewer (egui overlay), drawn inside the main egui pass.
//!
//! Rows are virtualized (`ScrollArea::show_rows`): only the visible range
//! is laid out, so a full ring buffer costs nothing per frame. Each row is
//! a single truncated line; the complete message is in the hover tooltip.

use std::sync::{Arc, Mutex};

use log::{Level, LevelFilter};

use crate::app::logs::LogsPanel;
use crate::logbuf::{self, LogBuffer, LogEntry};

/// Display-filter levels, least to most verbose (mirrors `log`).
const LEVELS: [LevelFilter; 6] = [
    LevelFilter::Off,
    LevelFilter::Error,
    LevelFilter::Warn,
    LevelFilter::Info,
    LevelFilter::Debug,
    LevelFilter::Trace,
];

/// Draw the log window when open. Called from the overlay pass; a no-op
/// (not even a buffer lock) while the panel is closed.
pub(crate) fn show_logs(
    ctx: &egui::Context,
    panel: &mut LogsPanel,
    buffer: &Arc<Mutex<LogBuffer>>,
) {
    if !panel.open {
        return;
    }
    let mut open = true;
    egui::Window::new("Logs")
        .id(egui::Id::new("logs-window"))
        .default_size(egui::vec2(560.0, 280.0))
        .resizable(true)
        .collapsible(false)
        .open(&mut open)
        .show(ctx, |ui| body(ui, panel, buffer));
    // Title-bar `x` was clicked: mirror it into panel state (which also
    // disables the per-record wakeups again).
    if !open {
        panel.close();
    }
}

fn body(ui: &mut egui::Ui, panel: &mut LogsPanel, buffer: &Arc<Mutex<LogBuffer>>) {
    let mut clear = false;
    let mut copy = false;
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("logs-level")
            .selected_text(level_label(panel.level))
            .width(80.0)
            .show_ui(ui, |ui| {
                for level in LEVELS {
                    ui.selectable_value(&mut panel.level, level, level_label(level));
                }
            });
        ui.add(
            egui::TextEdit::singleline(&mut panel.filter_text)
                .hint_text("filter")
                .desired_width(140.0),
        );
        ui.checkbox(&mut panel.following, "Follow");
        clear = ui.button("Clear").clicked();
        copy = ui.button("Copy").clicked();
    });

    if clear {
        lock(buffer).clear();
    }
    // One short lock for the whole frame: filter + counters together.
    let (entries, total, dropped) = {
        let buffer = lock(buffer);
        (
            buffer.filtered(panel.level, &panel.filter_text),
            buffer.len(),
            buffer.dropped(),
        )
    };
    if copy {
        ui.ctx().copy_text(format_entries(&entries));
    }

    ui.horizontal(|ui| {
        let mut summary = format!(
            "{} shown · {} in buffer · {} dropped",
            entries.len(),
            total,
            dropped
        );
        if dropped > 0 {
            summary.push_str(" · oldest evicted first");
        }
        summary.push_str(" · times UTC");
        ui.label(egui::RichText::new(summary).small().weak());
    });
    if let Some(filter) = crate::logging::record_filter_string() {
        ui.label(
            egui::RichText::new(format!("recording: {filter}"))
                .small()
                .weak(),
        )
        .on_hover_text(
            "Directives that decide what reaches this buffer: [log] filter, else \
             cometty=<level> plus dependency warnings (Settings ▸ Logs).",
        );
    }
    if let Some(error) = crate::logging::record_filter_error() {
        ui.label(
            egui::RichText::new(format!("filter ignored: {error}"))
                .small()
                .color(egui::Color32::YELLOW),
        )
        .on_hover_text(
            "Fix the filter in Settings ▸ Logs ▸ Advanced; typing an incomplete one is fine.",
        );
    }
    ui.separator();

    if entries.is_empty() {
        let hint = if total == 0 {
            "no entries recorded yet (check the record filter in Settings > Logs)"
        } else {
            "no matching entries"
        };
        ui.label(egui::RichText::new(hint).weak());
        return;
    }
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(panel.following)
        .show_rows(ui, row_height, entries.len(), |ui, range| {
            for index in range {
                row(ui, &entries[index], row_height);
            }
        });
}

/// One fixed-height row: time, level, target, message (truncated).
fn row(ui: &mut egui::Ui, entry: &LogEntry, row_height: f32) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_height),
        egui::Sense::hover(),
    );
    // Child UI pinned to `rect`: nothing here may grow the row, or the
    // virtualized scroll offset would drift over long buffers.
    let mut row_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    row_ui.spacing_mut().item_spacing.x = 6.0;
    row_ui.set_clip_rect(rect);

    let text = ui.visuals().text_color();
    let weak = ui.visuals().weak_text_color();
    row_ui.label(
        egui::RichText::new(logbuf::format_hhmmss_mmm(entry.at))
            .monospace()
            .color(weak),
    );
    row_ui.label(
        egui::RichText::new(format!("{:5}", entry.level.as_str()))
            .monospace()
            .color(level_color(entry.level, text, weak)),
    );
    row_ui.label(egui::RichText::new(&entry.target).monospace().color(weak));
    row_ui.add(
        egui::Label::new(egui::RichText::new(&entry.message).monospace().color(text)).truncate(),
    );

    response.on_hover_text(entry.message.clone());
}

fn level_color(level: Level, text: egui::Color32, weak: egui::Color32) -> egui::Color32 {
    match level {
        Level::Error => egui::Color32::LIGHT_RED,
        Level::Warn => egui::Color32::YELLOW,
        Level::Info => text,
        Level::Debug | Level::Trace => weak,
    }
}

/// Combo label: `log`'s own names, lowercased for the panel.
fn level_label(level: LevelFilter) -> &'static str {
    match level {
        LevelFilter::Off => "off",
        LevelFilter::Error => "error",
        LevelFilter::Warn => "warn",
        LevelFilter::Info => "info",
        LevelFilter::Debug => "debug",
        LevelFilter::Trace => "trace",
    }
}

/// Plain-text dump for the `Copy` button (bug reports, pasting in issues).
fn format_entries(entries: &[Arc<LogEntry>]) -> String {
    let mut text = String::new();
    for entry in entries {
        text.push_str(&logbuf::format_hhmmss_mmm(entry.at));
        text.push(' ');
        text.push_str(entry.level.as_str());
        text.push(' ');
        text.push_str(&entry.target);
        text.push(' ');
        text.push_str(&entry.message);
        text.push('\n');
    }
    text
}

fn lock(buffer: &Arc<Mutex<LogBuffer>>) -> std::sync::MutexGuard<'_, LogBuffer> {
    buffer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn entry(level: Level, message: &str) -> Arc<LogEntry> {
        Arc::new(LogEntry {
            level,
            target: "cometty::test".to_string(),
            message: message.to_string(),
            at: UNIX_EPOCH + Duration::from_millis(3_661_500),
        })
    }

    #[test]
    fn copied_text_is_one_line_per_entry() {
        let entries = vec![
            entry(Level::Warn, "clamping surface size"),
            entry(Level::Error, "render failed: lost"),
        ];
        let text = format_entries(&entries);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            "01:01:01.500 WARN cometty::test clamping surface size"
        );
        assert!(lines[1].starts_with("01:01:01.500 ERROR "));
    }

    #[test]
    fn level_labels_cover_every_filter() {
        for level in LevelFilter::iter() {
            assert!(!level_label(level).is_empty());
        }
    }

    /// egui layout is headless-testable, so this exercises the real widget
    /// tree (virtualized rows, tooltips, closed-panel early out) without a
    /// GPU or window.
    #[test]
    fn window_lays_out_headless() {
        let buffer = Arc::new(Mutex::new(LogBuffer::new(16)));
        {
            let mut ring = super::lock(&buffer);
            ring.push(
                Level::Warn,
                "cometty::renderer",
                "clamping surface size 4000x4000 to 2048x2048".to_string(),
            );
            ring.push(
                Level::Debug,
                "cometty::app::pty_io",
                "grid resized to 80x24".to_string(),
            );
        }
        let ctx = egui::Context::default();
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };

        let mut panel = LogsPanel {
            open: true,
            ..Default::default()
        };
        // First frame sizes the window, second paints it.
        let mut output = ctx.run_ui(input(), |ui| show_logs(ui.ctx(), &mut panel, &buffer));
        output.textures_delta.clear();
        let mut output = ctx.run_ui(input(), |ui| show_logs(ui.ctx(), &mut panel, &buffer));
        assert!(!output.shapes.is_empty(), "open panel paints rows");
        output.textures_delta.clear();

        // Closed panel: nothing painted, ring untouched (`LogsPanel::default`
        // also leaves the wake gate alone, which other tests assert on).
        let mut closed = LogsPanel::default();
        let mut output = ctx.run_ui(input(), |ui| show_logs(ui.ctx(), &mut closed, &buffer));
        assert!(output.shapes.is_empty(), "closed panel paints nothing");
        output.textures_delta.clear();
    }
}
