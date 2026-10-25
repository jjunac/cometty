//! Overlay scrollbar model for the scrollback.
//!
//! Pure, toolkit-free math so it stays unit-testable: `egui` only paints
//! what [`geometry`] returns and drives [`ScrollbarUi`] for fade/drag.
//! Dimensions come from [`crate::config::ScrollbarConfig`].

use std::time::{Duration, Instant};

use crate::config::ScrollbarConfig;

/// Thumb position within a vertical track.
///
/// Returns `(thumb_y, thumb_h)` in the same units as `track_h`.
/// `total` is `scrollback_len + rows`, `visible` is `rows`,
/// `offset` is `scroll_offset` (0 = live bottom).
pub fn geometry(
    track_h: f32,
    total: usize,
    visible: usize,
    offset: usize,
    config: &ScrollbarConfig,
) -> Option<(f32, f32)> {
    if track_h <= 0.0 || visible == 0 || total <= visible {
        return None;
    }
    let total = total as f32;
    let visible = visible as f32;
    let max_offset = (total - visible).max(1.0);
    let offset = (offset as f32).clamp(0.0, max_offset);
    let mut thumb_h = track_h * visible / total;
    thumb_h = thumb_h.clamp(config.min_thumb.min(track_h), track_h);
    let travel = (track_h - thumb_h).max(0.0);
    // offset 0 (bottom) -> thumb at track bottom; max offset (top) -> y=0.
    let frac = 1.0 - offset / max_offset;
    Some((frac * travel, thumb_h))
}

/// Map a thumb-drag position back to a scroll offset.
///
/// `thumb_y` is the dragged thumb top in track units; inverse of [`geometry`].
pub fn offset_for_thumb_y(
    thumb_y: f32,
    track_h: f32,
    total: usize,
    visible: usize,
    config: &ScrollbarConfig,
) -> usize {
    if track_h <= 0.0 || visible == 0 || total <= visible {
        return 0;
    }
    let total_f = total as f32;
    let visible_f = visible as f32;
    let max_offset = (total_f - visible_f).max(1.0);
    let mut thumb_h = track_h * visible_f / total_f;
    thumb_h = thumb_h.clamp(config.min_thumb.min(track_h), track_h);
    let travel = (track_h - thumb_h).max(0.0);
    if travel <= 0.0 {
        return 0;
    }
    let frac = (thumb_y / travel).clamp(0.0, 1.0);
    ((1.0 - frac) * max_offset).round() as usize
}

/// Hit-test the overlay strip in logical points.
///
/// True when `x_pts` falls inside the right-edge scrollbar track
/// (including padding). Pure so it stays unit-testable; callers convert
/// physical pixels to points before calling.
pub fn hit_test(x_pts: f32, screen_w_pts: f32, config: &ScrollbarConfig) -> bool {
    x_pts.is_finite()
        && screen_w_pts.is_finite()
        && x_pts >= screen_w_pts - config.track_width - config.pad
}

/// Fade/drag state owned by the app, painted by egui.
pub struct ScrollbarUi {
    pub opacity: f32,
    last_active: Instant,
    last_update: Instant,
    was_active: bool,
    repaint_needed: bool,
    drag_grab: Option<f32>,
}

