//! Pure visual wake decisions: steady/hidden/history cursors do not need a
//! timer. egui, search and PTY continuations have independent deadlines.

use std::time::{Duration, Instant};

pub(crate) fn cursor_blinks(suspended: bool, enabled: bool, blinking: bool, offset: usize) -> bool {
    !suspended && enabled && blinking && offset == 0
}

pub(crate) fn earlier(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

pub(crate) fn blink_step(
    now: Instant,
    last: &mut Instant,
    visible: &mut bool,
    enabled: bool,
    period: Duration,
) -> (bool, Option<Instant>) {
    if !enabled {
        *last = now;
        *visible = true;
        return (false, None);
    }
    let repaint = now.saturating_duration_since(*last) >= period;
    if repaint {
        *last = now;
        *visible = !*visible;
    }
    (repaint, last.checked_add(period))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_visible_blinking_live_cursor_needs_a_timer() {
        assert!(cursor_blinks(false, true, true, 0));
        for args in [
            (true, true, true, 0),
            (false, false, true, 0),
            (false, true, false, 0),
            (false, true, true, 10),
        ] {
            assert!(!cursor_blinks(args.0, args.1, args.2, args.3));
        }
    }

    #[test]
    fn blink_sleep_resume_and_independent_deadlines() {
        let now = Instant::now();
        let period = Duration::from_millis(530);
        let mut last = now;
        let mut visible = true;
        assert_eq!(
            blink_step(now, &mut last, &mut visible, true, period),
            (false, Some(now + period))
        );
        assert!(blink_step(now + period, &mut last, &mut visible, true, period).0);
        assert!(!visible);
        let resumed = now + Duration::from_secs(60);
        assert_eq!(
            blink_step(resumed, &mut last, &mut visible, false, period),
            (false, None)
        );
        assert!(visible);
        assert_eq!(
            blink_step(resumed, &mut last, &mut visible, true, period),
            (false, Some(resumed + period))
        );
        assert_eq!(earlier(None, Some(now)), Some(now));
        assert_eq!(earlier(Some(now + period), Some(now)), Some(now));
        assert_eq!(earlier(None, None), None);
    }
}
