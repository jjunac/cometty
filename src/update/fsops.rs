//! Staging lock, atomic JSON writes, bundle facts, `ditto`, and the bundle swap.
//!
//! Everything here blocks and touches the filesystem, so it runs only on
//! update worker threads, except [`open_new_instance`], which `exiting()`
//! calls once to relaunch an installed update. The decisions these helpers
//! feed (`bundle::validate_bundle`, `staging::plan_cleanup`) stay pure.
//!
//! `ditto` and `open` exist only on macOS. Elsewhere those helpers return
//! [`io::ErrorKind::Unsupported`]; they are never reached there, because
//! `mode::resolve_mode` always yields notify-only. See
//! `docs/update.md#jobs-and-the-staging-lock` and `#install-job`.

use std::fmt;
use std::fs::{self, File, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::bundle::{BUNDLE_DIR, BundleFacts, Kind};
use super::staging::LOCK;

/// How often [`StagingLock::acquire`] retries a busy lock.
pub(crate) const LOCK_RETRY: Duration = Duration::from_millis(250);
/// How long a worker job waits for the staging lock.
pub(crate) const JOB_LOCK_PATIENCE: Duration = Duration::from_secs(45);
/// Largest `Contents/Info.plist` read for bundle validation.
pub(crate) const MAX_PLIST: u64 = 64 * 1024;

/// An exclusive `flock` on `<staging>/lock`, released when dropped.
///
/// `flock` locks belong to open file descriptions, so two acquisitions in one
/// process exclude each other just as two Cometty instances do.
#[derive(Debug)]
pub(crate) struct StagingLock {
    _file: File,
}

/// Why [`StagingLock::acquire`] returned without the lock.
#[derive(Debug)]
pub(crate) enum LockError {
    /// Another job or instance held the lock for the whole patience window.
    Busy,
    /// Creating the directory, opening the lock file, or locking failed.
    Io(io::Error),
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("staging directory is locked by another update job"),
            Self::Io(e) => write!(f, "staging lock: {e}"),
        }
    }
}

impl StagingLock {
    /// Creates `dir` (mode 0700) if needed, opens `dir/lock`, and retries
    /// `File::try_lock` every [`LOCK_RETRY`] until it succeeds or `patience`
    /// has elapsed. A zero `patience` tries exactly once.
    pub(crate) fn acquire(dir: &Path, patience: Duration) -> Result<Self, LockError> {
        create_private_dir(dir).map_err(LockError::Io)?;
        let mut options = fs::OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(dir.join(LOCK)).map_err(LockError::Io)?;
        let deadline = Instant::now() + patience;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(e)) => return Err(LockError::Io(e)),
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(LockError::Busy);
            }
            thread::sleep(LOCK_RETRY.min(deadline - now));
        }
    }
}

/// `create_dir_all` that gives newly created directories mode 0700, since the
/// staging directory holds a bundle that is later installed and run.
fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// The temporary sibling [`write_json_atomic`] writes before renaming:
/// `.<name>.<pid>.tmp`. It classifies as `staging::Entry::Unknown`, so an
/// interrupted write is left alone by cleanup rather than matched loosely.
pub(crate) fn atomic_temp_path(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let mut temp = std::ffi::OsString::from(".");
    temp.push(name);
    temp.push(format!(".{}.tmp", std::process::id()));
    Ok(path.with_file_name(temp))
}

