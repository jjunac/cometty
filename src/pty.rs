use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::config::Config;

#[cfg(test)]
#[path = "pty_test_support.rs"]
pub(crate) mod test_support;

/// Pending PTY-output chunks buffered between the reader thread and the UI
/// drain. The reader blocks past this (~1MB at 8KB chunks), which applies
/// backpressure through the kernel PTY buffer to the child — correct
/// terminal semantics. The UI→PTY direction stays unbounded (`tx_to_pty`):
/// input volume is user-driven and the UI thread must never block on send.
pub(crate) const FROM_PTY_CHANNEL_BOUND: usize = 128;
/// Reader chunk size; also the unit for the bound above.
pub(crate) const PTY_READ_CHUNK: usize = 8192;

pub enum PtyEvent {
    Data(Vec<u8>),
    Exit,
}

struct QueuedEvent {
    event: PtyEvent,
    queued_at: Option<Instant>,
}

#[derive(Default)]
struct OutputState {
    notified: AtomicBool,
    // Includes the reader's one in-flight send when the bounded queue is full.
    bytes: AtomicUsize,
}

struct OutputSender<W> {
    tx: mpsc::SyncSender<QueuedEvent>,
    state: Arc<OutputState>,
    waker: W,
}

struct OutputReceiver {
    rx: Receiver<QueuedEvent>,
    state: Arc<OutputState>,
}

fn output_channel<W: Fn()>(waker: W) -> (OutputSender<W>, OutputReceiver) {
    let (tx, rx) = mpsc::sync_channel(FROM_PTY_CHANNEL_BOUND);
    let state = Arc::new(OutputState::default());
    (
        OutputSender {
            tx,
            state: state.clone(),
            waker,
        },
        OutputReceiver { rx, state },
    )
}

fn event_bytes(event: &PtyEvent) -> usize {
    match event {
        PtyEvent::Data(bytes) => bytes.len(),
        PtyEvent::Exit => 0,
    }
}

impl<W: Fn()> OutputSender<W> {
    fn send(&self, event: PtyEvent) -> bool {
        let bytes = event_bytes(&event);
        let queued = self.state.bytes.fetch_add(bytes, Ordering::Relaxed) + bytes;
        crate::perf::peak(crate::perf::Metric::QueuedBytes, queued);
        let queued_at = crate::perf::enabled().then(Instant::now);
        if self.tx.send(QueuedEvent { event, queued_at }).is_err() {
            self.state.bytes.fetch_sub(bytes, Ordering::Relaxed);
            return false;
        }
        if !self.state.notified.swap(true, Ordering::AcqRel) {
            (self.waker)();
        } else {
            crate::perf::add(crate::perf::Metric::CoalescedWake, 1);
        }
        true
    }
}

impl OutputReceiver {
    fn pending(&self) -> bool {
        self.state.notified.load(Ordering::Acquire)
    }

    fn try_recv(&self) -> Option<PtyEvent> {
        self.try_recv_rearm(|| {})
    }

    fn try_recv_rearm(&self, before_rearm: impl FnOnce()) -> Option<PtyEvent> {
        let queued = self.rx.try_recv().ok().or_else(|| {
            before_rearm();
            self.state.notified.store(false, Ordering::Release);
            // Close the empty-observation/re-arm race: a send between the
            // first empty read and the clear may have suppressed its wake.
            // After this second read, a new send necessarily wakes us.
            let queued = self.rx.try_recv().ok()?;
            self.state.notified.store(true, Ordering::Release);
            Some(queued)
        })?;
        self.state
            .bytes
            .fetch_sub(event_bytes(&queued.event), Ordering::Relaxed);
        if let Some(start) = queued.queued_at {
            crate::perf::duration(crate::perf::Metric::QueueWait, start.elapsed());
        }
        Some(queued.event)
    }
}

