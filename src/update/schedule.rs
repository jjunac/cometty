//! Check scheduling: launch delay, daily interval, and sleep-aware due checks
//! (`docs/update.md#state-machine`).
//!
//! The [`Scheduler`] is pure: every operation takes the current `Instant` and
//! `SystemTime` from its caller, so it reads no clock and is testable offline.
//! It contributes at most one event-loop deadline and adds no timer
//! of its own; worker results arrive as events.
//!
//! Each deadline is stored as a pair: the monotonic `Instant` that
//! `about_to_wait` folds into its wake time, plus the wall-clock time it stood
//! for. On macOS `Instant` does not advance during system sleep, so after a
//! long sleep the monotonic half may still lie in the future while the wall
//! clock has passed it. [`Scheduler::due`] fires when either clock reaches its
//! half, so the first loop turn after waking runs an overdue check.
use std::time::{Duration, Instant, SystemTime};

/// Delay from launch to the first check, so it does not compete with the first
/// frame and shell startup.
pub(crate) const STARTUP_DELAY: Duration = Duration::from_secs(5);
/// Delay from enabling the update setting to the next check.
pub(crate) const ENABLE_DELAY: Duration = Duration::from_secs(1);
/// Interval from a check's completion, whatever its outcome, to the next one
///.
pub(crate) const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Decides when the next update check is due.
#[derive(Debug, Default)]
pub(crate) struct Scheduler {
    enabled: bool,
    /// Monotonic deadline plus the wall-clock time it stood for.
    next: Option<(Instant, SystemTime)>,
    in_flight: bool,
    /// Staged or Installing: no checks until relaunch.
    suspended: bool,
}

/// The deadline pair `delay` after (`now`, `wall`).
///
/// `None` only if either clock would overflow, which no real clock reaches;
/// failing towards "no check" keeps an absurd clock from causing a check loop.
fn arm(now: Instant, wall: SystemTime, delay: Duration) -> Option<(Instant, SystemTime)> {
    Some((now.checked_add(delay)?, wall.checked_add(delay)?))
}

impl Scheduler {
    /// Initial setup: the first check is `STARTUP_DELAY` after launch when
    /// enabled, and nothing is scheduled when disabled.
    pub(crate) fn launch(&mut self, enabled: bool, now: Instant, wall: SystemTime) {
        self.enabled = enabled;
        self.next = if enabled {
            arm(now, wall, STARTUP_DELAY)
        } else {
            None
        };
    }

    /// Enabling schedules a check `ENABLE_DELAY` out; disabling
    /// clears the deadline. An in-flight job is left to the
    /// caller, which abandons it through [`Self::job_abandoned`] (or lets an
    /// install job finish).
    pub(crate) fn set_enabled(&mut self, enabled: bool, now: Instant, wall: SystemTime) {
        self.enabled = enabled;
        self.next = if enabled {
            arm(now, wall, ENABLE_DELAY)
        } else {
            None
        };
    }

    /// A job was spawned. Its deadline is consumed, so nothing is scheduled
    /// until the job finishes or is abandoned.
    pub(crate) fn job_started(&mut self) {
        self.in_flight = true;
        self.next = None;
    }

    /// The in-flight job was dropped without a result (the disable path).
    ///
    /// No deadline is re-armed: the caller follows this with
    /// `set_enabled(false)`, and a later enable schedules its own check.
    pub(crate) fn job_abandoned(&mut self) {
        self.in_flight = false;
    }

    /// The in-flight job ended with any outcome, failures included: the next
    /// check is one `CHECK_INTERVAL` after completion.
    pub(crate) fn job_finished(&mut self, now: Instant, wall: SystemTime) {
        self.in_flight = false;
        self.next = arm(now, wall, CHECK_INTERVAL);
    }

    /// Suspends scheduled checks while Staged or Installing. The
    /// stored deadline survives, so a failed install resumes the daily cadence.
    pub(crate) fn set_suspended(&mut self, suspended: bool) {
        self.suspended = suspended;
    }

    /// Whether a check may be scheduled at all right now.
    fn active(&self) -> bool {
        self.enabled && !self.in_flight && !self.suspended
    }