/// Serializes `value` as pretty JSON and replaces `path` atomically: write
/// the temporary sibling (mode 0600), `fsync` it, rename it over `path`, then
/// `fsync` the directory best effort. Readers see the old file or the new
/// one, never a prefix. On failure the temporary file is removed.
pub(crate) fn write_json_atomic<T: Serialize + ?Sized>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    let temp = atomic_temp_path(path)?;
    let result = write_synced(&temp, &bytes).and_then(|()| fs::rename(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
        return result;
    }
    // The rename is done; a failed directory sync must not report the
    // commit as failed.
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let _ = File::open(parent).and_then(|d| d.sync_all());
    }
    Ok(())
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Removes a file or (without following symlinks) a directory tree; a
/// missing path is already removed.
pub(crate) fn remove_entry(path: &Path) -> io::Result<()> {
    let result = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) => Err(e),
    };
    match result {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Whether the current user may create and rename entries in the directory
/// containing `bundle`, by `access(parent, W_OK)`. Any failure counts as not
/// writable, including `EROFS` (a read-only volume, such as a mounted disk
/// image) and a missing parent.
pub(crate) fn parent_writable(bundle: &Path) -> bool {
    let Some(parent) = bundle.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(path) = std::ffi::CString::new(parent.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `path` is a valid NUL-terminated string for the call.
        unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
        false
    }
}

// ---------------------------------------------------------------------------
// Bundle facts
// ---------------------------------------------------------------------------

/// Describes the extraction directory `root` (which should hold exactly one
/// `Cometty.app`) for `bundle::validate_bundle`, without following symlinks.
pub(crate) fn gather_facts(root: &Path) -> io::Result<BundleFacts> {
    let mut top_level = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let kind = kind_of(&entry.path().symlink_metadata()?);
        top_level.push((entry.file_name().to_string_lossy().into_owned(), kind));
    }
    top_level.sort_by(|a, b| a.0.cmp(&b.0));
    let (executable, plist) = bundle_contents(&root.join(BUNDLE_DIR))?;
    Ok(BundleFacts {
        top_level,
        executable,
        plist,
    })
}

/// [`BundleFacts`] for a bundle directory itself, such as the copy next to
/// the installed bundle, described as the single `Cometty.app` entry of an
/// extraction directory so `validate_bundle` applies unchanged.
pub(crate) fn bundle_facts(bundle: &Path) -> io::Result<BundleFacts> {
    let top = kind_of(&fs::symlink_metadata(bundle)?);
    let (executable, plist) = bundle_contents(bundle)?;
    Ok(BundleFacts {
        top_level: vec![(BUNDLE_DIR.to_owned(), top)],
        executable,
        plist,
    })
}

/// The kind of `Contents/MacOS/cometty` and the bytes of `Contents/Info.plist`
/// under `bundle`. Every component on the way must be a real directory, or the
/// fact is `None`. The plist is read only when it is a regular file of at
/// most [`MAX_PLIST`] bytes.
fn bundle_contents(bundle: &Path) -> io::Result<(Option<Kind>, Option<Vec<u8>>)> {
    let executable = kind_at(bundle, &["Contents", "MacOS", "cometty"])?;
    let plist = match kind_at(bundle, &["Contents", "Info.plist"])? {
        Some(Kind::File { .. }) => read_plist(&bundle.join("Contents/Info.plist"))?,
        _ => None,
    };
    Ok((executable, plist))
}

/// The kind of `root/parts…`, requiring `root` and every component but the
/// last to be a real directory. `None` when anything on the way is missing or
/// not a directory.
fn kind_at(root: &Path, parts: &[&str]) -> io::Result<Option<Kind>> {
    let mut path = root.to_path_buf();
    let mut kind = match fs::symlink_metadata(&path) {
        Ok(meta) => kind_of(&meta),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    for part in parts {
        if kind != Kind::Dir {
            return Ok(None);
        }
        path.push(part);
        kind = match fs::symlink_metadata(&path) {
            Ok(meta) => kind_of(&meta),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
    }
    Ok(Some(kind))
}

/// Reads at most [`MAX_PLIST`] bytes from a regular file, refusing to follow
/// a symlink swapped in after [`kind_at`] looked. `None` when it is larger.
fn read_plist(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > MAX_PLIST {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(MAX_PLIST + 1).read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= MAX_PLIST).then_some(bytes))
}

fn kind_of(meta: &fs::Metadata) -> Kind {
    let file_type = meta.file_type();
    if file_type.is_dir() {
        Kind::Dir
    } else if file_type.is_symlink() {
        Kind::Symlink
    } else if file_type.is_file() {
        Kind::File {
            executable: is_executable(meta),
        }
    } else {
        Kind::Other
    }
}

#[cfg(unix)]
fn is_executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &fs::Metadata) -> bool {
    false
}

// ---------------------------------------------------------------------------
// ditto and open
// ---------------------------------------------------------------------------

/// Creates the new directory `dest` (which must not exist) and runs
/// `ditto -x -k <archive> <dest>` into it. Any failure removes `dest`, so
/// only a successful extraction leaves a directory behind; committing it is
/// the caller's `staged.json` write.
pub(crate) fn extract(archive: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir(dest)?;
    let result = ditto_extract(archive, dest);
    if result.is_err() {
        let _ = fs::remove_dir_all(dest);
    }
    result
}

