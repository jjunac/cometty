//! Overlay scrollbar model for the scrollback.
//!
//! Pure, toolkit-free math so it stays unit-testable: `egui` only paints
//! what [`geometry`] returns and drives [`ScrollbarUi`] for fade/drag.

use std::time::{Duration, Instant};

/// Overlay width in logical points.
pub const TRACK_WIDTH_POINTS: f32 = 10.0;
/// Minimum thumb height in logical points so it stays grabbable.
pub const MIN_THUMB_POINTS: f32 = 20.0;
/// Right-edge padding in logical points.
pub const TRACK_PAD_POINTS: f32 = 2.0;
/// Idle delay before fading out when back at the live bottom.
pub const FADE_DELAY: Duration = Duration::from_millis(800);
/// Opacity animation speed (per second).
pub const FADE_SPEED: f32 = 5.0;

/// Thumb position within a vertical track.
///
/// Returns `(thumb_y, thumb_h)` in the same units as `track_h`.
/// `total` is `scrollback_len + rows`, `visible` is `rows`,
/// `offset` is `scroll_offset` (0 = live bottom).
pub fn geometry(track_h: f32, total: usize, visible: usize, offset: usize) -> Option<(f32, f32)> {
    if track_h <= 0.0 || visible == 0 || total <= visible {
        return None;
    }
    let total = total as f32;
    let visible = visible as f32;
    let max_offset = (total - visible).max(1.0);
    let offset = (offset as f32).clamp(0.0, max_offset);
    let mut thumb_h = track_h * visible / total;
    thumb_h = thumb_h.clamp(MIN_THUMB_POINTS.min(track_h), track_h);
    let travel = (track_h - thumb_h).max(0.0);
    // offset 0 (bottom) -> thumb at track bottom; max offset (top) -> y=0.
    let frac = 1.0 - offset / max_offset;
    Some((frac * travel, thumb_h))
}

/// Map a thumb-drag position back to a scroll offset.
///
/// `thumb_y` is the dragged thumb top in track units; inverse of [`geometry`].
pub fn offset_for_thumb_y(thumb_y: f32, track_h: f32, total: usize, visible: usize) -> usize {
    if track_h <= 0.0 || visible == 0 || total <= visible {
        return 0;
    }
    let total_f = total as f32;
    let visible_f = visible as f32;
    let max_offset = (total_f - visible_f).max(1.0);
    let mut thumb_h = track_h * visible_f / total_f;
    thumb_h = thumb_h.clamp(MIN_THUMB_POINTS.min(track_h), track_h);
    let travel = (track_h - thumb_h).max(0.0);
    if travel <= 0.0 {
        return 0;
    }
    let frac = (thumb_y / travel).clamp(0.0, 1.0);
    ((1.0 - frac) * max_offset).round() as usize
}

/// Fade/drag state owned by the app, painted by egui.
pub struct ScrollbarUi {
    pub opacity: f32,
    last_active: Instant,
    drag_grab: Option<f32>,
}

