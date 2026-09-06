//! egui overlay: tab strip + scrollbar chrome (terminal cells stay glyphon).

use super::{Renderer, ScrollCtx, TAB_BAR_HEIGHT_POINTS};

pub(crate) struct OverlayOutput {
    pub(crate) scroll_to: Option<usize>,
    pub(crate) selected_tab: Option<usize>,
    pub(crate) close_tab: Option<usize>,
    pub(crate) new_tab: bool,
    pub(crate) paint_jobs: Vec<egui::ClippedPrimitive>,
    pub(crate) screen_descriptor: egui_wgpu::ScreenDescriptor,
}

impl Renderer {
    pub(crate) fn paint_overlay(&mut self, scroll: ScrollCtx<'_>) -> OverlayOutput {
        let ScrollCtx {
            window,
            ui: scrollbar,
            total,
            visible,
            offset,
            is_alt,
            tab_titles,
            active_tab,
        } = scroll;
        let scale = self.scale_factor.max(1.0);
        let screen_w_pts = self.width as f32 / scale;
        let screen_h_pts = self.height as f32 / scale;
        let tab_h = TAB_BAR_HEIGHT_POINTS;
        let theme = self.theme;
        let opacity = scrollbar.opacity;
        let mut scroll_to: Option<usize> = None;
        let mut selected_tab: Option<usize> = None;
        let mut close_tab: Option<usize> = None;
        let mut new_tab = false;
        let mut hovered = false;

        let egui_input = self.egui_state.take_egui_input(window);
        let ctx = self.egui_ctx.clone();
        ctx.begin_pass(egui_input);
        // Tab strip along the top; the terminal grid is laid out below it.
        // Ghostty-minimal: fixed-width tabs, custom painted. The active tab
        // uses the terminal background with rounded top corners so it reads
        // as connected to the content; inactive tabs sit flat on the strip.
        egui::Area::new(egui::Id::new("tabbar"))
            .fixed_pos(egui::pos2(0.0, 0.0))
            .order(egui::Order::Foreground)
            .show(&ctx, |ui| {
                ui.set_width(screen_w_pts);
                let bar_rect = egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(screen_w_pts, tab_h),
                );
                ui.painter()
                    .rect_filled(bar_rect, 0.0, theme.scrollbar_track.as_egui_color());
                // Hairline separating the strip from the terminal. The active
                // tab is painted 1px taller below so it swallows its segment.
                ui.painter().line_segment(
                    [
                        egui::pos2(0.0, tab_h - 0.5),
                        egui::pos2(screen_w_pts, tab_h - 0.5),
                    ],
                    egui::Stroke::new(
                        1.0,
                        theme.scrollbar_thumb.as_egui_color().gamma_multiply(0.5),
                    ),
                );
                egui::ScrollArea::horizontal().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        let font_id = egui::TextStyle::Body.resolve(ui.style());
                        let fg = theme.foreground.as_egui_color();
                        let dim = fg.gamma_multiply(0.55);
                        // Scroll-content painter: tab coords shift with the
                        // scroll offset, unlike the full-width strip above.
                        let painter = ui.painter().clone();
                        let measure = |s: &str| {
                            painter
                                .layout_no_wrap(s.to_owned(), font_id.clone(), egui::Color32::WHITE)
                                .size()
                                .x
                        };
                        for (i, title) in tab_titles.iter().enumerate() {
                            ui.push_id(i, |ui| {
                                let tab_w = crate::tabbar::TAB_WIDTH_POINTS;
                                let (tab_rect, tab_resp) = ui.allocate_exact_size(
                                    egui::vec2(tab_w, tab_h),
                                    egui::Sense::click(),
                                );
                                let is_active = i == active_tab;
                                let tab_hovered = tab_resp.hovered();
                                if is_active {
                                    let fill = egui::Rect::from_min_max(
                                        tab_rect.min,
                                        egui::pos2(tab_rect.max.x, tab_rect.max.y + 1.0),
                                    );
                                    painter.rect_filled(
                                        fill,
                                        egui::CornerRadius {
                                            nw: crate::tabbar::TAB_CORNER_RADIUS_POINTS,
                                            ne: crate::tabbar::TAB_CORNER_RADIUS_POINTS,
                                            sw: 0,
                                            se: 0,
                                        },
                                        theme.background.as_egui_color(),
                                    );
                                } else if tab_hovered {
                                    painter.rect_filled(
                                        tab_rect,
                                        0.0,
                                        theme.scrollbar_thumb.as_egui_color().gamma_multiply(0.35),
                                    );
                                }
                                // Title, pixel-fitted with an ellipsis.
                                let tb = crate::tabbar::title_box(tab_rect.min.x, tab_h);
                                let fitted = crate::tabbar::fit_title(title, tb[2], &measure);
                                painter.text(
                                    egui::pos2(tb[0], tab_h * 0.5),
                                    egui::Align2::LEFT_CENTER,
                                    fitted,
                                    font_id.clone(),
                                    if is_active || tab_hovered { fg } else { dim },
                                );
                                // Close box: always allocated (stable hit
                                // area), only painted on tab hover.
                                let ch = crate::tabbar::close_hit(tab_rect.min.x, tab_h);
                                let ch_rect = egui::Rect::from_min_size(
                                    egui::pos2(ch[0], ch[1]),
                                    egui::vec2(ch[2], ch[3]),
                                );
                                let close_resp = ui.allocate_rect(ch_rect, egui::Sense::click());
                                let close_hovered = close_resp.hovered();
                                if tab_hovered {
                                    if close_hovered {
                                        painter.circle_filled(
                                            ch_rect.center(),
                                            8.0,
                                            theme
                                                .scrollbar_thumb
                                                .as_egui_color()
                                                .gamma_multiply(0.6),
                                        );
                                    }
                                    painter.text(
                                        ch_rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        "×",
                                        font_id.clone(),
                                        if close_hovered { fg } else { dim },
                                    );
                                }
                                if close_resp.clicked() {
                                    // Per-tab close; the last tab's close
                                    // exits the app (App::close_tab).
                                    close_tab = Some(i);
                                } else if tab_resp.clicked() {
                                    selected_tab = Some(i);
                                }
                            });
                        }
                        ui.add_space(crate::tabbar::NEW_TAB_GAP_POINTS);
                        ui.push_id("new_tab", |ui| {
                            let (plus_rect, plus_resp) = ui.allocate_exact_size(
                                egui::vec2(crate::tabbar::NEW_TAB_POINTS, tab_h),
                                egui::Sense::click(),
                            );
                            let plus_hovered = plus_resp.hovered();
                            if plus_hovered {
                                painter.circle_filled(
                                    plus_rect.center(),
                                    9.0,
                                    theme.scrollbar_thumb.as_egui_color().gamma_multiply(0.45),
                                );
                            }
                            painter.text(
                                plus_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "+",
                                font_id.clone(),
                                if plus_hovered { fg } else { dim },
                            );
                            if plus_resp.clicked() {
                                new_tab = true;
                            }
                        });
                    });
                });
            });
        egui::Area::new(egui::Id::new("scrollbar"))
            .fixed_pos(egui::pos2(0.0, 0.0))
            .order(egui::Order::Foreground)
            .show(&ctx, |ui| {
                let track_h = screen_h_pts - tab_h;
                let Some((thumb_y, thumb_h)) =
                    crate::scrollbar::geometry(track_h, total, visible, offset)
                else {
                    return;
                };
                if is_alt || (opacity <= 0.01 && !scrollbar.is_dragging()) {
                    return;
                }
                let track_w = crate::scrollbar::TRACK_WIDTH_POINTS;
                let pad = crate::scrollbar::TRACK_PAD_POINTS;
                let track_rect = egui::Rect::from_min_size(
                    egui::pos2(screen_w_pts - track_w - pad, tab_h),
                    egui::vec2(track_w, track_h),
                );
                let thumb_rect = egui::Rect::from_min_size(
                    egui::pos2(screen_w_pts - track_w - pad, tab_h + thumb_y),
                    egui::vec2(track_w, thumb_h),
                );
                let painter = ui.painter().clone();
                let track_col = theme
                    .scrollbar_track
                    .as_egui_color()
                    .gamma_multiply((opacity * 0.45).clamp(0.0, 1.0));
                painter.rect_filled(track_rect, egui::CornerRadius::same(4), track_col);

                let resp = ui.allocate_rect(track_rect, egui::Sense::click_and_drag());
                hovered = resp.hovered() || resp.dragged();
                let active = scrollbar.is_dragging() || resp.dragged() || resp.hovered();
                let thumb_base = if active {
                    theme.scrollbar_hover.as_egui_color()
                } else {
                    theme.scrollbar_thumb.as_egui_color()
                };
                painter.rect_filled(
                    thumb_rect,
                    egui::CornerRadius::same(4),
                    thumb_base.gamma_multiply(opacity.clamp(0.0, 1.0)),
                );

                if resp.drag_started() {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let y = pos.y - track_rect.min.y;
                        if y >= thumb_y && y <= thumb_y + thumb_h {
                            scrollbar.begin_drag(y - thumb_y);
                        } else {
                            let target = crate::scrollbar::offset_for_thumb_y(
                                y - thumb_h * 0.5,
                                track_h,
                                total,
                                visible,
                            );
                            scroll_to = Some(target);
                            scrollbar.begin_drag(thumb_h * 0.5);
                        }
                    }
                } else if resp.dragged()
                    && let (Some(pos), Some(grab)) =
                        (resp.interact_pointer_pos(), scrollbar.drag_grab())
                {
                    let y = pos.y - track_rect.min.y - grab;
                    scroll_to = Some(crate::scrollbar::offset_for_thumb_y(
                        y, track_h, total, visible,
                    ));
                }
                if resp.drag_stopped() {
                    scrollbar.end_drag();
                } else if resp.clicked()
                    && let Some(pos) = resp.interact_pointer_pos()
                {
                    let y = pos.y - track_rect.min.y;
                    if y < thumb_y || y > thumb_y + thumb_h {
                        scroll_to = Some(crate::scrollbar::offset_for_thumb_y(
                            y - thumb_h * 0.5,
                            track_h,
                            total,
                            visible,
                        ));
                    }
                }
            });
        let mut full_output = ctx.end_pass();
        self.egui_state
            .handle_platform_output(window, full_output.platform_output);
        let paint_jobs = ctx.tessellate(full_output.shapes, full_output.pixels_per_point);
        let now = std::time::Instant::now();
        if scrollbar.update(now, total, visible, offset, is_alt, hovered) {
            ctx.request_repaint();
        }

        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.width, self.height],
            pixels_per_point: scale,
        };
        // Drain (not just borrow): egui panics on drop with unapplied deltas,
        // and our early surface-loss returns below must not leak them either.
        for (id, deltas) in full_output.textures_delta.set.drain() {
            for delta in &deltas {
                self.egui_renderer
                    .update_texture(&self.device, &self.queue, id, delta);
            }
        }
        for id in full_output.textures_delta.free.drain() {
            self.egui_renderer.free_texture(&id);
        }

        OverlayOutput {
            scroll_to,
            selected_tab,
            close_tab,
            new_tab,
            paint_jobs,
            screen_descriptor,
        }
    }
}
