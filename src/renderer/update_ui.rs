//! Update notice (egui overlay), drawn inside the main egui pass.
//!
//! The model is [`crate::update::state::UpdateNotice`], derived by the
//! update controller; this module only paints it and reports a button
//! click back to the app, plus the painted rects for pointer routing.
//! Every control uses the non-focusable `Sense::CLICK`, and the labels are
//! not selectable, so the notice never takes egui keyboard focus or IME
//! from the terminal. The notice table is in `docs/update.md#state-machine`.

use crate::config::ScrollbarConfig;
use crate::theme::Theme;
use crate::update::state::{Failure, InstallIssue, UpdateNotice};

/// Gap (logical points) between the notice and the window's bottom edge,
/// and between the notice and the scrollbar track on its right.
const NOTICE_MARGIN: f32 = 8.0;
/// Upper bound on the notice width; text wraps inside it.
const NOTICE_MAX_WIDTH: f32 = 360.0;
/// Button padding around its label.
const BUTTON_PAD: egui::Vec2 = egui::vec2(10.0, 4.0);

/// What the user chose, applied by the app after the frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UpdateAction {
    /// "Restart to update" on a staged update.
    Restart,
    /// "Later": hide this version until relaunch.
    Later,
    /// "View release": open the release page in the default browser.
    OpenRelease(String),
}

/// The notice element under the pointer, for hover repaint decisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum NoticeTarget {
    /// Inside the notice but not on a button.
    Body,
    Restart,
    Later,
    Open,
}

/// Painted rects (logical points) from the last pass. Empty when the
/// notice is hidden.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct NoticeRects {
    pub(crate) body: Option<egui::Rect>,
    pub(crate) restart: Option<egui::Rect>,
    pub(crate) later: Option<egui::Rect>,
    pub(crate) open: Option<egui::Rect>,
}

impl NoticeRects {
    /// The element at `pos` (logical points), buttons before the body.
    pub(crate) fn target_at(&self, pos: egui::Pos2) -> Option<NoticeTarget> {
        let hit = |rect: Option<egui::Rect>| rect.is_some_and(|r| r.contains(pos));
        if hit(self.restart) {
            Some(NoticeTarget::Restart)
        } else if hit(self.later) {
            Some(NoticeTarget::Later)
        } else if hit(self.open) {
            Some(NoticeTarget::Open)
        } else if hit(self.body) {
            Some(NoticeTarget::Body)
        } else {
            None
        }
    }
}

/// Right inset that clears the scrollbar track: `track_width + pad + 8`.
fn right_inset(scrollbar: &ScrollbarConfig) -> f32 {
    scrollbar.track_width + scrollbar.pad + NOTICE_MARGIN
}

/// The line explaining why the last Restart did not install. Restart stays
/// available so the user can retry.
fn issue_line(issue: &InstallIssue) -> String {
    match issue {
        InstallIssue::Blocked { others: 1 } => {
            "Quit the other Cometty window, then restart to update.".to_owned()
        }
        InstallIssue::Blocked { others } => {
            format!("Quit the other {others} Cometty windows, then restart to update.")
        }
        InstallIssue::Failed(failure) => format!("The update could not be installed: {failure}."),
    }
}

/// The line explaining why the automatic download did not finish.
fn download_line(failure: &Failure) -> String {
    format!("The update could not be downloaded: {failure}.")
}

/// A passive, wrapping line of text. Not selectable: a drag-select would
/// claim the pointer and has nothing useful to copy.
fn text_line(ui: &mut egui::Ui, text: egui::RichText) {
    ui.add(egui::Label::new(text).selectable(false).wrap());
}

