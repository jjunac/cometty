//! One process-wide supervisor reaps children and joins completed PTY workers.
//! The UI only cancels I/O and requests shutdown; it never waits for a child.
use std::io;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(25);
const HANGUP_GRACE: Duration = Duration::from_millis(250);
const EXIT_TIMEOUT: Duration = Duration::from_secs(2);

type Child = Box<dyn portable_pty::Child + Send + Sync>;

#[derive(Default)]
struct Queue {
    incoming: Vec<Job>,
    outstanding: usize,
}

#[derive(Default)]
struct Supervisor {
    queue: Mutex<Queue>,
    changed: Condvar,
}

static SUPERVISOR: OnceLock<Result<Arc<Supervisor>, io::Error>> = OnceLock::new();

struct Control {
    closing: AtomicBool,
    foreground: AtomicI32,
    #[cfg(unix)]
    cancellation: Arc<super::unix::Cancellation>,
    #[cfg(test)]
    finished: Mutex<bool>,
    #[cfg(test)]
    completed: Condvar,
    #[cfg(test)]
    reaped: AtomicBool,
}

#[derive(Clone)]
pub(super) struct Session {
    control: Arc<Control>,
    supervisor: Arc<Supervisor>,
}

impl Session {
    pub(super) fn shutdown(&self, foreground: Option<i32>) {
        // Only the owning PtySession / startup guard requests shutdown. Store
        // the foreground group before publishing the request to the supervisor.
        if !self.control.closing.load(Ordering::Acquire) {
            self.control
                .foreground
                .store(foreground.unwrap_or(0), Ordering::Relaxed);
            self.control.closing.store(true, Ordering::Release);
            #[cfg(unix)]
            self.control.cancellation.cancel();
            self.supervisor.changed.notify_all();
        }
    }

    #[cfg(test)]
    pub(super) fn wait(&self, timeout: Duration) -> bool {
        let finished = self.control.finished.lock().unwrap();
        let (finished, _) = self
            .control
            .completed
            .wait_timeout_while(finished, timeout, |finished| !*finished)
            .unwrap();
        *finished
    }

    #[cfg(test)]
    pub(super) fn reaped(&self) -> bool {
        self.control.reaped.load(Ordering::Acquire)
    }
}

struct Job {
    child: Option<Child>,
    workers: Vec<JoinHandle<()>>,
    session: Session,
    hangup_at: Option<Instant>,
    forced: bool,
    wait_error_logged: bool,
}

/// Owns the child immediately after spawn, including all fallible worker setup.
/// An early return cancels, terminates and reaps it through the same supervisor.
pub(super) struct Startup {
    job: Option<Job>,
}

impl Startup {
    pub(super) fn supervisor() -> anyhow::Result<()> {
        supervisor().map(|_| ())
    }

    pub(super) fn new(
        child: Child,
        #[cfg(unix)] cancellation: Arc<super::unix::Cancellation>,
    ) -> Self {
        // The supervisor is initialized before spawning any child.
        let supervisor = supervisor().expect("PTY supervisor initialized before spawn");
        let session = Session {
            control: Arc::new(Control {
                closing: AtomicBool::new(false),
                foreground: AtomicI32::new(0),
                #[cfg(unix)]
                cancellation,
                #[cfg(test)]
                finished: Mutex::new(false),
                #[cfg(test)]
                completed: Condvar::new(),
                #[cfg(test)]
                reaped: AtomicBool::new(false),
            }),
            supervisor,
        };
        session
            .supervisor
            .queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .outstanding += 1;
        Self {
            job: Some(Job {
                child: Some(child),
                workers: Vec::with_capacity(2),
                session,
                hangup_at: None,
                forced: false,
                wait_error_logged: false,
            }),
        }
    }

    pub(super) fn worker(&mut self, worker: JoinHandle<()>) {
        self.job.as_mut().unwrap().workers.push(worker);
    }

    pub(super) fn activate(mut self) -> Session {
        let job = self.job.take().unwrap();
        let session = job.session.clone();
        submit(job);
        session
    }
}

impl Drop for Startup {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            job.session.shutdown(None);
            submit(job);
        }
    }
}

fn supervisor() -> anyhow::Result<Arc<Supervisor>> {
    SUPERVISOR
        .get_or_init(|| {
            let state = Arc::new(Supervisor::default());
            let worker = state.clone();
            std::thread::Builder::new()
                .name("pty-supervisor".into())
                .spawn(move || supervise(worker))?;
            Ok(state)
        })
        .as_ref()
        .cloned()
        .map_err(|error| anyhow::anyhow!("failed to start PTY supervisor: {error}"))
}

fn submit(job: Job) {
    let supervisor = job.session.supervisor.clone();
    supervisor
        .queue
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .incoming
        .push(job);
    supervisor.changed.notify_all();
}

fn supervise(supervisor: Arc<Supervisor>) {
    let mut jobs = Vec::new();
    loop {
        {
            let mut queue = supervisor.queue.lock().unwrap_or_else(|p| p.into_inner());
            if queue.incoming.is_empty() {
                queue = if jobs.is_empty() {
                    supervisor
                        .changed
                        .wait_while(queue, |queue| queue.incoming.is_empty())
                        .unwrap_or_else(|p| p.into_inner())
                } else {
                    supervisor
                        .changed
                        .wait_timeout(queue, POLL_INTERVAL)
                        .unwrap_or_else(|p| p.into_inner())
                        .0
                };
            }
            jobs.append(&mut queue.incoming);
        }
        // No UI-visible mutex is held across syscalls, joins, or logging.
        let mut index = 0;
        while index < jobs.len() {
            if jobs[index].poll() {
                let job = jobs.swap_remove(index);
                #[cfg(test)]
                {
                    *job.session.control.finished.lock().unwrap() = true;
                    job.session.control.completed.notify_all();
                }
                drop(job);
                supervisor
                    .queue
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .outstanding -= 1;
                supervisor.changed.notify_all();
            } else {
                index += 1;
            }
        }
    }
}

