//! Find bar (egui overlay), drawn inside the main egui pass.
//!
//! State lives in [`crate::app::search::SearchPanel`]; this module only
//! paints the bar and reports button clicks (next/prev/close) back to the
//! app. Query edits mutate the panel in place and are picked up after the
//! frame, like the settings panel's config edits.

use crate::app::search::SearchPanel;

/// Bar metrics (logical points): inset from the top-right corner and the
/// fixed query-field width. The bar is right-anchored, so only the inset
/// matters for placement; the painted rect is captured for hit-testing.
const BAR_MARGIN: f32 = 8.0;
const QUERY_WIDTH: f32 = 240.0;

/// Actions the app applies after the egui frame, plus the painted rect for
/// hit-testing (a press on the bar must not start a selection underneath).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SearchUiOutput {
    pub(crate) next: bool,
    pub(crate) prev: bool,
    pub(crate) close: bool,
    pub(crate) rect: Option<egui::Rect>,
}

/// Query field id: stable so a closed bar can surrender focus (a stale
/// egui focus would swallow shell keys through the `consumed` flag).
pub(crate) fn query_id() -> egui::Id {
    egui::Id::new("search-query")
}

/// One find-bar navigation button. egui's bundled fonts have no arrow
/// glyphs (U+2191/U+2193 shape as tofu boxes), so the chevron is painted
/// as two line segments instead of text.
fn chevron_button(ui: &mut egui::Ui, up: bool, hover: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
    let visuals = ui.style().interact(&resp);
    if resp.hovered() || resp.has_focus() {
        ui.painter()
            .rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
        ui.painter().rect_stroke(
            rect,
            visuals.corner_radius,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
    }
    let c = rect.center();
    let (dx, dy) = (4.0, 2.5);
    let (a, tip, b) = if up {
        (
            egui::pos2(c.x - dx, c.y + dy),
            egui::pos2(c.x, c.y - dy),
            egui::pos2(c.x + dx, c.y + dy),
        )
    } else {
        (
            egui::pos2(c.x - dx, c.y - dy),
            egui::pos2(c.x, c.y + dy),
            egui::pos2(c.x + dx, c.y - dy),
        )
    };
    let stroke = egui::Stroke::new(1.5, visuals.fg_stroke.color);
    ui.painter().line_segment([a, tip], stroke);
    ui.painter().line_segment([tip, b], stroke);
    resp.on_hover_text(hover).clicked()
}

/// Draw the bar when open. A no-op while closed, except that any stale
/// focus on the query field is surrendered.
pub(crate) fn show_search(
    ctx: &egui::Context,
    panel: &mut SearchPanel,
    tab_h_pts: f32,
) -> SearchUiOutput {
    if !panel.open {
        // Never keep keyboard focus once hidden: `consumed` would eat shell
        // keys with no widget on screen to show why.
        ctx.memory_mut(|m| m.surrender_focus(query_id()));
        return SearchUiOutput::default();
    }
    let area = egui::Area::new(egui::Id::new("search-bar"))
        .anchor(
            egui::Align2::RIGHT_TOP,
            egui::vec2(-BAR_MARGIN, tab_h_pts + BAR_MARGIN),
        )
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let mut out = SearchUiOutput::default();
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut panel.query)
                            .id(query_id())
                            .hint_text("Search")
                            .desired_width(QUERY_WIDTH)
                            // Enter navigates matches (handled by the app);
                            // it must not drop focus out of the field.
                            .return_key(None),
                    );
                    if std::mem::take(&mut panel.focus_requested) {
                        field.request_focus();
                    }
                    let (current, total) = panel.counter();
                    let label = if panel.query.is_empty() {
                        String::new()
                    } else if total == 0 {
                        "no match".to_string()
                    } else {
                        format!("{current}/{total}")
                    };
                    ui.label(egui::RichText::new(label).monospace());
                    if chevron_button(ui, true, "Previous match (Shift+Enter)") {
                        out.prev = true;
                    }
                    if chevron_button(ui, false, "Next match (Enter)") {
                        out.next = true;
                    }
                    if ui.small_button("×").on_hover_text("Close (Esc)").clicked() {
                        out.close = true;
                    }
                });
            });
            out
        });
    let mut out = area.inner;
    out.rect = Some(area.response.rect);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::Match;

    fn input() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1024.0, 768.0),
            )),
            ..Default::default()
        }
    }

    /// egui layout is headless-testable: open the bar, check it paints,
    /// holds focus and sits right-aligned below the tab bar, then close it
    /// and check focus is released (otherwise `consumed` would keep eating
    /// shell keys).
    #[test]
    fn bar_paints_focuses_and_releases_focus_on_close() {
        let ctx = egui::Context::default();
        let mut panel = SearchPanel::default();
        panel.focus_or_open();
        panel.query = "needle".to_string();
        panel.matches = vec![Match {
            line: 0,
            start: 0,
            end: 5,
        }];
        panel.current = Some(0);
        // First frame lays the area out, second paints and applies focus.
        let mut output = ctx.run_ui(input(), |ui| {
            let _ = show_search(ui.ctx(), &mut panel, 38.0);
        });
        output.textures_delta.clear();
        let mut out = SearchUiOutput::default();
        let mut output = ctx.run_ui(input(), |ui| {
            out = show_search(ui.ctx(), &mut panel, 38.0);
        });
        assert!(!output.shapes.is_empty(), "open bar paints");
        assert_eq!(ctx.memory(|m| m.focused()), Some(query_id()));
        let rect = out.rect.expect("open bar reports its painted rect");
        assert!(
            (rect.right() - (1024.0 - BAR_MARGIN)).abs() < 1.0,
            "right-aligned: {rect:?}"
        );
        assert!(rect.top() >= 38.0, "below the tab bar: {rect:?}");
        assert!(rect.width() > QUERY_WIDTH, "fits the field: {rect:?}");
        output.textures_delta.clear();

        panel.close();
        let mut output = ctx.run_ui(input(), |ui| {
            let _ = show_search(ui.ctx(), &mut panel, 38.0);
        });
        assert!(output.shapes.is_empty(), "closed bar paints nothing");
        assert_eq!(
            ctx.memory(|m| m.focused()),
            None,
            "closed bar keeps no focus"
        );
        output.textures_delta.clear();
    }

    /// The next/prev chevrons are painted (`Shape::LineSegment`), not
    /// shaped from text: egui's bundled fonts have no arrow glyphs, so a
    /// text button would render as a tofu box (the original bug).
    #[test]
    fn navigation_chevrons_are_painted_shapes() {
        let ctx = egui::Context::default();
        let mut panel = SearchPanel::default();
        panel.focus_or_open();
        let mut most_segments = 0usize;
        // Two passes: the first creates the area, the second paints it.
        for _ in 0..2 {
            ctx.begin_pass(input());
            let _ = show_search(&ctx, &mut panel, 38.0);
            let output = ctx.end_pass();
            most_segments = most_segments.max(
                output
                    .shapes
                    .iter()
                    .map(|clipped| count_segments(&clipped.shape))
                    .sum(),
            );
            output.drop_without_applying_deltas();
        }
        assert!(
            most_segments >= 4,
            "two chevrons = four painted segments, got {most_segments}"
        );
    }

    fn count_segments(shape: &egui::Shape) -> usize {
        match shape {
            egui::Shape::LineSegment { .. } => 1,
            egui::Shape::Vec(shapes) => shapes.iter().map(count_segments).sum(),
            _ => 0,
        }
    }
}