/// The only `ditto` ever run: a fixed absolute path, no `PATH` lookup.
#[cfg(target_os = "macos")]
const DITTO: &str = "/usr/bin/ditto";
/// Longest `ditto` stderr excerpt kept in an error.
#[cfg(target_os = "macos")]
const DITTO_STDERR_EXCERPT: usize = 512;

/// `ditto -x -k <archive> <dest>`: extracts a PKZip archive, restoring
/// symlinks, modes, and the extended attributes `ditto -c -k` stored.
#[cfg(target_os = "macos")]
fn ditto_extract(archive: &Path, dest: &Path) -> io::Result<()> {
    run_ditto(&["-x", "-k"], &[archive, dest])
}

/// `ditto <src> <dest>`: copies a bundle, preserving symlinks, modes,
/// extended attributes, and the code signature, across volumes. Blocks the
/// calling worker thread until `ditto` exits.
#[cfg(target_os = "macos")]
pub(crate) fn ditto_copy(src: &Path, dest: &Path) -> io::Result<()> {
    run_ditto(&[], &[src, dest])
}

/// Runs `ditto` with `flags` and then `paths`. Paths must be absolute, so
/// none can be mistaken for an option.
#[cfg(target_os = "macos")]
fn run_ditto(flags: &[&str], paths: &[&Path]) -> io::Result<()> {
    use std::process::{Command, Stdio};
    if let Some(p) = paths.iter().find(|p| !p.is_absolute()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("ditto path {} is not absolute", p.display()),
        ));
    }
    let output = Command::new(DITTO)
        .args(flags)
        .args(paths)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let excerpt: String = stderr.trim().chars().take(DITTO_STDERR_EXCERPT).collect();
    Err(io::Error::other(format!(
        "{DITTO} exited with {}: {excerpt}",
        output.status
    )))
}

