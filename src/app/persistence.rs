//! Debounced, ordered persistence for the config file and the session
//! snapshot. Each writer owns at most one write and one replaceable pending
//! snapshot; filesystem work never runs in a frame.
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::session::{self, Session};

pub const SAVE_DELAY: Duration = Duration::from_millis(300);
/// Session snapshots are crash insurance, not user edits: at most one
/// write per second, anchored at the first change so a busy tab can't
/// postpone it indefinitely.
pub const SESSION_SAVE_DELAY: Duration = Duration::from_secs(1);

pub struct SaveResult {
    pub generation: u64,
    pub result: Result<(), String>,
}

struct State<J> {
    pending: Option<(u64, J)>,
    result: Option<SaveResult>,
    stopping: bool,
}

/// One background thread running jobs in submission order; a newer
/// pending job replaces an older one that hasn't started yet.
struct Worker<J> {
    state: Arc<(Mutex<State<J>>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl<J: Send + 'static> Worker<J> {
    fn new(
        run: impl Fn(J) -> Result<(), String> + Send + 'static,
        wake: impl Fn() + Send + 'static,
    ) -> Self {
        let state = Arc::new((
            Mutex::new(State {
                pending: None,
                result: None,
                stopping: false,
            }),
            Condvar::new(),
        ));
        let shared = state.clone();
        let thread = std::thread::spawn(move || {
            loop {
                let (generation, job) = {
                    let (lock, ready) = &*shared;
                    let mut state = lock.lock().unwrap_or_else(|p| p.into_inner());
                    while state.pending.is_none() && !state.stopping {
                        state = ready.wait(state).unwrap_or_else(|p| p.into_inner());
                    }
                    match state.pending.take() {
                        Some(job) => job,
                        None => break,
                    }
                };
                let result = run(job);
                shared.0.lock().unwrap_or_else(|p| p.into_inner()).result =
                    Some(SaveResult { generation, result });
                wake();
            }
        });
        Self {
            state,
            thread: Some(thread),
        }
    }

    fn enqueue(&self, generation: u64, job: J) {
        let mut state = self.state.0.lock().unwrap_or_else(|p| p.into_inner());
        state.pending = Some((generation, job));
        self.state.1.notify_one();
    }
}

impl<J> Worker<J> {
    fn take_result(&self) -> Option<SaveResult> {
        self.state
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .result
            .take()
    }

    /// Run the pending job (if any), stop, and return the last result.
    fn finish(mut self) -> Option<SaveResult> {
        self.stop_and_join();
        self.take_result()
    }