pub struct PtySession {
    pub tx_to_pty: Sender<Vec<u8>>,
    output: OutputReceiver,
    master: Box<dyn MasterPty + Send>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Pid of the spawned shell (the session/group leader), the query
    /// target for the `$cwd` label fallback.
    shell_pid: Option<i32>,
    min_dim: usize,
    max_dim: usize,
}

impl PtySession {
    pub fn spawn_with_size(
        cols: usize,
        rows: usize,
        waker: impl Fn() + Send + 'static,
        config: &Config,
    ) -> anyhow::Result<Self> {
        let min_dim = config.terminal.min_dim.max(1);
        let max_dim = config.terminal.max_dim.max(min_dim);
        let pty_system = native_pty_system();
        let size = PtySize {
            rows: rows.clamp(min_dim, max_dim) as u16,
            cols: cols.clamp(min_dim, max_dim) as u16,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = pty_system.openpty(size)?;

        let shell = configured_shell(&config.shell);
        let (program, shell_args) = split_shell_command(&shell);
        let term = if config.shell.term.is_empty() {
            "xterm-256color".to_string()
        } else {
            config.shell.term.clone()
        };
        let mut cmd = CommandBuilder::new(program.clone());
        cmd.args(shell_args.clone());
        cmd.env("TERM", &term);
        cmd.cwd(configured_cwd(&config.shell));

        let child = match pair.slave.spawn_command(cmd) {
            Ok(child) => child,
            Err(e) => {
                // A bad `$SHELL` (missing binary, bad args) must not kill
                // the app: fall back to a plain shell and say so in the
                // log panel. Only the total failure below still errors.
                log::warn!("failed to spawn {program:?}: {e:#}; falling back to /bin/bash");
                let mut fallback = CommandBuilder::new("/bin/bash");
                fallback.env("TERM", &term);
                fallback.cwd(configured_cwd(&config.shell));
                pair.slave.spawn_command(fallback).map_err(|e2| {
                    anyhow::anyhow!(
                        "failed to spawn {program:?}: {e:#} (fallback also failed: {e2:#})"
                    )
                })?
            }
        };
        let shell_pid = child.process_id().and_then(|pid| i32::try_from(pid).ok());
        log::debug!(
            "pty spawned: {program} at {}x{} (term={term})",
            size.cols,
            size.rows
        );

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        // PTY→UI is bounded (see `FROM_PTY_CHANNEL_BOUND`): the `send`
        // below blocks when the UI falls behind, pushing backpressure to
        // the child. UI→PTY stays unbounded: input is user-driven and the
        // UI thread must never block.
        let (tx_from_pty, output) = output_channel(waker);
        let (tx_to_pty, rx_to_pty) = mpsc::channel::<Vec<u8>>();

        // Detached threads: they exit on their own when the PTY closes or the
        // session's channels are dropped.
        std::thread::spawn(move || {
            let mut buf = [0u8; PTY_READ_CHUNK];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        tx_from_pty.send(PtyEvent::Exit);
                        break;
                    }
                    Ok(n) => {
                        if !tx_from_pty.send(PtyEvent::Data(buf[..n].to_vec())) {
                            break;
                        }
                    }
                    Err(_) => {
                        tx_from_pty.send(PtyEvent::Exit);
                        break;
                    }
                }
            }
        });

        std::thread::spawn(move || {
            let mut w = writer;
            while let Ok(data) = rx_to_pty.recv() {
                if w.write_all(&data).is_err() {
                    break;
                }
                let _ = w.flush();
            }
        });

        Ok(Self {
            tx_to_pty,
            output,
            master: pair.master,
            _child: child,
            shell_pid,
            min_dim,
            max_dim,
        })
    }

    pub fn write(&self, data: Vec<u8>) {
        let _ = self.tx_to_pty.send(data);
    }

    pub fn try_recv(&self) -> Option<PtyEvent> {
        self.output.try_recv()
    }

    pub(crate) fn output_pending(&self) -> bool {
        self.output.pending()
    }

    /// Kernel-reported foreground process group of the PTY (`tcgetpgrp`
    /// on the master), used to resolve `$command` / `$cwd` tab labels.
    /// `None` off unix or before a job takes control.
    pub fn foreground_pgid(&self) -> Option<i32> {
        #[cfg(unix)]
        {
            self.master.process_group_leader()
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    /// Pid of the shell cometty spawned (the PTY session/group leader),
    /// used as the `$cwd` label fallback so a foreground job's transient
    /// `chdir`s (`ls -R`, `find`) never move the label.
    pub fn shell_pid(&self) -> Option<i32> {
        self.shell_pid
    }

    pub fn resize(&self, cols: usize, rows: usize) {
        let _ = self.master.resize(portable_pty::PtySize {
            rows: rows.clamp(self.min_dim, self.max_dim) as u16,
            cols: cols.clamp(self.min_dim, self.max_dim) as u16,
            pixel_width: 0,
            pixel_height: 0,
        });
    }
}

fn configured_shell(shell_config: &crate::config::ShellConfig) -> String {
    if !shell_config.shell.trim().is_empty() {
        return shell_config.shell.clone();
    }
    default_shell()
}

/// Split a configured shell string (`[shell] shell` or `$SHELL`) into
/// `(program, args)`: `"/bin/bash --norc"` spawns `/bin/bash` with
/// `--norc` instead of failing to find a binary with that whole name.
/// Single/double quotes group (`"/my dir/sh" -l`), backslash escapes the
/// next char; there is no other expansion. An empty result falls back to
/// the platform default so spawn never gets an empty program.
pub(crate) fn split_shell_command(shell: &str) -> (String, Vec<String>) {
    let mut args: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = shell.chars();
    let mut in_word = false;
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else if c == '\\' && q == '"' {
                match chars.next() {
                    Some(n) => cur.push(n),
                    None => cur.push(c),
                }
            } else {
                cur.push(c);
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                in_word = true;
            }
            '\\' => match chars.next() {
                Some(n) => {
                    cur.push(n);
                    in_word = true;
                }
                None => {
                    cur.push(c);
                    in_word = true;
                }
            },
            c if c.is_whitespace() => {
                if in_word {
                    args.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            _ => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        args.push(cur);
    }
    if args.is_empty() {
        return (default_shell(), Vec::new());
    }
    let program = args.remove(0);
    (program, args)
}

fn configured_cwd(shell_config: &crate::config::ShellConfig) -> String {
    if !shell_config.cwd.trim().is_empty() {
        return shell_config.cwd.clone();
    }
    std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
}

pub fn default_shell() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    }
    #[cfg(not(windows))]
    {
        if let Ok(s) = std::env::var("SHELL")
            && !s.is_empty()
        {
            return s;
        }
        "/bin/bash".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_notifications_coalesce_until_empty_and_rearm_for_exit() {
        let wakes = AtomicUsize::new(0);
        let (tx, rx) = output_channel(|| {
            wakes.fetch_add(1, Ordering::Relaxed);
        });
        for n in 0..FROM_PTY_CHANNEL_BOUND {
            assert!(tx.send(PtyEvent::Data(vec![n as u8])));
        }
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        for n in 0..FROM_PTY_CHANNEL_BOUND {
            assert!(rx.pending());
            assert!(matches!(rx.try_recv(), Some(PtyEvent::Data(bytes)) if bytes == [n as u8]));
        }
        assert!(rx.try_recv().is_none());
        assert!(!rx.pending());
        assert_eq!(rx.state.bytes.load(Ordering::Relaxed), 0);
        assert!(tx.send(PtyEvent::Exit));
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
        assert!(matches!(rx.try_recv(), Some(PtyEvent::Exit)));
        assert!(rx.try_recv().is_none());
        drop(rx);
        assert!(!tx.send(PtyEvent::Data(vec![1])));
        assert_eq!(tx.state.bytes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn send_during_empty_rearm_is_not_lost() {
        let wakes = AtomicUsize::new(0);
        let (tx, rx) = output_channel(|| {
            wakes.fetch_add(1, Ordering::Relaxed);
        });
        assert!(tx.send(PtyEvent::Data(vec![1])));
        assert!(rx.try_recv().is_some());
        let event = rx.try_recv_rearm(|| {
            // The reader sees the OLD outstanding notification, so it
            // intentionally sends no wake in this adversarial interleaving.
            assert!(tx.send(PtyEvent::Data(vec![2])));
            assert_eq!(wakes.load(Ordering::Relaxed), 1);
        });
        assert!(matches!(event, Some(PtyEvent::Data(bytes)) if bytes == [2]));
        assert!(rx.pending());
        assert!(rx.try_recv().is_none());
        assert!(!rx.pending());
        assert!(tx.send(PtyEvent::Exit));
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn threaded_output_preserves_order_without_lost_wakes() {
        let (wake_tx, wake_rx) = mpsc::channel();
        let (tx, rx) = output_channel(move || {
            let _ = wake_tx.send(());
        });
        let producer = std::thread::spawn(move || {
            for n in 0_u32..20_000 {
                assert!(tx.send(PtyEvent::Data(n.to_le_bytes().to_vec())));
            }
            assert!(tx.send(PtyEvent::Exit));
        });
        let mut next = 0_u32;
        'receive: loop {
            wake_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("lost output notification");
            while let Some(event) = rx.try_recv() {
                match event {
                    PtyEvent::Data(bytes) => {
                        assert_eq!(bytes, next.to_le_bytes());
                        next += 1;
                    }
                    PtyEvent::Exit => break 'receive,
                }
            }
        }
        producer.join().unwrap();
        assert_eq!(next, 20_000);
        assert_eq!(rx.state.bytes.load(Ordering::Relaxed), 0);
    }

    #[test]
    #[ignore = "manual release scheduling probe; not GUI/input-to-visible latency"]
    fn fair_drain_under_load() {
        use crate::app::scheduling::DrainSchedule;
        use crate::term::Terminal;
        use crate::theme::Theme;

        let wake = || {};
        let (flood_tx, flood_rx) = output_channel(wake);
        let (_idle_tx, idle_rx) = output_channel(wake);
        let (echo_tx, echo_rx) = output_channel(wake);
        let (exit_tx, exit_rx) = output_channel(wake);
        let producer =
            std::thread::spawn(
                move || {
                    while flood_tx.send(PtyEvent::Data(vec![b'a'; PTY_READ_CHUNK])) {}
                },
            );
        let (request_tx, request_rx) = mpsc::channel::<u32>();
        let echo = std::thread::spawn(move || {
            while let Ok(seq) = request_rx.recv() {
                if !echo_tx.send(PtyEvent::Data(format!("probe-{seq:08}\r\n").into_bytes())) {
                    break;
                }
            }
        });
        assert!(exit_tx.send(PtyEvent::Data(b"last output\r\n".to_vec())));
        assert!(exit_tx.send(PtyEvent::Exit));
        let queues = [flood_rx, idle_rx, echo_rx, exit_rx];
        let mut terminals: Vec<_> = (0..4)
            .map(|_| Terminal::new(80, 24, Theme::default()))
            .collect();
        let mut schedule = DrainSchedule::default();
        let mut exited = false;
        let mut samples = Vec::new();
        let mut flood_bytes = 0;
        for seq in 0..136 {
            let start = Instant::now();
            request_tx.send(seq).unwrap();
            let expected = format!("probe-{seq:08}\r\n").into_bytes();
            let mut received = false;
            while !received {
                assert!(
                    start.elapsed() < std::time::Duration::from_secs(5),
                    "active queue starved"
                );
                schedule.begin_turn();
                let turn = Instant::now();
                let mut empty = 0;
                while !schedule.exhausted(turn.elapsed()) && empty < queues.len() * 2 {
                    let i = schedule.next_tab(queues.len(), 2).unwrap();
                    match queues[i].try_recv() {
                        Some(PtyEvent::Data(bytes)) => {
                            schedule.add_bytes(bytes.len());
                            terminals[i].feed(&bytes);
                            empty = 0;
                            if i == 0 {
                                flood_bytes += bytes.len();
                            }
                            if i == 2 {
                                assert_eq!(bytes, expected);
                                received = true;
                                break;
                            }
                        }
                        Some(PtyEvent::Exit) => {
                            assert_eq!(i, 3);
                            exited = true;
                            empty += 1;
                        }
                        None => empty += 1,
                    }
                }
                schedule.finish_call(turn.elapsed());
                std::thread::yield_now();
            }
            if seq >= 8 {
                samples.push(start.elapsed().as_micros());
            }
        }
        samples.sort_unstable();
        assert!(exited, "a later tab's exit must be serviced under load");
        println!(
            "PERF scheduling=identified_request_to_parse samples={} p50_us={} p95_us={} p99_us={} max_us={} flood_bytes={} (headless; excludes metadata/render/presentation/OS PTY)",
            samples.len(),
            samples[samples.len() / 2],
            samples[(samples.len() * 95).div_ceil(100) - 1],
            samples[(samples.len() * 99).div_ceil(100) - 1],
            samples.last().unwrap(),
            flood_bytes
        );
        drop(request_tx);
        drop(queues);
        producer.join().unwrap();
        echo.join().unwrap();
    }

    #[test]
    fn shell_split_plain_and_args() {
        assert_eq!(
            split_shell_command("/bin/bash"),
            ("/bin/bash".to_string(), vec![])
        );
        assert_eq!(
            split_shell_command("/bin/bash --norc -l"),
            (
                "/bin/bash".to_string(),
                vec!["--norc".to_string(), "-l".to_string()]
            )
        );
        // Extra whitespace collapses.
        assert_eq!(
            split_shell_command("  /bin/zsh   --login  "),
            ("/bin/zsh".to_string(), vec!["--login".to_string()])
        );
    }

    #[test]
    fn shell_split_quotes_and_escapes() {
        assert_eq!(
            split_shell_command(r#""/my dir/sh" -l"#),
            ("/my dir/sh".to_string(), vec!["-l".to_string()])
        );
        assert_eq!(
            split_shell_command("/bin/sh -c 'echo hi'"),
            (
                "/bin/sh".to_string(),
                vec!["-c".to_string(), "echo hi".to_string()]
            )
        );
        assert_eq!(
            split_shell_command(r"/my\ dir/sh"),
            ("/my dir/sh".to_string(), vec![])
        );
        // Unterminated quote takes the rest literally rather than dropping it.
        assert_eq!(
            split_shell_command("/bin/sh 'abc"),
            ("/bin/sh".to_string(), vec!["abc".to_string()])
        );
    }

    #[test]
    fn shell_split_empty_falls_back_to_default() {
        let (program, args) = split_shell_command("   ");
        assert!(!program.is_empty());
        assert!(args.is_empty());
    }

    #[test]
    fn from_pty_bound_covers_burst_budget() {
        // The drain budget (see `app::pty_io`) moves at most 256KB/frame;
        // the channel bound must hold less than a few frames' worth so a
        // `cat` burst applies backpressure instead of queueing heap.
        let queued = FROM_PTY_CHANNEL_BOUND * PTY_READ_CHUNK;
        assert!(queued <= 4 * 256 * 1024, "queued={queued}");
    }

    #[test]
    fn bounded_channel_applies_backpressure() {
        // A `sync_channel` at the production bound stops accepting past
        // capacity (blocking `send`); the reader therefore stalls instead
        // of queueing unbounded heap.
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(FROM_PTY_CHANNEL_BOUND);
        for _ in 0..FROM_PTY_CHANNEL_BOUND {
            tx.try_send(vec![0u8; PTY_READ_CHUNK]).unwrap();
        }
        assert!(tx.try_send(vec![0u8; PTY_READ_CHUNK]).is_err());
        let _ = rx.try_recv().unwrap();
        tx.try_send(vec![0u8; PTY_READ_CHUNK]).unwrap();
    }
}