/// Starts `/usr/bin/open -n <bundle>` without waiting for it: a fresh
/// instance of the installed bundle, even while LaunchServices still lists
/// this exiting process.
#[cfg(target_os = "macos")]
pub(crate) fn open_new_instance(bundle: &Path) -> io::Result<()> {
    use std::process::{Command, Stdio};
    // Dropping the `Child` neither waits nor kills; `open` outlives us.
    drop(
        Command::new("/usr/bin/open")
            .arg("-n")
            .arg(bundle)
            .stdin(Stdio::null())
            .spawn()?,
    );
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn ditto_extract(_archive: &Path, _dest: &Path) -> io::Result<()> {
    Err(macos_only("ditto"))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn ditto_copy(_src: &Path, _dest: &Path) -> io::Result<()> {
    Err(macos_only("ditto"))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn open_new_instance(_bundle: &Path) -> io::Result<()> {
    Err(macos_only("open"))
}

#[cfg(not(target_os = "macos"))]
fn macos_only(tool: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{tool} is only available on macOS"),
    )
}

// ---------------------------------------------------------------------------
// Bundle swap
// ---------------------------------------------------------------------------

/// Replaces `installed` with `incoming` and returns where the old bundle
/// went (see [`replace_with`]).
pub(crate) fn replace_bundle(
    installed: &Path,
    incoming: &Path,
    backup: &Path,
) -> io::Result<PathBuf> {
    replace_with(installed, incoming, backup, rename_swap)
}

/// Swaps `incoming` and `installed` atomically with `swap`, so the installed
/// path never disappears and the old bundle ends up at `incoming`. Only when
/// the volume cannot swap (`ENOTSUP`/`EINVAL`) does it fall back to
/// `installed → backup`, `incoming → installed`, moving the old bundle back
/// if the second rename fails. Any other swap error, including App
/// Management's `EPERM`, moves nothing.
fn replace_with(
    installed: &Path,
    incoming: &Path,
    backup: &Path,
    swap: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    match swap(incoming, installed) {
        Ok(()) => return Ok(incoming.to_path_buf()),
        Err(e) if !swap_unsupported(&e) => return Err(e),
        Err(_) => {}
    }
    remove_entry(backup)?;
    fs::rename(installed, backup)?;
    if let Err(e) = fs::rename(incoming, installed) {
        return Err(match fs::rename(backup, installed) {
            Ok(()) => e,
            Err(restore) => io::Error::other(format!(
                "{e}; moving the previous bundle back also failed ({restore}); it is at {}",
                backup.display()
            )),
        });
    }
    Ok(backup.to_path_buf())
}

/// `renamex_np(a, b, RENAME_SWAP)`.
#[cfg(target_os = "macos")]
fn rename_swap(a: &Path, b: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = |p: &Path| {
        std::ffi::CString::new(p.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
    };
    let (a, b) = (c(a)?, c(b)?);
    // SAFETY: both arguments are valid NUL-terminated paths.
    if unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// No atomic swap off macOS: report it like a volume without `RENAME_SWAP`.
#[cfg(all(unix, not(target_os = "macos")))]
fn rename_swap(_a: &Path, _b: &Path) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::ENOTSUP))
}

#[cfg(not(unix))]
fn rename_swap(_a: &Path, _b: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// `ENOTSUP`/`EINVAL`: the volume cannot swap atomically, so the rename
/// fallback applies.
fn swap_unsupported(e: &io::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(e.raw_os_error(), Some(libc::ENOTSUP | libc::EINVAL))
    }
    #[cfg(not(unix))]
    {
        e.kind() == io::ErrorKind::Unsupported
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::update::bundle::validate_bundle;
    use crate::update::version::Version;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    const TEMPLATE: &[u8] = include_bytes!("../../packaging/macos/Info.plist");

    /// A unique temporary directory, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "cometty-fsops-{tag}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn v123() -> Version {
        Version {
            major: 1,
            minor: 2,
            patch: 3,
        }
    }

    /// `root/Cometty.app` with an XML plist for 1.2.3, a 0755 executable,
    /// and `Contents/Resources/current -> ../MacOS/cometty`.
    fn write_fixture_bundle(root: &Path) -> PathBuf {
        let app = root.join(BUNDLE_DIR);
        let contents = app.join("Contents");
        fs::create_dir_all(contents.join("MacOS")).unwrap();
        fs::create_dir_all(contents.join("Resources")).unwrap();
        let plist = std::str::from_utf8(TEMPLATE)
            .unwrap()
            .replace("__VERSION__", "1.2.3")
            .replace("__BUILD__", "1.2.3");
        fs::write(contents.join("Info.plist"), plist).unwrap();
        let exe = contents.join("MacOS/cometty");
        fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        symlink("../MacOS/cometty", contents.join("Resources/current")).unwrap();
        app
    }

    #[test]
    fn two_locks_in_one_process_serialize() {
        let dir = TempDir::new("lock");
        let staging = dir.path().join("updates");
        let first = StagingLock::acquire(&staging, Duration::ZERO).expect("first lock");
        assert!(staging.join(LOCK).is_file());

        let started = Instant::now();
        assert!(matches!(
            StagingLock::acquire(&staging, Duration::from_millis(300)),
            Err(LockError::Busy)
        ));
        assert!(started.elapsed() >= Duration::from_millis(300));

        let released = Arc::new(AtomicBool::new(false));
        let waiter = {
            let staging = staging.clone();
            let released = Arc::clone(&released);
            thread::spawn(move || {
                let lock = StagingLock::acquire(&staging, Duration::from_secs(10));
                assert!(lock.is_ok(), "waiter gets the lock after release");
                assert!(released.load(Ordering::SeqCst), "acquired while held");
            })
        };
        thread::sleep(Duration::from_millis(400));
        released.store(true, Ordering::SeqCst);
        drop(first);
        waiter.join().unwrap();
    }

    #[test]
    fn atomic_write_replaces_without_leaving_temp_files() {
        let dir = TempDir::new("atomic");
        let path = dir.path().join("staged.json");
        write_json_atomic(&path, &serde_json::json!({ "n": 1 })).unwrap();
        write_json_atomic(&path, &serde_json::json!({ "n": 2 })).unwrap();

        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["n"], 2);
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["staged.json"]);
        assert!(!atomic_temp_path(&path).unwrap().exists());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        // A failed write (missing directory) leaves nothing behind either.
        let missing = dir.path().join("missing/staged.json");
        assert!(write_json_atomic(&missing, &1).is_err());
        assert!(!dir.path().join("missing").exists());
    }

    #[test]
    fn parent_writable_probes_the_containing_directory() {
        let dir = TempDir::new("writable");
        assert!(parent_writable(&dir.path().join("Cometty.app")));
        assert!(!parent_writable(&dir.path().join("missing/Cometty.app")));
        assert!(!parent_writable(Path::new("Cometty.app")));
    }

    #[test]
    fn gather_facts_describes_the_bundle_without_following_symlinks() {
        let dir = TempDir::new("facts");
        let root = dir.path().join("extract");
        let app = write_fixture_bundle(&root);
        let facts = gather_facts(&root).unwrap();
        assert_eq!(facts.top_level, [(BUNDLE_DIR.to_owned(), Kind::Dir)]);
        assert_eq!(facts.executable, Some(Kind::File { executable: true }));
        assert_eq!(validate_bundle(&facts, v123()), Ok(()));
        assert_eq!(bundle_facts(&app).unwrap(), facts);

        // A symlinked `MacOS` directory hides the executable.
        let contents = app.join("Contents");
        fs::rename(contents.join("MacOS"), contents.join("Real")).unwrap();
        symlink("Real", contents.join("MacOS")).unwrap();
        assert_eq!(gather_facts(&root).unwrap().executable, None);

        // An oversized plist is not read.
        let big = vec![b' '; MAX_PLIST as usize + 1];
        fs::write(contents.join("Info.plist"), big).unwrap();
        assert_eq!(gather_facts(&root).unwrap().plist, None);

        // A symlinked bundle is described as a symlink with no contents.
        let link = dir.path().join("link.app");
        symlink(&app, &link).unwrap();
        let facts = bundle_facts(&link).unwrap();
        assert_eq!(facts.top_level, [(BUNDLE_DIR.to_owned(), Kind::Symlink)]);
        assert_eq!((facts.executable, facts.plist), (None, None));
    }

    /// Two directories standing in for bundles, each holding a `label` file.
    fn bundles(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let installed = dir.join("Cometty.app");
        let incoming = dir.join(".Cometty.app.cometty-new");
        let backup = dir.join(".Cometty.app.cometty-old");
        for (path, label) in [(&installed, "old"), (&incoming, "new")] {
            fs::create_dir(path).unwrap();
            fs::write(path.join("label"), label).unwrap();
        }
        (installed, incoming, backup)
    }

    fn label(path: &Path) -> String {
        fs::read_to_string(path.join("label")).unwrap()
    }

    fn errno(e: i32) -> impl FnOnce(&Path, &Path) -> io::Result<()> {
        move |_, _| Err(io::Error::from_raw_os_error(e))
    }

    #[test]
    fn unsupported_swap_falls_back_to_renames_and_clears_a_stale_backup() {
        for e in [libc::ENOTSUP, libc::EINVAL] {
            let dir = TempDir::new("fallback");
            let (installed, incoming, backup) = bundles(dir.path());
            fs::create_dir(&backup).unwrap();
            fs::write(backup.join("label"), "stale").unwrap();
            let moved = replace_with(&installed, &incoming, &backup, errno(e)).unwrap();
            assert_eq!(moved, backup);
            assert_eq!(label(&installed), "new");
            assert_eq!(label(&backup), "old");
            assert!(!incoming.exists());
        }
    }

    #[test]
    fn other_swap_errors_move_nothing() {
        for e in [libc::EPERM, libc::EACCES, libc::EIO, libc::EXDEV] {
            let dir = TempDir::new("eperm");
            let (installed, incoming, backup) = bundles(dir.path());
            let err = replace_with(&installed, &incoming, &backup, errno(e)).unwrap_err();
            assert_eq!(err.raw_os_error(), Some(e));
            assert_eq!(label(&installed), "old");
            assert_eq!(label(&incoming), "new");
            assert!(!backup.exists());
        }
    }

    #[test]
    fn a_failed_move_in_puts_the_old_bundle_back() {
        let dir = TempDir::new("restore");
        let (installed, incoming, backup) = bundles(dir.path());
        fs::remove_dir_all(&incoming).unwrap();
        assert!(replace_with(&installed, &incoming, &backup, errno(libc::ENOTSUP)).is_err());
        assert_eq!(label(&installed), "old");
        assert!(!backup.exists());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn macos_tools_are_unsupported_elsewhere() {
        let p = Path::new("/nonexistent");
        for result in [ditto_extract(p, p), ditto_copy(p, p), open_new_instance(p)] {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
        }
    }

    #[cfg(target_os = "macos")]
    mod macos {
        use super::*;
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::process::Command;

        const XATTR: &str = "com.github.jjunac.cometty.test";

        fn cstr(path: &Path) -> CString {
            CString::new(path.as_os_str().as_bytes()).unwrap()
        }

        fn set_xattr(path: &Path, value: &[u8]) {
            let name = CString::new(XATTR).unwrap();
            // SAFETY: valid NUL-terminated strings and a live buffer.
            let rc = unsafe {
                libc::setxattr(
                    cstr(path).as_ptr(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                    0,
                )
            };
            assert_eq!(rc, 0, "setxattr: {}", io::Error::last_os_error());
        }

        fn get_xattr(path: &Path) -> Option<Vec<u8>> {
            let name = CString::new(XATTR).unwrap();
            let mut buf = [0u8; 64];
            // SAFETY: valid NUL-terminated strings and a live buffer.
            let n = unsafe {
                libc::getxattr(
                    cstr(path).as_ptr(),
                    name.as_ptr(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    0,
                    0,
                )
            };
            usize::try_from(n).ok().map(|n| buf[..n].to_vec())
        }

        fn assert_preserved(app: &Path) {
            let exe = app.join("Contents/MacOS/cometty");
            let link = app.join("Contents/Resources/current");
            assert_eq!(
                fs::read_link(&link).unwrap(),
                PathBuf::from("../MacOS/cometty")
            );
            assert_eq!(
                fs::symlink_metadata(&exe).unwrap().permissions().mode() & 0o7777,
                0o755
            );
            assert_eq!(get_xattr(&exe).as_deref(), Some(&b"kept"[..]));
        }

        #[test]
        fn ditto_round_trip_preserves_metadata_and_validates() {
            let dir = TempDir::new("ditto");
            let app = write_fixture_bundle(&dir.path().join("src"));
            set_xattr(&app.join("Contents/MacOS/cometty"), b"kept");
            let archive = dir.path().join("Cometty-1.2.3-macos-universal.zip");
            let status = Command::new(DITTO)
                .args(["-c", "-k", "--keepParent"])
                .arg(&app)
                .arg(&archive)
                .status()
                .unwrap();
            assert!(status.success());

            let extract_dir = dir.path().join("extract");
            extract(&archive, &extract_dir).unwrap();
            // ditto restores AppleDouble data as xattrs: no `__MACOSX` or `._*`.
            let facts = gather_facts(&extract_dir).unwrap();
            assert_eq!(facts.top_level, [(BUNDLE_DIR.to_owned(), Kind::Dir)]);
            assert_eq!(validate_bundle(&facts, v123()), Ok(()));
            let extracted = extract_dir.join(BUNDLE_DIR);
            assert_preserved(&extracted);

            // The install job's copy preserves the same metadata.
            let copy_parent = dir.path().join("Applications");
            fs::create_dir(&copy_parent).unwrap();
            let copy = copy_parent.join(".Cometty.app.cometty-new");
            ditto_copy(&extracted, &copy).unwrap();
            assert_preserved(&copy);

            assert!(ditto_copy(Path::new("relative"), &copy).is_err());
        }

        #[test]
        fn a_corrupt_archive_leaves_no_extraction_directory() {
            let dir = TempDir::new("corrupt");
            let archive = dir.path().join("corrupt.zip");
            fs::write(&archive, b"not a zip archive").unwrap();
            let dest = dir.path().join("extract");
            assert!(extract(&archive, &dest).is_err());
            assert!(!dest.exists());
        }

        #[test]
        fn rename_swap_exchanges_two_directories() {
            let dir = TempDir::new("swap");
            let (installed, incoming, backup) = bundles(dir.path());
            let moved = replace_bundle(&installed, &incoming, &backup).unwrap();
            assert_eq!(moved, incoming);
            assert_eq!(label(&installed), "new");
            assert_eq!(label(&incoming), "old");
            assert!(!backup.exists());
        }
    }
}
