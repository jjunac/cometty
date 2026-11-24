//! App-owned updater model: [`UpdateController`] and its `impl App` glue.
//!
//! The controller holds the launch mode, the update state, the check
//! [`Scheduler`], and the channel that update jobs report through. It does no
//! I/O of its own: every network request, file write, and probe runs on a
//! detached worker (`crate::update::worker`), and results arrive as
//! [`WorkerMsg`]s tagged with a job id. The pure methods here
//! ([`UpdateController::apply_event`], [`UpdateController::accept`],
//! [`UpdateController::deadline`]) are testable without a window or event
//! loop. See `docs/update.md#state-machine`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Instant, SystemTime};

use winit::event_loop::ActiveEventLoop;

use super::{App, UserEvent};
use crate::app::settings::UpdateStatusView;
use crate::links::OpenError;
use crate::renderer::update_ui::UpdateAction;
use crate::update::fsops::{self, JOB_LOCK_PATIENCE};
use crate::update::mode::{Mode, NotifyReason, resolve_mode};
use crate::update::schedule::Scheduler;
use crate::update::state::{self, Event, Failure, InstallIssue, UpdateNotice, UpdateState};
use crate::update::version::{Version, parse_plain};
use crate::update::worker::{
    self, CheckParams, HousekeepingParams, InstallParams, JobCtx, WorkerMsg,
};
use crate::update::{RUNNING_VERSION, TARGET, staging};

/// The wake closure handed to every job (see [`App::update_wake`]).
pub(crate) type Wake = Arc<dyn Fn() + Send + Sync>;

/// Which pipeline a job runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JobKind {
    /// A scheduled check (and, in SelfUpdate mode, download and staging).
    Check,
    /// Copy and swap after "Restart to update".
    Install,
}

/// The current job: its id (messages carrying any other id are stale), the
/// cancel flag it polls between requests and body chunks, and whether its
/// terminal message has arrived.
#[derive(Debug)]
pub(crate) struct JobHandle {
    pub id: u64,
    pub cancel: Arc<AtomicBool>,
    pub kind: JobKind,
    /// `Installed` arrived: the install job's terminal message, so its
    /// trailing `Finished` is not a missing result.
    pub terminal_seen: bool,
}

/// What [`UpdateController::deliver`] asks of the app after one message.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// Nothing for the app beyond refreshing the settings status; `true`
    /// when the notice content changed, the only case that needs a repaint.
    Applied { notice_changed: bool },
    /// A message from an abandoned or superseded job, dropped unapplied.
    Stale,
    /// The update is installed: quit through the ordinary exit path, which
    /// relaunches the bundle last.
    Quit,
}

/// Launch-time inputs of [`UpdateController::new`], gathered by
/// [`UpdateController::from_env`] in the app and spelled out in tests.
pub(crate) struct Launch<'a> {
    /// `CARGO_PKG_VERSION` in the app.
    pub running: &'a str,
    /// `[update] enabled`.
    pub enabled: bool,
    /// `platform::launched_from_app_bundle()`.
    pub bundled: bool,
    /// `current_exe()`, or an empty path when it is unavailable.
    pub exe: &'a Path,
    /// `staging::default_dir()`.
    pub staging_dir: Option<PathBuf>,
}

/// The updater's model state, owned by `App` (`App.update`).
pub(crate) struct UpdateController {
    /// `None` if `CARGO_PKG_VERSION` is not X.Y.Z: the updater stays inert
    /// (logged once at construction).
    running: Option<Version>,
    mode: Mode,
    state: UpdateState,
    scheduler: Scheduler,
    /// "Later" for this version, until relaunch.
    dismissed: Option<Version>,
    job: Option<JobHandle>,
    next_job_id: u64,
    staging_dir: Option<PathBuf>,
    /// The bundle to reopen from `exiting()` once an install succeeded.
    relaunch: Option<PathBuf>,
    tx: mpsc::Sender<WorkerMsg>,
    rx: mpsc::Receiver<WorkerMsg>,
}

impl UpdateController {
    /// Builds the controller from this process's environment: the setting,
    /// the bundle-launch check shared with the login-shell default, the
    /// executable path, and the staging directory. All are pure path and
    /// environment reads; the writability probe is left to the workers, so
    /// the UI thread touches no file system.
    pub(crate) fn from_env(enabled: bool, now: Instant, wall: SystemTime) -> Self {
        let exe = std::env::current_exe().unwrap_or_default();
        Self::new(
            Launch {
                running: RUNNING_VERSION,
                enabled,
                bundled: crate::platform::launched_from_app_bundle(),
                exe: &exe,
                staging_dir: staging::default_dir(),
            },
            now,
            wall,
        )
    }

    /// Resolves the launch mode and arms the first check `STARTUP_DELAY`
    /// out when enabled. Spawns nothing.
    pub(crate) fn new(launch: Launch<'_>, now: Instant, wall: SystemTime) -> Self {
        let running = parse_plain(launch.running);
        if running.is_none() {
            log::warn!(
                target: TARGET,
                "running version {:?} is not X.Y.Z; update checks are disabled",
                launch.running
            );
        }
        let mode = resolve_mode(launch.bundled, launch.exe, launch.staging_dir.as_deref());
        let mut scheduler = Scheduler::default();
        // An inert updater never schedules, whatever the setting says.
        scheduler.launch(launch.enabled && running.is_some(), now, wall);
        let (tx, rx) = mpsc::channel();
        Self {
            running,
            mode,
            state: UpdateState::Idle,
            scheduler,
            dismissed: None,
            job: None,
            next_job_id: 0,
            staging_dir: launch.staging_dir,
            relaunch: None,
            tx,
            rx,
        }
    }

