//! In-memory production transport for app tests: no shell, tty, window or GPU.
use super::*;
use std::io::Write;

#[derive(Debug)]
struct Stub;

impl MasterPty for Stub {
    fn resize(&self, _: PtySize) -> anyhow::Result<()> {
        Ok(())
    }
    fn get_size(&self) -> anyhow::Result<PtySize> {
        Ok(PtySize::default())
    }
    fn try_clone_reader(&self) -> anyhow::Result<Box<dyn Read + Send>> {
        Ok(Box::new(std::io::empty()))
    }
    fn take_writer(&self) -> anyhow::Result<Box<dyn Write + Send>> {
        Ok(Box::new(std::io::sink()))
    }
    #[cfg(unix)]
    fn process_group_leader(&self) -> Option<libc::pid_t> {
        None
    }
    #[cfg(unix)]
    fn as_raw_fd(&self) -> Option<std::os::fd::RawFd> {
        None
    }
    #[cfg(unix)]
    fn tty_name(&self) -> Option<std::path::PathBuf> {
        None
    }
}

impl portable_pty::ChildKiller for Stub {
    fn kill(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
        Box::new(Stub)
    }
}

impl portable_pty::Child for Stub {
    fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
        Ok(None)
    }
    fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
        Ok(portable_pty::ExitStatus::with_exit_code(0))
    }
    fn process_id(&self) -> Option<u32> {
        None
    }
    #[cfg(windows)]
    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        None
    }
}

pub(crate) fn session() -> (PtySession, impl Fn(PtyEvent), input::InputReceiver) {
    let (output_tx, output) = output_channel(|| {});
    let (tx_to_pty, input) = input::channel(|| {});
    let session = PtySession {
        tx_to_pty,
        output,
        master: Box::new(Stub),
        lifecycle: None,
        shell_pid: None,
        min_dim: 1,
        max_dim: 1024,
        max_cells: 1024 * 1024,
    };
    (session, move |event| assert!(output_tx.send(event)), input)
}
