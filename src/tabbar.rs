//! Tab-strip layout math (Ghostty style).
//!
//! Pure, toolkit-free geometry so it stays unit-testable: the egui overlay
//! only paints what these helpers return. All values are logical points.
//! The strip is hidden for a single tab — except on macOS, where it is
//! always shown: the strip lives inside the OS titlebar area (transparent
//! titlebar + fullsize content view, Brave-style) and the reserved height
//! keeps terminal content from sliding under the traffic lights.
//!
//! Layout: an inset pill container holds equal-width tabs splitting the
//! available width (50% each for 2, 33% each for 3, …) down to
//! [`MIN_TAB_WIDTH_POINTS`], then scrolling. On macOS the container starts
//! right of the traffic lights (see [`leading_inset_points`]). The active
//! tab is a nested pill. The close `×` is reserved on the **left** of each
//! tab (painted on hover only, layout-stable); the `⌘N` shortcut hint sits
//! on the right for the first 9 tabs.

/// Minimum tab width: tabs split the container equally until this width
/// is reached, then the strip scrolls horizontally.
pub const MIN_TAB_WIDTH_POINTS: f32 = 100.0;

/// Fully rounded tab pills nested inside the container.
pub const TAB_CORNER_RADIUS_POINTS: u8 = 8;

/// Container pill corner radius.
pub const BAR_CORNER_RADIUS_POINTS: u8 = 10;

/// Horizontal inset of the container pill from the window edges.
pub const BAR_INSET_X_POINTS: f32 = 8.0;

/// Vertical inset of the container pill from the bar edges.
pub const BAR_INSET_Y_POINTS: f32 = 5.0;

/// Reserved hit width for the `×` close affordance at the tab's left edge.
/// Always allocated (layout-stable); only painted on hover.
pub const TAB_CLOSE_POINTS: f32 = 24.0;

/// Reserved width for the `⌘N` shortcut hint at the tab's right edge.
/// Only reserved when [`shortcut_label`] returns `Some`.
pub const TAB_SHORTCUT_POINTS: f32 = 34.0;

/// Gap between the title text and the close/shortcut reserves.
pub const TAB_GAP_POINTS: f32 = 4.0;

/// Diameter of the detached circular `+` new-tab button.
pub const PLUS_DIAMETER_POINTS: f32 = 28.0;

/// Gap between the tab container and the `+` button.
pub const PLUS_GAP_POINTS: f32 = 8.0;

/// Width reserved on the left of the bar for the OS traffic lights
/// (lights + margin). 76pt on macOS, 0 elsewhere. A single `cfg!`
/// definition so every platform type-checks the same code.
pub const TRAFFIC_LIGHTS_WIDTH_POINTS: f32 = if cfg!(target_os = "macos") { 76.0 } else { 0.0 };

/// Leading inset before the tab container: the normal margin plus the
/// traffic-light reserve on macOS.
pub fn leading_inset_points() -> f32 {
    BAR_INSET_X_POINTS + TRAFFIC_LIGHTS_WIDTH_POINTS
}

/// Width available for the tab container given the full window width
/// `screen_w` in points (minus leading/trailing insets, the `+` button
/// and its gap). Never negative.
pub fn container_width_points(screen_w: f32) -> f32 {
    (screen_w
        - leading_inset_points()
        - BAR_INSET_X_POINTS
        - PLUS_DIAMETER_POINTS
        - PLUS_GAP_POINTS)
        .max(0.0)
}

/// `+` new-tab button rect `[x, y, w, h]` in bar-local points.
pub fn plus_rect(screen_w: f32) -> [f32; 4] {
    let leading = leading_inset_points();
    let cw = container_width_points(screen_w);
    [
        leading + cw + PLUS_GAP_POINTS,
        BAR_INSET_Y_POINTS,
        PLUS_DIAMETER_POINTS,
        PLUS_DIAMETER_POINTS,
    ]
}

/// True when `(x, y)` (bar-local points) is empty draggable chrome: inside
/// the bar but not on the traffic lights, a tab, or the `+` button.
/// Callers on macOS start a native window drag (`Window::drag_window`)
/// in this case so the merged titlebar behaves like Brave's.
pub fn is_titlebar_drag(x: f32, y: f32, screen_w: f32, bar_h: f32) -> bool {
    if bar_h <= 0.0 || y < 0.0 || y >= bar_h {
        return false;
    }
    if x < 0.0 || x >= screen_w {
        return false;
    }
    // OS traffic lights handle their own clicks.
    if x < TRAFFIC_LIGHTS_WIDTH_POINTS {
        return false;
    }
    // Tabs tile the container; only the vertical insets above/below them
    // count as empty when `x` falls inside the container range.
    let leading = leading_inset_points();
    let cw = container_width_points(screen_w);
    let container_h = (bar_h - BAR_INSET_Y_POINTS * 2.0).max(0.0);
    let in_tab_row = y >= BAR_INSET_Y_POINTS && y < BAR_INSET_Y_POINTS + container_h;
    if in_tab_row && x >= leading && x < leading + cw {
        return false;
    }
    let p = plus_rect(screen_w);
    if x >= p[0] && x < p[0] + p[2] && y >= p[1] && y < p[1] + p[3] {
        return false;
    }
    true
}

/// Visual inset of the active/hover pill inside its tab cell.
pub const TAB_PILL_INSET_X: f32 = 1.0;
/// Visual inset of the active/hover pill inside its tab cell.
pub const TAB_PILL_INSET_Y: f32 = 2.0;

/// `⌘1`..`⌘9` for the first nine tabs, `None` afterwards.
pub fn shortcut_label(index: usize) -> Option<String> {
    if index < 9 {
        Some(format!("⌘{}", index + 1))
    } else {
        None
    }
}

