//! Tab-strip layout math (Ghostty style).
//!
//! Pure, toolkit-free geometry so it stays unit-testable: the egui overlay
//! only paints what these helpers return. All values are logical points.
//! Dimensions come from [`crate::config::TabbarConfig`].
//!
//! Layout: an inset pill container holds equal-width tabs splitting the
//! available width (50% each for 2, 33% each for 3, …) down to
//! `min_tab_width`, then scrolling. On macOS the container starts
//! right of the traffic lights (see [`leading_inset_points`]). The active
//! tab is a nested pill. The close `×` is reserved on the **left** of each
//! tab (painted on hover only, layout-stable); the `⌘N` shortcut hint sits
//! on the right for the first 9 tabs. A lone tab keeps the container outline
//! but renders as a plain title (no nested pill, close box or shortcut).

use crate::config::TabbarConfig;

/// A painted tab's identity is independent of its position (and label).
pub struct TabLabel {
    pub id: u64,
    pub title: String,
}

/// Move an existing tab to this final, zero-based position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabMove {
    pub id: u64,
    pub to: usize,
}

const DRAG_THRESHOLD: f32 = 6.0;
const SCROLL_EDGE: f32 = 24.0;
const SCROLL_SPEED: f32 = 600.0;

#[derive(Clone, Copy, Debug)]
struct TabPress {
    id: u64,
    x: f32,
    grab: f32,
    dragging: bool,
}

/// App-owned pointer gesture. Native events advance it even when presentation
/// is deferred; the widget only projects the preview and returns a drop action.
/// Cancellation retains the matching release, never forwarding it to a PTY.
#[derive(Default)]
pub struct TabDrag {
    press: Option<TabPress>,
    pointer: Option<[f32; 2]>,
    held: bool,
    swallow_release: bool,
    released: bool,
    cancelled: bool,
}

/// Last/current strip geometry, all in logical points. `left` includes the
/// horizontal scroll offset; `viewport` excludes traffic lights and `+`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DragLayout {
    pub viewport: [f32; 4],
    pub left: f32,
    pub width: f32,
    pub count: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DragPreview {
    pub to: usize,
    pub left: f32,
}

fn contains(rect: [f32; 4], [x, y]: [f32; 2]) -> bool {
    x >= rect[0] && x < rect[2] && y >= rect[1] && y < rect[3]
}

impl TabDrag {
    /// Begin a primary press on the strip. Only a tab body supplies a source;
    /// close/plus/empty chrome still own their release, but cannot start a drag.
    pub(crate) fn begin(&mut self, source: Option<(u64, [f32; 4])>, pointer: [f32; 2]) {
        *self = Self {
            press: source.and_then(|(id, rect)| {
                let width = rect[2] - rect[0];
                (width > 0.0 && width.is_finite()).then(|| TabPress {
                    id,
                    x: pointer[0],
                    grab: ((pointer[0] - rect[0]) / width).clamp(0.0, 1.0),
                    dragging: false,
                })
            }),
            pointer: Some(pointer),
            held: true,
            swallow_release: true,
            ..Self::default()
        };
    }

    /// Return the tab to activate exactly once, when horizontal motion crosses
    /// the threshold. A vertical move alone is not a reorder gesture.
    pub(crate) fn moved(&mut self, pointer: [f32; 2]) -> Option<u64> {
        self.pointer = Some(pointer);
        let press = self.press.as_mut()?;
        if self.held && !press.dragging && (pointer[0] - press.x).abs() >= DRAG_THRESHOLD {
            press.dragging = true;
            return Some(press.id);
        }
        None
    }

    pub(crate) fn pointer_gone(&mut self) {
        if self.held {
            self.pointer = None;
        }
    }

    pub(crate) fn captures_pointer(&self) -> bool {
        self.held
    }

    pub(crate) fn source(&self) -> Option<u64> {
        self.press.map(|press| press.id)
    }

    pub(crate) fn dragging(&self) -> bool {
        self.press.is_some_and(|press| press.dragging)
    }

    pub(crate) fn suppress_clicks(&self) -> bool {
        self.cancelled || self.dragging()
    }

