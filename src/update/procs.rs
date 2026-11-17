//! Listing processes that run an executable from the installed bundle (macOS only; empty elsewhere).
//!
//! [`executables`] feeds `mode::count_instances`, which decides whether
//! another Cometty instance is running from the bundle the install job is about
//! to replace. Detection is best effort: processes whose path cannot be read
//! (exited, zombies, or other users' processes when `proc_pidpath` is denied)
//! are skipped rather than counted (`docs/update.md#multiple-instances`).

use std::path::PathBuf;

/// Every readable `(pid, executable path)` pair on the system.
///
/// On macOS this uses `proc_listallpids` and `proc_pidpath`. Other platforms
/// return an empty list; the install pipeline is never reached there.
pub(crate) fn executables() -> Vec<(i32, PathBuf)> {
    platform::executables()
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    /// Extra slots beyond the kernel's estimate, so processes spawned between
    /// the sizing call and the listing call usually still fit.
    const SLACK: usize = 64;
    /// Bound on regrow attempts while the process table keeps growing.
    const ATTEMPTS: usize = 4;

    pub(super) fn executables() -> Vec<(i32, PathBuf)> {
        all_pids()
            .into_iter()
            .filter(|&pid| pid > 0)
            .filter_map(|pid| pid_path(pid).map(|path| (pid, path)))
            .collect()
    }

    /// All pids, read into a buffer that grows until the listing fits.
    fn all_pids() -> Vec<i32> {
        // SAFETY: a null buffer with size 0 asks only for the current count.
        let estimate = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        let mut capacity = usize::try_from(estimate).unwrap_or(0) + SLACK;
        for _ in 0..ATTEMPTS {
            let mut pids = vec![0i32; capacity];
            let Ok(bytes) = libc::c_int::try_from(capacity * size_of::<i32>()) else {
                return Vec::new();
            };
            // SAFETY: `pids` is a live, writable buffer of exactly `bytes` bytes.
            let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
            let Ok(count) = usize::try_from(count) else {
                return Vec::new();
            };
            if count < capacity {
                pids.truncate(count);
                return pids;
            }
            // A full buffer may have truncated the listing; grow and retry.
            capacity *= 2;
        }
        Vec::new()
    }

    /// The executable path of `pid`, or `None` when it cannot be read.
    fn pid_path(pid: i32) -> Option<PathBuf> {
        let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: `buf` is a live, writable buffer of exactly `buffersize` bytes.
        let len = unsafe {
            libc::proc_pidpath(
                pid,
                buf.as_mut_ptr().cast(),
                libc::PROC_PIDPATHINFO_MAXSIZE as u32,
            )
        };
        let len = usize::try_from(len).ok().filter(|&len| len > 0)?;
        let bytes = buf.get(..len)?;
        Some(PathBuf::from(OsStr::from_bytes(bytes)))
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::path::PathBuf;

    pub(super) fn executables() -> Vec<(i32, PathBuf)> {
        Vec::new()
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn lists_the_current_process_and_its_executable() {
        let pid = i32::try_from(std::process::id()).expect("pid fits in i32");
        let exe = std::env::current_exe().expect("current_exe");
        let exe = exe.canonicalize().unwrap_or(exe);
        let list = executables();
        let (_, path) = list
            .iter()
            .find(|(p, _)| *p == pid)
            .expect("own pid is listed");
        let path = path.canonicalize().unwrap_or_else(|_| path.clone());
        assert_eq!(path, exe);
    }

    #[test]
    fn skips_non_positive_pids() {
        assert!(executables().iter().all(|(pid, _)| *pid > 0));
    }
}
