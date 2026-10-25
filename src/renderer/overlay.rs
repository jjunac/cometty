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
    /// Consumed by the frame boundary: sets before acquisition, frees only
    /// after the normal or upload-only submission (also on skipped frames).
    pub(crate) textures_delta: egui::TexturesDelta,
    /// Earliest delay egui wants the next frame after (animations, hover
    /// fades, smooth scrolling, tooltips); `Duration::MAX` = none pending.
    pub(crate) egui_repaint_delay: std::time::Duration,
}

/// Skip the egui pass when no chrome is visible: hidden tab bar, closed
/// find/settings/log panels, and a hidden (or alt-screen) scrollbar.
/// Pure for headless testing; the caller still drains egui input and
/// advances the scrollbar fade so animations wake the loop.
pub(crate) fn should_skip_overlay(
    tab_h: f32,
    search_open: bool,
    settings_open: bool,
    logs_open: bool,
    is_alt: bool,
    scrollbar_opacity: f32,
    scrollbar_dragging: bool,
) -> bool {
    tab_h <= 0.0
        && !search_open
        && !settings_open
        && !logs_open
        && (is_alt || (scrollbar_opacity <= 0.01 && !scrollbar_dragging))
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
            input_notice,
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

        // No chrome visible: skip the egui pass entirely (tessellation +
        // buffer uploads on every blink/output frame). Scrollbar fade still
        // advances via `update` below so it can wake the loop while animating.
        if should_skip_overlay(
            tab_h,
            search.open,
            settings.open,
            logs.open,
            is_alt,
            opacity,
            scrollbar.is_dragging(),
        ) && input_notice.is_none()
        {
            let _ = self.egui_state.take_egui_input(window);
            self.search_bar_rect = None;
            // No chrome is visible, so any egui focus left over from a panel
            // or find bar that just closed is stale. This early return skips
            // the full pass that would run egui's dead-man's-switch, so drop
            // it here or the terminal would keep losing keys and IME.
            self.surrender_egui_focus();
            // No panels or text fields can be focused now: keep window
            // IME on so dead keys reach `App::on_ime` (egui leaves it off).
            self.ensure_terminal_ime(window, false);
            let now = std::time::Instant::now();
            let egui_repaint_delay =
                scrollbar.update(now, total, visible, offset, is_alt, false, &scrollbar_cfg);
            let screen_descriptor = egui_wgpu::ScreenDescriptor {
                size_in_pixels: [self.width, self.height],
                pixels_per_point: scale,
            };
            return OverlayOutput {
                scroll_to: None,
                selected_tab: None,
                close_tab: None,
                new_tab: false,
                search: super::search_ui::SearchUiOutput::default(),
                paint_jobs: Vec::new(),
                screen_descriptor,
                textures_delta: egui::TexturesDelta::default(),
                egui_repaint_delay,
            };
        }

        let egui_input = self.egui_state.take_egui_input(window);
        let ctx = self.egui_ctx.clone();
        ctx.begin_pass(egui_input);
        if let Some(notice) = input_notice {
            // Passive status, not a focusable control: terminal-owned Tab
            // and IME keep their normal route while input is backpressured.
            egui::Area::new(egui::Id::new("input-notice"))
                .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -12.0))
                .interactable(false)
                .order(egui::Order::Foreground)
                .show(&ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width((screen_w_pts - 24.0).clamp(1.0, 520.0));
                        ui.label(egui::RichText::new(notice).color(egui::Color32::YELLOW));
                    });
                });
        }
        // Tab strip along the top. Hidden for a single tab (except on
        // macOS, where it lives in the OS titlebar and is always shown);
        // a lone tab renders as a plain title next to the `+` button.
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
                    let single_tab = tab_count == 1;
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
                                // The strip still scrolls with the wheel or
                                // a trackpad, but never paints egui's
                                // scrollbar: once tabs bottom out at
                                // `min_tab_width` it drew a thumb inside the
                                // pills.
                                .scroll_bar_visibility(
                                    egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                                )
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 0.0;
                                        // Scroll-content painter: tab coords shift
                                        // with the scroll offset, unlike the
                                        // fixed container rect above.
                                        let painter = ui.painter().clone();
                                        // Cached widths: titles rarely change
                                        // but shaping ran per title per frame.
                                        if self.title_width_font.as_ref() != Some(&font_id) {
                                            self.title_width_cache.clear();
                                            self.title_width_font = Some(font_id.clone());
                                        }
                                        if self.title_width_cache.len() > 512 {
                                            self.title_width_cache.clear();
                                        }
                                        let cache = &mut self.title_width_cache;
                                        let mut measure = |s: &str| {
                                            if let Some(w) = cache.get(s) {
                                                return *w;
                                            }
                                            let w = painter
                                                .layout_no_wrap(
                                                    s.to_owned(),
                                                    font_id.clone(),
                                                    egui::Color32::WHITE,
                                                )
                                                .size()
                                                .x;
                                            cache.insert(s.to_owned(), w);
                                            w
                                        };
                                        for (i, title) in tab_titles.iter().enumerate() {
                                            ui.push_id(i, |ui| {
                                                let (tab_rect, tab_resp) = ui.allocate_exact_size(
                                                    egui::vec2(tab_w, container_h),
                                                    // Non-focusable click: the strip must
                                                    // never steal Tab/focus from the PTY.
                                                    egui::Sense::CLICK,
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
                                                if is_active && !single_tab {
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
                                                } else if !single_tab {
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
                                                // Title: centered. A multi-tab
                                                // bar fits it between the left
                                                // close reserve and the right
                                                // shortcut reserve; a lone tab
                                                // has neither.
                                                let shortcut = if single_tab {
                                                    None
                                                } else {
                                                    crate::tabbar::shortcut_label(i)
                                                };
                                                let max_title = if single_tab {
                                                    crate::tabbar::single_tab_title_max_width(
                                                        tab_w,
                                                        &tabbar_cfg,
                                                    )
                                                } else {
                                                    crate::tabbar::title_max_width(
                                                        tab_w,
                                                        shortcut.is_some(),
                                                        &tabbar_cfg,
                                                    )
                                                };
                                                let fitted = crate::tabbar::fit_title(
                                                    title,
                                                    max_title,
                                                    &mut measure,
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
                                                // only painted on tab hover. A
                                                // lone tab draws none: closing it
                                                // would exit the app and the
                                                // strip is just its title.
                                                if !single_tab {
                                                    let ch = crate::tabbar::close_hit(
                                                        tab_rect.min.x,
                                                        container_h,
                                                        &tabbar_cfg,
                                                    );
                                                    let ch_rect = egui::Rect::from_min_size(
                                                        egui::pos2(ch[0], tab_rect.min.y),
                                                        egui::vec2(ch[2], ch[3]),
                                                    );
                                                    let close_resp = ui.allocate_rect(
                                                        ch_rect,
                                                        // Non-focusable like the pill itself.
                                                        egui::Sense::CLICK,
                                                    );
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
                                    // Non-focusable like the tab pills.
                                    egui::Sense::CLICK,
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

                // Non-focusable thumb: dragging must not steal PTY focus.
                let resp = ui.allocate_rect(track_rect, egui::Sense::CLICK | egui::Sense::DRAG);
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
        // Find bar (floating overlay; Cmd+F on macOS, Ctrl+F elsewhere).
        // Drawn before the panels so their windows stack above it where
        // they overlap.
        let search_actions = search_ui::show_search(&ctx, search, tab_h);
        self.search_bar_rect = search_actions.rect;
        // Scrollbar fade/drag advances inside the pass, so its repaint
        // request (while still animating) is part of the frame's
        // `repaint_delay` below instead of being dropped after `end_pass`.
        let now = std::time::Instant::now();
        let delay = scrollbar.update(now, total, visible, offset, is_alt, hovered, &scrollbar_cfg);
        if delay != std::time::Duration::MAX {
            ctx.request_repaint_after(delay);
        }
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
        // Earliest frame egui wants next: animations, hover fades, smooth
        // scrolling and tooltip delays all land here. `MAX` = nothing
        // pending; the app schedules its event-loop wake off this.
        let egui_repaint_delay = full_output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(std::time::Duration::MAX, |out| out.repaint_delay);
        self.egui_state
            .handle_platform_output(window, full_output.platform_output);
        // egui enables IME only for its own text fields (disabling it
        // otherwise), which would make macOS dead keys vanish. Re-assert it
        // after egui's decision whenever no text field owns the keyboard;
        // commits flow to the PTY via `App::on_ime`.
        self.ensure_terminal_ime(window, settings.open);
        let paint_jobs = ctx.tessellate(full_output.shapes, full_output.pixels_per_point);

        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.width, self.height],
            pixels_per_point: scale,
        };
        OverlayOutput {
            scroll_to,
            selected_tab,
            close_tab,
            new_tab,
            search: search_actions,
            paint_jobs,
            screen_descriptor,
            textures_delta: std::mem::take(&mut full_output.textures_delta),
            egui_repaint_delay,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::should_skip_overlay;

    #[test]
    fn overlay_skips_only_when_no_chrome_visible() {
        // Idle single-tab, panels closed, scrollbar hidden: skip.
        assert!(should_skip_overlay(
            0.0, false, false, false, false, 0.0, false
        ));
        // Alt screen hides the scrollbar: skip even with opacity set.
        assert!(should_skip_overlay(
            0.0, false, false, false, true, 1.0, false
        ));
        // Any visible chrome keeps the pass.
        assert!(!should_skip_overlay(
            38.0, false, false, false, false, 0.0, false
        ));
        assert!(!should_skip_overlay(
            0.0, true, false, false, false, 0.0, false
        ));
        assert!(!should_skip_overlay(
            0.0, false, true, false, false, 0.0, false
        ));
        assert!(!should_skip_overlay(
            0.0, false, false, true, false, 0.0, false
        ));
        assert!(!should_skip_overlay(
            0.0, false, false, false, false, 1.0, false
        ));
        assert!(!should_skip_overlay(
            0.0, false, false, false, false, 0.0, true
        ));
    }
}