impl ScrollbarUi {
    pub fn new(now: Instant) -> Self {
        Self {
            opacity: 0.0,
            last_active: now,
            drag_grab: None,
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.drag_grab.is_some()
    }

    pub fn begin_drag(&mut self, grab_offset: f32) {
        self.drag_grab = Some(grab_offset);
        self.last_active = Instant::now();
        self.opacity = 1.0;
    }

    pub fn end_drag(&mut self) {
        self.drag_grab = None;
        self.last_active = Instant::now();
    }

    pub fn drag_grab(&self) -> Option<f32> {
        self.drag_grab
    }

    /// Advance opacity toward its target. Returns true while animating
    /// (caller should `request_redraw` / `request_repaint`).
    pub fn update(
        &mut self,
        now: Instant,
        total: usize,
        visible: usize,
        offset: usize,
        is_alt: bool,
        hovered: bool,
    ) -> bool {
        let can_scroll = !is_alt && total > visible;
        if !can_scroll {
            self.drag_grab = None;
            if self.opacity != 0.0 {
                self.opacity = 0.0;
                return true;
            }
            return false;
        }
        let want_visible = can_scroll
            && (self.drag_grab.is_some()
                || hovered
                || offset > 0
                || self.opacity > 0.0 && now.duration_since(self.last_active) < FADE_DELAY);
        // Mark active whenever the user is scrolled up or interacting.
        if can_scroll && (self.drag_grab.is_some() || hovered || offset > 0) {
            self.last_active = now;
        }
        let target = if want_visible
            && (self.drag_grab.is_some()
                || hovered
                || offset > 0
                || now.duration_since(self.last_active) < FADE_DELAY)
        {
            1.0
        } else {
            0.0
        };
        // Snap when freshly activated so the bar appears immediately.
        if target > self.opacity && (self.drag_grab.is_some() || hovered || offset > 0) {
            // Appear fast, fade slow: jump partway then ease.
            self.opacity = (self.opacity + FADE_SPEED * 0.08).clamp(0.35, 1.0);
            return true;
        }
        let dt = 1.0 / 60.0;
        let step = FADE_SPEED * dt;
        let next = if (target - self.opacity).abs() <= step {
            target
        } else if target > self.opacity {
            self.opacity + step
        } else {
            self.opacity - step
        };
        let animating = (next - self.opacity).abs() > f32::EPSILON;
        self.opacity = next;
        // Keep fading after reaching bottom until the delay expires.
        animating || (target > 0.0 && self.opacity > 0.0)
    }

    #[cfg(test)]
    fn set_opacity_for_test(&mut self, v: f32, at: Instant) {
        self.opacity = v;
        self.last_active = at;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_without_history() {
        assert_eq!(geometry(600.0, 40, 40, 0), None);
        assert_eq!(geometry(600.0, 30, 40, 0), None);
        assert_eq!(geometry(0.0, 100, 40, 0), None);
    }

    #[test]
    fn thumb_at_bottom_when_live() {
        let (y, h) = geometry(600.0, 100, 40, 0).unwrap();
        assert!((y + h - 600.0).abs() < 0.01, "y={y} h={h}");
    }

    #[test]
    fn thumb_at_top_when_scrolled_up() {
        let (y, _) = geometry(600.0, 100, 40, 60).unwrap();
        assert!(y.abs() < 0.01, "y={y}");
    }

    #[test]
    fn thumb_respects_min_size() {
        let (_, h) = geometry(600.0, 10_040, 40, 0).unwrap();
        assert!((h - MIN_THUMB_POINTS).abs() < 0.01, "h={h}");
    }

    #[test]
    fn drag_roundtrips() {
        let total = 100;
        let visible = 40;
        let track_h = 600.0;
        for offset in [0, 15, 30, 60] {
            let (y, _) = geometry(track_h, total, visible, offset).unwrap();
            let back = offset_for_thumb_y(y, track_h, total, visible);
            assert!(
                (back as isize - offset as isize).abs() <= 1,
                "offset={offset} back={back}"
            );
        }
    }

    #[test]
    fn fade_hides_at_bottom_after_delay() {
        let t0 = Instant::now();
        let mut ui = ScrollbarUi::new(t0);
        ui.set_opacity_for_test(1.0, t0);
        // At bottom, not hovered: still visible during delay, then fades.
        assert!(ui.update(t0 + Duration::from_millis(100), 100, 40, 0, false, false));
        assert!(ui.opacity > 0.0);
        let mut now = t0 + FADE_DELAY + Duration::from_millis(100);
        for _ in 0..120 {
            now += Duration::from_millis(16);
            ui.update(now, 100, 40, 0, false, false);
            if ui.opacity <= 0.0 {
                break;
            }
        }
        assert_eq!(ui.opacity, 0.0);
    }

    #[test]
    fn hidden_in_alt_or_without_history() {
        let t0 = Instant::now();
        let mut ui = ScrollbarUi::new(t0);
        ui.set_opacity_for_test(1.0, t0);
        ui.update(t0, 100, 40, 10, true, false);
        assert_eq!(ui.opacity, 0.0);
    }
}
