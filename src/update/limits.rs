//! Request kinds, size/time limits, and the capped body copy loop.
//!
//! The limits table is the HTTP contract (`docs/update.md#http-limits`);
//! `http.rs` only translates it into agent configuration, so it stays pure
//! and testable.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Largest release feed body accepted.
pub(crate) const FEED_CAP: u64 = 1024 * 1024;
/// Largest checksum file body accepted.
pub(crate) const CHECKSUM_CAP: u64 = 1024;
/// Absolute ceiling for an archive body, whatever its advertised size.
pub(crate) const ARCHIVE_CAP: u64 = 256 * 1024 * 1024;

/// Connection timeout shared by every request kind.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Overall timeout for the small feed and checksum requests.
const SMALL_GLOBAL_TIMEOUT: Duration = Duration::from_secs(30);
/// Overall timeout for an archive download.
const ARCHIVE_GLOBAL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Read-inactivity timeout for an archive download.
const ARCHIVE_PER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Largest single read; the cancel flag is checked before each one.
const CHUNK: usize = 64 * 1024;

/// What a request fetches; selects its [`Limits`] and `Accept` header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestKind {
    Feed,
    Checksum,
    Archive,
}

/// Timeouts and the body cap for one [`RequestKind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits {
    pub connect: Duration,
    pub global: Duration,
    /// Read-inactivity timeout; `None` relies on the global timeout alone.
    pub per_read: Option<Duration>,
    /// Absolute body ceiling. For archives, narrow it with [`archive_cap`].
    pub body_cap: u64,
}

/// The `docs/update.md#http-limits` table (asserted by a unit test).
pub(crate) const fn limits(kind: RequestKind) -> Limits {
    match kind {
        RequestKind::Feed => Limits {
            connect: CONNECT_TIMEOUT,
            global: SMALL_GLOBAL_TIMEOUT,
            per_read: None,
            body_cap: FEED_CAP,
        },
        RequestKind::Checksum => Limits {
            connect: CONNECT_TIMEOUT,
            global: SMALL_GLOBAL_TIMEOUT,
            per_read: None,
            body_cap: CHECKSUM_CAP,
        },
        RequestKind::Archive => Limits {
            connect: CONNECT_TIMEOUT,
            global: ARCHIVE_GLOBAL_TIMEOUT,
            per_read: Some(ARCHIVE_PER_READ_TIMEOUT),
            body_cap: ARCHIVE_CAP,
        },
    }
}

/// The body cap for an archive whose release metadata advertises `advertised`
/// bytes: `min(advertised, ARCHIVE_CAP)`.
pub(crate) const fn archive_cap(advertised: u64) -> u64 {
    if advertised < ARCHIVE_CAP {
        advertised
    } else {
        ARCHIVE_CAP
    }
}

/// Why [`copy_capped`] stopped before reaching end of input.
#[derive(Debug)]
pub(crate) enum CopyError {
    /// More than `cap` bytes arrived; the overflowing chunk was not written.
    Overflow,
    /// The cancel flag was set at a chunk boundary.
    Cancelled,
    /// Reading or writing failed.
    Io(io::Error),
}

