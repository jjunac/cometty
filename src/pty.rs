use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::config::Config;
pub(crate) mod input;
mod lifecycle;
#[cfg(unix)]
mod unix;

pub(crate) fn finish_shutdown() {
    lifecycle::finish_shutdown();
}

#[cfg(test)]
#[path = "pty_test_support.rs"]
pub(crate) mod test_support;

#[cfg(all(test, unix))]
#[path = "pty/lifecycle_tests.rs"]
mod lifecycle_tests;

/// Pending PTY-output chunks buffered between the reader thread and the UI
/// drain. The reader blocks past this (~1MB at 8KB chunks), which applies
/// backpressure through the kernel PTY buffer to the child — correct
/// terminal semantics. UI→PTY admission is separately byte-bounded and
/// nonblocking, with explicit rejection rather than silent input loss.
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
    #[cfg(test)]
    chunks: AtomicUsize,
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
        #[cfg(test)]
        self.state.chunks.fetch_add(1, Ordering::Relaxed);
        if self.tx.send(QueuedEvent { event, queued_at }).is_err() {
            self.state.bytes.fetch_sub(bytes, Ordering::Relaxed);
            #[cfg(test)]
            self.state.chunks.fetch_sub(1, Ordering::Relaxed);
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
        #[cfg(test)]
        self.state.chunks.fetch_sub(1, Ordering::Relaxed);
        if let Some(start) = queued.queued_at {
            crate::perf::duration(crate::perf::Metric::QueueWait, start.elapsed());
        }
        Some(queued.event)
    }
}

pub struct PtySession {
    tx_to_pty: input::InputSender,
    output: OutputReceiver,
    master: Box<dyn MasterPty + Send>,
    lifecycle: Option<lifecycle::Session>,
    /// Pid of the spawned shell (the session/group leader), the query
    /// target for the `$cwd` label fallback.
    shell_pid: Option<i32>,
    min_dim: usize,
    max_dim: usize,
    max_cells: usize,
}