impl ScrollbarUi {
    pub fn new(now: Instant) -> Self {
        Self {
            opacity: 0.0,
            last_active: now,
            last_update: now,
            was_active: false,
            repaint_needed: false,
            drag_grab: None,
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.drag_grab.is_some()
    }

    pub fn begin_drag(&mut self, grab_offset: f32) {
        self.drag_grab = Some(grab_offset);
        self.last_active = Instant::now();
        self.repaint_needed |= self.opacity != 1.0;
        self.opacity = 1.0;
    }

    pub fn end_drag(&mut self) {
        self.drag_grab = None;
        self.last_active = Instant::now();
    }

    pub fn drag_grab(&self) -> Option<f32> {
        self.drag_grab
    }

    /// Advance using wall-clock time. ZERO requests a frame (including the
    /// final changed opacity, since callers paint before updating); a finite
    /// delay wakes at the fade deadline, MAX means no repaint is needed.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        now: Instant,
        total: usize,
        visible: usize,
        offset: usize,
        is_alt: bool,
        hovered: bool,
        config: &ScrollbarConfig,
    ) -> Duration {
        let previous = self.last_update;
        self.last_update = now;
        let repaint_needed = std::mem::take(&mut self.repaint_needed);
        let fade_delay = Duration::from_millis(config.fade_delay_ms);
        let fade_speed = if config.fade_speed.is_finite() && config.fade_speed > 0.0 {
            config.fade_speed
        } else {
            ScrollbarConfig::default().fade_speed
        };
        let can_scroll = !is_alt && total > visible;
        if !can_scroll {
            self.drag_grab = None;
            self.was_active = false;
            if self.opacity != 0.0 {
                self.opacity = 0.0;
                return Duration::ZERO;
            }
            return Duration::MAX;
        }
        let active = self.drag_grab.is_some() || hovered || offset > 0;
        // A stationary hover need not render for minutes. Start the delay
        // on departure, not at its last rendered frame.
        if active || self.was_active {
            self.last_active = now;
        }
        self.was_active = active;
        let fade_at = self.last_active.checked_add(fade_delay).unwrap_or(now);
        let before = self.opacity;
        if active || (self.opacity > 0.0 && now < fade_at) {
            if active && self.opacity == 0.0 {
                self.opacity = 0.35;
            }
            self.opacity = (self.opacity
                + fade_speed * now.saturating_duration_since(previous).as_secs_f32())
            .min(1.0);
        } else {
            // Only count elapsed fade time, not time spent asleep waiting
            // for the deadline. Late frames catch up rather than slow down.
            let dt = now.saturating_duration_since(previous.max(fade_at));
            self.opacity = (self.opacity - fade_speed * dt.as_secs_f32()).max(0.0);
        }
        if repaint_needed || self.opacity != before || (self.opacity > 0.0 && self.opacity < 1.0) {
            Duration::ZERO
        } else if !active && self.opacity > 0.0 {
            // At the exact deadline opacity has not changed yet; begin
            // the animation now rather than losing its first frame.
            fade_at.saturating_duration_since(now)
        } else {
            Duration::MAX
        }
    }