/// One notice button. `ui.interact` with `Sense::CLICK` keeps it out of
/// egui's focus chain; returns the click and the painted rect.
fn button(
    ui: &mut egui::Ui,
    target: NoticeTarget,
    label: &str,
    primary: bool,
    theme: Theme,
) -> (bool, egui::Rect) {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let fg = theme.foreground.as_egui_color();
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, egui::Color32::PLACEHOLDER);
    let size = galley.size() + 2.0 * BUTTON_PAD;
    let (_, rect) = ui.allocate_space(size);
    let response = ui.interact(
        rect,
        egui::Id::new(("update-notice", target)),
        egui::Sense::CLICK,
    );
    let hovered = response.hovered();
    let fill = theme.tab_active_bg.as_egui_color();
    let fill = match (primary, hovered) {
        (true, true) => fill,
        (true, false) => fill.gamma_multiply(0.75),
        (false, true) => fill.gamma_multiply(0.55),
        (false, false) => egui::Color32::TRANSPARENT,
    };
    let radius = egui::CornerRadius::same(4);
    let painter = ui.painter();
    painter.rect_filled(rect, radius, fill);
    painter.rect_stroke(
        rect,
        radius,
        egui::Stroke::new(1.0, theme.tab_border.as_egui_color()),
        egui::StrokeKind::Inside,
    );
    let text_color = if primary || hovered {
        fg
    } else {
        fg.gamma_multiply(0.75)
    };
    painter.galley(rect.min + BUTTON_PAD, galley, text_color);
    (response.clicked(), rect)
}