    /// The matching release is swallowed even after Escape or focus loss.
    pub(crate) fn release(&mut self) -> bool {
        if !self.swallow_release {
            return false;
        }
        self.swallow_release = false;
        self.held = false;
        self.released = true;
        true
    }

    pub(crate) fn cancel(&mut self) -> bool {
        let changed = self.held || self.press.is_some();
        if changed {
            self.press = None;
            self.cancelled = true;
        }
        changed
    }

    pub(crate) fn focus_lost(&mut self) {
        self.cancel();
        // A release outside this window may never arrive. Stop capturing motion
        // now, but retain the release guard until a release or a fresh press.
        self.held = false;
        self.released = true;
    }

    /// A vanished source must not turn into the tab that took its old slot.
    pub(crate) fn retain_source(&mut self, valid: bool) {
        if self.press.is_some() && !valid {
            self.cancel();
        }
    }

    pub(crate) fn preview(&self, layout: DragLayout) -> Option<DragPreview> {
        let press = self.press.filter(|press| press.dragging)?;
        let pointer = self.pointer?;
        if layout.count < 2 || layout.width <= 0.0 || !layout.width.is_finite() {
            return None;
        }
        if self.released && !contains(layout.viewport, pointer) {
            return None;
        }
        let center = pointer[0] + (0.5 - press.grab) * layout.width;
        if !center.is_finite() {
            return None;
        }
        let to = ((center - layout.left) / layout.width)
            .floor()
            .clamp(0.0, (layout.count - 1) as f32) as usize;
        let left = if self.released {
            layout.left + to as f32 * layout.width
        } else {
            (pointer[0] - press.grab * layout.width).clamp(
                layout.viewport[0],
                (layout.viewport[2] - layout.width).max(layout.viewport[0]),
            )
        };
        Some(DragPreview { to, left })
    }

    pub(crate) fn drop_action(&self, layout: DragLayout, from: usize) -> Option<TabMove> {
        if !self.released {
            return None;
        }
        let preview = self.preview(layout)?;
        (preview.to != from).then_some(TabMove {
            id: self.source()?,
            to: preview.to,
        })
    }

    /// Retire the ended gesture after the widget has suppressed its click and
    /// gathered its drop action. Keep cancellation until the next native press.
    pub(crate) fn finish_frame(&mut self) {
        if self.released {
            self.press = None;
            self.released = false;
        }
    }

    pub(crate) fn scroll_offset(&self, layout: DragLayout, offset: f32, dt: f32) -> f32 {
        let max = (layout.width * layout.count as f32 - (layout.viewport[2] - layout.viewport[0]))
            .max(0.0);
        let offset = offset.clamp(0.0, max);
        if !self.held || !self.dragging() {
            return offset;
        }
        let Some(pointer) = self.pointer.filter(|&pos| contains(layout.viewport, pos)) else {
            return offset;
        };
        let edge = SCROLL_EDGE.min((layout.viewport[2] - layout.viewport[0]) * 0.5);
        if edge <= 0.0 || !dt.is_finite() {
            return offset;
        }
        let left = ((layout.viewport[0] + edge - pointer[0]) / edge).clamp(0.0, 1.0);
        let right = ((pointer[0] - (layout.viewport[2] - edge)) / edge).clamp(0.0, 1.0);
        (offset + (right - left) * SCROLL_SPEED * dt.clamp(0.0, 0.05)).clamp(0.0, max)
    }
}

/// Remap an index after removing `from` and inserting it at final index `to`.
/// Used for both the active tab and the persistent PTY service cursor.
pub(crate) fn index_after_move(index: usize, from: usize, to: usize) -> usize {
    if index == from {
        to
    } else if from < to && index > from && index <= to {
        index - 1
    } else if to < from && index >= to && index < from {
        index + 1
    } else {
        index
    }
}

/// Width reserved on the left of the bar for the OS traffic lights
/// (lights + margin). 76pt on macOS, 0 elsewhere. A single `cfg!`
/// definition so every platform type-checks the same code.
pub const TRAFFIC_LIGHTS_WIDTH_POINTS: f32 = if cfg!(target_os = "macos") { 76.0 } else { 0.0 };

/// Visual inset of the active/hover pill inside its tab cell.
pub const TAB_PILL_INSET_X: f32 = 1.0;
/// Visual inset of the active/hover pill inside its tab cell.
pub const TAB_PILL_INSET_Y: f32 = 2.0;

