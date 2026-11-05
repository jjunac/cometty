//! Tab-strip widgets and drag preview. The gesture itself belongs to the app;
//! this module owns only egui layout, hit rects, scroll position and text widths.

use std::collections::HashMap;
use std::time::Duration;

use crate::config::TabbarConfig;
use crate::tabbar::{self, DragLayout, StripTarget, TabDrag, TabLabel, TabMove};
use crate::theme::Theme;

#[derive(Default)]
pub(crate) struct TabbarOutput {
    pub selected_tab: Option<u64>,
    pub close_tab: Option<u64>,
    pub new_tab: bool,
    pub reordered_tab: Option<TabMove>,
}

pub(crate) struct TabbarView<'a> {
    pub width: f32,
    pub height: f32,
    pub tabs: &'a [TabLabel],
    pub active: usize,
}

#[derive(Default)]
pub(crate) struct TabbarUi {
    targets: Vec<(StripTarget, [f32; 4])>,
    /// Unclipped tab rects preserve the grab point on partially visible tabs.
    rects: Vec<(u64, [f32; 4])>,
    scroll_offset: f32,
    title_widths: HashMap<String, f32>,
    title_font: Option<egui::FontId>,
}

struct TabCell<'a> {
    tab: &'a TabLabel,
    rect: egui::Rect,
    index: usize,
    active: bool,
    single: bool,
    dragging: bool,
}

struct TabStyle<'a> {
    config: &'a TabbarConfig,
    theme: Theme,
    font: egui::FontId,
    clip: egui::Rect,
    suppress_clicks: bool,
    count: usize,
    active: usize,
}

fn coords(rect: egui::Rect) -> [f32; 4] {
    [rect.min.x, rect.min.y, rect.max.x, rect.max.y]
}

impl TabbarUi {
    pub(crate) fn clear(&mut self) {
        self.targets.clear();
        self.rects.clear();
    }

    pub(crate) fn target_at(&self, x: f32, y: f32) -> StripTarget {
        tabbar::strip_target_at(&self.targets, x, y)
    }

    pub(crate) fn source_at(&self, x: f32, y: f32) -> Option<(u64, [f32; 4])> {
        let StripTarget::Tab(id) = self.target_at(x, y) else {
            return None;
        };
        self.rects.iter().find(|(tab, _)| *tab == id).copied()
    }

    fn record(&mut self, target: StripTarget, rect: egui::Rect, clip: egui::Rect) {
        let rect = rect.intersect(clip);
        if rect.is_positive() {
            self.targets.push((target, coords(rect)));
        }
    }

