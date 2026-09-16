//! egui overlay: tab strip + scrollbar chrome (terminal cells stay glyphon).

use super::search_ui::{self, SearchUiOutput};
use super::{Renderer, ScrollCtx};

pub(crate) struct OverlayOutput {
    pub(crate) scroll_to: Option<usize>,
    pub(crate) selected_tab: Option<usize>,
    pub(crate) close_tab: Option<usize>,
    pub(crate) new_tab: bool,
    pub(crate) search: SearchUiOutput,
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
            settings,
            config,
            logs,
            log_buffer,
            search,
        } = scroll;
        let scale = self.scale_factor.max(1.0);
        let screen_w_pts = self.width as f32 / scale;
        let screen_h_pts = self.height as f32 / scale;
        // Hidden for a single tab: the grid gets the full window height.
        let tab_h = crate::app::tab::bar_height_points(tab_titles.len(), &self.user_config.tabbar);
        let tabbar_cfg = self.user_config.tabbar.clone();
        let scrollbar_cfg = self.user_config.scrollbar.clone();
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
        // Tab strip along the top. Hidden for a single tab (except on
        // macOS, where it lives in the OS titlebar and is always shown).
        // Ghostty style: an inset pill container holds equal-width tabs.
        // On macOS the container starts right of the traffic lights. The
        // active tab is a nested pill with a 1px border; inactive tabs are
        // separated by thin dividers. Close `×` is reserved on the left of
        // each tab (painted on hover only); `⌘N` sits right-aligned.
        if crate::app::tab::bar_height_points(tab_titles.len(), &tabbar_cfg) > 0.0 {
            egui::Area::new(egui::Id::new("tabbar"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .order(egui::Order::Foreground)
                .show(&ctx, |ui| {
                    ui.set_width(screen_w_pts);
                    ui.set_height(tab_h);
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(0.0, 0.0),
                            egui::vec2(screen_w_pts, tab_h),
                        ),
                        0.0,
                        theme.background.as_egui_color(),
                    );
                    let bar_inset_y = tabbar_cfg.inset_y;
                    let plus_d = tabbar_cfg.plus_diameter;
                    let plus_gap = tabbar_cfg.plus_gap;
                    // macOS: the container starts right of the traffic
                    // lights; elsewhere `leading` is just the margin.
                    let leading = crate::tabbar::leading_inset_points(&tabbar_cfg);
                    let container_w =
                        crate::tabbar::container_width_points(screen_w_pts, &tabbar_cfg);
                    let container_h = (tab_h - bar_inset_y * 2.0).max(1.0);
                    let container_rect = egui::Rect::from_min_size(
                        egui::pos2(leading, bar_inset_y),
                        egui::vec2(container_w, container_h),
                    );
                    ui.painter().rect_filled(
                        container_rect,
                        egui::CornerRadius::same(tabbar_cfg.corner_radius_bar),
                        theme.background.as_egui_color(),
                    );
                    let tab_count = tab_titles.len();
                    let tab_w = crate::tabbar::tab_width(container_w, tab_count, &tabbar_cfg);
                    let font_id = egui::TextStyle::Body.resolve(ui.style());
                    let fg = theme.foreground.as_egui_color();
                    let dim = fg.gamma_multiply(0.55);
                    let active_bg = theme.tab_active_bg.as_egui_color();
                    let border = theme.tab_border.as_egui_color();
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.add_space(leading);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.add_space(bar_inset_y);
                            egui::ScrollArea::horizontal()
                                .max_width(container_w)
                                .max_height(container_h)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 0.0;
                                        // Scroll-content painter: tab coords shift
                                        // with the scroll offset, unlike the
                                        // fixed container rect above.
                                        let painter = ui.painter().clone();
                                        let measure = |s: &str| {
                                            painter
                                                .layout_no_wrap(
                                                    s.to_owned(),
                                                    font_id.clone(),
                                                    egui::Color32::WHITE,
                                                )
                                                .size()
                                                .x
                                        };
                                        for (i, title) in tab_titles.iter().enumerate() {
                                            ui.push_id(i, |ui| {
                                                let (tab_rect, tab_resp) = ui.allocate_exact_size(
                                                    egui::vec2(tab_w, container_h),
                                                    egui::Sense::click(),
                                                );
                                                let is_active = i == active_tab;
                                                let tab_hovered = tab_resp.hovered();
                                                let pill = tab_rect.shrink2(egui::vec2(
                                                    crate::tabbar::TAB_PILL_INSET_X,
                                                    crate::tabbar::TAB_PILL_INSET_Y,
                                                ));
                                                let pill_radius = egui::CornerRadius::same(
                                                    tabbar_cfg.corner_radius_tab,
                                                );
                                                if is_active {
                                                    painter.rect_filled(
                                                        pill,
                                                        pill_radius,
                                                        active_bg,
                                                    );
                                                    painter.rect_stroke(
                                                        pill,
                                                        pill_radius,
                                                        egui::Stroke::new(1.0, border),
                                                        egui::StrokeKind::Inside,
                                                    );
                                                } else {
                                                    if tab_hovered {
                                                        painter.rect_filled(
                                                            pill,
                                                            pill_radius,
                                                            active_bg.gamma_multiply(0.45),
                                                        );
                                                    } else if i + 1 < tab_count
                                                        && i + 1 != active_tab
                                                    {
                                                        // Divider between inactive
                                                        // neighbours; hidden next
                                                        // to the active pill.
                                                        painter.line_segment(
                                                            [
                                                                egui::pos2(
                                                                    tab_rect.max.x,
                                                                    pill.min.y + 4.0,
                                                                ),
                                                                egui::pos2(
                                                                    tab_rect.max.x,
                                                                    pill.max.y - 4.0,
                                                                ),
                                                            ],
                                                            egui::Stroke::new(
                                                                1.0,
                                                                border.gamma_multiply(0.5),
                                                            ),
                                                        );
                                                    }
                                                }
                                                // Title: centered, fitted between
                                                // the left close reserve and the
                                                // right shortcut reserve.
                                                let shortcut = crate::tabbar::shortcut_label(i);
                                                let max_title = crate::tabbar::title_max_width(
                                                    tab_w,
                                                    shortcut.is_some(),
                                                    &tabbar_cfg,
                                                );
                                                let fitted = crate::tabbar::fit_title(
                                                    title, max_title, &measure,
                                                );
                                                painter.text(
                                                    tab_rect.center(),
                                                    egui::Align2::CENTER_CENTER,
                                                    fitted,
                                                    font_id.clone(),
                                                    if is_active || tab_hovered { fg } else { dim },
                                                );
                                                if let Some(label) = shortcut {
                                                    painter.text(
                                                        egui::pos2(
                                                            tab_rect.max.x - 8.0,
                                                            tab_rect.center().y,
                                                        ),
                                                        egui::Align2::RIGHT_CENTER,
                                                        label,
                                                        font_id.clone(),
                                                        dim,
                                                    );
                                                }
                                                // Close box on the left: always
                                                // allocated (stable hit area),
                                                // only painted on tab hover.
                                                let ch = crate::tabbar::close_hit(
                                                    tab_rect.min.x,
                                                    container_h,
                                                    &tabbar_cfg,
                                                );
                                                let ch_rect = egui::Rect::from_min_size(
                                                    egui::pos2(ch[0], tab_rect.min.y),
                                                    egui::vec2(ch[2], ch[3]),
                                                );
                                                let close_resp =
                                                    ui.allocate_rect(ch_rect, egui::Sense::click());
                                                let close_hovered = close_resp.hovered();
                                                // `close_hovered` included: the close rect
                                                // overlaps the tab and steals its hover
                                                // once the pointer moves onto the ×.
                                                if tab_hovered || close_hovered {
                                                    if close_hovered {
                                                        painter.circle_filled(
                                                            ch_rect.center(),
                                                            8.0,
                                                            border.gamma_multiply(0.6),
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
                                                    // Per-tab close; the last
                                                    // tab's close exits the app
                                                    // (App::close_tab).
                                                    close_tab = Some(i);
                                                } else if tab_resp.clicked() {
                                                    selected_tab = Some(i);
                                                }
                                            });
                                        }
                                    });
                                });
                        });
                        ui.add_space(plus_gap);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.add_space(bar_inset_y);
                            ui.push_id("new_tab", |ui| {
                                let (plus_rect, plus_resp) = ui.allocate_exact_size(
                                    egui::vec2(plus_d, plus_d),
                                    egui::Sense::click(),
                                );
                                let plus_hovered = plus_resp.hovered();
                                let plus_painter = ui.painter().clone();
                                plus_painter.circle_filled(
                                    plus_rect.center(),
                                    plus_d * 0.5 - 1.0,
                                    if plus_hovered {
                                        active_bg
                                    } else {
                                        theme.background.as_egui_color()
                                    },
                                );
                                plus_painter.circle_stroke(
                                    plus_rect.center(),
                                    plus_d * 0.5 - 1.0,
                                    egui::Stroke::new(1.0, border),
                                );
                                plus_painter.text(
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
        }
        egui::Area::new(egui::Id::new("scrollbar"))
            .fixed_pos(egui::pos2(0.0, 0.0))
            .order(egui::Order::Foreground)
            .show(&ctx, |ui| {
                let track_h = screen_h_pts - tab_h;
                let Some((thumb_y, thumb_h)) =
                    crate::scrollbar::geometry(track_h, total, visible, offset, &scrollbar_cfg)
                else {
                    return;
                };
                if is_alt || (opacity <= 0.01 && !scrollbar.is_dragging()) {
                    return;
                }
                let track_w = scrollbar_cfg.track_width;
                let pad = scrollbar_cfg.pad;
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
                                &scrollbar_cfg,
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
                        track_h,
                        total,
                        visible,
                        &scrollbar_cfg,
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
                            &scrollbar_cfg,
                        ));
                    }
                }
            });
        // Find bar (floating overlay; Ctrl/Cmd+F). Drawn before the panels
        // so their windows stack above it where they overlap.
        let search_actions = search_ui::show_search(&ctx, search, tab_h);
        self.search_bar_rect = search_actions.rect;
        let mut full_output = {
            // Settings overlay (sidebar window when open; the entry point
            // lives in the native OS menu bar). Edits mutate `config` in
            // place; the app diffs + saves after the frame via
            // `apply_settings_changes`.
            super::settings_ui::show_settings(&ctx, settings, config);
            // Log viewer (floating window when open; toggled from the menu
            // bar or Ctrl/Cmd+Shift+L). Reads the app-wide ring buffer.
            super::log_ui::show_logs(&ctx, logs, log_buffer);
            ctx.end_pass()
        };
        self.egui_state
            .handle_platform_output(window, full_output.platform_output);
        let paint_jobs = ctx.tessellate(full_output.shapes, full_output.pixels_per_point);
        let now = std::time::Instant::now();
        if scrollbar.update(now, total, visible, offset, is_alt, hovered, &scrollbar_cfg) {
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
            search: search_actions,
            paint_jobs,
            screen_descriptor,
        }
    }
}