impl Job {
    fn poll(&mut self) -> bool {
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(_)) => self.child = None,
                Ok(None) => {}
                #[cfg(unix)]
                Err(error) if error.raw_os_error() == Some(libc::ECHILD) => self.child = None,
                Err(error) => {
                    if !self.wait_error_logged && error.kind() != io::ErrorKind::Interrupted {
                        log::warn!("failed to reap PTY child: {error}");
                        self.wait_error_logged = true;
                    }
                }
            }
        }
        #[cfg(test)]
        if self.child.is_none() {
            self.session.control.reaped.store(true, Ordering::Release);
        }
        if self.session.control.closing.load(Ordering::Acquire) {
            let now = Instant::now();
            if let Some(started) = self.hangup_at {
                if !self.forced && now.duration_since(started) >= HANGUP_GRACE {
                    self.signal(true);
                    self.forced = true;
                }
            } else {
                self.signal(false);
                self.hangup_at = Some(now);
            }
        }
        let mut index = 0;
        while index < self.workers.len() {
            if self.workers[index].is_finished() {
                if self.workers.swap_remove(index).join().is_err() {
                    log::warn!("PTY worker panicked");
                }
            } else {
                index += 1;
            }
        }
        self.child.is_none() && self.workers.is_empty()
    }

    fn signal(&mut self, force: bool) {
        #[cfg(not(unix))]
        let _ = force;
        let Some(child) = self.child.as_mut() else {
            // Never use a cached pid after reaping: it may have been reused.
            return;
        };
        #[cfg(unix)]
        if let Some(pid) = child
            .process_id()
            .and_then(|pid| i32::try_from(pid).ok())
            .filter(|pid| *pid > 0)
        {
            let signal = if force { libc::SIGKILL } else { libc::SIGHUP };
            if !force {
                let foreground = self.session.control.foreground.load(Ordering::Relaxed);
                // The still-owned/unreaped child reserves its session id.
                // Refuse stale/reused foreground group ids from another session.
                // SAFETY: getsid/kill take integer ids, with positive ids checked.
                if foreground > 0 && foreground != pid && unsafe { libc::getsid(foreground) } == pid
                {
                    let _ = unsafe { libc::kill(-foreground, signal) };
                }
            }
            // Hang up the shell's group too (noninteractive children may share
            // it); escalation kills only the child we actually own, not a
            // recursively enumerated collection of detached descendants.
            // SAFETY: pid is positive, still owned and not yet reaped here.
            let target = if !force && unsafe { libc::getpgid(pid) } == pid {
                -pid
            } else {
                pid
            };
            if unsafe { libc::kill(target, signal) } != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    log::warn!("failed to signal PTY child {pid}: {error}");
                }
            }
            return;
        }
        // Other backends own their native process handles. In particular, do
        // not assume clone_killer() has the same escalation policy as kill().
        if let Err(error) = child.kill() {
            log::warn!("failed to terminate PTY child: {error}");
        }
    }
}

/// Called after all tabs have been dropped and the GUI event loop has ended.
/// One shared deadline covers both recent closes and still-visible sessions.
pub(super) fn finish_shutdown() {
    let Some(Ok(supervisor)) = SUPERVISOR.get() else {
        return;
    };
    let queue = supervisor.queue.lock().unwrap_or_else(|p| p.into_inner());
    let (queue, _) = supervisor
        .changed
        .wait_timeout_while(queue, EXIT_TIMEOUT, |queue| queue.outstanding != 0)
        .unwrap_or_else(|p| p.into_inner());
    let outstanding = queue.outstanding;
    drop(queue);
    if outstanding != 0 {
        log::warn!("{outstanding} PTY sessions did not finish shutdown");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn abandoned_startup_reaps_child_and_joins_started_worker() {
        Startup::supervisor().unwrap();
        let pair = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize::default())
            .unwrap();
        let cancellation = Arc::new(super::super::unix::Cancellation::new().unwrap());
        let mut reader = super::super::unix::Io::new(&*pair.master, cancellation.clone()).unwrap();
        let mut command = portable_pty::CommandBuilder::new("/bin/sleep");
        command.arg("60");
        let child = pair.slave.spawn_command(command).unwrap();
        let pid = child.process_id().unwrap() as i32;
        let mut startup = Startup::new(child, cancellation);
        drop(pair.slave);
        let session = startup.job.as_ref().unwrap().session.clone();
        let worker_done = Arc::new(AtomicBool::new(false));
        let done = worker_done.clone();
        startup.worker(std::thread::spawn(move || {
            use std::io::Read;
            assert_eq!(
                reader.read(&mut [0u8]).unwrap_err().kind(),
                io::ErrorKind::ConnectionAborted
            );
            done.store(true, Ordering::Release);
        }));
        // Models an error creating the second worker: the first handle and
        // the already-spawned child must not escape through an early return.
        drop(startup);
        drop(pair.master);
        session.shutdown(None);
        assert!(session.wait(EXIT_TIMEOUT));
        assert!(worker_done.load(Ordering::Acquire));
        assert!(session.reaped());
        // SAFETY: waitpid only probes the specific child created by this test.
        assert_eq!(
            unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}