/// Leading inset before the tab container: the normal margin plus the
/// traffic-light reserve on macOS.
pub fn leading_inset_points(config: &TabbarConfig) -> f32 {
    config.inset_x + TRAFFIC_LIGHTS_WIDTH_POINTS
}

/// Width available for the tab container given the full window width
/// `screen_w` in points (minus leading/trailing insets, the `+` button
/// and its gap). Never negative.
pub fn container_width_points(screen_w: f32, config: &TabbarConfig) -> f32 {
    (screen_w
        - leading_inset_points(config)
        - config.inset_x
        - config.plus_diameter
        - config.plus_gap)
        .max(0.0)
}

/// `+` new-tab button rect `[x, y, w, h]` in bar-local points.
pub fn plus_rect(screen_w: f32, config: &TabbarConfig) -> [f32; 4] {
    let leading = leading_inset_points(config);
    let cw = container_width_points(screen_w, config);
    [
        leading + cw + config.plus_gap,
        config.inset_y,
        config.plus_diameter,
        config.plus_diameter,
    ]
}

/// True when `(x, y)` (bar-local points) is empty draggable chrome: inside
/// the bar but not on the traffic lights, a tab, or the `+` button.
/// Callers on macOS start a native window drag (`Window::drag_window`)
/// in this case so the merged titlebar behaves like Brave's.
pub fn is_titlebar_drag(
    x: f32,
    y: f32,
    screen_w: f32,
    bar_h: f32,
    config: &TabbarConfig,
    painted: StripTarget,
) -> bool {
    // The last painted controls take precedence over geometric fallback (for
    // example during a resize/config transition). Never reinterpret a tab,
    // close box or plus press as a window drag.
    if painted != StripTarget::Empty {
        return false;
    }
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
    let leading = leading_inset_points(config);
    let cw = container_width_points(screen_w, config);
    let container_h = (bar_h - config.inset_y * 2.0).max(0.0);
    let in_tab_row = y >= config.inset_y && y < config.inset_y + container_h;
    if in_tab_row && x >= leading && x < leading + cw {
        return false;
    }
    let p = plus_rect(screen_w, config);
    if x >= p[0] && x < p[0] + p[2] && y >= p[1] && y < p[1] + p[3] {
        return false;
    }
    true
}

/// `⌘1`..`⌘9` for the first nine tabs, `None` afterwards.
pub fn shortcut_label(index: usize) -> Option<String> {
    if index < 9 {
        Some(format!("⌘{}", index + 1))
    } else {
        None
    }
}

/// Equal tab width splitting `available` points: 50% each for 2 tabs,
/// 33% each for 3, and so on down to `config.min_tab_width`.
///
/// Callers scroll horizontally when `tab_width * count > available`
/// (i.e. every tab is at the minimum and still doesn't fit).
pub fn tab_width(available: f32, count: usize, config: &TabbarConfig) -> f32 {
    if count == 0 || !available.is_finite() {
        return config.min_tab_width;
    }
    if available <= 0.0 {
        return config.min_tab_width;
    }
    (available / count as f32).max(config.min_tab_width)
}

/// Close hit box `[x, y, w, h]` for a tab starting at `tab_x` (left side).
pub fn close_hit(tab_x: f32, bar_h: f32, config: &TabbarConfig) -> [f32; 4] {
    [tab_x, 0.0, config.close_reserve, bar_h]
}

/// Max title width for a tab of width `tab_w`, reserving the left close
/// box and (when present) the right shortcut hint. Never negative.
pub fn title_max_width(tab_w: f32, has_shortcut: bool, config: &TabbarConfig) -> f32 {
    let reserved = config.close_reserve
        + config.gap * 2.0
        + if has_shortcut {
            config.shortcut_reserve
        } else {
            0.0
        };
    (tab_w - reserved).max(0.0)
}

/// Max title width for the lone tab, which spans the whole container and
/// renders without a close box or shortcut hint: only a side gap is kept
/// so the text never touches the container edge. Never negative.
pub fn single_tab_title_max_width(tab_w: f32, config: &TabbarConfig) -> f32 {
    (tab_w - config.gap * 2.0).max(0.0)
}