    /// The single updater deadline; `None` when disabled, in flight, or suspended.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.next.filter(|_| self.active()).map(|(at, _)| at)
    }

    /// True when enabled, idle, not suspended, and either clock reached `next`.
    pub(crate) fn due(&self, now: Instant, wall: SystemTime) -> bool {
        self.active()
            && self
                .next
                .is_some_and(|(at, wall_at)| now >= at || wall >= wall_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed, realistic wall-clock origin (2025-01-01T00:00:00Z).
    fn wall0() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_735_689_600)
    }

    fn launched(enabled: bool) -> (Scheduler, Instant, SystemTime) {
        let (now, wall) = (Instant::now(), wall0());
        let mut s = Scheduler::default();
        s.launch(enabled, now, wall);
        (s, now, wall)
    }

    #[test]
    fn launch_while_disabled_has_no_deadline() {
        let (s, now, wall) = launched(false);
        assert_eq!(s.deadline(), None);
        let later = Duration::from_secs(365 * 24 * 60 * 60);
        assert!(!s.due(now + later, wall + later));
    }

    #[test]
    fn launch_while_enabled_is_due_after_startup_delay() {
        let (s, now, wall) = launched(true);
        assert_eq!(s.deadline(), Some(now + STARTUP_DELAY));
        let just_before = STARTUP_DELAY - Duration::from_millis(1);
        assert!(!s.due(now + just_before, wall + just_before));
        assert!(s.due(now + STARTUP_DELAY, wall + STARTUP_DELAY));
    }

    #[test]
    fn job_finished_schedules_one_interval_after_completion_for_any_outcome() {
        // The scheduler has no outcome input: a failed check finishes the same
        // way as a successful one.
        let (mut s, now, wall) = launched(true);
        s.job_started();
        let done = now + Duration::from_secs(42);
        let done_wall = wall + Duration::from_secs(42);
        s.job_finished(done, done_wall);
        assert_eq!(s.deadline(), Some(done + CHECK_INTERVAL));
        let almost = CHECK_INTERVAL - Duration::from_secs(1);
        assert!(!s.due(done + almost, done_wall + almost));
        assert!(s.due(done + CHECK_INTERVAL, done_wall + CHECK_INTERVAL));
    }

    #[test]
    fn in_flight_has_no_deadline_and_is_never_due() {
        let (mut s, now, wall) = launched(true);
        s.job_started();
        assert_eq!(s.deadline(), None);
        assert!(!s.due(now + CHECK_INTERVAL * 2, wall + CHECK_INTERVAL * 2));
    }

    #[test]
    fn suspended_has_no_deadline_until_resumed() {
        let (mut s, now, wall) = launched(true);
        s.set_suspended(true);
        assert_eq!(s.deadline(), None);
        assert!(!s.due(now + STARTUP_DELAY, wall + STARTUP_DELAY));
        s.set_suspended(false);
        assert_eq!(s.deadline(), Some(now + STARTUP_DELAY));
    }

    #[test]
    fn wall_clock_overdue_after_sleep_is_due_before_the_instant() {
        let (mut s, now, wall) = launched(true);
        s.job_started();
        s.job_finished(now, wall);
        // Two days of sleep: the wall clock moved, the monotonic clock barely did.
        let awake = now + Duration::from_secs(10);
        let awake_wall = wall + CHECK_INTERVAL * 2;
        assert!(awake < s.deadline().unwrap());
        assert!(s.due(awake, awake_wall));
    }

    #[test]
    fn disable_clears_the_deadline_and_enable_rearms_it() {
        let (mut s, now, wall) = launched(true);
        s.set_enabled(false, now, wall);
        assert_eq!(s.deadline(), None);
        assert!(!s.due(now + CHECK_INTERVAL, wall + CHECK_INTERVAL));

        let t = now + Duration::from_secs(60);
        let t_wall = wall + Duration::from_secs(60);
        s.set_enabled(true, t, t_wall);
        assert_eq!(s.deadline(), Some(t + ENABLE_DELAY));
        assert!(s.due(t + ENABLE_DELAY, t_wall + ENABLE_DELAY));
    }

    #[test]
    fn abandoning_on_disable_leaves_nothing_scheduled() {
        let (mut s, now, wall) = launched(true);
        s.job_started();
        s.job_abandoned();
        s.set_enabled(false, now, wall);
        assert_eq!(s.deadline(), None);
        assert!(!s.due(now + CHECK_INTERVAL, wall + CHECK_INTERVAL));
    }
}
