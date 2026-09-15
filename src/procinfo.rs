//! Foreground-process introspection for `$command` / `$cwd` tab labels.
//!
//! Best-effort only: every function returns `None` when the platform or the
//! process state makes the answer unavailable (other-user processes, a pid
//! that just exited, Windows), and callers then fall back to terminal-
//! reported data (OSC 7) or to an empty format variable.
//!
//! Query cost is a couple of syscalls per call; the app caches results per
//! tab and only refreshes after that tab produced output (see
//! [`crate::app::tab::Tab::refresh_title`]).

/// `$HOME` with any trailing slash removed, when set and non-empty.
fn home() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let trimmed = home.trim_end_matches('/');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Rewrite a leading `$HOME` (or the whole path) to `~`.
/// Paths that merely share the prefix (`/Users/xavier` vs `/Users/x`) and
/// paths without a `$HOME` are returned unchanged.
pub fn shorten_home(path: &str) -> String {
    shorten_home_with(path, home().as_deref())
}

/// [`shorten_home`] with an explicit home (testable seam).
fn shorten_home_with(path: &str, home: Option<&str>) -> String {
    let Some(home) = home else {
        return path.to_string();
    };
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return path.to_string();
    }
    if path == home {
        return "~".to_string();
    }
    match path.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// Executable basename of `pid` (`nvim`, `cargo`, `zsh`, …).
pub fn command(pid: i32) -> Option<String> {
    platform::command(pid)
}

/// Absolute working directory of `pid`.
pub fn cwd(pid: i32) -> Option<String> {
    platform::cwd(pid)
}

/// Name of an executable path, ignoring a Linux ` (deleted)` suffix.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn exe_name(path: &str) -> Option<String> {
    let path = path.strip_suffix(" (deleted)").unwrap_or(path);
    let name = path.rsplit('/').next()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(target_os = "macos")]
mod platform {
    /// `MAXPATHLEN` bytes in the flattened `vip_path` buffer.
    const PATH_CAP: usize = 1024;

    pub fn command(pid: i32) -> Option<String> {
        let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: `buf` is a live buffer of exactly `buffersize` bytes.
        let len = unsafe {
            libc::proc_pidpath(
                pid,
                buf.as_mut_ptr().cast(),
                libc::PROC_PIDPATHINFO_MAXSIZE as u32,
            )
        };
        if len <= 0 {
            return None;
        }
        let path = std::str::from_utf8(&buf[..len as usize]).ok()?;
        super::exe_name(path)
    }

    pub fn cwd(pid: i32) -> Option<String> {
        // SAFETY: an all-zero `proc_vnodepathinfo` is a valid initial value.
        let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as i32;
        // SAFETY: `info` is a live buffer of `size` bytes for this flavor.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                (&raw mut info).cast(),
                size,
            )
        };
        if written != size {
            return None;
        }
        // `vip_path` is `[[c_char; 32]; 32]`; read the flat MAXPATHLEN bytes
        // bounded instead of trusting a NUL terminator.
        // SAFETY: the field is `PATH_CAP` contiguous `c_char`s.
        let bytes = unsafe {
            std::slice::from_raw_parts(info.pvi_cdir.vip_path.as_ptr().cast::<u8>(), PATH_CAP)
        };
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(PATH_CAP);
        let path = String::from_utf8_lossy(&bytes[..end]).trim().to_string();
        (!path.is_empty()).then_some(path)
    }
}

#[cfg(target_os = "linux")]
mod platform {
    pub fn command(pid: i32) -> Option<String> {
        let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        super::exe_name(&exe.to_string_lossy())
    }

    pub fn cwd(pid: i32) -> Option<String> {
        let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
        let path = cwd.to_string_lossy().trim().to_string();
        (!path.is_empty()).then_some(path)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    pub fn command(_pid: i32) -> Option<String> {
        None
    }

    pub fn cwd(_pid: i32) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortens_home_prefix_only() {
        assert_eq!(shorten_home_with("/Users/x/dev", Some("/Users/x")), "~/dev");
        assert_eq!(shorten_home_with("/Users/x", Some("/Users/x")), "~");
        assert_eq!(shorten_home_with("/Users/x/", Some("/Users/x/")), "~/");
        // Prefix that isn't a path boundary is untouched.
        assert_eq!(
            shorten_home_with("/Users/xavier/dev", Some("/Users/x")),
            "/Users/xavier/dev"
        );
        assert_eq!(shorten_home_with("/tmp/x", Some("/Users/x")), "/tmp/x");
        assert_eq!(shorten_home_with("/tmp/x", None), "/tmp/x");
        assert_eq!(shorten_home_with("/tmp/x", Some("")), "/tmp/x");
    }

    #[test]
    fn exe_name_takes_basename() {
        assert_eq!(exe_name("/usr/bin/python3"), Some("python3".to_string()));
        assert_eq!(exe_name("/usr/bin/foo (deleted)"), Some("foo".to_string()));
        assert_eq!(exe_name("zsh"), Some("zsh".to_string()));
        assert_eq!(exe_name("/usr/bin/"), None);
        assert_eq!(exe_name(""), None);
    }

    #[test]
    fn self_pid_is_introspectable() {
        let pid = std::process::id() as i32;
        let command = command(pid).expect("own executable name");
        assert!(!command.is_empty());
        let cwd = cwd(pid).expect("own working directory");
        assert_eq!(
            std::fs::canonicalize(&cwd).ok(),
            std::fs::canonicalize(std::env::current_dir().unwrap()).ok()
        );
    }

    #[test]
    fn missing_pid_returns_none() {
        // Pids are positive; a negative one is never valid.
        assert_eq!(command(-1), None);
        assert_eq!(cwd(-1), None);
    }
}