impl PtySession {
    /// Spawn the configured shell. `cwd` (a restored tab's directory)
    /// overrides `[shell] cwd` for this session only.
    pub fn spawn_with_size(
        cols: usize,
        rows: usize,
        waker: impl Fn() + Send + Sync + 'static,
        config: &Config,
        cwd: Option<&str>,
    ) -> anyhow::Result<Self> {
        let min_dim = config.terminal.min_dim.max(1);
        let max_dim = config.terminal.max_dim.max(min_dim);
        let (cols, rows) = crate::app::geometry::clamp_grid_size(
            cols,
            rows,
            min_dim,
            max_dim,
            config.terminal.max_cells,
        );
        let pty_system = native_pty_system();
        let size = PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = pty_system.openpty(size)?;
        // Do fallible transport setup before starting a child. Once spawned,
        // the startup guard owns cleanup even if a worker cannot be created.
        lifecycle::Startup::supervisor()?;
        #[cfg(unix)]
        let cancellation = Arc::new(unix::Cancellation::new()?);
        #[cfg(unix)]
        let mut reader = unix::Io::new(&*pair.master, cancellation.clone())?;
        #[cfg(unix)]
        let writer = unix::Io::new(&*pair.master, cancellation.clone())?;
        #[cfg(not(unix))]
        let mut reader = pair.master.try_clone_reader()?;
        #[cfg(not(unix))]
        let writer = pair.master.take_writer()?;

        // A Finder-launched bundle inherits launchd's minimal PATH; the
        // default shell therefore starts as a login shell so it reads
        // /etc/zprofile (path_helper) and the user's profile. An explicit
        // [shell] shell is used verbatim.
        let login_default =
            config.shell.shell.trim().is_empty() && crate::platform::launched_from_app_bundle();
        let (program, shell_args) = configured_shell(&config.shell, login_default);
        let term = if config.shell.term.is_empty() {
            "xterm-256color".to_string()
        } else {
            config.shell.term.clone()
        };
        let mut cmd = CommandBuilder::new(program.clone());
        cmd.args(shell_args.clone());
        apply_terminal_env(&mut cmd, &term);
        let cwd = cwd.map_or_else(|| configured_cwd(&config.shell), str::to_string);
        cmd.cwd(&cwd);

        let child = match pair.slave.spawn_command(cmd) {
            Ok(child) => child,
            Err(e) => {
                // A bad `$SHELL` (missing binary, bad args) must not kill
                // the app: fall back to a plain shell and say so in the
                // log panel. Only the total failure below still errors.
                log::warn!("failed to spawn {program:?}: {e:#}; falling back to /bin/bash");
                let mut fallback = CommandBuilder::new("/bin/bash");
                if login_default {
                    fallback.arg("-l");
                }
                apply_terminal_env(&mut fallback, &term);
                fallback.cwd(&cwd);
                pair.slave.spawn_command(fallback).map_err(|e2| {
                    anyhow::anyhow!(
                        "failed to spawn {program:?}: {e:#} (fallback also failed: {e2:#})"
                    )
                })?
            }
        };
        let shell_pid = child.process_id().and_then(|pid| i32::try_from(pid).ok());
        let mut startup = lifecycle::Startup::new(
            child,
            #[cfg(unix)]
            cancellation,
        );
        // Retaining a slave in the parent prevents the reader seeing EOF.
        drop(pair.slave);
        log::debug!(
            "pty spawned: {program} at {}x{} (term={term})",
            size.cols,
            size.rows
        );

        // PTY→UI is bounded (see `FROM_PTY_CHANNEL_BOUND`): the `send`
        // below blocks when the UI falls behind, pushing backpressure to
        // the child. The opposite direction uses nonblocking admission.
        let waker = Arc::new(waker);
        let output_wake = waker.clone();
        let (tx_from_pty, output) = output_channel(move || output_wake());
        let (tx_to_pty, rx_to_pty) = input::channel(move || waker());

        startup.spawn_worker("pty-reader", move || {
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
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                        continue;
                    }
                    Err(_) => {
                        tx_from_pty.send(PtyEvent::Exit);
                        break;
                    }
                }
            }
        })?;

        startup.spawn_worker("pty-writer", move || rx_to_pty.run(writer))?;
        let lifecycle = Some(startup.activate());

        Ok(Self {
            tx_to_pty,
            output,
            master: pair.master,
            lifecycle,
            shell_pid,
            min_dim,
            max_dim,
            max_cells: config.terminal.max_cells,
        })
    }

    pub fn write(&self, data: Vec<u8>) -> bool {
        self.tx_to_pty.send(data, false).is_ok()
    }

    pub fn paste(&self, data: Vec<u8>) -> bool {
        self.tx_to_pty.send(data, true).is_ok()
    }

    pub fn reject_large_paste(&self) {
        self.tx_to_pty.reject_large_paste();
    }

    pub fn take_input_notice(&self) -> Option<String> {
        self.tx_to_pty.take_notice()
    }

    pub fn reply(&self, data: Vec<u8>) -> Option<Vec<u8>> {
        self.tx_to_pty.reply(data)
    }

    pub fn reply_blocked(&self) -> bool {
        self.tx_to_pty.reply_blocked()
    }

    pub fn apply_terminal_config(&mut self, config: &crate::config::TerminalConfig) {
        self.min_dim = config.min_dim;
        self.max_dim = config.max_dim;
        self.max_cells = config.max_cells;
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
        let (cols, rows) = crate::app::geometry::clamp_grid_size(
            cols,
            rows,
            self.min_dim,
            self.max_dim,
            self.max_cells,
        );
        let _ = self.master.resize(portable_pty::PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: 0,
            pixel_height: 0,
        });
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.tx_to_pty.cancel();
        if let Some(lifecycle) = &self.lifecycle {
            lifecycle.shutdown(self.foreground_pgid());
        }
        // Dropping output disconnects even a reader blocked on a full send.
        // Worker-owned descriptors are closed by those workers, never raced
        // against a blocking syscall from the UI thread.
    }
}

