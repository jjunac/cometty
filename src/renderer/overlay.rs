//! egui overlay: scrollbar chrome only (terminal cells stay glyphon).

use super::{Renderer, ScrollCtx};

pub(crate) struct OverlayOutput {
    pub(crate) scroll_to: Option<usize>,
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
        } = scroll;
        let scale = self.scale_factor.max(1.0);
        let screen_w_pts = self.width as f32 / scale;
        let screen_h_pts = self.height as f32 / scale;
        let theme = self.theme;
        let opacity = scrollbar.opacity;
        let mut scroll_to: Option<usize> = None;
        let mut hovered = false;

        let egui_input = self.egui_state.take_egui_input(window);
        let ctx = self.egui_ctx.clone();
        ctx.begin_pass(egui_input);
        egui::Area::new(egui::Id::new("scrollbar"))
            .fixed_pos(egui::pos2(0.0, 0.0))
            .order(egui::Order::Foreground)
            .show(&ctx, |ui| {
                let Some((thumb_y, thumb_h)) =
                    crate::scrollbar::geometry(screen_h_pts, total, visible, offset)
                else {
                    return;
                };
                if is_alt || (opacity <= 0.01 && !scrollbar.is_dragging()) {
                    return;
                }
                let track_w = crate::scrollbar::TRACK_WIDTH_POINTS;
                let pad = crate::scrollbar::TRACK_PAD_POINTS;
                let track_rect = egui::Rect::from_min_size(
                    egui::pos2(screen_w_pts - track_w - pad, 0.0),
                    egui::vec2(track_w, screen_h_pts),
                );
                let thumb_rect = egui::Rect::from_min_size(
                    egui::pos2(screen_w_pts - track_w - pad, thumb_y),
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
                                screen_h_pts,
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
                        y,
                        screen_h_pts,
                        total,
                        visible,
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
                            screen_h_pts,
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
            paint_jobs,
            screen_descriptor,
        }
    }
}