    /// The single updater deadline for `about_to_wait`; `None` when
    /// disabled, inert, in flight, or suspended.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.scheduler.deadline()
    }

    /// Whether `about_to_wait` should start a check now: enabled, not
    /// inert, no check in flight, not suspended, and either clock reached
    /// the deadline. Reads no clock and allocates nothing.
    pub(crate) fn check_due(&self, now: Instant, wall: SystemTime) -> bool {
        self.running.is_some() && self.scheduler.due(now, wall)
    }

    /// The launch housekeeping job's inputs, or `None` when this launch
    /// must leave the staging directory untouched: only a `SelfUpdate`
    /// launch with a staging directory and a plain version qualifies.
    pub(crate) fn housekeeping_params(&self) -> Option<HousekeepingParams> {
        let Mode::SelfUpdate { bundle } = &self.mode else {
            return None;
        };
        Some(HousekeepingParams {
            running: self.running?,
            installed: bundle.clone(),
            staging_dir: self.staging_dir.clone()?,
            lock_patience: JOB_LOCK_PATIENCE,
        })
    }

    /// Starts a check job through `spawn` (the caller checked
    /// [`Self::check_due`]): applies `CheckStarted`, consumes the deadline,
    /// and hands the job the launch mode and staging directory. A check runs
    /// in every mode; notify-only jobs stop after the feed. A spawn error
    /// becomes `Failed(Spawn)` and re-arms the daily schedule like any other
    /// failed check. Returns whether the notice changed.
    pub(crate) fn begin_check(
        &mut self,
        now: Instant,
        wall: SystemTime,
        wake: Wake,
        spawn: impl FnOnce(JobCtx, CheckParams) -> io::Result<()>,
    ) -> bool {
        let Some(running) = self.running else {
            return false;
        };
        let ctx = self.start_job(JobKind::Check, wake);
        let mut changed = self.apply_event(Event::CheckStarted);
        self.scheduler.job_started();
        let params = CheckParams {
            running,
            mode: self.mode.clone(),
            staging_dir: self.staging_dir.clone(),
            lock_patience: JOB_LOCK_PATIENCE,
        };
        if let Err(e) = spawn(ctx, params) {
            log::warn!(target: TARGET, "could not start the update check: {e}");
            self.job = None;
            changed |= self.apply_event(Event::Failed(Failure::Spawn(e.to_string())));
            self.scheduler.job_finished(now, wall);
        }
        changed
    }

    /// The update setting changed to `enabled`. Returns whether the notice
    /// changed.
    ///
    /// Enabling arms a check `ENABLE_DELAY` (1 s) out; an inert updater
    /// stays unscheduled. Disabling clears the deadline. While `Installing`
    /// that is all: the install job keeps its handle so its `Installed` is
    /// not discarded as stale. Otherwise a check job is cancelled, dropped,
    /// and abandoned, and the state returns to `Idle` through `Disabled`.
    pub(crate) fn set_enabled(&mut self, enabled: bool, now: Instant, wall: SystemTime) -> bool {
        if enabled {
            if self.running.is_some() {
                self.scheduler.set_enabled(true, now, wall);
            }
            return false;
        }
        self.scheduler.set_enabled(false, now, wall);
        if matches!(self.state, UpdateState::Installing(_)) {
            return false;
        }
        if let Some(job) = self.job.take() {
            job.cancel.store(true, Ordering::Release);
            self.scheduler.job_abandoned();
            log::debug!(target: TARGET, "abandoned update job {} ({:?})", job.id, job.kind);
        }
        self.apply_event(Event::Disabled)
    }

    /// "Restart to update". Valid only for a `Staged` update in
    /// `SelfUpdate` mode; anything else is logged at `debug` and ignored.
    /// Starts the install job through `spawn` and applies
    /// `InstallRequested`, which keeps checks suspended. The job ends in
    /// `Installed` (the app quits) or returns to `Staged` with the reason. A
    /// spawn error is such a failure. Returns whether the notice changed.
    pub(crate) fn request_install(
        &mut self,
        now: Instant,
        wall: SystemTime,
        wake: Wake,
        spawn: impl FnOnce(JobCtx, InstallParams) -> io::Result<()>,
    ) -> bool {
        let UpdateState::Staged { version, .. } = self.state else {
            log::debug!(target: TARGET, "ignored Restart in {}", self.state.name());
            return false;
        };
        let (Mode::SelfUpdate { bundle }, Some(staging_dir)) = (&self.mode, &self.staging_dir)
        else {
            log::debug!(target: TARGET, "ignored Restart in {:?} mode", self.mode);
            return false;
        };
        let params = InstallParams {
            release: version,
            installed: bundle.clone(),
            staging_dir: staging_dir.clone(),
            lock_patience: JOB_LOCK_PATIENCE,
        };
        let ctx = self.start_job(JobKind::Install, wake);
        let mut changed = self.apply_event(Event::InstallRequested);
        if let Err(e) = spawn(ctx, params) {
            log::warn!(target: TARGET, "could not start the update install: {e}");
            self.job = None;
            changed |= self.apply_event(Event::Failed(Failure::Spawn(e.to_string())));
            self.scheduler.job_finished(now, wall);
        }
        changed
    }

    /// "Later": hides the shown version until relaunch. Only a `Ready` or
    /// `Available` notice can be dismissed; a later, different version shows
    /// again. Returns whether the notice changed.
    pub(crate) fn dismiss(&mut self) -> bool {
        let version = match self.notice() {
            Some(UpdateNotice::Ready { version, .. } | UpdateNotice::Available { version, .. }) => {
                version
            }
            other => {
                log::debug!(target: TARGET, "ignored Later with notice {other:?}");
                return false;
            }
        };
        log::debug!(target: TARGET, "dismissed the notice for Cometty {version}");
        self.dismissed = Some(version);
        true
    }

    /// What the notice shows now, if anything.
    pub(crate) fn notice(&self) -> Option<UpdateNotice> {
        state::notice(&self.state, &self.mode, self.dismissed)
    }

    /// The plain snapshot the Updates settings section renders.
    pub(crate) fn status_view(&self) -> UpdateStatusView {
        UpdateStatusView {
            version: RUNNING_VERSION.to_string(),
            mode: self.mode_text(),
            last_outcome: outcome_text(&self.state),
        }
    }

    fn mode_text(&self) -> String {
        if self.running.is_none() {
            return format!(
                "Update checks are off: this build's version {RUNNING_VERSION:?} is not X.Y.Z."
            );
        }
        match &self.mode {
            Mode::SelfUpdate { .. } => {
                "Updates download in the background and install when you restart.".into()
            }
            Mode::NotifyOnly(reason) => match reason {
                NotifyReason::NotBundled => {
                    "Not running from Cometty.app: new releases are announced only.".into()
                }
                NotifyReason::Translocated | NotifyReason::NotWritable => {
                    format!("New releases are announced only. {}", state::MOVE_HINT)
                }
                NotifyReason::NoStagingDir => {
                    "No home directory for downloads: new releases are announced only.".into()
                }
            },
        }
    }

    /// Applies `event` through the transition table and keeps the scheduler
    /// suspended exactly while the state is Staged or Installing. An invalid
    /// pair leaves the state unchanged and is logged at `debug`.
    ///
    /// Returns whether the notice content changed, which is the only case in
    /// which the app requests a repaint.
    pub(crate) fn apply_event(&mut self, event: Event) -> bool {
        let before = self.notice();
        match state::transition(&self.state, &event) {
            Ok(next) => self.state = next,
            Err(invalid) => log::debug!(
                target: TARGET,
                "ignored {} in {}",
                invalid.event,
                invalid.state
            ),
        }
        self.scheduler.set_suspended(self.state.suspends_checks());
        self.notice() != before
    }

    /// Passes `msg` through only when it comes from the current job; a
    /// message from an abandoned or superseded job is dropped.
    pub(crate) fn accept(&self, msg: WorkerMsg) -> Option<WorkerMsg> {
        let job = msg_job(&msg);
        if self.job.as_ref().is_some_and(|current| current.id == job) {
            Some(msg)
        } else {
            log::debug!(target: TARGET, "dropped a message from stale update job {job}");
            None
        }
    }

    /// Allocates the next job id and cancel flag, makes that job current,
    /// and returns the context to hand to a `worker::spawn_*` function.
    /// `wake` wakes the event loop after each message
    /// ([`App::update_wake`] in the app; a no-op or counter in tests).
    pub(crate) fn start_job(&mut self, kind: JobKind, wake: Wake) -> JobCtx {
        let id = self.next_job_id;
        self.next_job_id += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        self.job = Some(JobHandle {
            id,
            cancel: Arc::clone(&cancel),
            kind,
            terminal_seen: false,
        });
        JobCtx::new(id, cancel, self.tx.clone(), wake)
    }

    /// Records the mode a check job resolved after probing writability (the
    /// `NotWritable` downgrade). Returns whether the notice changed, since
    /// notify-only and self-update show different notices.
    pub(crate) fn set_mode(&mut self, mode: Mode) -> bool {
        let before = self.notice();
        if mode != self.mode {
            log::debug!(target: TARGET, "updater mode {:?} -> {mode:?}", self.mode);
            self.mode = mode;
        }
        self.notice() != before
    }

    /// Applies one worker message from the channel.
    ///
    /// Stale messages are dropped. `Finished` ends the current job: when
    /// the state still shows work in progress (Checking, Downloading,
    /// Verifying, Installing) and no terminal message came, the job died
    /// without a result and fails with `WorkerPanicked`. The job's end then
    /// re-arms the daily schedule. `Installed` marks the install job's
    /// terminal message and records the bundle to relaunch.
    pub(crate) fn deliver(&mut self, msg: WorkerMsg, now: Instant, wall: SystemTime) -> Delivery {
        let Some(msg) = self.accept(msg) else {
            return Delivery::Stale;
        };
        let notice_changed = match msg {
            WorkerMsg::Event { event, .. } => self.apply_event(event),
            WorkerMsg::Mode { mode, .. } => self.set_mode(mode),
            WorkerMsg::Installed { bundle, .. } => {
                if let Some(job) = self.job.as_mut() {
                    job.terminal_seen = true;
                }
                self.relaunch = Some(bundle);
                return Delivery::Quit;
            }
            WorkerMsg::Finished { panicked, .. } => {
                // `accept` passed it, so the job is current.
                let Some(job) = self.job.take() else {
                    return Delivery::Stale;
                };
                let mut changed = false;
                if !job.terminal_seen && in_progress(&self.state) {
                    log::warn!(
                        target: TARGET,
                        "update job {} ({:?}) ended without a result in {} (panicked: {panicked})",
                        job.id,
                        job.kind,
                        self.state.name()
                    );
                    changed = self.apply_event(Event::Failed(Failure::WorkerPanicked));
                } else if panicked {
                    log::warn!(
                        target: TARGET,
                        "update job {} ({:?}) panicked; state stays {}",
                        job.id,
                        job.kind,
                        self.state.name()
                    );
                }
                self.scheduler.job_finished(now, wall);
                changed
            }
        };
        Delivery::Applied { notice_changed }
    }
}

