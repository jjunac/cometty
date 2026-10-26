//! Session snapshot: which tabs were open at quit, so the next launch can
//! bring them back. Pure data + file helpers; the event-loop glue lives in
//! [`crate::app::session`].
//!
//! `session.toml` stores shell working directories, tab order, the active
//! tab, and window geometry — never commands. With `restore = "scrollback"`,
//! a quit snapshot also references one owner-only VT dump per tab in the
//! sibling `scrollback/` directory; dumps exist only while referenced.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// On-disk format version; any other value is ignored (fresh start).
pub const VERSION: u32 = 1;

/// Upper bound on tabs spawned from a file, so a corrupt or hostile
/// snapshot can't fork an unbounded number of shells.
pub const MAX_TABS: usize = 64;

/// Largest scrollback dump written or replayed per tab. Far above what the
/// default history caps produce; a bigger dump is skipped, not truncated
/// mid-escape.
pub const MAX_SCROLLBACK_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    #[serde(default)]
    pub active: usize,
    /// When the snapshot was taken (Unix seconds), shown in each restored
    /// tab's banner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowState>,
    #[serde(default)]
    pub tabs: Vec<TabState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    /// Logical inner size.
    pub width: f64,
    pub height: f64,
    /// Physical outer position; absent where the platform can't report it
    /// (Wayland).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<(i32, i32)>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TabState {
    /// The shell's working directory; absent = configured cwd.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// File name of this tab's VT dump in [`scrollback_dir`]; absent = no
    /// contents to replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrollback: Option<String>,
}

/// A physical monitor rectangle: `(x, y, width, height)`.
pub type MonitorRect = (i32, i32, u32, u32);

impl Session {
    /// Snapshot form; see [`load`] for the inverse.
    pub fn new(active: usize, window: Option<WindowState>, tabs: Vec<TabState>) -> Self {
        let saved_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());
        Self {
            version: VERSION,
            active,
            saved_at,
            window,
            tabs,
        }
    }

    /// Block fed to each restored tab's terminal (not the shell), after any
    /// replayed scrollback and before the shell's first output: the date in
    /// bold accent (ANSI cyan, so it follows the theme palette) between
    /// `cols`-wide rules, plus a plain-accent opt-out hint when scrollback
    /// was replayed.
    pub fn banner(&self, cols: usize, scrollback: bool) -> String {
        let rule = "─".repeat(cols.clamp(1, 512));
        let label = match self.saved_at.and_then(format_local_time) {
            Some(date) => format!("Session restored: {date}"),
            None => "Session restored".to_string(),
        };
        const ACCENT: &str = "\x1b[0;36m";
        const STRONG: &str = "\x1b[0;1;36m";
        let hint = if scrollback {
            format!(
                "{ACCENT}To stop restoring scrollback: Settings › Session, or \
                 [session] restore = \"tabs\"\r\n"
            )
        } else {
            String::new()
        };
        format!("{STRONG}{rule}\r\n{label}\r\n{hint}{STRONG}{rule}\x1b[0m\r\n")
    }

    /// Drop what can't be restored as-is: vanished directories fall back
    /// to the configured cwd (the tab itself is kept), degenerate window
    /// sizes are dropped, `active` is clamped. `None` when no tab is left.
    fn sanitized(mut self, is_dir: impl Fn(&Path) -> bool) -> Option<Self> {
        self.tabs.truncate(MAX_TABS);
        if self.tabs.is_empty() {
            return None;
        }
        for tab in &mut self.tabs {
            if tab
                .cwd
                .as_deref()
                .is_some_and(|cwd| !is_dir(Path::new(cwd)))
            {
                tab.cwd = None;
            }
            // Only a bare name inside `scrollback/`: a hostile snapshot
            // must not point the replay at an arbitrary file.
            if tab
                .scrollback
                .as_deref()
                .is_some_and(|name| !valid_scrollback_name(name))
            {
                tab.scrollback = None;
            }
        }
        self.active = self.active.min(self.tabs.len() - 1);
        if self.window.as_ref().is_some_and(|w| {
            !(w.width.is_finite() && w.width >= 1.0 && w.height.is_finite() && w.height >= 1.0)
        }) {
            self.window = None;
        }
        Some(self)
    }
}

