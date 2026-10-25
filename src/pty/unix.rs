//! Nonblocking PTY handles whose readiness waits can be cancelled together.
//!
//! Closing a descriptor in another thread does not reliably interrupt read(2)
//! or write(2). Each worker instead owns its descriptor and polls it alongside
//! a socket that becomes permanently readable/hung up on session shutdown.
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct Cancellation {
    stopped: AtomicBool,
    reader: UnixStream,
    writer: UnixStream,
}

impl Cancellation {
    pub(super) fn new() -> io::Result<Self> {
        let (reader, writer) = UnixStream::pair()?;
        Ok(Self {
            stopped: AtomicBool::new(false),
            reader,
            writer,
        })
    }

    pub(super) fn cancel(&self) {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            // No bytes to drain: both workers see the hangup, including a
            // worker that enters poll after the other one has already left.
            let _ = self.writer.shutdown(Shutdown::Both);
        }
    }

    fn check(&self) -> io::Result<()> {
        if self.stopped.load(Ordering::Acquire) {
            Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "PTY session closed",
            ))
        } else {
            Ok(())
        }
    }
}

pub(super) struct Io {
    fd: OwnedFd,
    cancellation: Arc<Cancellation>,
}

impl Io {
    pub(super) fn new(
        master: &dyn portable_pty::MasterPty,
        cancellation: Arc<Cancellation>,
    ) -> anyhow::Result<Self> {
        let fd = master
            .as_raw_fd()
            .ok_or_else(|| anyhow::anyhow!("native PTY has no pollable master descriptor"))?;
        // SAFETY: master owns this live fd throughout the duplication. The
        // worker gets its own CLOEXEC owner; it never borrows the UI's fd.
        let fd = unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?;
        // O_NONBLOCK is shared by the duplicated master handles, not by the
        // separately opened slave used for the child's standard streams.
        // SAFETY: both fcntl calls use a live owned descriptor and no pointers.
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        if flags == -1
            || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
        {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Self { fd, cancellation })
    }

    fn wait(&self, events: libc::c_short) -> io::Result<()> {
        let mut fds = [
            libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events,
                revents: 0,
            },
            libc::pollfd {
                fd: self.cancellation.reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        loop {
            self.cancellation.check()?;
            // SAFETY: fds is writable for both pollfd entries for this call.
            let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            self.cancellation.check()?;
            if fds[0].revents & libc::POLLNVAL != 0 {
                return Err(io::Error::from_raw_os_error(libc::EBADF));
            }
            if fds[0].revents & (events | libc::POLLHUP | libc::POLLERR) != 0 {
                return Ok(());
            }
        }
    }
}

impl Read for Io {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            self.cancellation.check()?;
            // SAFETY: buf is writable for its length and fd is owned/live.
            let result =
                unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if result >= 0 {
                return Ok(result as usize);
            }
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => self.wait(libc::POLLIN)?,
                // Linux reports a closed slave as EIO rather than EOF.
                _ if error.raw_os_error() == Some(libc::EIO) => return Ok(0),
                _ => return Err(error),
            }
        }
    }
}

impl Write for Io {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            self.cancellation.check()?;
            // SAFETY: buf is readable for its length and fd is owned/live.
            let result =
                unsafe { libc::write(self.fd.as_raw_fd(), buf.as_ptr().cast(), buf.len()) };
            if result >= 0 {
                return Ok(result as usize);
            }
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => self.wait(libc::POLLOUT)?,
                _ => return Err(error),
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        // PTY writes go straight to the kernel; there is no userspace buffer.
        self.cancellation.check()
    }
}