    fn stop_and_join(&mut self) {
        self.state
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .stopping = true;
        self.state.1.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl<J> Drop for Worker<J> {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

struct ConfigJob {
    config: Config,
    path: Option<PathBuf>,
}

#[derive(Default)]
pub struct ConfigSaver {
    generation: u64,
    dirty: bool,
    deadline: Option<Instant>,
    worker: Option<Worker<ConfigJob>>,
}

impl ConfigSaver {
    pub fn changed(&mut self, now: Instant) {
        self.generation += 1;
        self.dirty = true;
        self.deadline = now.checked_add(SAVE_DELAY);
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// `make_wake` runs only when the worker is first created: on macOS each
    /// winit `EventLoopProxy` clone adds a run-loop source and wakes the loop,
    /// so building one per call would keep `about_to_wait` spinning.
    pub fn dispatch<W: Fn() + Send + 'static>(
        &mut self,
        now: Instant,
        force: bool,
        config: &Config,
        path: Option<PathBuf>,
        make_wake: impl FnOnce() -> W,
    ) {
        if !self.dirty || (!force && self.deadline.is_some_and(|at| at > now)) {
            return;
        }
        let worker = self.worker.get_or_insert_with(|| {
            Worker::new(
                |job: ConfigJob| match job.path {
                    Some(path) => job.config.save_to_path(&path).map_err(|e| format!("{e:#}")),
                    None => Err("no HOME set; cannot resolve config path".into()),
                },
                make_wake(),
            )
        });
        worker.enqueue(
            self.generation,
            ConfigJob {
                config: config.clone(),
                path,
            },
        );
        self.dirty = false;
        self.deadline = None;
    }

    pub fn take_result(&self) -> Option<Result<(), String>> {
        let result = self.worker.as_ref()?.take_result()?;
        // An old success/error cannot clear/replace the status of newer edits.
        (result.generation == self.generation).then_some(result.result)
    }

    pub fn finish(&mut self) -> Option<Result<(), String>> {
        let result = self.worker.take()?.finish()?;
        (result.generation == self.generation).then_some(result.result)
    }
}

/// What the session writer does with the snapshot file.
pub enum SessionJob {
    /// The snapshot plus the `(name, dump)` scrollback files it references
    /// (empty except at quit).
    Write(Session, Vec<(String, Vec<u8>)>),
    /// Fresh start next launch (every tab closed, or restore turned off).
    Remove,
}

/// Ordered writer for the session snapshot. Writes and removals share one
/// queue, so a stale debounced snapshot can never resurrect a session that
/// a later removal deleted.
#[derive(Default)]
pub struct SessionSaver {
    generation: u64,
    dirty: bool,
    deadline: Option<Instant>,
    worker: Option<Worker<(PathBuf, SessionJob)>>,
}

impl SessionSaver {
    pub fn changed(&mut self, now: Instant) {
        self.dirty = true;
        if self.deadline.is_none() {
            self.deadline = now.checked_add(SESSION_SAVE_DELAY);
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// A change is waiting and its debounce elapsed (or `force`).
    pub fn due(&self, now: Instant, force: bool) -> bool {
        self.dirty && (force || self.deadline.is_none_or(|at| at <= now))
    }

    pub fn submit(&mut self, path: PathBuf, job: SessionJob, wake: impl Fn() + Send + 'static) {
        self.generation += 1;
        let worker = self.worker.get_or_insert_with(|| {
            Worker::new(
                |(path, job): (PathBuf, SessionJob)| match job {
                    SessionJob::Write(s, scrollback) => {
                        session::save_to_path(&s, &scrollback, &path).map_err(|e| format!("{e:#}"))
                    }
                    SessionJob::Remove => session::remove(&path).map_err(|e| e.to_string()),
                },
                wake,
            )
        });
        worker.enqueue(self.generation, (path, job));
        self.dirty = false;
        self.deadline = None;
    }

    /// Only failures matter (logged by the caller); success is silent.
    pub fn take_error(&self) -> Option<String> {
        self.worker.as_ref()?.take_result()?.result.err()
    }

    pub fn finish(&mut self) -> Option<String> {
        self.worker.take()?.finish()?.result.err()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_debounce_even_without_visual_frames_and_close_flushes_latest() {
        let dir = std::env::temp_dir().join(format!("cometty-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let now = Instant::now();
        let mut saver = ConfigSaver::default();
        let mut cfg = Config::default();
        for i in 0..100 {
            cfg.window.title = format!("generation {i}");
            saver.changed(now);
            saver.dispatch(now, false, &cfg, Some(path.clone()), || || {});
        }
        assert!(saver.worker.is_none(), "drag edits do no filesystem work");
        assert_eq!(saver.deadline(), Some(now + SAVE_DELAY));
        saver.dispatch(now, true, &cfg, Some(path.clone()), || || {});
        assert!(saver.finish().unwrap().is_ok());
        assert_eq!(
            Config::load_from_path(&path).unwrap().window.title,
            "generation 99"
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn ordered_writer_finishes_with_latest_generation_and_reports_errors() {
        let mut saver = ConfigSaver::default();
        let now = Instant::now();
        let cfg = Config::default();
        saver.changed(now);
        saver.dispatch(now, true, &cfg, None, || || {});
        saver.changed(now);
        saver.dispatch(now, true, &cfg, None, || || {});
        assert!(saver.finish().unwrap().unwrap_err().contains("no HOME"));
    }

    #[test]
    fn session_writer_debounce_is_anchored_and_last_job_wins() {
        let dir = std::env::temp_dir().join(format!("cometty-session-save-{}", std::process::id()));
        let path = dir.join("session.toml");
        let now = Instant::now();
        let mut saver = SessionSaver::default();
        saver.changed(now);
        saver.changed(now + Duration::from_millis(900));
        assert_eq!(saver.deadline(), Some(now + SESSION_SAVE_DELAY));
        assert!(!saver.due(now, false));
        assert!(saver.due(now + SESSION_SAVE_DELAY, false));
        assert!(saver.due(now, true));

        let snapshot = Session::new(0, None, vec![session::TabState::default()]);
        saver.submit(
            path.clone(),
            SessionJob::Write(snapshot.clone(), Vec::new()),
            || {},
        );
        assert!(!saver.due(now, true), "submitting clears the change");
        saver.submit(path.clone(), SessionJob::Remove, || {});
        assert_eq!(saver.finish(), None);
        assert!(!path.exists(), "remove after write leaves no snapshot");

        let mut saver = SessionSaver::default();
        saver.submit(path.clone(), SessionJob::Remove, || {});
        saver.submit(
            path.clone(),
            SessionJob::Write(snapshot.clone(), Vec::new()),
            || {},
        );
        assert_eq!(saver.finish(), None);
        assert_eq!(session::load(&path), Some(snapshot));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