/// Resolve the shell program and arguments.
///
/// An explicit `[shell] shell` is split and used verbatim. Otherwise the
/// platform default shell (`$SHELL` or a fallback) is split the same way;
/// when cometty was launched from its macOS app bundle, `login_default` adds
/// `-l` so the shell reads login profiles (including `/etc/zprofile` →
/// `path_helper`) instead of inheriting launchd's minimal PATH.
fn configured_shell(
    shell_config: &crate::config::ShellConfig,
    login_default: bool,
) -> (String, Vec<String>) {
    let explicit = shell_config.shell.trim();
    if !explicit.is_empty() {
        return split_shell_command(explicit);
    }

    let (program, mut args) = split_shell_command(&default_shell());
    if login_default && !args.iter().any(|arg| arg == "-l" || arg == "--login") {
        args.push("-l".to_string());
    }
    (program, args)
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

/// Identity variables of the terminal cometty itself was launched from.
/// Inherited, they make shells act as that terminal's session: with
/// `TERM_PROGRAM=Apple_Terminal`, macOS zsh sources
/// `/etc/zshrc_Apple_Terminal`, and every tab shares the parent tab's
/// `TERM_SESSION_ID` session/history files (racing to restore and delete
/// them when restored tabs start together).
const INHERITED_TERMINAL_ENV: &[&str] = &[
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERM_SESSION_ID",
    "ITERM_SESSION_ID",
    "ITERM_PROFILE",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "ALACRITTY_WINDOW_ID",
    "WT_SESSION",
    "VTE_VERSION",
    "TMUX",
    "TMUX_PANE",
    "STY",
];

/// Advertise cometty (not the launching terminal) to the child.
fn apply_terminal_env(cmd: &mut CommandBuilder, term: &str) {
    for key in INHERITED_TERMINAL_ENV {
        cmd.env_remove(key);
    }
    cmd.env("TERM", term);
    cmd.env("TERM_PROGRAM", "cometty");
    cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
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
    fn child_env_drops_the_launching_terminal_identity() {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.env("TERM_PROGRAM", "Apple_Terminal");
        cmd.env("TERM_SESSION_ID", "52CD32D0-75EB-45E6-BF7C-3F70713180D2");
        cmd.env("TMUX", "/tmp/tmux-501/default,1,0");
        cmd.env("HOME", "/home/u");
        apply_terminal_env(&mut cmd, "xterm-256color");
        assert_eq!(cmd.get_env("TERM_PROGRAM"), Some("cometty".as_ref()));
        assert_eq!(cmd.get_env("TERM"), Some("xterm-256color".as_ref()));
        assert_eq!(cmd.get_env("TERM_SESSION_ID"), None);
        assert_eq!(cmd.get_env("TMUX"), None);
        assert_eq!(cmd.get_env("HOME"), Some("/home/u".as_ref()));
    }

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
    fn default_shell_is_login_only_for_bundled_launch() {
        let config = crate::config::ShellConfig::default();
        let (expected_program, expected_args) = split_shell_command(&default_shell());

        let (program, args) = configured_shell(&config, false);
        assert_eq!(program, expected_program);
        assert_eq!(args, expected_args);

        let (program, args) = configured_shell(&config, true);
        assert_eq!(program, expected_program);
        let mut expected_login_args = expected_args;
        if !expected_login_args
            .iter()
            .any(|arg| arg == "-l" || arg == "--login")
        {
            expected_login_args.push("-l".to_string());
        }
        assert_eq!(args, expected_login_args);
    }

    #[test]
    fn explicit_shell_ignores_bundle_login_default() {
        let config = crate::config::ShellConfig {
            shell: "/bin/zsh --norc".to_string(),
            ..Default::default()
        };
        assert_eq!(
            configured_shell(&config, true),
            ("/bin/zsh".to_string(), vec!["--norc".to_string()])
        );
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