    #[cfg(test)]
    fn set_opacity_for_test(&mut self, v: f32, at: Instant) {
        self.opacity = v;
        self.last_active = at;
        self.last_update = at;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ScrollbarConfig;

    fn cfg() -> ScrollbarConfig {
        ScrollbarConfig::default()
    }

    #[test]
    fn hidden_without_history() {
        assert_eq!(geometry(600.0, 40, 40, 0, &cfg()), None);
        assert_eq!(geometry(600.0, 30, 40, 0, &cfg()), None);
        assert_eq!(geometry(0.0, 100, 40, 0, &cfg()), None);
    }

    #[test]
    fn thumb_at_bottom_when_live() {
        let (y, h) = geometry(600.0, 100, 40, 0, &cfg()).unwrap();
        assert!((y + h - 600.0).abs() < 0.01, "y={y} h={h}");
    }

    #[test]
    fn thumb_at_top_when_scrolled_up() {
        let (y, _) = geometry(600.0, 100, 40, 60, &cfg()).unwrap();
        assert!(y.abs() < 0.01, "y={y}");
    }

    #[test]
    fn thumb_respects_min_size() {
        let cfg = cfg();
        let (_, h) = geometry(600.0, 10_040, 40, 0, &cfg).unwrap();
        assert!((h - cfg.min_thumb).abs() < 0.01, "h={h}");
    }

    #[test]
    fn drag_roundtrips() {
        let cfg = cfg();
        let total = 100;
        let visible = 40;
        let track_h = 600.0;
        for offset in [0, 15, 30, 60] {
            let (y, _) = geometry(track_h, total, visible, offset, &cfg).unwrap();
            let back = offset_for_thumb_y(y, track_h, total, visible, &cfg);
            assert!(
                (back as isize - offset as isize).abs() <= 1,
                "offset={offset} back={back}"
            );
        }
    }

    #[test]
    fn fade_hides_at_bottom_after_delay() {
        let cfg = cfg();
        let t0 = Instant::now();
        let mut ui = ScrollbarUi::new(t0);
        ui.set_opacity_for_test(1.0, t0);
        // At bottom, not hovered: still visible during delay, then fades.
        assert_eq!(
            ui.update(
                t0 + Duration::from_millis(100),
                100,
                40,
                0,
                false,
                false,
                &cfg
            ),
            Duration::from_millis(cfg.fade_delay_ms - 100)
        );
        assert!(ui.opacity > 0.0);
        let mut now = t0 + Duration::from_millis(cfg.fade_delay_ms + 100);
        for _ in 0..120 {
            now += Duration::from_millis(16);
            ui.update(now, 100, 40, 0, false, false, &cfg);
            if ui.opacity <= 0.0 {
                break;
            }
        }
        assert_eq!(ui.opacity, 0.0);
    }

    #[test]
    fn hidden_in_alt_or_without_history() {
        let cfg = cfg();
        let t0 = Instant::now();
        let mut ui = ScrollbarUi::new(t0);
        ui.set_opacity_for_test(1.0, t0);
        ui.update(t0, 100, 40, 10, true, false, &cfg);
        assert_eq!(ui.opacity, 0.0);
    }

    #[test]
    fn settled_history_hover_and_drag_do_not_request_frames() {
        let cfg = cfg();
        let t0 = Instant::now();
        for (offset, hover, drag) in [(10, false, false), (0, true, false), (0, false, true)] {
            let mut ui = ScrollbarUi::new(t0);
            ui.set_opacity_for_test(1.0, t0);
            if drag {
                ui.begin_drag(0.0);
            }
            for frame in 0..600 {
                assert_eq!(
                    ui.update(
                        t0 + Duration::from_millis(frame * 16),
                        100,
                        40,
                        offset,
                        false,
                        hover,
                        &cfg
                    ),
                    Duration::MAX
                );
            }
        }
    }

    #[test]
    fn leaving_settled_hover_schedules_one_fade_deadline() {
        let cfg = cfg();
        let t0 = Instant::now();
        let mut ui = ScrollbarUi::new(t0);
        ui.set_opacity_for_test(1.0, t0);
        assert_eq!(ui.update(t0, 100, 40, 0, false, true, &cfg), Duration::MAX);
        let leave = t0 + Duration::from_secs(60);
        assert_eq!(
            ui.update(leave, 100, 40, 0, false, false, &cfg),
            Duration::from_millis(cfg.fade_delay_ms)
        );
        let deadline = leave + Duration::from_millis(cfg.fade_delay_ms);
        assert_eq!(
            ui.update(deadline, 100, 40, 0, false, false, &cfg),
            Duration::ZERO
        );
        ui.update(
            deadline + Duration::from_secs(1),
            100,
            40,
            0,
            false,
            false,
            &cfg,
        );
        assert_eq!(ui.opacity, 0.0);
        assert_eq!(
            ui.update(
                deadline + Duration::from_secs(2),
                100,
                40,
                0,
                false,
                false,
                &cfg
            ),
            Duration::MAX
        );
    }

    #[test]
    fn drag_activation_requests_only_the_changed_opacity_frame() {
        let cfg = cfg();
        let t0 = Instant::now();
        let mut ui = ScrollbarUi::new(t0);
        ui.set_opacity_for_test(0.5, t0);
        ui.begin_drag(0.0);
        let now = t0 + Duration::from_secs(1);
        assert_eq!(
            ui.update(now, 100, 40, 0, false, false, &cfg),
            Duration::ZERO
        );
        assert_eq!(
            ui.update(now, 100, 40, 0, false, false, &cfg),
            Duration::MAX
        );
    }

    #[test]
    fn fade_progress_depends_on_elapsed_time_not_frame_count() {
        let cfg = cfg();
        let t0 = Instant::now();
        let start = t0 + Duration::from_millis(cfg.fade_delay_ms);
        let mut one = ScrollbarUi::new(t0);
        let mut many = ScrollbarUi::new(t0);
        one.set_opacity_for_test(1.0, t0);
        many.set_opacity_for_test(1.0, t0);
        one.update(
            start + Duration::from_millis(100),
            100,
            40,
            0,
            false,
            false,
            &cfg,
        );
        for i in 1..=10 {
            many.update(
                start + Duration::from_millis(i * 10),
                100,
                40,
                0,
                false,
                false,
                &cfg,
            );
        }
        assert!((one.opacity - many.opacity).abs() < 0.0001);
        assert!((one.opacity - 0.5).abs() < 0.0001);
    }

    #[test]
    fn hit_test_covers_track_strip_only() {
        let cfg = cfg();
        // 800pt wide screen: strip starts at 800 - 10 - 2 = 788.
        assert!(!hit_test(500.0, 800.0, &cfg));
        assert!(!hit_test(787.9, 800.0, &cfg));
        assert!(hit_test(788.0, 800.0, &cfg));
        assert!(hit_test(799.9, 800.0, &cfg));
        assert!(!hit_test(f32::NAN, 800.0, &cfg));
    }
}