/// Draw `notice` in the bottom-right corner, clear of the scrollbar track.
/// Returns the clicked action, if any, and the painted rects for routing.
pub(crate) fn show_update_notice(
    ctx: &egui::Context,
    notice: &UpdateNotice,
    theme: Theme,
    screen_w: f32,
    scrollbar: &ScrollbarConfig,
) -> (Option<UpdateAction>, NoticeRects) {
    let inset = right_inset(scrollbar);
    let max_width = (screen_w - inset - NOTICE_MARGIN).clamp(1.0, NOTICE_MAX_WIDTH);
    let fg = theme.foreground.as_egui_color();
    let area = egui::Area::new(egui::Id::new("update-notice"))
        .anchor(
            egui::Align2::RIGHT_BOTTOM,
            egui::vec2(-inset, -NOTICE_MARGIN),
        )
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let mut action = None;
            let mut rects = NoticeRects::default();
            egui::Frame::popup(ui.style())
                .fill(theme.background.as_egui_color())
                .stroke(egui::Stroke::new(1.0, theme.tab_border.as_egui_color()))
                .show(ui, |ui| {
                    ui.set_max_width(max_width);
                    text_line(
                        ui,
                        egui::RichText::new(notice.headline()).color(fg).strong(),
                    );
                    if let Some(line) = notice.privacy_line() {
                        text_line(ui, egui::RichText::new(line).color(fg.gamma_multiply(0.7)));
                    }
                    match notice {
                        UpdateNotice::Ready {
                            issue: Some(issue), ..
                        } => {
                            text_line(ui, egui::RichText::new(issue_line(issue)).color(fg));
                        }
                        UpdateNotice::Available { hint, failure, .. } => {
                            if let Some(failure) = failure {
                                text_line(
                                    ui,
                                    egui::RichText::new(download_line(failure)).color(fg),
                                );
                            }
                            if let Some(hint) = hint {
                                let hint = egui::RichText::new(*hint).color(fg.gamma_multiply(0.7));
                                text_line(ui, hint);
                            }
                        }
                        _ => {}
                    }
                    let (target, label) = match notice {
                        UpdateNotice::Ready { .. } => (NoticeTarget::Restart, "Restart to update"),
                        UpdateNotice::Available { .. } => (NoticeTarget::Open, "View release"),
                        // The install job is preparing the swap: no actions.
                        UpdateNotice::Preparing { .. } => return,
                    };
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let (clicked, rect) = button(ui, target, label, true, theme);
                        if let UpdateNotice::Available { url, .. } = notice {
                            rects.open = Some(rect);
                            action = clicked.then(|| UpdateAction::OpenRelease(url.clone()));
                        } else {
                            rects.restart = Some(rect);
                            action = clicked.then_some(UpdateAction::Restart);
                        }
                        let (clicked, rect) =
                            button(ui, NoticeTarget::Later, "Later", false, theme);
                        rects.later = Some(rect);
                        if clicked {
                            action = Some(UpdateAction::Later);
                        }
                    });
                });
            (action, rects)
        });
    let (action, mut rects) = area.inner;
    rects.body = Some(area.response.rect);
    (action, rects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::version::Version;

    const SCREEN: egui::Vec2 = egui::vec2(1024.0, 768.0);
    const V: Version = Version {
        major: 1,
        minor: 2,
        patch: 0,
    };

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
            events,
            ..Default::default()
        }
    }

    fn pass(
        ctx: &egui::Context,
        notice: &UpdateNotice,
        events: Vec<egui::Event>,
    ) -> (Option<UpdateAction>, NoticeRects) {
        let mut out = (None, NoticeRects::default());
        let output = ctx.run_ui(input(events), |ui| {
            out = show_update_notice(
                ui.ctx(),
                notice,
                Theme::default(),
                SCREEN.x,
                &ScrollbarConfig::default(),
            );
        });
        output.drop_without_applying_deltas();
        out
    }

    fn click(pos: egui::Pos2) -> Vec<Vec<egui::Event>> {
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        vec![
            vec![egui::Event::PointerMoved(pos)],
            vec![button(true)],
            vec![button(false)],
        ]
    }

    /// Clicks `target` after two layout passes; returns every action seen.
    fn click_target(notice: &UpdateNotice, target: NoticeTarget) -> Vec<UpdateAction> {
        let ctx = egui::Context::default();
        // The first pass sizes the area, the second places it.
        pass(&ctx, notice, Vec::new());
        let (_, rects) = pass(&ctx, notice, Vec::new());
        let rect = match target {
            NoticeTarget::Restart => rects.restart,
            NoticeTarget::Later => rects.later,
            NoticeTarget::Open => rects.open,
            NoticeTarget::Body => rects.body,
        }
        .expect("target is painted");
        assert_eq!(rects.target_at(rect.center()), Some(target));
        let actions = click(rect.center())
            .into_iter()
            .filter_map(|events| pass(&ctx, notice, events).0)
            .collect();
        assert_eq!(ctx.memory(|m| m.focused()), None, "click takes no focus");
        actions
    }

    #[test]
    fn ready_lays_out_bottom_right_clear_of_the_scrollbar() {
        let ctx = egui::Context::default();
        let notice = UpdateNotice::Ready {
            version: V,
            issue: None,
        };
        pass(&ctx, &notice, Vec::new());
        let (action, rects) = pass(&ctx, &notice, Vec::new());
        assert_eq!(action, None);
        let body = rects.body.expect("body rect");
        let inset = right_inset(&ScrollbarConfig::default());
        assert!(
            (body.right() - (SCREEN.x - inset)).abs() < 1.0,
            "right edge clears the track: {body:?}"
        );
        assert!(
            (body.bottom() - (SCREEN.y - NOTICE_MARGIN)).abs() < 1.0,
            "bottom-anchored: {body:?}"
        );
        assert!(body.width() <= NOTICE_MAX_WIDTH + 40.0, "bounded: {body:?}");
        let (restart, later) = (rects.restart.unwrap(), rects.later.unwrap());
        assert!(body.contains_rect(restart) && body.contains_rect(later));
        assert_eq!(rects.open, None);
        assert_eq!(
            rects.target_at(body.left_top() + egui::vec2(2.0, 2.0)),
            Some(NoticeTarget::Body)
        );
        assert_eq!(rects.target_at(egui::pos2(1.0, 1.0)), None);
    }

    #[test]
    fn clicks_return_actions_without_focus() {
        let ready = UpdateNotice::Ready {
            version: V,
            issue: None,
        };
        assert_eq!(
            click_target(&ready, NoticeTarget::Restart),
            vec![UpdateAction::Restart]
        );
        assert_eq!(
            click_target(&ready, NoticeTarget::Later),
            vec![UpdateAction::Later]
        );
        let url = "https://github.com/jjunac/cometty/releases/tag/v1.2.0".to_string();
        let available = UpdateNotice::Available {
            version: V,
            url: url.clone(),
            hint: Some(crate::update::state::MOVE_HINT),
            failure: None,
        };
        assert_eq!(
            click_target(&available, NoticeTarget::Open),
            vec![UpdateAction::OpenRelease(url)]
        );
        assert!(click_target(&available, NoticeTarget::Body).is_empty());
    }

    fn tab_keys() -> Vec<egui::Event> {
        let key = |pressed, modifiers| egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        };
        vec![
            key(true, egui::Modifiers::NONE),
            key(false, egui::Modifiers::NONE),
            key(true, egui::Modifiers::SHIFT),
            key(false, egui::Modifiers::SHIFT),
        ]
    }

    /// The Restart click and Tab/Shift+Tab in the same pass leave
    /// egui with no focused widget, so the terminal keeps the keyboard.
    #[test]
    fn restart_click_with_tab_keys_leaves_focus_unset() {
        // Control: the same Tab events do focus an ordinary egui button, so
        // a `None` below means the notice has nothing focusable.
        let control = egui::Context::default();
        for _ in 0..3 {
            control
                .run_ui(input(tab_keys()[..1].to_vec()), |ui| {
                    let _ = ui.button("focusable");
                })
                .drop_without_applying_deltas();
        }
        assert!(
            control.memory(|m| m.focused()).is_some(),
            "harness Tab works"
        );

        let ctx = egui::Context::default();
        let notice = UpdateNotice::Ready {
            version: V,
            issue: None,
        };
        pass(&ctx, &notice, Vec::new());
        let (_, rects) = pass(&ctx, &notice, Vec::new());
        let pos = rects.restart.expect("restart rect").center();
        let mut passes = click(pos);
        passes.last_mut().unwrap().extend(tab_keys());
        let mut actions = Vec::new();
        for events in passes {
            actions.extend(pass(&ctx, &notice, events).0);
            assert_eq!(ctx.memory(|m| m.focused()), None);
        }
        assert_eq!(actions, vec![UpdateAction::Restart]);
        // Further Tab presses after the click still find nothing to focus.
        for _ in 0..2 {
            assert_eq!(pass(&ctx, &notice, tab_keys()).0, None);
            assert_eq!(ctx.memory(|m| m.focused()), None);
        }
        assert!(!ctx.egui_wants_keyboard_input());
    }

    /// Every text the notice paints after two layout passes.
    fn painted_texts(notice: &UpdateNotice) -> Vec<String> {
        fn collect(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.galley.text().to_owned()),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, out)),
                _ => {}
            }
        }
        let ctx = egui::Context::default();
        pass(&ctx, notice, Vec::new());
        let output = ctx.run_ui(input(Vec::new()), |ui| {
            show_update_notice(
                ui.ctx(),
                notice,
                Theme::default(),
                SCREEN.x,
                &ScrollbarConfig::default(),
            );
        });
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut texts);
        }
        output.drop_without_applying_deltas();
        texts
    }

    /// A blocked or failed Restart explains itself, and Restart stays
    /// clickable for the retry.
    #[test]
    fn ready_with_an_issue_shows_its_line_and_keeps_restart() {
        assert_eq!(
            issue_line(&InstallIssue::Blocked { others: 1 }),
            "Quit the other Cometty window, then restart to update."
        );
        assert_eq!(
            issue_line(&InstallIssue::Blocked { others: 3 }),
            "Quit the other 3 Cometty windows, then restart to update."
        );
        let clear = UpdateNotice::Ready {
            version: V,
            issue: None,
        };
        let clear_texts = painted_texts(&clear);

        for issue in [
            InstallIssue::Blocked { others: 1 },
            InstallIssue::Blocked { others: 3 },
            InstallIssue::Failed(Failure::Install("Operation not permitted".into())),
        ] {
            let line = issue_line(&issue);
            assert!(!clear_texts.contains(&line));
            let notice = UpdateNotice::Ready {
                version: V,
                issue: Some(issue),
            };
            let texts = painted_texts(&notice);
            assert!(texts.contains(&line), "{texts:?}");
            assert!(texts.iter().any(|t| t == "Restart to update"), "{texts:?}");
            assert_eq!(
                click_target(&notice, NoticeTarget::Restart),
                vec![UpdateAction::Restart]
            );
        }
    }

    #[test]
    fn a_failed_download_is_explained_beside_view_release() {
        let failure = Failure::Network("proxy refused".into());
        let notice = UpdateNotice::Available {
            version: V,
            url: "https://github.com/jjunac/cometty/releases/tag/v1.2.0".into(),
            hint: None,
            failure: Some(failure.clone()),
        };
        let texts = painted_texts(&notice);
        assert!(texts.contains(&download_line(&failure)), "{texts:?}");
        assert!(texts.iter().any(|t| t == "View release"), "{texts:?}");
    }

    #[test]
    fn preparing_has_no_buttons() {
        let ctx = egui::Context::default();
        let notice = UpdateNotice::Preparing { version: V };
        pass(&ctx, &notice, Vec::new());
        let (_, rects) = pass(&ctx, &notice, Vec::new());
        assert!(rects.body.is_some());
        assert_eq!((rects.restart, rects.later, rects.open), (None, None, None));
    }
}
