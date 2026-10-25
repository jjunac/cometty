//! Real PTYs, but no display/GPU. Process-wide resource checks run in an
//! isolated test process, so parallel app tests cannot change the baseline.
use super::*;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(5);

fn until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "PTY lifecycle did not make progress"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn spawn(command: &str) -> PtySession {
    let mut config = Config::default();
    config.shell.shell = format!("/bin/sh -c '{command}'");
    PtySession::spawn_with_size(80, 24, || {}, &config).unwrap()
}

fn ready(session: &PtySession) {
    let mut output = Vec::new();
    until(|| {
        if let Some(event) = session.try_recv() {
            match event {
                PtyEvent::Data(bytes) => output.extend(bytes),
                PtyEvent::Exit => panic!("child exited before READY"),
            }
        }
        output.windows(5).any(|word| word == b"READY")
    });
}

fn assert_reaped(pid: i32) {
    // Do not send cleanup signals to cached pids: a successful implementation
    // has reaped them already, allowing the OS to reuse their numeric ids.
    // SAFETY: this only probes a specific child spawned by this test.
    assert_eq!(
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

fn close(session: PtySession) {
    let lifecycle = session.lifecycle.as_ref().unwrap().clone();
    let pid = session.shell_pid().unwrap();
    let input = session.tx_to_pty.observe();
    drop(session);
    assert!(
        lifecycle.wait(TIMEOUT),
        "workers or child survived tab close"
    );
    assert_eq!(
        input(),
        (0, 0),
        "input allocation still charged after close"
    );
    assert_reaped(pid);
}

#[test]
fn idle_raw_child_and_blocked_paste_are_cancellable() {
    for _ in 0..3 {
        let session = spawn("stty raw -echo; printf READY; exec /bin/sleep 60");
        ready(&session);
        close(session);

        let session = spawn("stty raw -echo; printf READY; exec /bin/sleep 60");
        ready(&session);
        assert!(session.paste(vec![b'x'; 4 * 1024 * 1024]));
        until(|| {
            let (bytes, queued) = session.tx_to_pty.observe()();
            bytes >= 4 * 1024 * 1024 && queued == 0
        });
        // Also exercise a second queued allocation behind the blocked one.
        assert!(session.paste(vec![b'y'; 1024 * 1024]));
        close(session);
    }
}

#[test]
fn open_session_preserves_paste_keys_and_replies_through_backpressure() {
    let session = spawn("stty raw -echo; printf READY; /bin/sleep 0.2; exec /bin/cat");
    ready(&session);
    let mut paste = b"\x1b[200~".to_vec();
    paste.extend(std::iter::repeat_n(b'x', 512 * 1024));
    paste.extend_from_slice(b"\x1b[201~");
    let mut expected = paste.clone();
    expected.extend_from_slice(b"keyreply");
    assert!(session.paste(paste));
    assert!(session.write(b"key".to_vec()));
    assert!(session.reply(b"reply".to_vec()).is_none());
    let mut output = Vec::new();
    until(|| {
        while let Some(event) = session.try_recv() {
            match event {
                PtyEvent::Data(bytes) => output.extend(bytes),
                PtyEvent::Exit => panic!("live cat unexpectedly exited"),
            }
        }
        output.len() >= expected.len()
    });
    assert_eq!(output, expected);
    until(|| session.tx_to_pty.observe()() == (0, 0));
    assert!(session.take_input_notice().is_none());
    close(session);
}

#[test]
fn full_output_channel_does_not_prevent_shutdown() {
    let session = spawn("stty raw -echo; printf READY; exec /usr/bin/yes x");
    ready(&session);
    until(|| session.output.state.chunks.load(Ordering::Relaxed) > FROM_PTY_CHANNEL_BOUND);
    close(session);
}

#[test]
fn natural_exit_is_reaped_before_tab_drop_and_preserves_final_output() {
    let session = spawn("printf final-output");
    let lifecycle = session.lifecycle.as_ref().unwrap().clone();
    let pid = session.shell_pid().unwrap();
    until(|| lifecycle.reaped());
    assert_reaped(pid);
    let mut output = Vec::new();
    let mut exited = false;
    until(|| {
        while let Some(event) = session.try_recv() {
            match event {
                PtyEvent::Data(bytes) => {
                    assert!(!exited, "Exit overtook the last PTY bytes");
                    output.extend(bytes);
                }
                PtyEvent::Exit => {
                    assert!(!exited, "duplicate Exit");
                    exited = true;
                }
            }
        }
        exited
    });
    assert_eq!(output, b"final-output");
    close(session);
}

#[test]
fn hangup_resistant_children_escalate_without_serial_close_waits() {
    let mut sessions = Vec::new();
    for _ in 0..6 {
        // Ignored SIGHUP survives exec; only the supervisor's escalation (not
        // closing the last master descriptor) can terminate this child.
        let session = spawn("trap \"\" HUP; stty raw -echo; printf READY; exec /bin/sleep 60");
        ready(&session);
        sessions.push(session);
    }
    let observers: Vec<_> = sessions
        .iter()
        .map(|session| {
            (
                session.lifecycle.as_ref().unwrap().clone(),
                session.shell_pid().unwrap(),
            )
        })
        .collect();
    let start = Instant::now();
    drop(sessions);
    // A synchronous kill on every tab would spend ~1.2s here (portable-pty's
    // grace loop); one shared, asynchronous supervisor must not serialize it.
    assert!(start.elapsed() < Duration::from_millis(500));
    for (session, pid) in observers {
        assert!(session.wait(TIMEOUT));
        assert_reaped(pid);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn descriptors() -> usize {
    std::fs::read_dir("/dev/fd").unwrap().count()
}

#[cfg(target_os = "linux")]
fn threads() -> usize {
    std::fs::read_dir("/proc/self/task").unwrap().count()
}

#[cfg(target_os = "macos")]
fn threads() -> usize {
    // SAFETY: proc_pidinfo writes at most the supplied struct size; its result
    // is checked before inspecting the initialized task-info fields.
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = size_of_val(&info) as i32;
    let result = unsafe {
        libc::proc_pidinfo(
            std::process::id() as i32,
            libc::PROC_PIDTASKINFO,
            0,
            (&mut info as *mut libc::proc_taskinfo).cast(),
            size,
        )
    };
    assert_eq!(result, size);
    info.pti_threadnum as usize
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn repeated_closes_return_os_resources_to_baseline() {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "pty::lifecycle_tests::isolated_resource_probe",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("COMETTY_PTY_RESOURCE_PROBE", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("isolated PTY resource probe hung");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "PTY resource probe failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn isolated_resource_probe() {
    if std::env::var("COMETTY_PTY_RESOURCE_PROBE").as_deref() != Ok("1") {
        return;
    }
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            finish_shutdown();
        }
    }
    let _cleanup = Cleanup;
    lifecycle::Startup::supervisor().unwrap();
    let baseline_fds = descriptors();
    let baseline_threads = threads();
    for _ in 0..3 {
        idle_raw_child_and_blocked_paste_are_cancellable();
        open_session_preserves_paste_keys_and_replies_through_backpressure();
        full_output_channel_does_not_prevent_shutdown();
        natural_exit_is_reaped_before_tab_drop_and_preserves_final_output();
        hangup_resistant_children_escalate_without_serial_close_waits();
        finish_shutdown();
        until(|| descriptors() == baseline_fds && threads() == baseline_threads);
    }
    println!(
        "PTY resources after repeated close: fds={baseline_fds}, threads={baseline_threads} (includes one supervisor); input charges=0; children reaped"
    );
}