/// States a job leaves only by reporting a result; a `Finished` in one of
/// them means the result never came.
fn in_progress(state: &UpdateState) -> bool {
    matches!(
        state,
        UpdateState::Checking
            | UpdateState::Downloading(_)
            | UpdateState::Verifying(_)
            | UpdateState::Installing(_)
    )
}

impl App {
    /// The wake closure for [`UpdateController::start_job`]. It clones the
    /// shared proxy `Arc`, never the proxy itself (on macOS each proxy clone
    /// registers a run-loop source that is never removed; see `App::proxy`).
    pub(crate) fn update_wake(&self) -> Wake {
        let proxy = self.proxy.clone();
        Arc::new(move || {
            if let Some(p) = &proxy {
                let _ = p.send_event(UserEvent::UpdateWorker);
            }
        })
    }

    /// End of `resumed`, after the tabs spawn: starts the launch
    /// housekeeping job in `SelfUpdate` mode, whether or not checks are
    /// enabled (it needs no network). It reports only through the log.
    pub(crate) fn start_update_housekeeping(&mut self) {
        let Some(params) = self.update.housekeeping_params() else {
            return;
        };
        if let Err(e) = worker::spawn_housekeeping(params) {
            log::warn!(target: TARGET, "could not start update housekeeping: {e}");
        }
    }

    /// `about_to_wait`: starts a check job when the scheduler says it is
    /// due. Spawning never blocks; with nothing due this only compares
    /// clocks.
    pub(crate) fn service_update(&mut self, now: Instant, wall: SystemTime) {
        if !self.update.check_due(now, wall) {
            return;
        }
        let wake = self.update_wake();
        let changed = self
            .update
            .begin_check(now, wall, wake, worker::spawn_check);
        self.update_view_changed(changed);
    }

    /// `apply_settings_changes`: `[update] enabled` changed. Touches neither
    /// the renderer nor its shaping caches.
    pub(crate) fn update_enabled_changed(&mut self, now: Instant) {
        let changed = self
            .update
            .set_enabled(self.config.update.enabled, now, SystemTime::now());
        self.update_view_changed(changed);
    }

    /// After the frame, beside the tab actions: applies the notice button
    /// the user clicked. Nothing here blocks: Restart spawns the install
    /// job, Later only records the version, and the release page goes
    /// through the link opener's scheme allowlist and detached spawn.
    /// Repaints at most once, when the notice changed.
    pub(crate) fn on_update_action(&mut self, action: UpdateAction) {
        let notice_changed = match action {
            UpdateAction::Restart => {
                let wake = self.update_wake();
                self.update.request_install(
                    Instant::now(),
                    SystemTime::now(),
                    wake,
                    worker::spawn_install,
                )
            }
            UpdateAction::Later => self.update.dismiss(),
            UpdateAction::OpenRelease(url) => {
                match crate::links::open(&url) {
                    Ok(pid) => {
                        log::info!(target: TARGET, "opened release page {url:?} (pid {pid})")
                    }
                    Err(OpenError::Rejected | OpenError::NotFound) => {
                        log::debug!(target: TARGET, "refusing to open release page {url:?}");
                    }
                    Err(OpenError::Spawn(e)) => {
                        log::warn!(target: TARGET, "failed to open release page {url:?}: {e}");
                    }
                }
                return;
            }
        };
        self.update_view_changed(notice_changed);
    }

