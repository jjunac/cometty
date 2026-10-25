//! Debounced, ordered config persistence. The worker owns at most one write
//! and one replaceable pending snapshot; filesystem work never runs in a frame.
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::config::Config;

pub const SAVE_DELAY: Duration = Duration::from_millis(300);

struct Job {
    generation: u64,
    config: Config,
    path: Option<PathBuf>,
}
pub struct SaveResult {
    pub generation: u64,
    pub result: Result<(), String>,
}

#[derive(Default)]
struct State {
    pending: Option<Job>,
    result: Option<SaveResult>,
    stopping: bool,
}

struct Worker {
    state: Arc<(Mutex<State>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    fn new(wake: impl Fn() + Send + 'static) -> Self {
        let state = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let shared = state.clone();
        let thread = std::thread::spawn(move || {
            loop {
                let job = {
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
                let result = match job.path {
                    Some(path) => job.config.save_to_path(&path).map_err(|e| format!("{e:#}")),
                    None => Err("no HOME set; cannot resolve config path".into()),
                };
                shared.0.lock().unwrap_or_else(|p| p.into_inner()).result = Some(SaveResult {
                    generation: job.generation,
                    result,
                });
                wake();
            }
        });
        Self {
            state,
            thread: Some(thread),
        }
    }

    fn enqueue(&self, job: Job) {
        let mut state = self.state.0.lock().unwrap_or_else(|p| p.into_inner());
        state.pending = Some(job);
        self.state.1.notify_one();
    }

    fn take_result(&self) -> Option<SaveResult> {
        self.state
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .result
            .take()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
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

#[derive(Default)]
pub struct ConfigSaver {
    generation: u64,
    dirty: bool,
    deadline: Option<Instant>,
    worker: Option<Worker>,
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

    pub fn dispatch(
        &mut self,
        now: Instant,
        force: bool,
        config: &Config,
        path: Option<PathBuf>,
        wake: impl Fn() + Send + 'static,
    ) {
        if !self.dirty || (!force && self.deadline.is_some_and(|at| at > now)) {
            return;
        }
        let worker = self.worker.get_or_insert_with(|| Worker::new(wake));
        worker.enqueue(Job {
            generation: self.generation,
            config: config.clone(),
            path,
        });
        self.dirty = false;
        self.deadline = None;
    }

    pub fn take_result(&self) -> Option<Result<(), String>> {
        let result = self.worker.as_ref()?.take_result()?;
        // An old success/error cannot clear/replace the status of newer edits.
        (result.generation == self.generation).then_some(result.result)
    }

    pub fn finish(&mut self) -> Option<Result<(), String>> {
        let mut worker = self.worker.take()?;
        worker
            .state
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .stopping = true;
        worker.state.1.notify_one();
        if let Some(thread) = worker.thread.take() {
            let _ = thread.join();
        }
        let result = worker.take_result()?;
        (result.generation == self.generation).then_some(result.result)
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
            saver.dispatch(now, false, &cfg, Some(path.clone()), || {});
        }
        assert!(saver.worker.is_none(), "drag edits do no filesystem work");
        assert_eq!(saver.deadline(), Some(now + SAVE_DELAY));
        saver.dispatch(now, true, &cfg, Some(path.clone()), || {});
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
        saver.dispatch(now, true, &cfg, None, || {});
        saver.changed(now);
        saver.dispatch(now, true, &cfg, None, || {});
        assert!(saver.finish().unwrap().unwrap_err().contains("no HOME"));
    }
}