    pub(crate) fn show(
        &mut self,
        ctx: &egui::Context,
        view: TabbarView<'_>,
        drag: &mut TabDrag,
        config: &TabbarConfig,
        theme: Theme,
    ) -> TabbarOutput {
        self.clear();
        drag.retain_source(
            view.tabs.len() > 1 && view.tabs.iter().any(|tab| Some(tab.id) == drag.source()),
        );
        let mut out = TabbarOutput::default();
        if view.height <= 0.0 || view.tabs.is_empty() {
            drag.finish_frame();
            return out;
        }
        let suppress_clicks = drag.suppress_clicks();
        egui::Area::new(egui::Id::new("tabbar"))
            .fixed_pos(egui::Pos2::ZERO)
            .movable(false)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_width(view.width);
                ui.set_height(view.height);
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(view.width, view.height),
                    ),
                    0.0,
                    theme.background.as_egui_color(),
                );
                let container_w = tabbar::container_width_points(view.width, config);
                let container_h = (view.height - config.inset_y * 2.0).max(1.0);
                let container = egui::Rect::from_min_size(
                    egui::pos2(tabbar::leading_inset_points(config), config.inset_y),
                    egui::vec2(container_w, container_h),
                );
                let tab_w = tabbar::tab_width(container_w, view.tabs.len(), config);
                let font = egui::TextStyle::Body.resolve(ui.style());
                if self.title_font.as_ref() != Some(&font) || self.title_widths.len() > 512 {
                    self.title_widths.clear();
                    self.title_font = Some(font.clone());
                }
                let layout = DragLayout {
                    viewport: coords(container),
                    left: container.left() - self.scroll_offset,
                    width: tab_w,
                    count: view.tabs.len(),
                };
                let next_offset = drag.scroll_offset(
                    layout,
                    self.scroll_offset,
                    ctx.input(|input| input.stable_dt),
                );
                if next_offset != self.scroll_offset {
                    self.scroll_offset = next_offset;
                    // Only a live edge drag with scroll room schedules frames.
                    // No permanent polling or animation remains after release.
                    ctx.request_repaint_after(Duration::from_millis(16));
                }
                if container_w > 0.0 {
                    ui.scope_builder(
                        egui::UiBuilder::new()
                            .max_rect(container)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                        |ui| {
                            let scroll = egui::ScrollArea::horizontal()
                                .id_salt("tabs")
                                .max_width(container_w)
                                .max_height(container_h)
                                .auto_shrink([false, false])
                                // The pointer reorders tabs, never drags the
                                // scroll area. Wheel/trackpad scrolling remains.
                                .scroll_source(egui::scroll_area::ScrollSource::MOUSE_WHEEL)
                                .scroll_bar_visibility(
                                    egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                                )
                                .horizontal_scroll_offset(self.scroll_offset)
                                .show(ui, |ui| {
                                    let (_, content) = ui.allocate_space(egui::vec2(
                                        tab_w * view.tabs.len() as f32,
                                        container_h,
                                    ));
                                    let clip = container.intersect(ui.clip_rect());
                                    ui.set_clip_rect(clip);
                                    let layout = DragLayout {
                                        viewport: coords(clip),
                                        left: content.left(),
                                        ..layout
                                    };
                                    let source = view
                                        .tabs
                                        .iter()
                                        .position(|tab| Some(tab.id) == drag.source());
                                    let preview = source.and_then(|_| drag.preview(layout));
                                    let style = TabStyle {
                                        config,
                                        theme,
                                        font: font.clone(),
                                        clip,
                                        suppress_clicks,
                                        count: view.tabs.len(),
                                        active: view.active,
                                    };
                                    for (i, tab) in view.tabs.iter().enumerate() {
                                        if preview.is_some() && source == Some(i) {
                                            continue;
                                        }
                                        let slot = match (source, preview) {
                                            (Some(from), Some(preview)) => {
                                                tabbar::index_after_move(i, from, preview.to)
                                            }
                                            _ => i,
                                        };
                                        self.paint_tab(
                                            ui,
                                            TabCell {
                                                tab,
                                                rect: egui::Rect::from_min_size(
                                                    egui::pos2(
                                                        content.left() + slot as f32 * tab_w,
                                                        content.top(),
                                                    ),
                                                    egui::vec2(tab_w, container_h),
                                                ),
                                                index: i,
                                                active: i == view.active,
                                                single: view.tabs.len() == 1,
                                                dragging: false,
                                            },
                                            &style,
                                            &mut out,
                                        );
                                    }
                                    // Paint the grabbed tab last, above its
                                    // neighbours, but still clipped to the strip.
                                    if let (Some(from), Some(preview)) = (source, preview) {
                                        self.paint_tab(
                                            ui,
                                            TabCell {
                                                tab: &view.tabs[from],
                                                rect: egui::Rect::from_min_size(
                                                    egui::pos2(preview.left, content.top()),
                                                    egui::vec2(tab_w, container_h),
                                                ),
                                                index: from,
                                                active: true,
                                                single: false,
                                                dragging: true,
                                            },
                                            &style,
                                            &mut out,
                                        );
                                        out.reordered_tab = drag.drop_action(layout, from);
                                    }
                                });
                            self.scroll_offset = scroll.state.offset.x;
                        },
                    );
                    let painter = ui.painter();
                    painter.rect_stroke(
                        container,
                        egui::CornerRadius::same(config.corner_radius_bar),
                        egui::Stroke::new(
                            2.0 / painter.pixels_per_point(),
                            theme.tab_border.as_egui_color().gamma_multiply(0.5),
                        ),
                        egui::StrokeKind::Inside,
                    );
                }
                let p = tabbar::plus_rect(view.width, config);
                let rect =
                    egui::Rect::from_min_size(egui::pos2(p[0], p[1]), egui::vec2(p[2], p[3]));
                let response = ui.interact(rect, egui::Id::new("new_tab"), egui::Sense::CLICK);
                self.record(StripTarget::NewTab, rect, ui.clip_rect());
                let fg = theme.foreground.as_egui_color();
                let painter = ui.painter();
                painter.circle_filled(
                    rect.center(),
                    config.plus_diameter * 0.5 - 1.0,
                    if response.hovered() {
                        theme.tab_active_bg.as_egui_color()
                    } else {
                        theme.background.as_egui_color()
                    },
                );
                painter.circle_stroke(
                    rect.center(),
                    config.plus_diameter * 0.5 - 1.0,
                    egui::Stroke::new(1.0, theme.tab_border.as_egui_color()),
                );
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "+",
                    font,
                    if response.hovered() {
                        fg
                    } else {
                        fg.gamma_multiply(0.55)
                    },
                );
                out.new_tab = response.clicked() && !suppress_clicks;
            });
        if drag.dragging() && drag.captures_pointer() {
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        drag.finish_frame();
        out
    }

    fn paint_tab(
        &mut self,
        ui: &mut egui::Ui,
        cell: TabCell<'_>,
        style: &TabStyle<'_>,
        out: &mut TabbarOutput,
    ) {
        let TabCell {
            tab,
            rect,
            index,
            active,
            single,
            dragging,
        } = cell;
        let config = style.config;
        let ch = tabbar::close_hit(rect.left(), rect.height(), config);
        let close = egui::Rect::from_min_size(rect.min, egui::vec2(ch[2], ch[3]));
        let body = if single {
            rect
        } else {
            egui::Rect::from_min_max(
                egui::pos2(close.right().min(rect.right()), rect.top()),
                rect.max,
            )
        };
        // Explicit stable IDs, including the close control. The two hit regions
        // do not overlap: a press on × can never become a tab drag.
        let response = ui.interact(
            body,
            egui::Id::new(("tab", tab.id)),
            if single {
                egui::Sense::CLICK
            } else {
                egui::Sense::CLICK | egui::Sense::DRAG
            },
        );
        let close_response = (!single).then(|| {
            ui.interact(
                close,
                egui::Id::new(("tab-close", tab.id)),
                egui::Sense::CLICK,
            )
        });
        self.rects.push((tab.id, coords(rect)));
        self.record(StripTarget::Tab(tab.id), rect, style.clip);
        if !single && !dragging {
            self.record(StripTarget::Close(tab.id), close, style.clip);
        }
        let close_hovered = close_response.as_ref().is_some_and(|r| r.hovered());
        let hovered = response.hovered() || close_hovered;
        let painter = ui.painter().with_clip_rect(style.clip);
        let fg = style.theme.foreground.as_egui_color();
        let dim = fg.gamma_multiply(0.55);
        let bg = style.theme.tab_active_bg.as_egui_color();
        let border = style.theme.tab_border.as_egui_color();
        let pill = rect.shrink2(egui::vec2(
            tabbar::TAB_PILL_INSET_X,
            tabbar::TAB_PILL_INSET_Y,
        ));
        let radius = egui::CornerRadius::same(config.corner_radius_tab);
        if active && !single {
            painter.rect_filled(pill, radius, bg);
            painter.rect_stroke(
                pill,
                radius,
                egui::Stroke::new(1.0, border),
                egui::StrokeKind::Inside,
            );
        } else if !single && hovered {
            painter.rect_filled(pill, radius, bg.gamma_multiply(0.45));
        } else if !single
            && !style.suppress_clicks
            && index + 1 < style.count
            && index + 1 != style.active
        {
            painter.line_segment(
                [
                    egui::pos2(rect.right(), pill.top() + 4.0),
                    egui::pos2(rect.right(), pill.bottom() - 4.0),
                ],
                egui::Stroke::new(1.0, border.gamma_multiply(0.5)),
            );
        }
        let shortcut = (!single).then(|| tabbar::shortcut_label(index)).flatten();
        let max_title = if single {
            tabbar::single_tab_title_max_width(rect.width(), config)
        } else {
            tabbar::title_max_width(rect.width(), shortcut.is_some(), config)
        };
        let mut measure = |text: &str| {
            if let Some(width) = self.title_widths.get(text) {
                return *width;
            }
            let width = painter
                .layout_no_wrap(text.to_owned(), style.font.clone(), egui::Color32::WHITE)
                .size()
                .x;
            self.title_widths.insert(text.to_owned(), width);
            width
        };
        let title = tabbar::fit_title(&tab.title, max_title, &mut measure);
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            title,
            style.font.clone(),
            if active || hovered { fg } else { dim },
        );
        if let Some(shortcut) = shortcut {
            painter.text(
                egui::pos2(rect.right() - 8.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                shortcut,
                style.font.clone(),
                dim,
            );
        }
        if !single && !style.suppress_clicks && hovered {
            if close_hovered {
                painter.circle_filled(close.center(), 8.0, border.gamma_multiply(0.6));
            }
            painter.text(
                close.center(),
                egui::Align2::CENTER_CENTER,
                "×",
                style.font.clone(),
                if close_hovered { fg } else { dim },
            );
        }
        if !style.suppress_clicks {
            if close_response.is_some_and(|r| r.clicked()) {
                out.close_tab = Some(tab.id);
            } else if response.clicked() {
                out.selected_tab = Some(tab.id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, PointerButton, Pos2, pos2};

    /// Replay the same native gesture updates as App before running the real
    /// egui widget. No Window, surface, GPU or mock tab-strip painter is used.
    struct Harness {
        ctx: egui::Context,
        strip: TabbarUi,
        drag: TabDrag,
        config: TabbarConfig,
        tabs: Vec<TabLabel>,
        active: usize,
        width: f32,
        time: f64,
        delay: Duration,
        shapes: Vec<egui::epaint::ClippedShape>,
    }

    impl Harness {
        fn new(count: usize, width: f32) -> Self {
            let mut harness = Self {
                ctx: egui::Context::default(),
                strip: TabbarUi::default(),
                drag: TabDrag::default(),
                config: TabbarConfig::default(),
                tabs: (0..count)
                    .map(|i| TabLabel {
                        id: 100 + i as u64 * 10,
                        title: "same label".into(),
                    })
                    .collect(),
                active: 0,
                width,
                time: 0.0,
                delay: Duration::MAX,
                shapes: Vec::new(),
            };
            // egui Areas need an initial layout pass before interaction.
            for _ in 0..3 {
                harness.frame(Vec::new());
            }
            harness
        }

        fn frame(&mut self, events: Vec<Event>) -> TabbarOutput {
            for event in &events {
                match *event {
                    Event::PointerMoved(pos) => {
                        if self.drag.captures_pointer()
                            && let Some(id) = self.drag.moved([pos.x, pos.y])
                        {
                            self.active = self.tabs.iter().position(|tab| tab.id == id).unwrap();
                        }
                    }
                    Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed,
                        ..
                    } => {
                        if pressed {
                            let source = self
                                .strip
                                .source_at(pos.x, pos.y)
                                .filter(|_| self.tabs.len() > 1);
                            self.drag.begin(source, [pos.x, pos.y]);
                        } else {
                            self.drag.release();
                        }
                    }
                    Event::PointerGone => self.drag.pointer_gone(),
                    _ => {}
                }
            }
            self.time += 1.0 / 60.0;
            self.ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    egui::vec2(self.width, 600.0),
                )),
                events,
                time: Some(self.time),
                ..Default::default()
            });
            let out = self.strip.show(
                &self.ctx,
                TabbarView {
                    width: self.width,
                    height: self.config.height,
                    tabs: &self.tabs,
                    active: self.active,
                },
                &mut self.drag,
                &self.config,
                Theme::default(),
            );
            let mut full = self.ctx.end_pass();
            self.delay = full
                .viewport_output
                .get(&egui::ViewportId::ROOT)
                .map_or(Duration::MAX, |out| out.repaint_delay);
            self.shapes = std::mem::take(&mut full.shapes);
            full.drop_without_applying_deltas();
            out
        }

        fn rect(&self, id: u64) -> [f32; 4] {
            self.strip
                .rects
                .iter()
                .find(|(tab, _)| *tab == id)
                .unwrap()
                .1
        }

        fn body(&self, id: u64) -> Pos2 {
            let r = self.rect(id);
            pos2((r[0] + r[2]) * 0.5, (r[1] + r[3]) * 0.5)
        }

        fn button(pos: Pos2, pressed: bool) -> Vec<Event> {
            vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        }

        fn idle(&mut self) {
            for _ in 0..10 {
                self.frame(Vec::new());
            }
            assert_eq!(
                self.delay,
                Duration::MAX,
                "no perpetual drag/edge-scroll repaint"
            );
            assert_eq!(
                self.ctx.memory(|m| m.focused()),
                None,
                "chrome never takes keyboard focus"
            );
        }
    }

    #[test]
    fn border_outlines_only_the_tab_group_including_a_single_tab() {
        for count in [1, 3, 8] {
            for scale in [1.0, 1.25, 2.0] {
                let mut h = Harness::new(count, 380.0);
                h.config.corner_radius_bar = 9;
                h.ctx.set_pixels_per_point(scale);
                // Inspect the final color, after the Area's initial fade-in.
                h.time += 1.0;
                h.idle();
                let container = egui::Rect::from_min_size(
                    pos2(tabbar::leading_inset_points(&h.config), h.config.inset_y),
                    egui::vec2(
                        tabbar::container_width_points(h.width, &h.config),
                        h.config.height - h.config.inset_y * 2.0,
                    ),
                );
                let border = h.shapes.iter().find_map(|clipped| match &clipped.shape {
                    egui::Shape::Rect(rect)
                        if rect.rect == container && rect.stroke.width > 0.0 =>
                    {
                        Some(rect)
                    }
                    _ => None,
                });
                let border = border.expect("visible group paints a border even for one tab");
                assert_eq!(border.stroke.width, 2.0 / scale);
                assert_eq!(
                    border.stroke.color,
                    Theme::default()
                        .tab_border
                        .as_egui_color()
                        .gamma_multiply(0.5)
                );
                assert_eq!(
                    border.corner_radius,
                    egui::CornerRadius::same(h.config.corner_radius_bar)
                );
                assert_eq!(border.stroke_kind, egui::StrokeKind::Inside);
                assert_eq!(border.fill, egui::Color32::TRANSPARENT);
                assert!(container.right() < tabbar::plus_rect(h.width, &h.config)[0]);
                assert!(
                    h.shapes.iter().all(|clipped| !matches!(
                        &clipped.shape,
                        egui::Shape::LineSegment { points, .. }
                            if points[0].x == 0.0 && points[1].x == h.width
                    )),
                    "no border spans the whole bar"
                );

                h.config.height = 0.0;
                h.frame(Vec::new());
                assert!(h.shapes.is_empty(), "hidden bar paints no border");
            }
        }
    }

    #[test]
    fn body_click_close_and_plus_keep_their_actions_without_keyboard_focus() {
        let mut h = Harness::new(3, 800.0);
        let pos = h.body(110);
        h.frame(Harness::button(pos, true));
        let out = h.frame(Harness::button(pos + egui::vec2(2.0, 0.0), false));
        assert_eq!(out.selected_tab, Some(110));
        assert_eq!(out.reordered_tab, None);

        let rect = h.rect(110);
        let close = pos2(rect[0] + 8.0, pos.y);
        assert!(h.strip.source_at(close.x, close.y).is_none());
        h.frame(Harness::button(close, true));
        let out = h.frame(Harness::button(close, false));
        assert_eq!(out.close_tab, Some(110));
        assert_eq!(out.selected_tab, None);
        assert_eq!(out.reordered_tab, None);

        let p = tabbar::plus_rect(h.width, &h.config);
        let plus = pos2(p[0] + p[2] * 0.5, p[1] + p[3] * 0.5);
        assert!(h.strip.source_at(plus.x, plus.y).is_none());
        h.frame(Harness::button(plus, true));
        assert!(h.frame(Harness::button(plus, false)).new_tab);
        h.idle();
    }

    #[test]
    fn titlebar_window_drag_is_reserved_for_empty_chrome_not_painted_controls() {
        for count in [1, 3, 8] {
            let mut h = Harness::new(count, 380.0);
            if count == 8 {
                h.strip.scroll_offset = 100.0;
                h.frame(Vec::new());
            }
            for &(target, rect) in &h.strip.targets {
                let (x, y) = ((rect[0] + rect[2]) * 0.5, (rect[1] + rect[3]) * 0.5);
                assert!(
                    !tabbar::is_titlebar_drag(x, y, h.width, h.config.height, &h.config, target),
                    "{target:?}: {rect:?}"
                );
            }
            // The top inset and both side margins are real empty chrome,
            // unlike the controls or macOS traffic-light reserve.
            let side_inset = h.config.inset_x * 0.5;
            let mid_y = h.config.height * 0.5;
            for pos in [
                pos2(150.0, 2.0),
                pos2(tabbar::TRAFFIC_LIGHTS_WIDTH_POINTS + side_inset, mid_y),
                pos2(h.width - side_inset, mid_y),
            ] {
                let target = h.strip.target_at(pos.x, pos.y);
                assert_eq!(target, StripTarget::Empty);
                assert!(tabbar::is_titlebar_drag(
                    pos.x,
                    pos.y,
                    h.width,
                    h.config.height,
                    &h.config,
                    target
                ));
            }
        }
    }

    #[test]
    fn a_tab_press_keeps_its_owner_when_the_pointer_crosses_empty_titlebar_chrome() {
        let mut h = Harness::new(3, 800.0);
        let from = h.body(110);
        h.frame(Harness::button(from, true));
        h.frame(vec![Event::PointerMoved(pos2(from.x + 30.0, 2.0))]);
        assert_eq!(h.drag.source(), Some(110));
        assert!(h.drag.captures_pointer());
        assert!(h.drag.dragging());
        assert_eq!(h.strip.target_at(from.x + 30.0, 2.0), StripTarget::Empty);
        // It is still the original tab gesture; returning to the row commits
        // that tab, while a release in the empty inset cancels the reorder.
        let to = h.body(120);
        let out = h.frame(Harness::button(to, false));
        assert_eq!(out.reordered_tab, Some(TabMove { id: 110, to: 2 }));
        h.idle();
    }

    #[test]
    fn drag_previews_neighbours_then_emits_one_stable_drop_in_either_direction() {
        for (id, destination) in [(100, 2), (120, 0)] {
            let mut h = Harness::new(3, 800.0);
            h.active = 1;
            let from = h.body(id);
            let to = h.body(if destination == 2 { 120 } else { 100 });
            let neighbour_left = h.rect(110)[0];
            h.frame(Harness::button(from, true));
            let out = h.frame(vec![Event::PointerMoved(to)]);
            assert_eq!(out.reordered_tab, None, "preview does not commit");
            assert_eq!(h.tabs[h.active].id, id, "drag activates the source");
            assert_eq!(
                h.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
                [100, 110, 120]
            );
            assert_ne!(h.rect(110)[0], neighbour_left, "neighbour makes room");
            let out = h.frame(Harness::button(to, false));
            assert_eq!(
                out.reordered_tab,
                Some(TabMove {
                    id,
                    to: destination
                })
            );
            assert_eq!(out.selected_tab, None, "drop is not also a click");
            assert_eq!(out.close_tab, None);
            assert!(!out.new_tab);
            assert_eq!(h.frame(Vec::new()).reordered_tab, None);
            h.idle();
        }
    }

    #[test]
    fn a_complete_native_drag_between_two_frames_is_not_lost() {
        let mut h = Harness::new(3, 800.0);
        let from = h.body(100);
        let to = h.body(120);
        let mut events = Harness::button(from, true);
        events.extend(Harness::button(to, false));
        let out = h.frame(events);
        assert_eq!(out.reordered_tab, Some(TabMove { id: 100, to: 2 }));
        assert_eq!(out.selected_tab, None);
        h.idle();
    }

    #[test]
    fn close_and_plus_presses_cannot_become_reorders() {
        let mut h = Harness::new(3, 800.0);
        let rect = h.rect(100);
        let p = tabbar::plus_rect(h.width, &h.config);
        for from in [pos2(rect[0] + 8.0, 20.0), pos2(p[0] + 10.0, p[1] + 10.0)] {
            h.frame(Harness::button(from, true));
            let to = h.body(120);
            h.frame(vec![Event::PointerMoved(to)]);
            assert!(!h.drag.dragging());
            let out = h.frame(Harness::button(to, false));
            assert_eq!(out.reordered_tab, None);
            assert_eq!(out.close_tab, None);
            assert!(!out.new_tab);
        }
        h.idle();
    }

    #[test]
    fn escape_source_exit_and_outside_release_cancel_without_a_click() {
        for cancellation in 0..4 {
            let mut h = Harness::new(3, 800.0);
            let from = h.body(100);
            let to = h.body(120);
            h.frame(Harness::button(from, true));
            h.frame(vec![Event::PointerMoved(to)]);
            let release = match cancellation {
                0 => {
                    h.drag.cancel();
                    to
                }
                1 => {
                    h.tabs.remove(0);
                    h.active = 0;
                    to
                }
                2 => pos2(to.x, 100.0),
                _ => {
                    h.drag.focus_lost();
                    to
                }
            };
            h.frame(Vec::new());
            let out = h.frame(Harness::button(release, false));
            assert_eq!(out.reordered_tab, None);
            assert_eq!(out.selected_tab, None);
            assert_eq!(out.close_tab, None);
            assert!(!out.new_tab);
            h.idle();
        }
    }

    #[test]
    fn neighbour_exit_and_resize_keep_the_source_identity_not_its_old_index() {
        let mut h = Harness::new(4, 800.0);
        let from = h.body(120);
        h.frame(Harness::button(from, true));
        h.frame(vec![Event::PointerMoved(from + egui::vec2(-10.0, 0.0))]);
        h.tabs.remove(0);
        h.width = 700.0;
        h.frame(Vec::new());
        assert_eq!(h.drag.source(), Some(120));
        let leading = tabbar::leading_inset_points(&h.config);
        let out = h.frame(Harness::button(pos2(leading + 50.0, 20.0), false));
        assert_eq!(out.reordered_tab, Some(TabMove { id: 120, to: 0 }));
        h.idle();
    }

    #[test]
    fn overflow_edges_scroll_only_while_held_and_hit_rects_stay_clipped() {
        let mut h = Harness::new(8, 380.0);
        let from = h.body(100);
        let leading = tabbar::leading_inset_points(&h.config);
        let available = tabbar::container_width_points(h.width, &h.config);
        let to = pos2(leading + available - 1.0, 20.0);
        h.frame(Harness::button(from, true));
        h.frame(vec![Event::PointerMoved(to)]);
        for _ in 0..200 {
            h.frame(Vec::new());
        }
        let max = h.config.min_tab_width * 8.0 - available;
        assert!(
            (h.strip.scroll_offset - max).abs() < 0.1,
            "reached the final tab"
        );
        for (target, rect) in &h.strip.targets {
            if *target != StripTarget::NewTab {
                assert!(
                    rect[0] >= leading && rect[2] <= leading + available,
                    "{target:?}: {rect:?}"
                );
            }
        }
        assert!(h.strip.source_at(leading - 1.0, 20.0).is_none());
        assert_eq!(
            h.strip.target_at(leading + available + 1.0, 20.0),
            StripTarget::Empty
        );
        let out = h.frame(Harness::button(to, false));
        assert_eq!(out.reordered_tab, Some(TabMove { id: 100, to: 7 }));
        let offset = h.strip.scroll_offset;
        h.idle();
        assert_eq!(h.strip.scroll_offset, offset);
    }

    #[test]
    fn wheel_still_scrolls_the_strip_without_dragging_or_focus() {
        let mut h = Harness::new(8, 380.0);
        let pos = h.body(100);
        h.frame(vec![
            Event::PointerMoved(pos),
            Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(-80.0, 0.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        for _ in 0..100 {
            h.frame(Vec::new());
        }
        assert!(h.strip.scroll_offset > 0.0);
        assert!(!h.drag.dragging());
        h.idle();
    }
}
