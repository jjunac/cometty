//! Tab-strip layout math (Ghostty-minimal style).
//!
//! Pure, toolkit-free geometry so it stays unit-testable: the egui overlay
//! only paints what these helpers return. All values are logical points.
//! Rects are `[x, y, w, h]` with the strip origin at the top-left of the
//! window (`y = 0`, height = `TAB_BAR_HEIGHT_POINTS`).

/// Fixed tab width: browser-style, so titles changing length never
/// reflow the strip. Overflow scrolls horizontally.
pub const TAB_WIDTH_POINTS: f32 = 180.0;

/// Rounded top corners of the active tab (it merges with the terminal bg).
pub const TAB_CORNER_RADIUS_POINTS: u8 = 6;

/// Left padding before the title text.
pub const TAB_PAD_POINTS: f32 = 10.0;

/// Reserved hit width for the `×` close affordance at the tab's right edge.
/// Always allocated (layout-stable); only painted on hover/active.
pub const TAB_CLOSE_POINTS: f32 = 22.0;

/// Gap between the title text and the close hit box.
pub const TAB_GAP_POINTS: f32 = 4.0;

/// Width of the `+` new-tab cell.
pub const NEW_TAB_POINTS: f32 = 32.0;

/// Gap between the last tab and the `+` cell.
pub const NEW_TAB_GAP_POINTS: f32 = 6.0;

/// Close hit box `[x, y, w, h]` for a tab starting at `tab_x`.
pub fn close_hit(tab_x: f32, bar_h: f32) -> [f32; 4] {
    [
        tab_x + TAB_WIDTH_POINTS - TAB_CLOSE_POINTS,
        0.0,
        TAB_CLOSE_POINTS,
        bar_h,
    ]
}

/// Title text box `[x, y, w, h]`: left-padded, stopping before the close
/// hit box. Width can be zero on tiny bars; callers must handle that.
pub fn title_box(tab_x: f32, bar_h: f32) -> [f32; 4] {
    let x = tab_x + TAB_PAD_POINTS;
    let w = TAB_WIDTH_POINTS - TAB_PAD_POINTS - TAB_CLOSE_POINTS - TAB_GAP_POINTS;
    [x, 0.0, w.max(0.0), bar_h]
}

/// Truncate `title` with an ellipsis so `measure(fitted) <= max_width`.
///
/// `measure` maps text to logical width (the overlay passes a font-based
/// measurer; tests pass a stub). Always terminates: worst case returns an
/// empty string when even `…` doesn't fit.
pub fn fit_title(title: &str, max_width: f32, measure: &dyn Fn(&str) -> f32) -> String {
    if max_width <= 0.0 {
        return String::new();
    }
    if measure(title) <= max_width {
        return title.to_string();
    }
    let chars: Vec<char> = title.chars().collect();
    for len in (0..chars.len()).rev() {
        let candidate: String = chars[..len].iter().collect::<String>() + "…";
        if measure(&candidate) <= max_width {
            return candidate;
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_hit_sits_at_tab_right_edge() {
        assert_eq!(close_hit(0.0, 32.0), [158.0, 0.0, 22.0, 32.0]);
        assert_eq!(close_hit(180.0, 32.0), [338.0, 0.0, 22.0, 32.0]);
    }

    #[test]
    fn title_box_pads_left_and_reserves_close() {
        // x = 0 + 10, w = 180 - 10 - 22 - 4 = 144.
        assert_eq!(title_box(0.0, 32.0), [10.0, 0.0, 144.0, 32.0]);
        assert_eq!(title_box(180.0, 32.0), [190.0, 0.0, 144.0, 32.0]);
    }

    #[test]
    fn fit_title_passes_through_when_narrow() {
        let m = |s: &str| s.chars().count() as f32 * 8.0;
        assert_eq!(fit_title("hi", 144.0, &m), "hi");
    }

    #[test]
    fn fit_title_truncates_with_ellipsis() {
        let m = |s: &str| s.chars().count() as f32 * 8.0;
        // 144pt / 8 = 18 chars max; 17 chars + ellipsis.
        let out = fit_title("abcdefghijklmnopqrstuvwxyz", 144.0, &m);
        assert_eq!(out, "abcdefghijklmnopq…");
        assert!(m(&out) <= 144.0);
    }

    #[test]
    fn fit_title_empty_when_nothing_fits() {
        let m = |s: &str| s.chars().count() as f32 * 8.0;
        assert_eq!(fit_title("hi", 0.0, &m), "");
        assert_eq!(fit_title("hi", 4.0, &m), "");
    }

    #[test]
    fn fit_title_handles_multibyte_boundaries() {
        let m = |s: &str| s.chars().count() as f32 * 8.0;
        let out = fit_title("héllo wörld", 40.0, &m);
        // 5 chars max: 4 chars + ellipsis, split on char boundary.
        assert_eq!(out, "héll…");
        assert!(out.is_char_boundary(out.len()));
    }
}