/// `date(1)`-style local time, e.g. `Mon Oct 26 14:03:12 CET 2026`.
#[cfg(unix)]
fn format_local_time(secs: u64) -> Option<String> {
    let time = libc::time_t::try_from(secs).ok()?;
    // SAFETY: an all-zero `tm` is a valid out-parameter.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` (which loads the zone itself) writes into the
    // live `tm` from a valid `time_t`.
    if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
        return None;
    }
    let mut buf = [0u8; 64];
    // SAFETY: `buf` is writable for its full length, the format is
    // NUL-terminated, and `tm` was filled by `localtime_r`.
    let len = unsafe {
        libc::strftime(
            buf.as_mut_ptr().cast(),
            buf.len(),
            c"%a %b %e %H:%M:%S %Z %Y".as_ptr(),
            &tm,
        )
    };
    (len > 0).then(|| String::from_utf8_lossy(&buf[..len]).into_owned())
}

#[cfg(not(unix))]
fn format_local_time(_secs: u64) -> Option<String> {
    None
}

/// `${XDG_STATE_HOME:-$HOME/.local/state}/cometty/session.toml`.
pub fn default_path() -> Option<PathBuf> {
    path_from(
        std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

fn path_from(state_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    // XDG: relative values are invalid and must be ignored.
    let base = state_home
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|h| h.join(".local/state")))?;
    Some(base.join("cometty/session.toml"))
}

/// Take the per-user session lock next to `path`. Only the holder may read
/// or write the snapshot: a second concurrent instance starts fresh instead
/// of duplicating (or later clobbering) the first one's tabs. The lock is
/// released when the returned file is dropped or the process exits.
pub fn acquire_lock(path: &Path) -> Option<File> {
    let lock_path = path.with_extension("lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .ok()?;
    file.try_lock().ok()?;
    Some(file)
}

/// Read a snapshot. A missing file is silent; unreadable or unsupported
/// ones are logged and treated as a fresh start.
pub fn load(path: &Path) -> Option<Session> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            log::warn!(
                "unreadable session {} ({e}); starting fresh",
                path.display()
            );
            return None;
        }
    };
    parse(&text, |p| p.is_dir()).unwrap_or_else(|e| {
        log::warn!("ignoring session {} ({e}); starting fresh", path.display());
        None
    })
}

fn parse(text: &str, is_dir: impl Fn(&Path) -> bool) -> Result<Option<Session>, String> {
    let session: Session = toml::from_str(text).map_err(|e| e.to_string())?;
    if session.version != VERSION {
        return Err(format!("unsupported version {}", session.version));
    }
    Ok(session.sanitized(is_dir))
}

/// The dump directory next to the snapshot at `path`.
pub fn scrollback_dir(path: &Path) -> PathBuf {
    path.with_file_name("scrollback")
}

/// A fresh dump name for tab `index`. Unique per snapshot, so a new quit
/// never overwrites a dump the committed snapshot still references.
pub fn scrollback_name(index: usize) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{nanos}-{index}.vt")
}

