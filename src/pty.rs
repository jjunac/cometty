use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, Sender};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

pub enum PtyEvent {
    Data(Vec<u8>),
    Exit,
}

pub struct PtySession {
    pub tx_to_pty: Sender<Vec<u8>>,
    pub rx_from_pty: Receiver<PtyEvent>,
    master: Box<dyn MasterPty + Send>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl PtySession {
    pub fn spawn_with_size(
        cols: usize,
        rows: usize,
        waker: impl Fn() + Send + 'static,
    ) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();
        let size = PtySize {
            rows: rows.clamp(1, 1024) as u16,
            cols: cols.clamp(1, 1024) as u16,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = pty_system.openpty(size)?;

        let shell = default_shell();
        let mut cmd = CommandBuilder::new(shell.clone());
        // Login-ish behavior: keep env TERM.
        cmd.env("TERM", "xterm-256color");
        cmd.cwd(std::env::var("HOME").unwrap_or_else(|_| "/".to_string()));

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| anyhow::anyhow!("failed to spawn {:?}: {:#}", shell, e))?;

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let (tx_from_pty, rx_from_pty) = mpsc::channel::<PtyEvent>();
        let (tx_to_pty, rx_to_pty) = mpsc::channel::<Vec<u8>>();

        // Detached threads: they exit on their own when the PTY closes or the
        // session's channels are dropped.
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        let _ = tx_from_pty.send(PtyEvent::Exit);
                        waker();
                        break;
                    }
                    Ok(n) => {
                        if tx_from_pty.send(PtyEvent::Data(buf[..n].to_vec())).is_err() {
                            break;
                        }
                        waker();
                    }
                    Err(_) => {
                        let _ = tx_from_pty.send(PtyEvent::Exit);
                        waker();
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
            rx_from_pty,
            master: pair.master,
            _child: child,
        })
    }

    pub fn write(&self, data: Vec<u8>) {
        let _ = self.tx_to_pty.send(data);
    }

    pub fn try_recv(&self) -> Option<PtyEvent> {
        self.rx_from_pty.try_recv().ok()
    }

    pub fn resize(&self, cols: usize, rows: usize) {
        let _ = self.master.resize(portable_pty::PtySize {
            rows: rows.clamp(1, 1024) as u16,
            cols: cols.clamp(1, 1024) as u16,
            pixel_width: 0,
            pixel_height: 0,
        });
    }
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

// anyhow is used for ergonomics; add it to Cargo.toml via codegen below if missing.
