//! egui overlay: tab strip + scrollbar chrome (terminal cells stay glyphon).

use super::search_ui::{self, SearchUiOutput};
use super::tabbar_ui::TabbarView;
use super::update_ui::{self, NoticeRects, UpdateAction};
use super::{Renderer, ScrollCtx};

pub(crate) struct OverlayOutput {
    pub(crate) scroll_to: Option<usize>,
    pub(crate) selected_tab: Option<u64>,
    pub(crate) close_tab: Option<u64>,
    pub(crate) park_tab: Option<u64>,
    pub(crate) new_tab: bool,
    pub(crate) reordered_tab: Option<crate::tabbar::TabMove>,
    pub(crate) toggle_bg_popover: bool,
    pub(crate) peek_tab: Option<u64>,
    pub(crate) unpark_tab: Option<u64>,
    pub(crate) search: SearchUiOutput,
    /// Update-notice button clicked this frame, applied by the app.
    pub(crate) update_action: Option<UpdateAction>,
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
/// find/settings/log panels, no input or update notice, and a hidden (or
/// alt-screen) scrollbar. Pure for headless testing; the caller still
/// drains egui input and advances the scrollbar fade so animations wake
/// the loop.
#[allow(clippy::too_many_arguments)] // Flat flags keep the rule table-testable.
pub(crate) fn should_skip_overlay(
    tab_h: f32,
    search_open: bool,
    settings_open: bool,
    logs_open: bool,
    notice_visible: bool,
    is_alt: bool,
    scrollbar_opacity: f32,
    scrollbar_dragging: bool,
) -> bool {
    tab_h <= 0.0
        && !search_open
        && !settings_open
        && !logs_open
        && !notice_visible
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
            tabs,
            background_tabs,
            active_tab,
            peeking,
            total_tabs,
            bg_popover_open,
            tab_drag,
            settings,
            config,
            logs,
            log_buffer,
            search,
            input_notice,
            update_notice,
        } = scroll;
        let scale = self.scale_factor.max(1.0);
        let screen_w_pts = self.width as f32 / scale;
        let screen_h_pts = self.height as f32 / scale;
        // Hidden for a single tab (and no parked tabs): the grid gets the
        // full window height.
        let tab_h = crate::app::tab::bar_height_points(total_tabs, &self.user_config.tabbar);
        let tabbar_cfg = self.user_config.tabbar.clone();
        let scrollbar_cfg = self.user_config.scrollbar.clone();
        let theme = self.theme;
        let opacity = scrollbar.opacity;
        let mut scroll_to: Option<usize> = None;
        let mut hovered = false;

        // No chrome visible: skip the egui pass entirely (tessellation +
        // buffer uploads on every blink/output frame). Scrollbar fade still
        // advances via `update` below so it can wake the loop while animating.
        if should_skip_overlay(
            tab_h,
            search.open,
            settings.open,
            logs.open,
            input_notice.is_some() || update_notice.is_some(),
            is_alt,
            opacity,
            scrollbar.is_dragging(),
        ) {
            let _ = self.egui_state.take_egui_input(window);
            self.search_bar_rect = None;
            self.update_notice_rects = NoticeRects::default();
            self.tabbar_ui.clear();
            tab_drag.retain_source(false);
            tab_drag.finish_frame();
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
                park_tab: None,
                new_tab: false,
                reordered_tab: None,
                toggle_bg_popover: false,
                peek_tab: None,
                unpark_tab: None,
                search: super::search_ui::SearchUiOutput::default(),
                update_action: None,
                paint_jobs: Vec::new(),
                screen_descriptor,
                textures_delta: egui::TexturesDelta::default(),
                egui_repaint_delay,
            };
        }

        let egui_input = self.egui_state.take_egui_input(window);
        let ctx = self.egui_ctx.clone();
        crate::perf::add(crate::perf::Metric::EguiPass, 1);
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
        let tab_actions = self.tabbar_ui.show(
            &ctx,
            TabbarView {
                width: screen_w_pts,
                height: tab_h,
                tabs,
                active: active_tab,
                background: background_tabs,
                peeking,
                popover_open: bg_popover_open,
                now: std::time::Instant::now(),
            },
            tab_drag,
            &tabbar_cfg,
            theme,
        );
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
                if !scrollbar.interactive(is_alt) {
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
        // Update notice: passive chrome in the bottom-right corner, clear of
        // the scrollbar track. Its buttons use the non-focusable click
        // sense, so terminal-owned Tab and IME keep their route. Rects are
        // kept for pointer routing; the action is applied after the frame.
        let (update_action, notice_rects) = match update_notice {
            Some(notice) => {
                update_ui::show_update_notice(&ctx, notice, theme, screen_w_pts, &scrollbar_cfg)
            }
            None => (None, NoticeRects::default()),
        };
        self.update_notice_rects = notice_rects;
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
            selected_tab: tab_actions.selected_tab,
            close_tab: tab_actions.close_tab,
            park_tab: tab_actions.park_tab,
            new_tab: tab_actions.new_tab,
            reordered_tab: tab_actions.reordered_tab,
            toggle_bg_popover: tab_actions.toggle_popover,
            peek_tab: tab_actions.peek_tab,
            unpark_tab: tab_actions.unpark_tab,
            search: search_actions,
            update_action,
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
        // Idle single-tab, panels closed, no notice, scrollbar hidden: skip.
        assert!(should_skip_overlay(
            0.0, false, false, false, false, false, 0.0, false
        ));
        // Alt screen hides the scrollbar: skip even with opacity set.
        assert!(should_skip_overlay(
            0.0, false, false, false, false, true, 1.0, false
        ));
        // Any visible chrome keeps the pass.
        assert!(!should_skip_overlay(
            38.0, false, false, false, false, false, 0.0, false
        ));
        assert!(!should_skip_overlay(
            0.0, true, false, false, false, false, 0.0, false
        ));
        assert!(!should_skip_overlay(
            0.0, false, true, false, false, false, 0.0, false
        ));
        assert!(!should_skip_overlay(
            0.0, false, false, true, false, false, 0.0, false
        ));
        assert!(!should_skip_overlay(
            0.0, false, false, false, false, false, 1.0, false
        ));
        assert!(!should_skip_overlay(
            0.0, false, false, false, false, false, 0.0, true
        ));
        // An input or update notice keeps the pass, even on the alt screen.
        assert!(!should_skip_overlay(
            0.0, false, false, false, true, false, 0.0, false
        ));
        assert!(!should_skip_overlay(
            0.0, false, false, false, true, true, 1.0, false
        ));
    }
}