fn valid_scrollback_name(name: &str) -> bool {
    name.len() <= 64
        && !name.starts_with('.')
        && name.ends_with(".vt")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Read the dump `name` referenced by the snapshot at `path`. Missing is
/// silent; unreadable or oversized dumps are logged and skipped (the tab
/// still opens).
pub fn load_scrollback(path: &Path, name: &str) -> Option<Vec<u8>> {
    let file = scrollback_dir(path).join(name);
    let read = || -> std::io::Result<Option<Vec<u8>>> {
        let mut bytes = Vec::new();
        File::open(&file)?
            .take(MAX_SCROLLBACK_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        Ok((bytes.len() <= MAX_SCROLLBACK_BYTES).then_some(bytes))
    };
    match read() {
        Ok(Some(bytes)) => Some(bytes),
        Ok(None) => {
            log::warn!("skipping oversized scrollback {}", file.display());
            None
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            log::warn!("unreadable scrollback {} ({e})", file.display());
            None
        }
    }
}

/// Create or truncate `path`, owner-only on Unix from the first byte.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)?;
    // `mode` only applies on creation; a leftover file keeps its own.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Atomically replace `path` (temp file + rename), owner-only on Unix.
/// `scrollback` holds the `(name, dump)` pairs the tabs reference. Commit
/// order keeps the pair consistent across a crash: dumps first, then the
/// snapshot rename, then removal of every dump it no longer references.
pub fn save_to_path(
    session: &Session,
    scrollback: &[(String, Vec<u8>)],
    path: &Path,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let dir = scrollback_dir(path);
    if !scrollback.is_empty() {
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        for (name, dump) in scrollback {
            anyhow::ensure!(valid_scrollback_name(name), "bad scrollback name {name}");
            write_private(&dir.join(name), dump)?;
        }
    }
    let text = toml::to_string_pretty(session)?;
    let tmp = path.with_extension("toml.tmp");
    write_private(&tmp, text.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    let referenced: HashSet<&str> = session
        .tabs
        .iter()
        .filter_map(|t| t.scrollback.as_deref())
        .collect();
    prune_scrollback(&dir, &referenced);
    Ok(())
}

/// Best effort: a leftover dump is retried by the next save or removal.
fn prune_scrollback(dir: &Path, keep: &HashSet<&str>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.ends_with(".vt")
            && !keep.contains(name)
            && let Err(e) = std::fs::remove_file(entry.path())
        {
            log::warn!("failed to remove scrollback {name} ({e})");
        }
    }
    if keep.is_empty() {
        let _ = std::fs::remove_dir(dir);
    }
}

/// Delete the snapshot, then its dumps; already gone is success.
pub fn remove(path: &Path) -> std::io::Result<()> {
    let gone = |r: std::io::Result<()>| match r {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    };
    gone(std::fs::remove_file(path))?;
    gone(std::fs::remove_dir_all(scrollback_dir(path)))
}

/// Whether a window placed at physical `position` would keep its title
/// bar on one of `monitors` (a disconnected display must not strand the
/// window off-screen).
pub fn position_on_monitor(position: (i32, i32), monitors: &[MonitorRect]) -> bool {
    // A point a little inside the top-left corner: grabbable title bar.
    let (px, py) = (i64::from(position.0) + 64, i64::from(position.1) + 16);
    monitors.iter().any(|&(x, y, w, h)| {
        let (x, y) = (i64::from(x), i64::from(y));
        px >= x && px < x + i64::from(w) && py >= y && py < y + i64::from(h)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(cwd: &str) -> TabState {
        TabState {
            cwd: Some(cwd.to_string()),
            scrollback: None,
        }
    }

    #[test]
    fn round_trips_through_toml() {
        let session = Session::new(
            1,
            Some(WindowState {
                width: 1200.0,
                height: 800.0,
                position: Some((-40, 25)),
            }),
            vec![
                tab("/a"),
                TabState::default(),
                TabState {
                    scrollback: Some("1-2.vt".into()),
                    ..tab("/b c")
                },
            ],
        );
        let text = toml::to_string_pretty(&session).unwrap();
        assert_eq!(parse(&text, |_| true).unwrap(), Some(session));
    }

    #[test]
    fn rejects_garbage_and_other_versions() {
        assert!(parse("not toml [", |_| true).is_err());
        assert!(parse("version = 2\n[[tabs]]\n", |_| true).is_err());
        assert!(
            parse("[[tabs]]\n", |_| true).is_err(),
            "version is required"
        );
    }

    #[test]
    fn sanitizes_tabs_active_and_window() {
        let text = "version = 1\nactive = 9\n[window]\nwidth = 0.0\nheight = 600.0\n\
                    [[tabs]]\ncwd = \"/kept\"\n[[tabs]]\ncwd = \"/gone\"\n";
        let s = parse(text, |p| p == Path::new("/kept")).unwrap().unwrap();
        assert_eq!(s.tabs, vec![tab("/kept"), TabState::default()]);
        assert_eq!(s.active, 1);
        assert_eq!(s.window, None);
        assert_eq!(parse("version = 1\n", |_| true).unwrap(), None);
        let names = "version = 1\n[[tabs]]\nscrollback = \"../../etc/passwd\"\n\
                     [[tabs]]\nscrollback = \"a/b.vt\"\n[[tabs]]\nscrollback = \".x.vt\"\n\
                     [[tabs]]\nscrollback = \"12-0.vt\"\n";
        let s = parse(names, |_| true).unwrap().unwrap();
        let kept: Vec<_> = s.tabs.iter().map(|t| t.scrollback.as_deref()).collect();
        assert_eq!(kept, [None, None, None, Some("12-0.vt")]);
        assert!(valid_scrollback_name(&scrollback_name(3)));
        let many = format!("version = 1\n{}", "[[tabs]]\n".repeat(MAX_TABS + 5));
        assert_eq!(
            parse(&many, |_| true).unwrap().unwrap().tabs.len(),
            MAX_TABS
        );
    }

    #[test]
    fn path_prefers_absolute_xdg_state_home() {
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(
            path_from(Some("/state".into()), home.clone()),
            Some(PathBuf::from("/state/cometty/session.toml"))
        );
        assert_eq!(
            path_from(Some("relative".into()), home.clone()),
            Some(PathBuf::from("/home/u/.local/state/cometty/session.toml"))
        );
        assert_eq!(path_from(None, None), None);
    }

    #[test]
    fn banner_is_a_bold_accent_dated_block() {
        let mut session = Session::new(0, None, vec![TabState::default()]);
        let banner = session.banner(4, true);
        assert!(banner.starts_with("\x1b[0;1;36m────\r\nSession restored: "));
        assert!(banner.contains("\x1b[0;36mTo stop restoring scrollback"));
        assert!(banner.contains("restore = \"tabs\""));
        assert!(banner.ends_with("\r\n\x1b[0;1;36m────\x1b[0m\r\n"));
        #[cfg(unix)]
        assert!(format_local_time(1_793_000_000).unwrap().contains("2026"));
        session.saved_at = None;
        assert_eq!(
            session.banner(2, false),
            "\x1b[0;1;36m──\r\nSession restored\r\n\x1b[0;1;36m──\x1b[0m\r\n"
        );
    }

    #[test]
    fn position_must_land_on_a_monitor() {
        let monitors = [(0, 0, 1920, 1080), (1920, -200, 1280, 1024)];
        assert!(position_on_monitor((100, 50), &monitors));
        assert!(position_on_monitor((2000, -150), &monitors));
        assert!(!position_on_monitor((5000, 50), &monitors));
        assert!(!position_on_monitor((1900, 1070), &[(0, 0, 1920, 1080)]));
        assert!(!position_on_monitor((0, 0), &[]));
    }

    #[test]
    fn save_load_remove_and_lock() {
        let dir = std::env::temp_dir().join(format!("cometty-session-{}", std::process::id()));
        let path = dir.join("session.toml");
        let session = Session::new(0, None, vec![tab(dir.to_str().unwrap())]);
        save_to_path(&session, &[], &path).unwrap();
        assert_eq!(load(&path), Some(session.clone()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // A quit with dumps writes them owner-only; the next snapshot that
        // stops referencing one deletes it.
        let with_dump = |name: &str| {
            Session::new(
                0,
                None,
                vec![TabState {
                    scrollback: Some(name.to_string()),
                    ..tab(dir.to_str().unwrap())
                }],
            )
        };
        let first = with_dump("1-0.vt");
        save_to_path(&first, &[("1-0.vt".into(), b"one\r\n".to_vec())], &path).unwrap();
        assert_eq!(load_scrollback(&path, "1-0.vt").unwrap(), b"one\r\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(scrollback_dir(&path))
                .unwrap()
                .permissions();
            assert_eq!(dir_mode.mode() & 0o777, 0o700);
            let file = scrollback_dir(&path).join("1-0.vt");
            let mode = std::fs::metadata(file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let second = with_dump("2-0.vt");
        save_to_path(&second, &[("2-0.vt".into(), b"two".to_vec())], &path).unwrap();
        assert_eq!(load_scrollback(&path, "1-0.vt"), None, "pruned");
        assert_eq!(load_scrollback(&path, "2-0.vt").unwrap(), b"two");
        save_to_path(&session, &[], &path).unwrap();
        assert!(!scrollback_dir(&path).exists(), "unreferenced dumps vanish");
        save_to_path(&second, &[("2-0.vt".into(), b"two".to_vec())], &path).unwrap();
        remove(&path).unwrap();
        assert!(!scrollback_dir(&path).exists(), "removal takes the dumps");
        assert!(
            save_to_path(&second, &[("../x.vt".into(), Vec::new())], &path).is_err(),
            "never writes outside the dump directory"
        );
        remove(&path).unwrap();
        remove(&path).unwrap();
        assert_eq!(load(&path), None);

        let held = acquire_lock(&path).expect("first instance locks");
        assert!(acquire_lock(&path).is_none(), "second instance is refused");
        drop(held);
        assert!(acquire_lock(&path).is_some(), "released on drop");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
