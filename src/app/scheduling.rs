//! Pure visual wake decisions: steady/hidden/history cursors do not need a
//! timer. egui, search and PTY continuations have independent deadlines.

use std::time::{Duration, Instant};

pub(crate) const MAX_DRAIN_BYTES: usize = 256 * 1024;
pub(crate) const MAX_DRAIN_TIME: Duration = Duration::from_millis(4);
pub(crate) const SYNC_TIMEOUT: Duration = Duration::from_millis(150);

/// Output frames coalesce independently of the fair parser allowance. Leave at
/// least one frame's measured UI cost for parsing/input before another output
/// frame; ordinary output is capped at 60 Hz. Explicit UI redraws bypass this.
#[derive(Default)]
pub(crate) struct OutputPresentation {
    not_before: Option<Instant>,
    pending: bool,
    input: bool,
}

impl OutputPresentation {
    pub(crate) fn input(&mut self) {
        self.input = true;
    }

    pub(crate) fn dirty(&mut self, now: Instant) -> bool {
        self.pending = true;
        self.take_due(now)
    }

    pub(crate) fn deadline(&self, now: Instant) -> Option<Instant> {
        self.pending.then_some(if self.input {
            now
        } else {
            self.not_before.unwrap_or(now)
        })
    }

    pub(crate) fn take_due(&mut self, now: Instant) -> bool {
        if self.deadline(now).is_some_and(|at| at <= now) {
            self.pending = false;
            self.input = false;
            true
        } else {
            false
        }
    }

    pub(crate) fn begin_frame(&mut self) {
        // An explicit UI frame also includes the latest output. Do not leave
        // a trailing timer after it, or consume urgency before the echo arrives.
        self.pending = false;
    }

    pub(crate) fn finish_frame(&mut self, start: Instant, end: Instant) {
        let cost = end.saturating_duration_since(start);
        let interval = Duration::from_micros(16_667);
        let gap = interval
            .saturating_sub(cost)
            .max(cost.min(Duration::from_millis(50)));
        self.not_before = end.checked_add(gap);
    }
}

pub(crate) fn drain_budget_exceeded(bytes: usize, elapsed: Duration) -> bool {
    bytes >= MAX_DRAIN_BYTES || elapsed >= MAX_DRAIN_TIME
}

/// Weighted round robin: one chunk for the active tab alternates with one
/// chunk at a persistent round-robin cursor. Even if a single callback uses
/// the entire allowance, the next turn resumes the order, not tab zero (or
/// always the active tab). All drain call sites share this turn's allowance.
#[derive(Default)]
pub(crate) struct DrainSchedule {
    next: usize,
    active_next: bool,
    bytes: usize,
    spent: Duration,
}

impl DrainSchedule {
    pub(crate) fn begin_turn(&mut self) {
        self.bytes = 0;
        self.spent = Duration::ZERO;
    }

    pub(crate) fn exhausted(&self, elapsed: Duration) -> bool {
        drain_budget_exceeded(self.bytes, self.spent.saturating_add(elapsed))
    }

    pub(crate) fn next_tab(&mut self, len: usize, active: usize) -> Option<usize> {
        if len == 0 {
            return None;
        }
        self.next %= len;
        if self.active_next && active < len && active != self.next {
            self.active_next = false;
            return Some(active);
        }
        let next = self.next;
        self.next = (next + 1) % len;
        self.active_next = true;
        Some(next)
    }

    pub(crate) fn add_bytes(&mut self, bytes: usize) {
        self.bytes = self.bytes.saturating_add(bytes);
    }

    pub(crate) fn finish_call(&mut self, elapsed: Duration) {
        self.spent = self.spent.saturating_add(elapsed);
    }

    pub(crate) fn removed(&mut self, removed: &[usize], len: usize) {
        self.next = super::tab::active_after_removals(self.next, removed, len);
    }
}

/// Synchronized output suppresses presentation, never parsing. A missing
/// end marker releases presentation after 150ms; subsequent output remains
/// visible until the application ends this synchronization interval.
#[derive(Default)]
pub(crate) struct SyncPresentation {
    started: Option<Instant>,
    released: bool,
}

impl SyncPresentation {
    pub(crate) fn update(&mut self, in_sync: bool, now: Instant) {
        if !in_sync {
            self.started = None;
            self.released = false;
        } else {
            self.started.get_or_insert(now);
        }
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        if self.released {
            None
        } else {
            self.started.and_then(|t| t.checked_add(SYNC_TIMEOUT))
        }
    }

    pub(crate) fn blocked(&self, now: Instant) -> bool {
        self.deadline().is_some_and(|at| now < at)
    }