/// Equal tab width splitting `available` points: 50% each for 2 tabs,
/// 33% each for 3, and so on down to [`MIN_TAB_WIDTH_POINTS`].
///
/// Callers scroll horizontally when `tab_width * count > available`
/// (i.e. every tab is at the minimum and still doesn't fit).
pub fn tab_width(available: f32, count: usize) -> f32 {
    if count == 0 || !available.is_finite() {
        return MIN_TAB_WIDTH_POINTS;
    }
    if available <= 0.0 {
        return MIN_TAB_WIDTH_POINTS;
    }
    (available / count as f32).max(MIN_TAB_WIDTH_POINTS)
}

/// Close hit box `[x, y, w, h]` for a tab starting at `tab_x` (left side).
pub fn close_hit(tab_x: f32, bar_h: f32) -> [f32; 4] {
    [tab_x, 0.0, TAB_CLOSE_POINTS, bar_h]
}

/// Max title width for a tab of width `tab_w`, reserving the left close
/// box and (when present) the right shortcut hint. Never negative.
pub fn title_max_width(tab_w: f32, has_shortcut: bool) -> f32 {
    let reserved = TAB_CLOSE_POINTS
        + TAB_GAP_POINTS * 2.0
        + if has_shortcut {
            TAB_SHORTCUT_POINTS
        } else {
            0.0
        };
    (tab_w - reserved).max(0.0)
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
    fn close_hit_sits_at_tab_left_edge() {
        assert_eq!(close_hit(0.0, 38.0), [0.0, 0.0, 24.0, 38.0]);
        assert_eq!(close_hit(180.0, 38.0), [180.0, 0.0, 24.0, 38.0]);
    }

    #[test]
    fn title_max_width_reserves_close_and_shortcut() {
        // 180 - 24 - 2*4 - 34 = 114 with shortcut.
        assert_eq!(title_max_width(180.0, true), 114.0);
        // 180 - 24 - 2*4 = 148 without shortcut.
        assert_eq!(title_max_width(180.0, false), 148.0);
        assert_eq!(title_max_width(10.0, true), 0.0);
    }

    #[test]
    fn shortcut_label_first_nine_only() {
        assert_eq!(shortcut_label(0), Some("⌘1".to_string()));
        assert_eq!(shortcut_label(8), Some("⌘9".to_string()));
        assert_eq!(shortcut_label(9), None);
        assert_eq!(shortcut_label(99), None);
    }

    #[test]
    fn tab_width_splits_equally_then_scrolls() {
        // 2 tabs split 50/50, 3 tabs 33/33/33 — no maximum.
        assert_eq!(tab_width(600.0, 2), 300.0);
        assert_eq!(tab_width(600.0, 3), 200.0);
        assert_eq!(tab_width(1200.0, 3), 400.0);
        // Narrow windows shrink to the minimum, then scroll.
        assert_eq!(tab_width(240.0, 3), MIN_TAB_WIDTH_POINTS);
        assert_eq!(tab_width(0.0, 3), MIN_TAB_WIDTH_POINTS);
        assert_eq!(tab_width(600.0, 0), MIN_TAB_WIDTH_POINTS);
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

    #[test]
    fn traffic_reserve_matches_platform() {
        if cfg!(target_os = "macos") {
            assert_eq!(TRAFFIC_LIGHTS_WIDTH_POINTS, 76.0);
        } else {
            assert_eq!(TRAFFIC_LIGHTS_WIDTH_POINTS, 0.0);
        }
        assert_eq!(
            leading_inset_points(),
            BAR_INSET_X_POINTS + TRAFFIC_LIGHTS_WIDTH_POINTS
        );
    }

    #[test]
    fn container_and_plus_math() {
        let leading = leading_inset_points();
        assert_eq!(
            container_width_points(800.0),
            800.0 - leading - BAR_INSET_X_POINTS - PLUS_DIAMETER_POINTS - PLUS_GAP_POINTS
        );
        assert_eq!(container_width_points(0.0), 0.0);
        let p = plus_rect(800.0);
        assert_eq!(
            p[0],
            leading + container_width_points(800.0) + PLUS_GAP_POINTS
        );
        assert_eq!(p[1], BAR_INSET_Y_POINTS);
        assert_eq!((p[2], p[3]), (PLUS_DIAMETER_POINTS, PLUS_DIAMETER_POINTS));
    }

    #[test]
    fn titlebar_drag_hit_testing() {
        let bar_h = 38.0;
        let screen_w = 800.0;
        let mid_y = BAR_INSET_Y_POINTS + 4.0;
        // Hidden bar: nothing is draggable.
        assert!(!is_titlebar_drag(400.0, 10.0, screen_w, 0.0));
        // On a tab: not draggable.
        assert!(!is_titlebar_drag(
            leading_inset_points() + 4.0,
            mid_y,
            screen_w,
            bar_h
        ));
        // On the `+` button: not draggable.
        let p = plus_rect(screen_w);
        assert!(!is_titlebar_drag(p[0] + 1.0, p[1] + 1.0, screen_w, bar_h));
        // Trailing margin past the `+` button: draggable.
        assert!(is_titlebar_drag(screen_w - 2.0, mid_y, screen_w, bar_h));
        // Vertical inset above the tab row, inside the container x-range:
        // draggable.
        assert!(is_titlebar_drag(
            leading_inset_points() + 100.0,
            2.0,
            screen_w,
            bar_h
        ));
        // Left margin: draggable on plain platforms, traffic lights on macOS.
        if cfg!(target_os = "macos") {
            assert!(!is_titlebar_drag(4.0, mid_y, screen_w, bar_h));
        } else {
            assert!(is_titlebar_drag(4.0, mid_y, screen_w, bar_h));
        }
    }
}