/// Copies `r` into `w` until end of input, refusing more than `cap` bytes.
///
/// Reads at most 64 KiB at a time and checks `cancel` before each read, so a
/// flag set beforehand reads nothing. Returns `Ok(len)` when the whole input
/// fits in `cap`, `Overflow` once more than `cap` bytes arrive (having written
/// at most `cap`), and `Cancelled` when the flag is seen. A read already in
/// progress is not interrupted; its own timeout bounds it.
pub(crate) fn copy_capped(
    r: &mut impl Read,
    w: &mut impl Write,
    cap: u64,
    cancel: &AtomicBool,
) -> Result<u64, CopyError> {
    let mut buf = vec![0u8; CHUNK];
    let mut written: u64 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(CopyError::Cancelled);
        }
        // Ask for one byte past the remaining allowance so an overflow is
        // detected without buffering more than a chunk.
        let want = (cap - written).saturating_add(1);
        let len = usize::try_from(want).map_or(CHUNK, |want| want.min(CHUNK));
        let n = match r.read(&mut buf[..len]) {
            Ok(0) => return Ok(written),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(CopyError::Io(e)),
        };
        if n as u64 > cap - written {
            return Err(CopyError::Overflow);
        }
        w.write_all(&buf[..n]).map_err(CopyError::Io)?;
        written += n as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    /// A reader that fails the test if it is ever read.
    struct Untouchable;

    impl Read for Untouchable {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("copy_capped read after cancellation");
        }
    }

    /// Returns at most `step` bytes per read to exercise chunk boundaries.
    struct Trickle<'a> {
        data: &'a [u8],
        step: usize,
    }

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.step.min(buf.len()).min(self.data.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }

    fn copy(data: &[u8], cap: u64, step: usize) -> (Result<u64, CopyError>, Vec<u8>) {
        let mut out = Vec::new();
        let result = copy_capped(
            &mut Trickle { data, step },
            &mut out,
            cap,
            &AtomicBool::new(false),
        );
        (result, out)
    }

    #[test]
    fn limits_match_the_documented_table() {
        let secs = Duration::from_secs;
        assert_eq!(
            limits(RequestKind::Feed),
            Limits {
                connect: secs(10),
                global: secs(30),
                per_read: None,
                body_cap: MIB,
            }
        );
        assert_eq!(
            limits(RequestKind::Checksum),
            Limits {
                connect: secs(10),
                global: secs(30),
                per_read: None,
                body_cap: 1024,
            }
        );
        assert_eq!(
            limits(RequestKind::Archive),
            Limits {
                connect: secs(10),
                global: secs(15 * 60),
                per_read: Some(secs(30)),
                body_cap: 256 * MIB,
            }
        );
        assert_eq!(FEED_CAP, MIB);
        assert_eq!(CHECKSUM_CAP, 1024);
        assert_eq!(ARCHIVE_CAP, 256 * MIB);
    }

    #[test]
    fn archive_cap_is_min_of_advertised_and_ceiling() {
        assert_eq!(archive_cap(0), 0);
        assert_eq!(archive_cap(12_345), 12_345);
        assert_eq!(archive_cap(ARCHIVE_CAP), ARCHIVE_CAP);
        assert_eq!(archive_cap(ARCHIVE_CAP + 1), ARCHIVE_CAP);
        assert_eq!(archive_cap(u64::MAX), ARCHIVE_CAP);
    }

    #[test]
    fn exactly_at_cap_succeeds() {
        let data: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        for step in [1, 7, CHUNK, usize::MAX] {
            let (result, out) = copy(&data, data.len() as u64, step);
            assert_eq!(result.unwrap(), data.len() as u64, "step {step}");
            assert_eq!(out, data, "step {step}");
        }
        // Empty input under a zero cap is also exactly at the cap.
        let (result, out) = copy(&[], 0, 1);
        assert_eq!(result.unwrap(), 0);
        assert!(out.is_empty());
    }

    #[test]
    fn cap_plus_one_overflows() {
        let data: Vec<u8> = (0..200_001u32).map(|i| i as u8).collect();
        let cap = data.len() as u64 - 1;
        for step in [1, 7, CHUNK, usize::MAX] {
            let (result, out) = copy(&data, cap, step);
            assert!(
                matches!(result, Err(CopyError::Overflow)),
                "step {step}: {result:?}"
            );
            assert!(out.len() as u64 <= cap, "step {step}");
            assert_eq!(out, data[..out.len()], "step {step}");
        }
        let (result, out) = copy(b"x", 0, 1);
        assert!(matches!(result, Err(CopyError::Overflow)));
        assert!(out.is_empty());
    }

    #[test]
    fn preset_cancel_reads_nothing() {
        let mut out = Vec::new();
        let result = copy_capped(&mut Untouchable, &mut out, FEED_CAP, &AtomicBool::new(true));
        assert!(matches!(result, Err(CopyError::Cancelled)));
        assert!(out.is_empty());
    }

    #[test]
    fn io_errors_propagate() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("boom"))
            }
        }
        let result = copy_capped(&mut Broken, &mut Vec::new(), 10, &AtomicBool::new(false));
        assert!(matches!(result, Err(CopyError::Io(_))));
    }
}