/// Truncate `title` with an ellipsis so `measure(fitted) <= max_width`.
///
/// `measure` maps text to logical width (the overlay passes a font-based
/// measurer; tests pass a stub). Always terminates: worst case returns an
/// empty string when even `…` doesn't fit.
pub fn fit_title(title: &str, max_width: f32, measure: &mut dyn FnMut(&str) -> f32) -> String {
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

/// Tab-strip element whose hover state is painted: a tab's pill, its close
/// `×`, or the `+` button. Elsewhere on the strip nothing reacts to hover.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StripTarget {
    #[default]
    Empty,
    Tab(u64),
    Close(u64),
    NewTab,
}

/// Hovered element at `(x, y)` points, from the interactive rects
/// `[min_x, min_y, max_x, max_y]` recorded by the last painted strip in paint
/// order. Later rects are on top: a close box wins over the tab under it,
/// like egui's own hover.
pub fn strip_target_at(targets: &[(StripTarget, [f32; 4])], x: f32, y: f32) -> StripTarget {
    targets
        .iter()
        .rev()
        .find(|(_, r)| x >= r[0] && x < r[2] && y >= r[1] && y < r[3])
        .map_or(StripTarget::Empty, |(target, _)| *target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TabbarConfig;

    fn cfg() -> TabbarConfig {
        TabbarConfig::default()
    }

    fn drag_layout() -> DragLayout {
        DragLayout {
            viewport: [100.0, 5.0, 500.0, 35.0],
            left: 100.0,
            width: 100.0,
            count: 4,
        }
    }

    fn pressed_drag() -> TabDrag {
        let mut drag = TabDrag::default();
        drag.begin(Some((42, [100.0, 5.0, 200.0, 35.0])), [150.0, 20.0]);
        drag
    }

    #[test]
    fn move_index_matches_remove_then_insert_for_every_position() {
        for len in 1..10 {
            for from in 0..len {
                for to in 0..len {
                    let mut tabs: Vec<_> = (0..len).collect();
                    let moved = tabs.remove(from);
                    tabs.insert(to, moved);
                    for old in 0..len {
                        assert_eq!(tabs[index_after_move(old, from, to)], old);
                    }
                }
            }
        }
    }

    #[test]
    fn drag_threshold_is_horizontal_and_activates_once_without_committing() {
        let mut drag = pressed_drag();
        assert!(drag.captures_pointer());
        assert_eq!(drag.moved([155.0, 100.0]), None);
        assert!(!drag.dragging());
        assert_eq!(drag.moved([156.0, 20.0]), Some(42));
        assert_eq!(drag.moved([350.0, 20.0]), None);
        assert_eq!(drag.preview(drag_layout()).unwrap().to, 2);
        assert_eq!(drag.drop_action(drag_layout(), 0), None);
        assert!(drag.suppress_clicks());
        assert!(drag.release());
        assert!(!drag.captures_pointer());
        assert_eq!(
            drag.drop_action(drag_layout(), 0),
            Some(TabMove { id: 42, to: 2 })
        );
        drag.finish_frame();
        assert_eq!(drag.drop_action(drag_layout(), 0), None);
        assert!(!drag.dragging());
        assert!(!drag.release());
    }

    #[test]
    fn drop_uses_the_grab_fraction_current_width_and_scrolled_origin() {
        let mut drag = TabDrag::default();
        // Grab near the right edge, not the centre. Motion alone must not
        // immediately jump to the neighbouring slot.
        drag.begin(Some((8, [300.0, 5.0, 400.0, 35.0])), [390.0, 20.0]);
        drag.moved([383.0, 20.0]);
        assert_eq!(drag.preview(drag_layout()).unwrap().to, 2);
        // Recompute after a resize + horizontal scroll; no stored tab index or
        // pixel offset survives as an identity/layout assumption.
        let layout = DragLayout {
            left: -100.0,
            width: 200.0,
            ..drag_layout()
        };
        drag.moved([185.0, 20.0]);
        assert_eq!(drag.preview(layout).unwrap().to, 1);
        drag.release();
        assert_eq!(drag.drop_action(layout, 2), Some(TabMove { id: 8, to: 1 }));
        assert_eq!(
            drag.preview(layout).unwrap().left,
            100.0,
            "release snaps to the final slot"
        );
    }

    #[test]
    fn invalid_or_outside_drops_do_not_reorder_or_turn_into_clicks() {
        for outside in [[99.0, 20.0], [500.0, 20.0], [350.0, 35.0], [350.0, 100.0]] {
            let mut drag = pressed_drag();
            drag.moved(outside);
            drag.release();
            assert_eq!(drag.drop_action(drag_layout(), 0), None);
            assert!(drag.suppress_clicks());
        }
        let mut drag = pressed_drag();
        drag.moved([450.0, 20.0]);
        drag.pointer_gone();
        drag.release();
        assert_eq!(drag.drop_action(drag_layout(), 0), None);
        for count in [0, 1] {
            assert!(
                drag.preview(DragLayout {
                    count,
                    ..drag_layout()
                })
                .is_none()
            );
        }
        let mut click = pressed_drag();
        click.release();
        assert_eq!(click.drop_action(drag_layout(), 0), None);
        assert!(!click.suppress_clicks());
    }

    #[test]
    fn leaving_the_window_after_releasing_inside_does_not_change_the_drop() {
        let mut drag = pressed_drag();
        drag.moved([350.0, 20.0]);
        drag.release();
        drag.pointer_gone();
        assert_eq!(
            drag.drop_action(drag_layout(), 0),
            Some(TabMove { id: 42, to: 2 })
        );
    }

    #[test]
    fn cancel_source_exit_and_focus_loss_keep_only_the_matching_release() {
        for focus_loss in [false, true] {
            let mut drag = pressed_drag();
            drag.moved([350.0, 20.0]);
            if focus_loss {
                drag.focus_lost();
                assert!(
                    !drag.captures_pointer(),
                    "a missing outside release cannot stick capture"
                );
            } else {
                drag.retain_source(false);
                assert!(drag.captures_pointer());
            }
            assert_eq!(drag.moved([450.0, 20.0]), None);
            assert!(drag.release());
            assert!(drag.suppress_clicks());
            assert_eq!(drag.drop_action(drag_layout(), 0), None);
            drag.finish_frame();
            assert!(!drag.release());
            drag.begin(None, [480.0, 20.0]);
            assert!(!drag.suppress_clicks(), "a new press retires cancellation");
            assert_eq!(
                drag.moved([110.0, 20.0]),
                None,
                "close/plus cannot start a drag"
            );
            assert!(!drag.dragging());
        }
    }

    #[test]
    fn edge_scroll_is_clamped_and_stops_outside_in_the_middle_or_after_release() {
        let layout = DragLayout {
            count: 10,
            ..drag_layout()
        };
        let mut drag = pressed_drag();
        drag.moved([499.0, 20.0]);
        assert!(drag.scroll_offset(layout, 0.0, 1.0 / 60.0) > 0.0);
        assert_eq!(drag.scroll_offset(layout, 600.0, 1.0), 600.0);
        assert_eq!(
            drag.scroll_offset(drag_layout(), 0.0, 1.0),
            0.0,
            "no overflow"
        );
        drag.moved([100.0, 20.0]);
        assert!(drag.scroll_offset(layout, 100.0, 1.0 / 60.0) < 100.0);
        assert_eq!(drag.scroll_offset(layout, 0.0, 1.0), 0.0);
        for pos in [[300.0, 20.0], [499.0, 50.0], [501.0, 20.0]] {
            drag.moved(pos);
            assert_eq!(drag.scroll_offset(layout, 100.0, 1.0), 100.0);
        }
        drag.moved([499.0, 20.0]);
        drag.release();
        assert_eq!(drag.scroll_offset(layout, 100.0, 1.0), 100.0);
        let mut drag = pressed_drag();
        drag.moved([499.0, 20.0]);
        drag.cancel();
        assert_eq!(drag.scroll_offset(layout, 100.0, 1.0), 100.0);
    }

    #[test]
    fn close_hit_sits_at_tab_left_edge() {
        let cfg = cfg();
        assert_eq!(close_hit(0.0, 38.0, &cfg), [0.0, 0.0, 24.0, 38.0]);
        assert_eq!(close_hit(180.0, 38.0, &cfg), [180.0, 0.0, 24.0, 38.0]);
    }

    #[test]
    fn strip_hover_follows_painted_rects_with_close_boxes_on_top() {
        let targets = [
            (StripTarget::Tab(0), [84.0, 5.0, 284.0, 33.0]),
            (StripTarget::Close(0), [84.0, 5.0, 108.0, 33.0]),
            (StripTarget::Tab(1), [284.0, 5.0, 484.0, 33.0]),
            (StripTarget::Close(1), [284.0, 5.0, 308.0, 33.0]),
            (StripTarget::NewTab, [492.0, 5.0, 520.0, 33.0]),
        ];
        assert_eq!(strip_target_at(&targets, 200.0, 20.0), StripTarget::Tab(0));
        assert_eq!(strip_target_at(&targets, 90.0, 20.0), StripTarget::Close(0));
        // Shared edges belong to the right-hand tab, once.
        assert_eq!(
            strip_target_at(&targets, 284.0, 20.0),
            StripTarget::Close(1)
        );
        assert_eq!(strip_target_at(&targets, 400.0, 20.0), StripTarget::Tab(1));
        assert_eq!(strip_target_at(&targets, 500.0, 20.0), StripTarget::NewTab);
        // Traffic lights, insets and the gap before `+` react to nothing.
        for (x, y) in [(30.0, 20.0), (200.0, 2.0), (488.0, 20.0), (600.0, 20.0)] {
            assert_eq!(strip_target_at(&targets, x, y), StripTarget::Empty);
        }
        assert_eq!(strip_target_at(&[], 200.0, 20.0), StripTarget::Empty);
    }

    #[test]
    fn title_max_width_reserves_close_and_shortcut() {
        let cfg = cfg();
        // 180 - 24 - 2*4 - 34 = 114 with shortcut.
        assert_eq!(title_max_width(180.0, true, &cfg), 114.0);
        // 180 - 24 - 2*4 = 148 without shortcut.
        assert_eq!(title_max_width(180.0, false, &cfg), 148.0);
        assert_eq!(title_max_width(10.0, true, &cfg), 0.0);
    }

    #[test]
    fn single_tab_title_max_width_keeps_only_side_gaps() {
        let cfg = cfg();
        // No close box or shortcut reserve: 180 - 2*4 = 172.
        assert_eq!(single_tab_title_max_width(180.0, &cfg), 172.0);
        assert_eq!(single_tab_title_max_width(0.0, &cfg), 0.0);
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
        let cfg = cfg();
        // 2 tabs split 50/50, 3 tabs 33/33/33 — no maximum.
        assert_eq!(tab_width(600.0, 2, &cfg), 300.0);
        assert_eq!(tab_width(600.0, 3, &cfg), 200.0);
        assert_eq!(tab_width(1200.0, 3, &cfg), 400.0);
        // Narrow windows shrink to the minimum, then scroll.
        assert_eq!(tab_width(240.0, 3, &cfg), cfg.min_tab_width);
        assert_eq!(tab_width(0.0, 3, &cfg), cfg.min_tab_width);
        assert_eq!(tab_width(600.0, 0, &cfg), cfg.min_tab_width);
    }

    #[test]
    fn fit_title_passes_through_when_narrow() {
        let mut m = |s: &str| s.chars().count() as f32 * 8.0;
        assert_eq!(fit_title("hi", 144.0, &mut m), "hi");
    }

    #[test]
    fn fit_title_truncates_with_ellipsis() {
        let mut m = |s: &str| s.chars().count() as f32 * 8.0;
        // 144pt / 8 = 18 chars max; 17 chars + ellipsis.
        let out = fit_title("abcdefghijklmnopqrstuvwxyz", 144.0, &mut m);
        assert_eq!(out, "abcdefghijklmnopq…");
        assert!(m(&out) <= 144.0);
    }

    #[test]
    fn fit_title_empty_when_nothing_fits() {
        let mut m = |s: &str| s.chars().count() as f32 * 8.0;
        assert_eq!(fit_title("hi", 0.0, &mut m), "");
        assert_eq!(fit_title("hi", 4.0, &mut m), "");
    }

    #[test]
    fn fit_title_handles_multibyte_boundaries() {
        let mut m = |s: &str| s.chars().count() as f32 * 8.0;
        let out = fit_title("héllo wörld", 40.0, &mut m);
        // 5 chars max: 4 chars + ellipsis, split on char boundary.
        assert_eq!(out, "héll…");
        assert!(out.is_char_boundary(out.len()));
    }

    #[test]
    fn traffic_reserve_matches_platform() {
        let cfg = cfg();
        if cfg!(target_os = "macos") {
            assert_eq!(TRAFFIC_LIGHTS_WIDTH_POINTS, 76.0);
        } else {
            assert_eq!(TRAFFIC_LIGHTS_WIDTH_POINTS, 0.0);
        }
        assert_eq!(
            leading_inset_points(&cfg),
            cfg.inset_x + TRAFFIC_LIGHTS_WIDTH_POINTS
        );
    }

    #[test]
    fn container_and_plus_math() {
        let cfg = cfg();
        let leading = leading_inset_points(&cfg);
        assert_eq!(
            container_width_points(800.0, &cfg),
            800.0 - leading - cfg.inset_x - cfg.plus_diameter - cfg.plus_gap
        );
        assert_eq!(container_width_points(0.0, &cfg), 0.0);
        let p = plus_rect(800.0, &cfg);
        assert_eq!(
            p[0],
            leading + container_width_points(800.0, &cfg) + cfg.plus_gap
        );
        assert_eq!(p[1], cfg.inset_y);
        assert_eq!((p[2], p[3]), (cfg.plus_diameter, cfg.plus_diameter));
        assert_eq!(800.0 - (p[0] + p[2]), cfg.inset_x);
    }

    #[test]
    fn titlebar_drag_hit_testing() {
        let cfg = cfg();
        let bar_h = 38.0;
        let screen_w = 800.0;
        let mid_y = cfg.inset_y + 4.0;
        // Hidden bar: nothing is draggable.
        assert!(!is_titlebar_drag(
            400.0,
            10.0,
            screen_w,
            0.0,
            &cfg,
            StripTarget::Empty
        ));
        // On a tab: not draggable.
        assert!(!is_titlebar_drag(
            leading_inset_points(&cfg) + 4.0,
            mid_y,
            screen_w,
            bar_h,
            &cfg,
            StripTarget::Empty
        ));
        // On the `+` button: not draggable.
        let p = plus_rect(screen_w, &cfg);
        assert!(!is_titlebar_drag(
            p[0] + 1.0,
            p[1] + 1.0,
            screen_w,
            bar_h,
            &cfg,
            StripTarget::Empty
        ));
        // Trailing margin past the `+` button: draggable.
        assert!(is_titlebar_drag(
            screen_w - 2.0,
            mid_y,
            screen_w,
            bar_h,
            &cfg,
            StripTarget::Empty
        ));
        // Vertical inset above the tab row, inside the container x-range:
        // draggable.
        assert!(is_titlebar_drag(
            leading_inset_points(&cfg) + 100.0,
            2.0,
            screen_w,
            bar_h,
            &cfg,
            StripTarget::Empty
        ));
        // Left margin: draggable on plain platforms, traffic lights on macOS.
        if cfg!(target_os = "macos") {
            assert!(!is_titlebar_drag(
                4.0,
                mid_y,
                screen_w,
                bar_h,
                &cfg,
                StripTarget::Empty
            ));
        } else {
            assert!(is_titlebar_drag(
                4.0,
                mid_y,
                screen_w,
                bar_h,
                &cfg,
                StripTarget::Empty
            ));
        }
    }

    #[test]
    fn painted_controls_win_over_empty_chrome_geometry_during_layout_transitions() {
        let cfg = cfg();
        // Geometric fallback alone considers this trailing margin draggable.
        // A control painted here by the previous layout must still own its hit.
        let (x, y) = (798.0, 20.0);
        assert!(is_titlebar_drag(
            x,
            y,
            800.0,
            38.0,
            &cfg,
            StripTarget::Empty
        ));
        for target in [
            StripTarget::Tab(42),
            StripTarget::Close(42),
            StripTarget::NewTab,
        ] {
            assert!(
                !is_titlebar_drag(x, y, 800.0, 38.0, &cfg, target),
                "{target:?}"
            );
        }
    }
}