    /// Refreshes the settings status after a controller change and repaints
    /// only when the notice content changed. An open settings panel picks up
    /// the new status on its next frame.
    fn update_view_changed(&mut self, notice_changed: bool) {
        self.settings.update_status = self.update.status_view();
        if notice_changed && let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// `UserEvent::UpdateWorker`: drains every queued worker message. A
    /// batch repaints at most once. After `Installed` the loop exits
    /// normally, so `exiting()` saves the session and shuts PTYs down before
    /// [`Self::relaunch_after_update`] opens the new bundle. Nothing here
    /// waits.
    pub(crate) fn on_update_worker(&mut self, event_loop: &ActiveEventLoop) {
        let (now, wall) = (Instant::now(), SystemTime::now());
        let mut notice_changed = false;
        let mut quit = false;
        let mut delivered = false;
        while let Ok(msg) = self.update.rx.try_recv() {
            match self.update.deliver(msg, now, wall) {
                Delivery::Stale => continue,
                Delivery::Applied { notice_changed: c } => notice_changed |= c,
                Delivery::Quit => quit = true,
            }
            delivered = true;
        }
        if delivered {
            self.update_view_changed(notice_changed);
        }
        if quit {
            log::info!(target: TARGET, "quitting to relaunch the updated Cometty");
            event_loop.exit();
        }
    }

    /// The end of `exiting()`: once an install succeeded, release the
    /// session lock (the snapshot is already written) and start the new
    /// bundle with `open -n`, without waiting for it.
    pub(crate) fn relaunch_after_update(&mut self) {
        let Some(bundle) = self.update.relaunch.take() else {
            return;
        };
        self.session.release();
        match fsops::open_new_instance(&bundle) {
            Ok(()) => log::info!(target: TARGET, "relaunching {}", bundle.display()),
            Err(e) => log::warn!(
                target: TARGET,
                "could not relaunch {} ({e}); start Cometty again to use the update",
                bundle.display()
            ),
        }
    }
}

/// The job id every [`WorkerMsg`] carries.
fn msg_job(msg: &WorkerMsg) -> u64 {
    match msg {
        WorkerMsg::Event { job, .. }
        | WorkerMsg::Mode { job, .. }
        | WorkerMsg::Installed { job, .. }
        | WorkerMsg::Finished { job, .. } => *job,
    }
}

/// The "Last check" status line for `state`; `None` before any check.
fn outcome_text(state: &UpdateState) -> Option<String> {
    Some(match state {
        UpdateState::Idle => return None,
        UpdateState::Checking => "checking…".into(),
        UpdateState::UpToDate => "up to date".into(),
        UpdateState::Available(release) => format!("Cometty {} is available", release.version),
        UpdateState::Downloading(r) => format!("downloading Cometty {}", r.version),
        UpdateState::Verifying(r) => format!("verifying Cometty {}", r.version),
        UpdateState::Staged { version, issue } => match issue {
            None | Some(InstallIssue::Blocked { .. }) => {
                format!("Cometty {version} is ready; restart to update")
            }
            Some(InstallIssue::Failed(failure)) => {
                format!("Cometty {version} is ready; the last install failed: {failure}")
            }
        },
        UpdateState::Installing(v) => format!("installing Cometty {v}"),
        UpdateState::Failed {
            failure,
            release: Some(release),
        } => format!(
            "Cometty {} is available; download failed: {failure}",
            release.version
        ),
        UpdateState::Failed { failure, .. } => format!("failed: {failure}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::release::{Asset, AssetProblem, Release, SelectedAssets};
    use crate::update::schedule::{CHECK_INTERVAL, ENABLE_DELAY, STARTUP_DELAY};
    use std::time::Duration;

    fn wall0() -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_735_689_600)
    }

    fn controller(running: &str, enabled: bool, now: Instant) -> UpdateController {
        UpdateController::new(
            Launch {
                running,
                enabled,
                bundled: false,
                exe: Path::new("/Users/me/dev/cometty/target/debug/cometty"),
                staging_dir: Some(PathBuf::from("/tmp/cometty-test/updates")),
            },
            now,
            wall0(),
        )
    }

    fn release(v: Version) -> Release {
        Release {
            version: v,
            page_url: "https://github.com/jjunac/cometty/releases/latest".into(),
            assets: Err(AssetProblem::Missing("archive".into())),
        }
    }

    /// `release(v)` with both assets selected: self-update downloads it, so
    /// its `Available` state shows no notice.
    fn downloadable(v: Version) -> Release {
        let asset = |name: String| Asset {
            url: format!("https://github.com/jjunac/cometty/releases/download/v{v}/{name}"),
            name,
            size: 1,
        };
        Release {
            assets: Ok(SelectedAssets {
                archive: asset(format!("Cometty-{v}-macos-universal.zip")),
                checksum: asset(format!("Cometty-{v}-macos-universal.zip.sha256")),
            }),
            ..release(v)
        }
    }

    fn noop_wake() -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(|| {})
    }

    #[test]
    fn enabled_launch_arms_the_startup_deadline_and_disabled_does_not() {
        let now = Instant::now();
        assert_eq!(
            controller("0.4.0", true, now).deadline(),
            Some(now + STARTUP_DELAY)
        );
        assert_eq!(controller("0.4.0", false, now).deadline(), None);
    }

    #[test]
    fn non_plain_version_is_inert() {
        let now = Instant::now();
        for bad in ["0.4.0-dev", "v0.4.0", "0.4"] {
            let c = controller(bad, true, now);
            assert_eq!(c.running, None);
            assert_eq!(c.deadline(), None, "{bad}");
            assert!(c.status_view().mode.contains("not X.Y.Z"), "{bad}");
        }
    }

    #[test]
    fn cargo_launch_is_notify_only() {
        let c = controller("0.4.0", true, Instant::now());
        assert_eq!(c.mode, Mode::NotifyOnly(NotifyReason::NotBundled));
        let view = c.status_view();
        assert_eq!(view.version, RUNNING_VERSION);
        assert_eq!(view.last_outcome, None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bundle_launch_is_self_update_without_a_probe() {
        let c = UpdateController::new(
            Launch {
                running: "0.4.0",
                enabled: true,
                bundled: true,
                exe: Path::new("/Applications/Cometty.app/Contents/MacOS/cometty"),
                staging_dir: Some(PathBuf::from("/tmp/cometty-test/updates")),
            },
            Instant::now(),
            wall0(),
        );
        assert_eq!(
            c.mode,
            Mode::SelfUpdate {
                bundle: PathBuf::from("/Applications/Cometty.app")
            }
        );
    }

    #[test]
    fn apply_event_reports_notice_changes_only() {
        let mut c = controller("0.4.0", true, Instant::now());
        let v = Version {
            major: 0,
            minor: 5,
            patch: 0,
        };
        assert!(!c.apply_event(Event::CheckStarted), "no notice either side");
        assert_eq!(c.status_view().last_outcome.as_deref(), Some("checking…"));
        assert!(c.apply_event(Event::Newer(release(v))), "Available notice");
        assert!(
            !c.apply_event(Event::VerifyStarted),
            "invalid pair is ignored"
        );
        assert!(matches!(c.state, UpdateState::Available(_)));
        assert!(c.apply_event(Event::Disabled), "notice hidden");
        assert_eq!(c.state, UpdateState::Idle);
    }

    /// Valid transitions that keep the notice content report no change;
    /// a different notice (another variant, or the same variant with other
    /// fields) does.
    #[test]
    fn apply_event_ignores_state_changes_that_keep_the_notice() {
        let mut c = self_update(Instant::now(), true);
        let v = v050();
        // A self-update pipeline shows nothing until the bundle is staged.
        for (event, state) in [
            (Event::CheckStarted, UpdateState::Checking),
            (
                Event::Newer(downloadable(v)),
                UpdateState::Available(downloadable(v)),
            ),
            (
                Event::DownloadStarted,
                UpdateState::Downloading(downloadable(v)),
            ),
            (
                Event::VerifyStarted,
                UpdateState::Verifying(downloadable(v)),
            ),
        ] {
            let name = event.name();
            assert!(!c.apply_event(event), "{name}: no notice either side");
            assert_eq!(c.state, state, "{name}");
        }
        assert!(c.apply_event(Event::Staged(v)), "Ready appears");
        let ready = c.notice();
        assert!(c.apply_event(Event::InstallRequested), "Ready -> Preparing");
        assert!(
            c.apply_event(Event::InstallBlocked { others: 2 }),
            "Preparing -> Ready with a blocked count"
        );
        assert_ne!(c.notice(), ready, "the issue is part of the content");

        // A dismissed version keeps the notice hidden across a fresh check.
        let mut c = controller("0.4.0", true, Instant::now());
        c.dismissed = Some(v);
        assert!(!c.apply_event(Event::CheckStarted));
        assert!(!c.apply_event(Event::Newer(release(v))));
        assert!(matches!(c.state, UpdateState::Available(_)));
        assert_eq!(c.notice(), None);
    }

    #[test]
    fn staged_suspends_the_deadline() {
        let now = Instant::now();
        let mut c = controller("0.4.0", true, now);
        let v = Version {
            major: 0,
            minor: 5,
            patch: 0,
        };
        c.state = UpdateState::Available(release(v));
        assert_eq!(c.deadline(), Some(now + STARTUP_DELAY));
        assert!(c.apply_event(Event::AlreadyStaged(v)));
        assert_eq!(c.deadline(), None);
        assert!(c.notice().is_some_and(|n| n.privacy_line().is_some()));
        // Suspended on both clocks, however late, through Installing and a
        // blocked install's return to Staged.
        let late = 2 * CHECK_INTERVAL;
        assert!(!c.check_due(now + late, wall0() + late));
        assert!(c.apply_event(Event::InstallRequested));
        assert_eq!(c.deadline(), None, "Installing");
        assert!(c.apply_event(Event::InstallBlocked { others: 1 }));
        assert_eq!(c.deadline(), None, "Staged again");
        assert!(!c.check_due(now + late, wall0() + late));
        // A failed install returns to Staged with the reason: still
        // suspended, and Restart can retry.
        assert!(c.apply_event(Event::InstallRequested));
        assert!(c.apply_event(Event::Failed(Failure::Spawn("no threads".into()))));
        assert!(matches!(
            c.state,
            UpdateState::Staged {
                issue: Some(InstallIssue::Failed(_)),
                ..
            }
        ));
        assert_eq!(c.deadline(), None);
    }

    #[test]
    fn staging_through_verification_suspends_the_deadline() {
        let now = Instant::now();
        let mut c = self_update(now, true);
        let job = begin_check(&mut c);
        for event in [
            Event::Newer(downloadable(v050())),
            Event::DownloadStarted,
            Event::VerifyStarted,
            Event::Staged(v050()),
        ] {
            c.deliver(WorkerMsg::Event { job, event }, now, wall0());
        }
        assert!(matches!(c.state, UpdateState::Staged { .. }));
        // The check's `Finished` re-arms the daily schedule, which stays
        // suspended while the bundle is staged.
        assert!(!applied(c.deliver(finished(job), now, wall0())));
        assert!(matches!(c.state, UpdateState::Staged { .. }));
        assert_eq!(c.deadline(), None);
        let late = 2 * CHECK_INTERVAL;
        assert!(!c.check_due(now + late, wall0() + late));
    }

    #[test]
    fn accept_drops_messages_from_other_jobs() {
        let mut c = controller("0.4.0", true, Instant::now());
        let finished = |job| WorkerMsg::Finished {
            job,
            panicked: false,
        };
        assert!(c.accept(finished(0)).is_none(), "no current job");
        let first = c.start_job(JobKind::Check, noop_wake()).id;
        let second = c.start_job(JobKind::Check, noop_wake()).id;
        assert_ne!(first, second);
        assert!(c.accept(finished(first)).is_none(), "superseded job");
        assert!(c.accept(finished(second)).is_some());
    }

    #[test]
    fn job_ctx_messages_reach_the_controller_channel() {
        let mut c = controller("0.4.0", true, Instant::now());
        let ctx = c.start_job(JobKind::Check, noop_wake());
        ctx.emit(Event::CheckStarted);
        let msg = c.rx.try_recv().expect("delivered");
        assert!(matches!(
            c.accept(msg),
            Some(WorkerMsg::Event {
                event: Event::CheckStarted,
                ..
            })
        ));
    }

    fn v050() -> Version {
        Version {
            major: 0,
            minor: 5,
            patch: 0,
        }
    }

    fn finished(job: u64) -> WorkerMsg {
        WorkerMsg::Finished {
            job,
            panicked: false,
        }
    }

    fn applied(d: Delivery) -> bool {
        match d {
            Delivery::Applied { notice_changed } => notice_changed,
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    /// A check job as `service_update` starts it, with a spawner that
    /// accepts the job and drops it.
    fn begin_check(c: &mut UpdateController) -> u64 {
        c.begin_check(Instant::now(), wall0(), noop_wake(), |_, _| Ok(()));
        c.job.as_ref().expect("check job is current").id
    }

    fn self_update(now: Instant, enabled: bool) -> UpdateController {
        let mut c = controller("0.4.0", enabled, now);
        c.mode = Mode::SelfUpdate {
            bundle: PathBuf::from("/Applications/Cometty.app"),
        };
        c
    }

    #[test]
    fn housekeeping_runs_only_in_self_update_even_when_disabled() {
        for reason in [
            NotifyReason::NotBundled,
            NotifyReason::Translocated,
            NotifyReason::NotWritable,
            NotifyReason::NoStagingDir,
        ] {
            let mut c = self_update(Instant::now(), true);
            c.mode = Mode::NotifyOnly(reason.clone());
            assert!(c.housekeeping_params().is_none(), "{reason:?}");
        }
        let now = Instant::now();
        let c = self_update(now, false);
        let params = c.housekeeping_params().expect("self-update cleans up");
        assert_eq!(params.installed, PathBuf::from("/Applications/Cometty.app"));
        assert_eq!(
            params.staging_dir,
            PathBuf::from("/tmp/cometty-test/updates")
        );
        assert_eq!(params.lock_patience, JOB_LOCK_PATIENCE);
        assert_eq!(c.deadline(), None, "housekeeping schedules no check");
        assert!(c.job.is_none(), "housekeeping is not a tracked job");
    }

    #[test]
    fn due_check_spawns_in_notify_only_mode_and_consumes_the_deadline() {
        let now = Instant::now();
        let mut c = controller("0.4.0", true, now);
        assert!(!c.check_due(now, wall0()));
        let at = now + STARTUP_DELAY;
        assert!(c.check_due(at, wall0() + STARTUP_DELAY));
        let mut seen = None;
        c.begin_check(at, wall0(), noop_wake(), |ctx, params| {
            seen = Some((ctx.id, params));
            Ok(())
        });
        let (id, params) = seen.expect("spawned");
        assert_eq!(
            c.job.as_ref().map(|j| (j.id, j.kind)),
            Some((id, JobKind::Check))
        );
        assert_eq!(params.mode, Mode::NotifyOnly(NotifyReason::NotBundled));
        assert_eq!(params.running.to_string(), "0.4.0");
        assert_eq!(params.lock_patience, JOB_LOCK_PATIENCE);
        assert_eq!(c.state, UpdateState::Checking);
        assert_eq!(c.deadline(), None, "in flight");
        assert!(!c.check_due(at + CHECK_INTERVAL, wall0() + CHECK_INTERVAL));
    }

    #[test]
    fn check_spawn_error_fails_and_retries_a_day_later() {
        let now = Instant::now();
        let mut c = controller("0.4.0", true, now);
        c.begin_check(now, wall0(), noop_wake(), |_, _| {
            Err(io::Error::other("no threads"))
        });
        assert_eq!(
            c.state,
            UpdateState::Failed {
                failure: Failure::Spawn("no threads".into()),
                release: None,
            }
        );
        assert!(c.job.is_none());
        assert_eq!(c.deadline(), Some(now + CHECK_INTERVAL));
    }

    #[test]
    fn disabling_cancels_and_abandons_the_check() {
        let now = Instant::now();
        let mut c = controller("0.4.0", true, now);
        // The spawner keeps the job's own context, as the worker would.
        let mut worker_ctx = None;
        c.begin_check(now, wall0(), noop_wake(), |ctx, _| {
            worker_ctx = Some(ctx);
            Ok(())
        });
        let ctx = worker_ctx.expect("spawned");
        let job = c.job.as_ref().unwrap().id;
        assert_eq!(ctx.id, job);
        assert!(!ctx.cancelled());
        assert!(!c.set_enabled(false, now, wall0()), "no notice either side");
        assert!(ctx.cancelled(), "the worker's JobCtx sees the cancel flag");
        assert!(c.job.is_none());
        assert_eq!(c.state, UpdateState::Idle);
        assert_eq!(c.deadline(), None);
        assert!(!c.check_due(now + CHECK_INTERVAL, wall0() + CHECK_INTERVAL));
        assert!(matches!(
            c.deliver(finished(job), now, wall0()),
            Delivery::Stale
        ));
    }

    #[test]
    fn disabling_hides_an_available_notice() {
        let now = Instant::now();
        let mut c = controller("0.4.0", true, now);
        c.state = UpdateState::Available(release(v050()));
        assert_eq!(c.deadline(), Some(now + STARTUP_DELAY));
        assert!(c.set_enabled(false, now, wall0()));
        assert_eq!(c.notice(), None);
        assert_eq!(c.deadline(), None, "the pending check is cleared");
        let late = 2 * CHECK_INTERVAL;
        assert!(!c.check_due(now + late, wall0() + late));
    }

    #[test]
    fn disabling_while_installing_keeps_the_install_job() {
        let now = Instant::now();
        let mut c = self_update(now, true);
        c.state = UpdateState::Staged {
            version: v050(),
            issue: None,
        };
        assert!(c.apply_event(Event::InstallRequested));
        let ctx = c.start_job(JobKind::Install, noop_wake());
        assert!(!c.set_enabled(false, now, wall0()));
        assert!(!ctx.cancelled());
        assert_eq!(c.job.as_ref().map(|j| j.id), Some(ctx.id));
        assert_eq!(c.state, UpdateState::Installing(v050()));
        assert_eq!(c.deadline(), None);
        assert_eq!(c.deliver(installed(ctx.id), now, wall0()), Delivery::Quit);
        assert!(!applied(c.deliver(finished(ctx.id), now, wall0())));
        assert_eq!(c.state, UpdateState::Installing(v050()));
        assert!(c.job.is_none(), "Finished ended the install job");
        assert_eq!(c.deadline(), None, "still disabled");
    }

    fn v060() -> Version {
        Version {
            major: 0,
            minor: 6,
            patch: 0,
        }
    }

    /// The install job's terminal message for the test bundle.
    fn installed(job: u64) -> WorkerMsg {
        WorkerMsg::Installed {
            job,
            bundle: PathBuf::from("/Applications/Cometty.app"),
        }
    }

    /// A self-update controller with `v050` staged.
    fn staged(now: Instant) -> UpdateController {
        let mut c = self_update(now, true);
        c.state = UpdateState::Staged {
            version: v050(),
            issue: None,
        };
        // As `apply_event` leaves it in Staged.
        c.scheduler.set_suspended(true);
        c
    }

    #[test]
    fn restart_in_staged_spawns_the_install_job() {
        let now = Instant::now();
        let mut c = staged(now);
        let mut seen = None;
        assert!(
            c.request_install(now, wall0(), noop_wake(), |ctx, params| {
                seen = Some((ctx.id, params));
                Ok(())
            }),
            "Ready -> Preparing"
        );
        let (id, params) = seen.expect("spawned");
        assert_eq!(
            c.job.as_ref().map(|j| (j.id, j.kind)),
            Some((id, JobKind::Install))
        );
        assert_eq!(params.release, v050());
        assert_eq!(params.installed, PathBuf::from("/Applications/Cometty.app"));
        assert_eq!(
            params.staging_dir,
            PathBuf::from("/tmp/cometty-test/updates")
        );
        assert_eq!(params.lock_patience, JOB_LOCK_PATIENCE);
        assert_eq!(c.state, UpdateState::Installing(v050()));
        assert_eq!(
            c.notice(),
            Some(UpdateNotice::Preparing { version: v050() })
        );
        let late = 2 * CHECK_INTERVAL;
        assert_eq!(c.deadline(), None, "suspended while Installing");
        assert!(!c.check_due(now + late, wall0() + late));
    }

    #[test]
    fn restart_outside_staged_self_update_does_nothing() {
        let now = Instant::now();
        let refuse = |_: JobCtx, _: InstallParams| -> io::Result<()> {
            panic!("Restart must not spawn here")
        };
        for state in [
            UpdateState::Idle,
            UpdateState::Checking,
            UpdateState::UpToDate,
            UpdateState::Available(downloadable(v050())),
            UpdateState::Downloading(downloadable(v050())),
            UpdateState::Verifying(downloadable(v050())),
            UpdateState::Installing(v050()),
            UpdateState::Failed {
                failure: Failure::WorkerPanicked,
                release: None,
            },
        ] {
            let mut c = self_update(now, true);
            c.state = state.clone();
            assert!(!c.request_install(now, wall0(), noop_wake(), refuse));
            assert_eq!(c.state, state);
            assert!(c.job.is_none(), "{}", state.name());
        }
        // Staged, but a worker downgraded this launch to notify-only (or it
        // never was self-update).
        for reason in [NotifyReason::NotWritable, NotifyReason::NotBundled] {
            let mut c = staged(now);
            c.mode = Mode::NotifyOnly(reason.clone());
            assert!(!c.request_install(now, wall0(), noop_wake(), refuse));
            assert!(matches!(c.state, UpdateState::Staged { .. }), "{reason:?}");
            assert!(c.job.is_none(), "{reason:?}");
        }
    }

    #[test]
    fn install_spawn_error_returns_to_ready_with_the_reason() {
        let now = Instant::now();
        let mut c = staged(now);
        assert_eq!(c.deadline(), None, "suspended while Staged");
        let at = now + Duration::from_secs(30);
        let wall = wall0() + Duration::from_secs(30);
        assert!(
            c.request_install(at, wall, noop_wake(), |_, _| {
                Err(io::Error::other("no threads"))
            }),
            "Ready gains the failure"
        );
        let issue = Some(InstallIssue::Failed(Failure::Spawn("no threads".into())));
        assert_eq!(
            c.notice(),
            Some(UpdateNotice::Ready {
                version: v050(),
                issue
            })
        );
        assert!(c.job.is_none());
        assert_eq!(c.deadline(), None, "still staged: checks stay suspended");
        assert!(c.status_view().last_outcome.unwrap().contains("no threads"));
        // Restart stays available.
        assert!(c.request_install(at, wall, noop_wake(), |_, _| Ok(())));
        assert_eq!(c.state, UpdateState::Installing(v050()));
    }

    #[test]
    fn a_failed_download_announces_the_release() {
        let now = Instant::now();
        let mut c = self_update(now, true);
        let job = begin_check(&mut c);
        let mut changed = false;
        for event in [
            Event::Newer(downloadable(v050())),
            Event::DownloadStarted,
            Event::Failed(Failure::Network("proxy".into())),
        ] {
            changed |= applied(c.deliver(WorkerMsg::Event { job, event }, now, wall0()));
        }
        assert!(changed);
        assert!(matches!(
            c.notice(),
            Some(UpdateNotice::Available { version, failure: Some(Failure::Network(_)), .. })
                if version == v050()
        ));
        assert!(
            c.status_view()
                .last_outcome
                .unwrap()
                .contains("download failed")
        );
    }

    #[test]
    fn later_hides_the_notice_for_that_version_only() {
        let now = Instant::now();
        // Notify-only Available notice.
        let mut c = controller("0.4.0", true, now);
        c.apply_event(Event::CheckStarted);
        assert!(c.apply_event(Event::Newer(release(v050()))));
        assert!(c.dismiss(), "Available hides");
        assert_eq!(c.notice(), None);
        assert!(!c.dismiss(), "nothing left to dismiss");
        // The same version again stays hidden; a newer one shows.
        c.apply_event(Event::CheckStarted);
        assert!(!c.apply_event(Event::Newer(release(v050()))));
        assert_eq!(c.notice(), None);
        c.apply_event(Event::CheckStarted);
        assert!(c.apply_event(Event::Newer(release(v060()))));
        assert!(matches!(
            c.notice(),
            Some(UpdateNotice::Available { version, .. }) if version == v060()
        ));

        // A staged update's Ready notice.
        let mut c = staged(now);
        assert!(c.dismiss(), "Ready hides");
        assert_eq!(c.notice(), None);
        assert!(
            matches!(c.state, UpdateState::Staged { .. }),
            "still staged"
        );

        // Preparing has no Later button and cannot be dismissed.
        let mut c = staged(now);
        c.request_install(now, wall0(), noop_wake(), |_, _| Ok(()));
        assert!(!c.dismiss());
        assert_eq!(c.dismissed, None);
        assert!(c.notice().is_some());
    }

    #[test]
    fn enabling_arms_a_check_within_a_second() {
        let now = Instant::now();
        let mut c = controller("0.4.0", false, now);
        assert_eq!(c.deadline(), None);
        let t = now + Duration::from_secs(90);
        let wall = wall0() + Duration::from_secs(90);
        assert!(!c.set_enabled(true, t, wall));
        let deadline = c.deadline().expect("armed");
        assert!(deadline > t, "not immediate");
        assert!(
            deadline <= t + Duration::from_secs(5),
            "within five seconds"
        );
        assert_eq!(deadline, t + ENABLE_DELAY);
        assert!(!c.check_due(t, wall));
        assert!(c.check_due(deadline, wall));
        assert!(
            c.check_due(t, wall + ENABLE_DELAY),
            "the wall-clock half fires too"
        );
    }

    #[test]
    fn reenabling_after_an_abandoned_check_arms_a_fresh_one() {
        let now = Instant::now();
        let mut c = controller("0.4.0", true, now);
        let old = begin_check(&mut c);
        c.set_enabled(false, now, wall0());
        let t = now + Duration::from_secs(10);
        c.set_enabled(true, t, wall0());
        assert_eq!(c.deadline(), Some(t + ENABLE_DELAY), "not left in flight");
        let current = begin_check(&mut c);
        assert_ne!(current, old);
        assert!(matches!(
            c.deliver(finished(old), t, wall0()),
            Delivery::Stale
        ));
        assert_eq!(c.state, UpdateState::Checking);
    }

    #[test]
    fn enabling_an_inert_updater_schedules_nothing() {
        let now = Instant::now();
        let mut c = controller("0.4.0-dev", false, now);
        c.set_enabled(true, now, wall0());
        assert_eq!(c.deadline(), None);
        assert!(!c.check_due(now + CHECK_INTERVAL, wall0() + CHECK_INTERVAL));
    }

    #[test]
    fn finished_without_a_result_fails_and_schedules_a_day_out() {
        let now = Instant::now();
        let mut c = controller("0.4.0", true, now);
        let job = begin_check(&mut c);
        assert_eq!(c.deadline(), None, "in flight");
        let later = now + Duration::from_secs(60);
        let wall = wall0() + Duration::from_secs(60);
        assert!(!applied(c.deliver(finished(job), later, wall)));
        assert_eq!(
            c.state,
            UpdateState::Failed {
                failure: Failure::WorkerPanicked,
                release: None,
            }
        );
        assert_eq!(c.deadline(), Some(later + CHECK_INTERVAL));
        assert!(c.job.is_none());
        let just_before = CHECK_INTERVAL - Duration::from_secs(1);
        assert!(!c.check_due(later + just_before, wall + just_before));
        assert!(c.check_due(later + CHECK_INTERVAL, wall));
    }

    /// The same holds when the job dies mid-download or mid-verification,
    /// whether or not the worker reports a panic.
    #[test]
    fn finished_mid_pipeline_fails_and_schedules_a_day_out() {
        let stages: [&[Event]; 2] = [
            &[Event::Newer(downloadable(v050())), Event::DownloadStarted],
            &[
                Event::Newer(downloadable(v050())),
                Event::DownloadStarted,
                Event::VerifyStarted,
            ],
        ];
        for events in stages {
            for panicked in [false, true] {
                let now = Instant::now();
                let mut c = self_update(now, true);
                let job = begin_check(&mut c);
                for event in events {
                    let event = event.clone();
                    c.deliver(WorkerMsg::Event { job, event }, now, wall0());
                }
                let reached = c.state.name();
                assert!(in_progress(&c.state), "{reached}");
                let end = WorkerMsg::Finished { job, panicked };
                c.deliver(end, now, wall0());
                assert_eq!(
                    c.state,
                    UpdateState::Failed {
                        failure: Failure::WorkerPanicked,
                        release: Some(downloadable(v050())),
                    },
                    "{reached}, panicked: {panicked}"
                );
                assert_eq!(c.deadline(), Some(now + CHECK_INTERVAL), "{reached}");
            }
        }
    }

    #[test]
    fn finished_after_a_result_keeps_it() {
        let now = Instant::now();
        let mut c = controller("0.4.0", true, now);
        let job = begin_check(&mut c);
        let up_to_date = WorkerMsg::Event {
            job,
            event: Event::UpToDate,
        };
        assert!(!applied(c.deliver(up_to_date, now, wall0())));
        assert!(!applied(c.deliver(finished(job), now, wall0())));
        assert_eq!(c.state, UpdateState::UpToDate);
        assert_eq!(c.deadline(), Some(now + CHECK_INTERVAL));
    }

    #[test]
    fn installed_is_the_install_jobs_terminal_message() {
        let now = Instant::now();
        let mut c = staged(now);
        assert!(c.apply_event(Event::InstallRequested));
        let job = c.start_job(JobKind::Install, noop_wake()).id;
        assert_eq!(c.deliver(installed(job), now, wall0()), Delivery::Quit);
        assert_eq!(
            c.relaunch.as_deref(),
            Some(Path::new("/Applications/Cometty.app"))
        );
        assert!(!applied(c.deliver(finished(job), now, wall0())));
        assert_eq!(c.state, UpdateState::Installing(v050()));
    }

    #[test]
    fn an_install_job_without_a_result_returns_to_ready() {
        let now = Instant::now();
        let mut c = staged(now);
        assert!(c.apply_event(Event::InstallRequested));
        let job = c.start_job(JobKind::Install, noop_wake()).id;
        assert!(
            applied(c.deliver(finished(job), now, wall0())),
            "Preparing -> Ready with the failure"
        );
        assert_eq!(
            c.state,
            UpdateState::Staged {
                version: v050(),
                issue: Some(InstallIssue::Failed(Failure::WorkerPanicked)),
            }
        );
        assert_eq!(c.relaunch, None);
    }

    #[test]
    fn stale_messages_change_nothing() {
        let now = Instant::now();
        let mut c = controller("0.4.0", true, now);
        let old = begin_check(&mut c);
        let current = c.start_job(JobKind::Check, noop_wake()).id;
        let newer = WorkerMsg::Event {
            job: old,
            event: Event::Newer(release(v050())),
        };
        assert!(matches!(c.deliver(newer, now, wall0()), Delivery::Stale));
        let mode = WorkerMsg::Mode {
            job: old,
            mode: Mode::NotifyOnly(NotifyReason::NotWritable),
        };
        assert!(matches!(c.deliver(mode, now, wall0()), Delivery::Stale));
        assert!(matches!(
            c.deliver(installed(old), now, wall0()),
            Delivery::Stale
        ));
        assert_eq!(c.relaunch, None);
        assert!(matches!(
            c.deliver(finished(old), now, wall0()),
            Delivery::Stale
        ));
        assert!(matches!(
            c.deliver(finished(current + 1), now, wall0()),
            Delivery::Stale
        ));
        assert_eq!(c.state, UpdateState::Checking);
        assert_eq!(c.mode, Mode::NotifyOnly(NotifyReason::NotBundled));
        assert_eq!(c.notice(), None);
        assert_eq!(c.deadline(), None, "a stale Finished re-arms nothing");
        let job = c.job.as_ref().expect("current job kept");
        assert_eq!((job.id, job.terminal_seen), (current, false));
        // The current job still ends normally.
        assert!(!applied(c.deliver(finished(current), now, wall0())));
        assert!(matches!(
            c.state,
            UpdateState::Failed {
                failure: Failure::WorkerPanicked,
                ..
            }
        ));
    }

    #[test]
    fn mode_message_downgrades_and_reports_notice_changes() {
        let now = Instant::now();
        let mut c = controller("0.4.0", true, now);
        c.mode = Mode::SelfUpdate {
            bundle: PathBuf::from("/Applications/Cometty.app"),
        };
        c.state = UpdateState::Available(release(v050()));
        let job = c.start_job(JobKind::Check, noop_wake()).id;
        let not_writable = || WorkerMsg::Mode {
            job,
            mode: Mode::NotifyOnly(NotifyReason::NotWritable),
        };
        assert!(
            applied(c.deliver(not_writable(), now, wall0())),
            "hint added"
        );
        assert_eq!(c.mode, Mode::NotifyOnly(NotifyReason::NotWritable));
        assert!(c.status_view().mode.contains(state::MOVE_HINT));
        assert!(
            !applied(c.deliver(not_writable(), now, wall0())),
            "unchanged"
        );
    }

    /// Shared staging across instances, end to end: a real check job on its
    /// own thread, through `spawn_job`'s `Finished` guard,
    /// against a temp staging directory, with every message delivered
    /// through the controller.
    #[cfg(unix)]
    mod shared_staging {
        use std::io::Cursor;
        use std::sync::Mutex;

        use super::*;
        use crate::update::fsops::{self, StagingLock};
        use crate::update::http::{Fetch, FetchResponse};
        use crate::update::limits::RequestKind;
        use crate::update::release::{archive_name, checksum_name};
        use crate::update::staging::{SCHEMA, STAGED, StagedRecord, extract_name};
        use crate::update::worker::check_tests::{Fixture, NEW, RUNNING, write_bundle};

        /// Serves the release feed for `NEW` (both assets selected) and a
        /// checksum matching [`stage`]'s record, and records every request.
        /// An archive request is a download, which neither case may make, so
        /// it fails the job visibly.
        #[derive(Clone, Default)]
        struct FeedOnly {
            requests: Arc<Mutex<Vec<RequestKind>>>,
        }

        impl FeedOnly {
            fn requests(&self) -> Vec<RequestKind> {
                self.requests.lock().unwrap().clone()
            }
        }

        impl Fetch for FeedOnly {
            fn get(&self, url: &str, kind: RequestKind) -> Result<FetchResponse, Failure> {
                self.requests.lock().unwrap().push(kind);
                let body = match kind {
                    RequestKind::Feed => feed_body(),
                    RequestKind::Checksum => {
                        format!("{}  {}\n", "ab".repeat(32), archive_name(NEW)).into_bytes()
                    }
                    RequestKind::Archive => {
                        return Err(Failure::Network(format!("unexpected download of {url}")));
                    }
                };
                Ok(FetchResponse {
                    status: 200,
                    body: Box::new(Cursor::new(body)),
                })
            }
        }

        fn feed_body() -> Vec<u8> {
            let base = "https://github.com/jjunac/cometty/releases";
            let doc = serde_json::json!({
                "tag_name": format!("v{NEW}"),
                "html_url": format!("{base}/tag/v{NEW}"),
                "assets": [
                    {
                        "name": archive_name(NEW),
                        "size": 1000,
                        "browser_download_url": format!("{base}/download/v{NEW}/{}", archive_name(NEW)),
                    },
                    {
                        "name": checksum_name(NEW),
                        "size": 100,
                        "browser_download_url": format!("{base}/download/v{NEW}/{}", checksum_name(NEW)),
                    },
                ],
            });
            serde_json::to_vec(&doc).unwrap()
        }

        /// A SelfUpdate controller running `RUNNING`, whose bundle sits in
        /// the fixture's writable `Applications/` and whose staging
        /// directory is the fixture's.
        fn instance(fx: &Fixture) -> UpdateController {
            let running = RUNNING.to_string();
            let mut c = UpdateController::new(
                Launch {
                    running: &running,
                    enabled: true,
                    bundled: false,
                    exe: Path::new(""),
                    staging_dir: Some(fx.staging()),
                },
                Instant::now(),
                wall0(),
            );
            c.mode = Mode::SelfUpdate {
                bundle: fx.root.join("Applications/Cometty.app"),
            };
            c
        }

        /// What one check did, as seen by the controller.
        struct Outcome {
            /// Worker event names, in delivery order.
            events: Vec<&'static str>,
            /// Whether any step (including `CheckStarted`) changed the notice.
            notice_changed: bool,
        }

        /// Starts a check as `service_update` does, except that the job
        /// runs on `fetch` and waits `patience` for the lock, then delivers
        /// every message through `Finished`.
        fn check(c: &mut UpdateController, fetch: &FeedOnly, patience: Duration) -> Outcome {
            let fetch = fetch.clone();
            let mut notice_changed =
                c.begin_check(Instant::now(), wall0(), noop_wake(), move |ctx, params| {
                    assert_eq!(params.lock_patience, JOB_LOCK_PATIENCE);
                    let params = CheckParams {
                        lock_patience: patience,
                        ..params
                    };
                    worker::spawn_job("update-check-test", ctx, move |ctx| {
                        worker::run_check(ctx, &fetch, &params);
                    })
                });
            let mut events = Vec::new();
            loop {
                let msg =
                    c.rx.recv_timeout(Duration::from_secs(30))
                        .expect("the job reports Finished");
                let finished = matches!(msg, WorkerMsg::Finished { .. });
                if let WorkerMsg::Event { event, .. } = &msg {
                    events.push(event.name());
                }
                notice_changed |= applied(c.deliver(msg, Instant::now(), wall0()));
                if finished {
                    return Outcome {
                        events,
                        notice_changed,
                    };
                }
            }
        }

        /// A valid staged bundle for `NEW`, as another instance commits it.
        fn stage(fx: &Fixture) {
            let extract = extract_name(NEW, 1, 2);
            write_bundle(&fx.staging().join(&extract), NEW);
            let record = StagedRecord {
                schema: SCHEMA,
                version: NEW,
                sha256: "ab".repeat(32),
                extract_dir: extract,
            };
            fsops::write_json_atomic(&fx.staging().join(STAGED), &record).unwrap();
        }

        /// An instance whose job cannot take the lock another instance
        /// holds stays `Available` with no notice, no download, and no
        /// `WorkerPanicked`, even with a valid record already on disk. The
        /// trailing `Finished` re-arms the schedule, and the next check
        /// finds the record.
        #[test]
        fn a_busy_lock_stays_available_quietly_until_the_next_check_finds_the_staged_bundle() {
            let fx = Fixture::new("ctl-busy");
            let mut c = instance(&fx);
            let held = StagingLock::acquire(&fx.staging(), Duration::ZERO).unwrap();
            stage(&fx);
            let fetch = FeedOnly::default();

            let busy = check(&mut c, &fetch, Duration::from_millis(300));
            assert_eq!(busy.events, ["Newer"]);
            assert!(
                matches!(&c.state, UpdateState::Available(r) if r.version == NEW && r.assets.is_ok()),
                "{:?}",
                c.state
            );
            assert!(!busy.notice_changed);
            assert_eq!(c.notice(), None);
            assert!(c.job.is_none(), "Finished ended the job");
            assert!(c.deadline().is_some(), "the daily schedule is re-armed");
            assert_eq!(fetch.requests(), [RequestKind::Feed]);

            drop(held);
            let next = check(&mut c, &fetch, Duration::ZERO);
            assert_eq!(next.events, ["Newer", "AlreadyStaged"]);
            assert_eq!(
                c.state,
                UpdateState::Staged {
                    version: NEW,
                    issue: None
                }
            );
            assert!(next.notice_changed);
            assert!(matches!(
                c.notice(),
                Some(UpdateNotice::Ready { version, issue: None }) if version == NEW
            ));
            assert_eq!(c.deadline(), None, "Staged suspends checks");
            assert_eq!(
                fetch.requests(),
                [RequestKind::Feed, RequestKind::Feed, RequestKind::Checksum]
            );
        }

        /// Clicks Restart as `on_update_action` does, except that the real
        /// install job runs with a fake process listing of `others` other
        /// instances of `exe` (plus this process, which never counts), then
        /// delivers every message through `Finished`. Returns the job id,
        /// the worker event names, and whether any step changed the notice.
        fn restart(
            c: &mut UpdateController,
            exe: &Path,
            others: i32,
        ) -> (u64, Vec<&'static str>, bool) {
            let exe = exe.to_path_buf();
            let mut job = None;
            let mut notice_changed =
                c.request_install(Instant::now(), wall0(), noop_wake(), |ctx, params| {
                    job = Some(ctx.id);
                    let params = InstallParams {
                        lock_patience: Duration::ZERO,
                        ..params
                    };
                    let own = std::process::id() as i32;
                    let list = move || {
                        let mut list = vec![(own, exe.clone())];
                        list.extend((1..=others).map(|n| (own + n, exe.clone())));
                        list
                    };
                    worker::spawn_job("update-install-test", ctx, move |ctx| {
                        worker::run_install(ctx, &list, &params);
                    })
                });
            assert_eq!(c.state, UpdateState::Installing(NEW), "Preparing first");
            let mut events = Vec::new();
            loop {
                let msg =
                    c.rx.recv_timeout(Duration::from_secs(30))
                        .expect("the job reports Finished");
                let finished = matches!(msg, WorkerMsg::Finished { .. });
                if let WorkerMsg::Event { event, .. } = &msg {
                    events.push(event.name());
                }
                notice_changed |= applied(c.deliver(msg, Instant::now(), wall0()));
                if finished {
                    return (job.expect("spawned"), events, notice_changed);
                }
            }
        }

        /// Through the controller: Restart with another instance running
        /// goes Installing → `InstallBlocked` → `Staged` with the blocked
        /// issue, shows it in the Ready notice, copies nothing, and keeps
        /// the staged update; the trailing `Finished` is not a missing
        /// result. A later Restart runs a fresh install job.
        #[test]
        fn a_blocked_restart_returns_to_ready_and_a_later_restart_reruns_the_job() {
            let fx = Fixture::new("ctl-blocked");
            let mut c = instance(&fx);
            let installed = fx.root.join("Applications/Cometty.app");
            let exe = installed.join("Contents/MacOS/cometty");
            stage(&fx);
            c.state = UpdateState::Staged {
                version: NEW,
                issue: None,
            };
            // As `apply_event` leaves it in Staged.
            c.scheduler.set_suspended(true);

            let (first, events, changed) = restart(&mut c, &exe, 1);
            assert_eq!(events, ["InstallBlocked"]);
            assert!(changed);
            let blocked = |others| Some(InstallIssue::Blocked { others });
            assert_eq!(
                c.state,
                UpdateState::Staged {
                    version: NEW,
                    issue: blocked(1)
                }
            );
            assert_eq!(
                c.notice(),
                Some(UpdateNotice::Ready {
                    version: NEW,
                    issue: blocked(1)
                })
            );
            assert!(c.job.is_none(), "Finished ended the job");
            assert_eq!(c.deadline(), None, "Staged suspends checks");
            assert_eq!(c.relaunch, None, "no quit");
            let (incoming, _) = crate::update::mode::sibling_names(&installed);
            assert!(!incoming.exists(), "no incoming copy");
            assert!(fx.staging().join(STAGED).exists(), "staged update kept");

            // Retry while two others run: a new install job, a new count.
            let (second, events, changed) = restart(&mut c, &exe, 2);
            assert_ne!(second, first);
            assert_eq!(events, ["InstallBlocked"]);
            assert!(changed);
            assert_eq!(
                c.notice(),
                Some(UpdateNotice::Ready {
                    version: NEW,
                    issue: blocked(2)
                })
            );
            assert!(!incoming.exists());
        }
    }
}