    pub(crate) fn release_due(&mut self, now: Instant) -> bool {
        if self.deadline().is_some_and(|at| at <= now) {
            self.released = true;
            true
        } else {
            false
        }
    }
}

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
    fn output_frames_coalesce_without_a_timer_at_idle_or_an_extra_drain_allowance() {
        let now = Instant::now();
        let mut pacing = OutputPresentation::default();
        assert_eq!(pacing.deadline(now), None);
        assert!(pacing.dirty(now));
        pacing.begin_frame();
        pacing.finish_frame(now, now + Duration::from_millis(5));
        assert_eq!(pacing.deadline(now), None);
        let mut schedule = DrainSchedule::default();
        for turn in 1..=3 {
            let at = now + Duration::from_millis(turn * 3 + 5);
            schedule.begin_turn();
            schedule.add_bytes(MAX_DRAIN_BYTES);
            assert!(schedule.exhausted(Duration::ZERO));
            assert!(!pacing.dirty(at));
        }
        let deadline = pacing.deadline(now).unwrap();
        assert_eq!(deadline, now + Duration::from_micros(16_667));
        assert!(pacing.take_due(deadline)); // final output cannot be stranded
        assert_eq!(pacing.deadline(deadline), None);
        assert!(!pacing.take_due(deadline));
    }

    #[test]
    fn expensive_frames_leave_parse_time_but_never_hold_input_or_explicit_ui_redraws() {
        let now = Instant::now();
        let mut pacing = OutputPresentation::default();
        pacing.finish_frame(now, now + Duration::from_millis(30));
        assert!(!pacing.dirty(now + Duration::from_millis(31)));
        assert_eq!(pacing.deadline(now), Some(now + Duration::from_millis(60)));
        pacing.input();
        assert!(pacing.take_due(now + Duration::from_millis(32)));
        assert_eq!(pacing.deadline(now), None);
        assert!(!pacing.dirty(now + Duration::from_millis(33)));
        // A mouse/resize/chrome frame bypasses the deadline and consumes output.
        pacing.begin_frame();
        assert_eq!(pacing.deadline(now), None);
        // An early input-triggered repaint must not consume urgency before echo.
        pacing.input();
        pacing.begin_frame();
        pacing.finish_frame(now, now + Duration::from_millis(40));
        assert!(pacing.dirty(now + Duration::from_millis(41)));
    }

    #[test]
    fn shared_turn_budget_is_not_reset_by_another_call_site() {
        let mut schedule = DrainSchedule::default();
        schedule.begin_turn();
        schedule.finish_call(MAX_DRAIN_TIME / 2);
        assert!(!schedule.exhausted(Duration::ZERO));
        assert!(schedule.exhausted(MAX_DRAIN_TIME / 2));
        schedule.finish_call(MAX_DRAIN_TIME / 2);
        assert!(schedule.exhausted(Duration::ZERO));
        schedule.begin_turn();
        assert!(!schedule.exhausted(Duration::ZERO));
        schedule.add_bytes(MAX_DRAIN_BYTES);
        assert!(schedule.exhausted(Duration::ZERO));
    }

    #[test]
    fn every_tab_progresses_even_when_one_chunk_uses_a_whole_turn() {
        let mut schedule = DrainSchedule::default();
        let mut last = [0; 7];
        let mut counts = [0; 7];
        let active = 5;
        for turn in 0..1000 {
            schedule.begin_turn();
            let tab = schedule.next_tab(last.len(), active).unwrap();
            // A flood in tab zero must not strand the active tab or an
            // exit behind output in the final tab. No wall-clock assertions.
            assert!(turn - last[tab] <= 2 * last.len());
            last[tab] = turn;
            counts[tab] += 1;
            schedule.finish_call(MAX_DRAIN_TIME * 10);
            assert!(schedule.exhausted(Duration::ZERO));
        }
        assert!(counts.iter().all(|count| *count > 60));
        assert!(counts[active] > counts[0] * 3);
        for last in last {
            assert!(999 - last <= 14);
        }
    }

    #[test]
    fn round_robin_cursor_survives_switches_and_removal() {
        let mut schedule = DrainSchedule::default();
        assert_eq!(schedule.next_tab(4, 3), Some(0));
        assert_eq!(schedule.next_tab(4, 3), Some(3));
        // Old tab 1 (the next background service) becomes slot 0.
        schedule.removed(&[0], 3);
        assert_eq!(schedule.next_tab(3, 1), Some(0));
        assert_eq!(schedule.next_tab(3, 2), Some(2));
        assert_eq!(schedule.next_tab(3, 2), Some(1));
        schedule.removed(&[0, 1, 2], 0);
        assert_eq!(schedule.next_tab(0, 0), None);
        assert_eq!(schedule.next_tab(1, 0), Some(0));
    }

    #[test]
    fn sync_suppresses_presentation_not_progress_and_times_out_once() {
        let now = Instant::now();
        let mut sync = SyncPresentation::default();
        sync.update(true, now);
        assert!(sync.blocked(now));
        assert_eq!(sync.deadline(), Some(now + SYNC_TIMEOUT));
        // Continuing/nested begin markers cannot move the recovery deadline.
        sync.update(true, now + SYNC_TIMEOUT / 2);
        assert_eq!(sync.deadline(), Some(now + SYNC_TIMEOUT));
        assert!(!sync.release_due(now));
        assert!(sync.release_due(now + SYNC_TIMEOUT));
        assert!(!sync.blocked(now + SYNC_TIMEOUT));
        assert_eq!(sync.deadline(), None);
        assert!(!sync.release_due(now + SYNC_TIMEOUT * 2));
        sync.update(true, now + SYNC_TIMEOUT * 2);
        assert_eq!(sync.deadline(), None);
        sync.update(false, now + SYNC_TIMEOUT * 2);
        sync.update(true, now + SYNC_TIMEOUT * 3);
        assert!(sync.blocked(now + SYNC_TIMEOUT * 3));
        sync.update(false, now + SYNC_TIMEOUT * 3);
        assert!(!sync.blocked(now + SYNC_TIMEOUT * 3));
    }

    #[test]
    fn sync_markers_split_across_chunks_do_not_strand_the_final_frame() {
        let mut terminal = crate::term::Terminal::new(20, 2, crate::theme::Theme::default());
        let mut sync = SyncPresentation::default();
        let now = Instant::now();
        for chunk in [b"\x1b[?20".as_slice(), b"26hhello", b"\x1b[?202", b"6l"] {
            terminal.feed(chunk);
            sync.update(terminal.in_sync(), now);
        }
        assert_eq!(sync.deadline(), None);
        assert!(!sync.blocked(now));
        assert_eq!(terminal.grid().cell(0, 0).unwrap().ch, 'h');
    }

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
