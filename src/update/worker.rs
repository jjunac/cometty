//! Detached update job threads and the check, housekeeping, and install pipelines.
//!
//! Jobs mirror `Startup::spawn_worker`: a named thread with a drop guard armed
//! inside it that announces return or panic. Unlike PTY workers they have no
//! supervisor and are never joined; the `JoinHandle` is dropped so the process
//! can exit while a job is still running (`docs/update.md#jobs-and-the-staging-lock`).

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::update::TARGET;
use crate::update::bundle::{BUNDLE_DIR, validate_bundle};
use crate::update::checksum::{Digest, HashingWriter, hex_lower, parse_checksum};
use crate::update::fsops::{self, LockError, StagingLock, remove_entry};
use crate::update::http::{Fetch, FetchResponse, UreqFetcher, classify_io_error};
use crate::update::limits::{
    CHECKSUM_CAP, CopyError, FEED_CAP, RequestKind, archive_cap, copy_capped,
};
use crate::update::mode::{Mode, NotifyReason, count_instances, sibling_names};
use crate::update::procs;
use crate::update::release::{
    Asset, CheckOutcome, Release, StatusClass, UpToDateReason, classify_release, classify_status,
    parse_release,
};
use crate::update::staging::{
    Entry, SCHEMA, STAGED, StagedRecord, classify_entry, extract_name, partial_name, plan_cleanup,
};
use crate::update::state::{Event, Failure};
use crate::update::version::Version;

/// A message from an update job to the UI thread. Every variant carries the
/// job id, so the controller can discard messages from an abandoned job.
#[derive(Debug)]
pub(crate) enum WorkerMsg {
    /// A state-machine input.
    Event { job: u64, event: Event },
    /// The mode a check job resolved after probing writability.
    Mode { job: u64, mode: Mode },
    /// The install job replaced `bundle` with the staged update; the app
    /// quits and relaunches it. This is the install job's terminal message.
    Installed { job: u64, bundle: PathBuf },
    /// The job returned or panicked. Always the job's last message.
    Finished { job: u64, panicked: bool },
}

/// What a job needs to report results and observe cancellation.
pub(crate) struct JobCtx {
    pub id: u64,
    pub cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<WorkerMsg>,
    /// Wakes the event loop (an `EventLoopProxy` send in the app; a counter
    /// in tests), so delivery needs no polling.
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl JobCtx {
    pub(crate) fn new(
        id: u64,
        cancel: Arc<AtomicBool>,
        tx: mpsc::Sender<WorkerMsg>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            id,
            cancel,
            tx,
            wake,
        }
    }

    /// Send a state-machine event and wake the event loop.
    pub(crate) fn emit(&self, event: Event) {
        self.send(WorkerMsg::Event {
            job: self.id,
            event,
        });
    }

    /// Whether the controller abandoned this job. Checked before every
    /// request and every body chunk.
    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    /// Send any message and wake the event loop. A closed channel means the
    /// controller is gone (the app is exiting), so the message is dropped.
    pub(crate) fn send(&self, msg: WorkerMsg) {
        if self.tx.send(msg).is_ok() {
            (self.wake)();
        }
    }
}

/// Start `work` on a named, detached thread. A drop guard armed inside the
/// thread sends `Finished { panicked }` on return or during unwinding, so a
/// failed spawn reports nothing. The thread is never joined.
pub(crate) fn spawn_job(
    name: &'static str,
    ctx: JobCtx,
    work: impl FnOnce(&JobCtx) + Send + 'static,
) -> std::io::Result<()> {
    /// Announces the end of the job, including a panic.
    struct Finished(JobCtx);
    impl Drop for Finished {
        fn drop(&mut self) {
            let panicked = std::thread::panicking();
            self.0.send(WorkerMsg::Finished {
                job: self.0.id,
                panicked,
            });
        }
    }
    // The handle is dropped: update jobs are detached.
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let finished = Finished(ctx);
            work(&finished.0);
        })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Check job
// ---------------------------------------------------------------------------

/// The release feed: the latest published, non-draft, non-prerelease release.
pub(crate) const RELEASE_FEED_URL: &str =
    "https://api.github.com/repos/jjunac/cometty/releases/latest";

/// Inputs of one check job, captured by the controller when it spawns it.
#[derive(Clone, Debug)]
pub(crate) struct CheckParams {
    pub running: Version,
    /// The launch-time mode; the job probes writability before any
    /// self-update work.
    pub mode: Mode,
    pub staging_dir: Option<PathBuf>,
    /// How long to wait for the staging lock ([`fsops::JOB_LOCK_PATIENCE`]).
    pub lock_patience: Duration,
}

/// Starts the check job on the detached `update-check` thread with a fresh
/// [`UreqFetcher`], dropped (with its agents) when the job ends.
pub(crate) fn spawn_check(ctx: JobCtx, params: CheckParams) -> io::Result<()> {
    spawn_job("update-check", ctx, move |ctx| {
        let fetcher = UreqFetcher::new();
        run_check(ctx, &fetcher, &params);
    })
}

/// Why the check pipeline stopped early.
#[derive(Debug)]
enum Stop {
    /// The controller abandoned the job; nothing more is reported.
    Cancelled,
    /// The job fails with this reason.
    Failed(Failure),
}

impl From<Failure> for Stop {
    fn from(failure: Failure) -> Self {
        Self::Failed(failure)
    }
}

fn cancel_point(ctx: &JobCtx) -> Result<(), Stop> {
    if ctx.cancelled() {
        Err(Stop::Cancelled)
    } else {
        Ok(())
    }
}

/// Runs the check job (`docs/update.md#check-job`) and reports its outcome
/// through `ctx`. Every step that fails or is cancelled has removed the files
/// it created by the time `Failed` is emitted; a cancelled job emits nothing
/// further.
pub(crate) fn run_check<F: Fetch>(ctx: &JobCtx, fetch: &F, params: &CheckParams) {
    log::debug!(target: TARGET, "update check started (running {})", params.running);
    match check(ctx, fetch, params) {
        Ok(()) => {}
        Err(Stop::Cancelled) => log::debug!(target: TARGET, "update check cancelled"),
        Err(Stop::Failed(failure)) => {
            log::warn!(target: TARGET, "update check failed: {failure}");
            ctx.emit(Event::Failed(failure));
        }
    }
}

fn check<F: Fetch>(ctx: &JobCtx, fetch: &F, params: &CheckParams) -> Result<(), Stop> {
    // Step 1: the feed.
    let Some(release) = fetch_release(ctx, fetch, params.running)? else {
        return Ok(());
    };
    let v = release.version;

    // Step 2: notify-only, no usable assets, or cancelled.
    cancel_point(ctx)?;
    if let Mode::NotifyOnly(reason) = &params.mode {
        log::debug!(target: TARGET, "notify-only mode ({reason:?}): not downloading {v}");
        return Ok(());
    }
    let assets = match &release.assets {
        Ok(assets) => assets,
        Err(problem) => {
            log::info!(target: TARGET, "Cometty {v}: {problem}; notify only for this release");
            return Ok(());
        }
    };

    // Step 3: probe writability now that self-update work would start.
    let mode = probe_mode(&params.mode);
    ctx.send(WorkerMsg::Mode {
        job: ctx.id,
        mode: mode.clone(),
    });
    if let Mode::NotifyOnly(reason) = &mode {
        log::info!(target: TARGET, "bundle cannot be updated in place ({reason:?}); notify only");
        return Ok(());
    }
    let Some(dir) = params.staging_dir.as_deref() else {
        log::debug!(target: TARGET, "no staging directory; notify only");
        return Ok(());
    };

    // Step 4: serialize with other jobs and instances.
    let _lock = match StagingLock::acquire(dir, params.lock_patience) {
        Ok(lock) => lock,
        Err(LockError::Busy) => {
            log::debug!(target: TARGET, "staging directory busy; skipping download of {v}");
            return Ok(());
        }
        Err(LockError::Io(e)) => return Err(io_failure("staging lock", &e).into()),
    };

    // Step 5: the published digest, before any archive byte.
    let expected = fetch_checksum(ctx, fetch, &assets.checksum, &assets.archive.name)?;

    // Step 6: a staged bundle of this exact archive needs no download. A
    // re-uploaded release under the same tag has a new digest.
    match valid_staged(dir, v) {
        Some(record) if record.sha256.eq_ignore_ascii_case(&hex_lower(&expected)) => {
            log::info!(target: TARGET, "Cometty {v} is already staged");
            ctx.emit(Event::AlreadyStaged(v));
            return Ok(());
        }
        Some(_) => log::info!(
            target: TARGET,
            "the staged Cometty {v} differs from the published archive; downloading it again"
        ),
        None => {}
    }

    // Step 7: clean up, then stream and hash the archive.
    cancel_point(ctx)?;
    ctx.emit(Event::DownloadStarted);
    cleanup_before_download(dir, params.running, v)?;
    let archive_path = dir.join(partial_name(v));
    // The archive is removed on every exit: failure, cancellation, or after
    // a successful commit (step 9).
    let archive = RemoveOnDrop::new(archive_path.clone());
    let actual = download_archive(ctx, fetch, &assets.archive, &archive_path)?;

    // Step 8: digest, extraction, and bundle validation.
    cancel_point(ctx)?;
    ctx.emit(Event::VerifyStarted);
    if actual != expected {
        return Err(Failure::Digest { expected, actual }.into());
    }
    let extract_dir = extract_name(v, std::process::id(), now_nanos());
    let dest = dir.join(&extract_dir);
    fsops::extract(&archive_path, &dest).map_err(|e| Failure::Extract(e.to_string()))?;
    let extracted = RemoveOnDrop::new(dest.clone());
    let facts = fsops::gather_facts(&dest).map_err(|e| io_failure("reading bundle", &e))?;
    validate_bundle(&facts, v).map_err(Failure::from)?;
    cancel_point(ctx)?;

    // Step 9: the commit point.
    let record = StagedRecord {
        schema: SCHEMA,
        version: v,
        sha256: hex_lower(&expected),
        extract_dir: extract_dir.clone(),
    };
    fsops::write_json_atomic(&dir.join(STAGED), &record)
        .map_err(|e| io_failure("writing staged.json", &e))?;
    extracted.disarm();
    remove_other_extracts(dir, &extract_dir);
    drop(archive);
    log::info!(target: TARGET, "Cometty {v} is staged in {}", dest.display());
    ctx.emit(Event::Staged(v));
    Ok(())
}

/// Step 1: GET the feed, classify the status, parse, and compare. Emits
/// `UpToDate` or `Newer`; returns the release only when it is newer.
fn fetch_release<F: Fetch>(
    ctx: &JobCtx,
    fetch: &F,
    running: Version,
) -> Result<Option<Release>, Stop> {
    let mut response = get(ctx, fetch, RELEASE_FEED_URL, RequestKind::Feed)?;
    match classify_status(response.status) {
        StatusClass::Parse => {}
        StatusClass::UpToDate => {
            log::debug!(target: TARGET, "release feed returned 404: no published release");
            ctx.emit(Event::UpToDate);
            return Ok(None);
        }
        StatusClass::Failed(code) => return Err(Failure::Status(code).into()),
    }
    let mut body = Vec::new();
    copy_body(
        ctx,
        &mut response.body,
        &mut body,
        RequestKind::Feed,
        FEED_CAP,
    )?;
    drop(response);
    let raw = parse_release(&body)?;
    match classify_release(raw, running) {
        CheckOutcome::UpToDate(reason) => {
            match reason {
                UpToDateReason::BadTag(tag) => {
                    log::info!(target: TARGET, "ignoring release with tag {tag:?}: not vX.Y.Z");
                }
                other => log::debug!(target: TARGET, "up to date ({other:?})"),
            }
            ctx.emit(Event::UpToDate);
            Ok(None)
        }
        CheckOutcome::Newer(release) => {
            log::info!(
                target: TARGET,
                "Cometty {} is available (running {running})",
                release.version
            );
            ctx.emit(Event::Newer(release.clone()));
            Ok(Some(release))
        }
    }
}

/// Step 5: GET the checksum file into memory and parse it against the
/// archive name.
fn fetch_checksum<F: Fetch>(
    ctx: &JobCtx,
    fetch: &F,
    asset: &Asset,
    archive_name: &str,
) -> Result<Digest, Stop> {
    let mut response = get(ctx, fetch, &asset.url, RequestKind::Checksum)?;
    expect_ok(response.status)?;
    let mut body = Vec::new();
    copy_body(
        ctx,
        &mut response.body,
        &mut body,
        RequestKind::Checksum,
        CHECKSUM_CAP,
    )?;
    Ok(parse_checksum(&body, archive_name).map_err(Failure::from)?)
}

/// Step 7: stream the archive into `path`, capped at `min(advertised,
/// 256 MiB)` and required to match the advertised size. Returns the SHA-256
/// of the bytes written.
fn download_archive<F: Fetch>(
    ctx: &JobCtx,
    fetch: &F,
    asset: &Asset,
    path: &Path,
) -> Result<Digest, Stop> {
    let mut response = get(ctx, fetch, &asset.url, RequestKind::Archive)?;
    expect_ok(response.status)?;
    let file = create_private_file(path).map_err(|e| io_failure("creating download", &e))?;
    let mut file = HashingWriter::new(file);
    let cap = archive_cap(asset.size);
    let received = copy_body(
        ctx,
        &mut response.body,
        &mut file,
        RequestKind::Archive,
        cap,
    )?;
    if received != asset.size {
        return Err(Failure::Network(format!(
            "archive ended after {received} of {} bytes",
            asset.size
        ))
        .into());
    }
    Ok(file.digest())
}

/// One request, refused once the job is cancelled.
fn get<F: Fetch>(
    ctx: &JobCtx,
    fetch: &F,
    url: &str,
    kind: RequestKind,
) -> Result<FetchResponse, Stop> {
    cancel_point(ctx)?;
    Ok(fetch.get(url, kind)?)
}

/// Asset downloads accept only 200; redirects are followed by the client.
fn expect_ok(status: u16) -> Result<(), Stop> {
    match status {
        200 => Ok(()),
        code => Err(Failure::Status(code).into()),
    }
}

/// `copy_capped` with errors mapped to the pipeline's outcomes: overflow is
/// `TooLarge`, a read error is classified as transport, and a write error is
/// a staging `Io` failure.
fn copy_body(
    ctx: &JobCtx,
    body: &mut Box<dyn Read + Send>,
    w: impl Write,
    kind: RequestKind,
    cap: u64,
) -> Result<u64, Stop> {
    let mut w = TrackedWriter {
        inner: w,
        failed: false,
    };
    copy_capped(body, &mut w, cap, &ctx.cancel).map_err(|e| match e {
        CopyError::Overflow => Stop::Failed(Failure::TooLarge { what: kind, cap }),
        CopyError::Cancelled => Stop::Cancelled,
        CopyError::Io(e) if w.failed => Stop::Failed(io_failure("writing download", &e)),
        CopyError::Io(e) => Stop::Failed(classify_io_error(e)),
    })
}

/// Remembers whether the error `copy_capped` reports came from the writer.
struct TrackedWriter<W> {
    inner: W,
    failed: bool,
}

impl<W: Write> Write for TrackedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf).inspect_err(|e| self.note(e))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush().inspect_err(|e| self.note(e))
    }
}

impl<W> TrackedWriter<W> {
    fn note(&mut self, e: &io::Error) {
        if e.kind() != io::ErrorKind::Interrupted {
            self.failed = true;
        }
    }
}

/// Step 3: a self-update launch whose bundle parent is not writable
/// downgrades to notify-only.
fn probe_mode(mode: &Mode) -> Mode {
    match mode {
        Mode::SelfUpdate { bundle } if !fsops::parent_writable(bundle) => {
            Mode::NotifyOnly(NotifyReason::NotWritable)
        }
        other => other.clone(),
    }
}

/// The `staged.json` record when it names `v`, records a digest, and points
/// at a canonical extract directory of `v` that holds a valid bundle. The
/// install job re-applies these rules before copying the bundle.
fn valid_staged(dir: &Path, v: Version) -> Option<StagedRecord> {
    let record = read_staged(dir)?;
    let canonical = matches!(
        classify_entry(&record.extract_dir),
        Entry::Extract(ev, _) if ev == v
    );
    if record.schema != SCHEMA || record.version != v || !canonical || !is_hex(&record.sha256) {
        return None;
    }
    let root = dir.join(&record.extract_dir);
    let is_dir = fs::symlink_metadata(&root).is_ok_and(|m| m.is_dir());
    let valid =
        is_dir && fsops::gather_facts(&root).is_ok_and(|facts| validate_bundle(&facts, v).is_ok());
    if !valid {
        log::debug!(target: TARGET, "staged.json for {v} does not describe a valid bundle");
        return None;
    }
    Some(record)
}

/// Largest `staged.json` read; it is a few hundred bytes.
const MAX_RECORD: u64 = 64 * 1024;

/// The parsed `staged.json`, or `None` when it is missing, oversized, or
/// unreadable.
fn read_staged(dir: &Path) -> Option<StagedRecord> {
    let mut bytes = Vec::new();
    File::open(dir.join(STAGED))
        .ok()?
        .take(MAX_RECORD + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_RECORD {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn is_hex(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Step 7 cleanup: every partial, every unreferenced extract, and a staged
/// record for `v` itself, which this download replaces. A staged bundle of
/// another version stays until this one commits.
fn cleanup_before_download(dir: &Path, running: Version, v: Version) -> Result<(), Stop> {
    let listing = list_names(dir).map_err(|e| io_failure("listing staging directory", &e))?;
    let staged = read_staged(dir).filter(|r| r.version != v);
    for name in plan_cleanup(&listing, staged.as_ref(), running) {
        remove_logged(&dir.join(name));
    }
    Ok(())
}

/// Step 9 cleanup: every extract other than the committed one.
fn remove_other_extracts(dir: &Path, keep: &str) {
    let listing = match list_names(dir) {
        Ok(listing) => listing,
        Err(e) => {
            log::warn!(target: TARGET, "listing staging directory: {e}");
            return;
        }
    };
    for name in listing {
        if name != keep && matches!(classify_entry(&name), Entry::Extract(..)) {
            remove_logged(&dir.join(name));
        }
    }
}

/// Bare UTF-8 entry names of `dir`. Other names are never produced by the
/// updater, so skipping them matches `staging::Entry::Unknown`.
fn list_names(dir: &Path) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        if let Ok(name) = entry?.file_name().into_string() {
            names.push(name);
        }
    }
    Ok(names)
}

fn remove_logged(path: &Path) {
    if let Err(e) = remove_entry(path) {
        log::warn!(target: TARGET, "could not remove {}: {e}", path.display());
    }
}

/// Removes `path` when dropped, unless disarmed: the cleanup of a failed or
/// cancelled step, including during a panic.
struct RemoveOnDrop(Option<PathBuf>);

impl RemoveOnDrop {
    fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }

    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            remove_logged(&path);
        }
    }
}

/// Creates (or truncates) `path` with mode 0600.
fn create_private_file(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn io_failure(what: &str, e: &io::Error) -> Failure {
    Failure::Io(format!("{what}: {e}"))
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

// ---------------------------------------------------------------------------
// Housekeeping job
// ---------------------------------------------------------------------------

/// Inputs of the launch-time housekeeping job. The controller builds these
/// only in `SelfUpdate` mode, so a `cargo run` or translocated launch never
/// prunes a bundle's staging directory.
#[derive(Clone, Debug)]
pub(crate) struct HousekeepingParams {
    pub running: Version,
    /// The installed bundle this launch runs from (`…/Cometty.app`).
    pub installed: PathBuf,
    pub staging_dir: PathBuf,
    /// How long to wait for the staging lock ([`fsops::JOB_LOCK_PATIENCE`]).
    pub lock_patience: Duration,
}

/// Starts the housekeeping job on a detached `update-housekeeping` thread.
/// It needs no network, reports only through the log, and runs whether or
/// not update checks are enabled.
pub(crate) fn spawn_housekeeping(params: HousekeepingParams) -> io::Result<()> {
    std::thread::Builder::new()
        .name("update-housekeeping".into())
        .spawn(move || run_housekeeping(&params))?;
    Ok(())
}

/// Runs the housekeeping job (`docs/update.md#housekeeping-job`): prune the
/// staging directory and remove the previous install's sibling bundles. A
/// missing staging directory or a busy lock skips the whole job, and nothing
/// is created.
pub(crate) fn run_housekeeping(params: &HousekeepingParams) {
    let dir = params.staging_dir.as_path();
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            log::warn!(target: TARGET, "{} is not a directory; skipping housekeeping", dir.display());
            return;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            log::debug!(target: TARGET, "no staging directory; nothing to clean up");
            return;
        }
        Err(e) => {
            log::warn!(target: TARGET, "staging directory {}: {e}", dir.display());
            return;
        }
    }

    // Step 1: serialize with jobs and other instances.
    let _lock = match StagingLock::acquire(dir, params.lock_patience) {
        Ok(lock) => lock,
        Err(LockError::Busy) => {
            log::debug!(target: TARGET, "staging directory busy; skipping housekeeping");
            return;
        }
        Err(LockError::Io(e)) => {
            log::warn!(target: TARGET, "staging lock: {e}");
            return;
        }
    };

    // Step 2: staged bundles at or below the running version, partial
    // downloads, and orphaned extracts.
    match list_names(dir) {
        Ok(listing) => {
            let staged = read_staged(dir);
            for name in plan_cleanup(&listing, staged.as_ref(), params.running) {
                log::debug!(target: TARGET, "removing staging entry {name}");
                remove_logged(&dir.join(name));
            }
        }
        Err(e) => log::warn!(target: TARGET, "listing staging directory: {e}"),
    }

    // Step 3: the previous bundle and any interrupted copy next to this one.
    // Both paths are derived from this launch's bundle, never read from disk.
    let (incoming, backup) = sibling_names(&params.installed);
    for sibling in [incoming, backup] {
        match fs::symlink_metadata(&sibling) {
            Ok(_) => match remove_entry(&sibling) {
                Ok(()) => log::info!(target: TARGET, "removed {}", sibling.display()),
                Err(e) => {
                    log::warn!(target: TARGET, "could not remove {}: {e}", sibling.display())
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => log::warn!(target: TARGET, "{}: {e}", sibling.display()),
        }
    }
}

// ---------------------------------------------------------------------------
// Install job
// ---------------------------------------------------------------------------

/// Inputs of the install job, captured when the user chooses Restart.
#[derive(Clone, Debug)]
pub(crate) struct InstallParams {
    /// The staged version the user confirmed (`UpdateState::Staged`).
    pub release: Version,
    /// The installed bundle this launch runs from (`…/Cometty.app`).
    pub installed: PathBuf,
    pub staging_dir: PathBuf,
    /// How long to wait for the staging lock ([`fsops::JOB_LOCK_PATIENCE`]).
    pub lock_patience: Duration,
}

/// Starts the install job on the detached `update-install` thread with the
/// real process listing.
pub(crate) fn spawn_install(ctx: JobCtx, params: InstallParams) -> io::Result<()> {
    spawn_job("update-install", ctx, move |ctx| {
        run_install(ctx, &procs::executables, &params)
    })
}

/// How the install job ended without a failure.
enum Installed {
    /// The staged bundle is installed; the app quits and relaunches it.
    Swapped,
    /// Other instances run from the installed bundle.
    Blocked { others: usize },
}

/// Runs the install job (`docs/update.md#install-job`): revalidate the staged
/// bundle, count other instances, copy it next to the installed bundle, and
/// swap the two. `list_processes` returns every readable `(pid, executable)`
/// pair ([`procs::executables`] outside tests).
///
/// Ends with exactly one terminal message: `Installed` after the swap;
/// `InstallBlocked` when other instances run, with nothing copied; or
/// `Failed`, with the copy removed and the installed bundle untouched. The
/// staged update is kept unless the swap succeeded.
pub(crate) fn run_install<L>(ctx: &JobCtx, list_processes: &L, params: &InstallParams)
where
    L: Fn() -> Vec<(i32, PathBuf)>,
{
    log::info!(target: TARGET, "installing Cometty {}", params.release);
    match install(list_processes, params) {
        Ok(Installed::Swapped) => ctx.send(WorkerMsg::Installed {
            job: ctx.id,
            bundle: params.installed.clone(),
        }),
        Ok(Installed::Blocked { others }) => {
            log::info!(
                target: TARGET,
                "not installing Cometty {}: {others} other instance(s) running from {}",
                params.release,
                params.installed.display()
            );
            ctx.emit(Event::InstallBlocked { others });
        }
        Err(failure) => {
            log::warn!(target: TARGET, "update install failed: {failure}");
            ctx.emit(Event::Failed(failure));
        }
    }
}

fn install<L>(list_processes: &L, params: &InstallParams) -> Result<Installed, Failure>
where
    L: Fn() -> Vec<(i32, PathBuf)>,
{
    let dir = params.staging_dir.as_path();
    let installed = params.installed.as_path();
    let v = params.release;
    let self_pid = i32::try_from(std::process::id())
        .map_err(|_| Failure::Install("process id out of range".into()))?;

    // Step 1: the lock, then the staged bundle exactly as the check job
    // accepted it.
    let _lock = StagingLock::acquire(dir, params.lock_patience)
        .map_err(|e| Failure::Install(e.to_string()))?;
    let record = valid_staged(dir, v)
        .ok_or_else(|| Failure::Install(format!("no valid staged bundle for Cometty {v}")))?;
    let staged_app = dir.join(&record.extract_dir).join(BUNDLE_DIR);

    // Step 2: other instances, counted with the lock held and before
    // anything is copied.
    let others = count_instances(&list_processes(), installed, self_pid);
    if others > 0 {
        return Ok(Installed::Blocked { others });
    }

    // Step 3: the bundle's folder may have become read-only since launch.
    if !fsops::parent_writable(installed) {
        return Err(Failure::Install(format!(
            "{} is not in a writable folder",
            installed.display()
        )));
    }

    // Step 4: a fresh copy next to the bundle, so the swap is a rename on
    // one volume. The guard removes it on any failure below.
    let (incoming, backup) = sibling_names(installed);
    remove_entry(&incoming).map_err(|e| io_failure("removing stale incoming copy", &e))?;
    let incoming_copy = RemoveOnDrop::new(incoming.clone());
    fsops::ditto_copy(&staged_app, &incoming)
        .map_err(|e| Failure::Install(format!("copying the staged bundle: {e}")))?;
    let facts =
        fsops::bundle_facts(&incoming).map_err(|e| io_failure("reading incoming copy", &e))?;
    validate_bundle(&facts, v).map_err(Failure::from)?;

    // Step 5: the swap. The previous bundle, which this process still runs
    // from, stays at a sibling name until the next launch's housekeeping.
    let previous = fsops::replace_bundle(installed, &incoming, &backup)
        .map_err(|e| Failure::Install(format!("replacing {}: {e}", installed.display())))?;
    incoming_copy.disarm();
    log::info!(
        target: TARGET,
        "installed Cometty {v} at {}; the previous bundle is at {} until the next launch",
        installed.display(),
        previous.display()
    );
    Ok(Installed::Swapped)
}
#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(10);

    fn ctx(id: u64) -> (JobCtx, mpsc::Receiver<WorkerMsg>, Arc<AtomicUsize>) {
        let (tx, rx) = mpsc::channel();
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = wakes.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            counter.fetch_add(1, Ordering::AcqRel);
        });
        let ctx = JobCtx::new(id, Arc::new(AtomicBool::new(false)), tx, wake);
        (ctx, rx, wakes)
    }

    /// Waits until the job's `JobCtx` (the channel's only sender) is dropped,
    /// and asserts nothing else arrives first.
    ///
    /// `send` delivers before it wakes, so a receiver can see `Finished`
    /// while the guard's wake is still running. The guard drops its context,
    /// and with it the sender, only after the wake returns; the disconnect
    /// therefore orders every wake before the counter is read.
    fn expect_disconnect(rx: &mpsc::Receiver<WorkerMsg>) {
        match rx.recv_timeout(TIMEOUT) {
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
            other => panic!("expected the job's sender to drop, got {other:?}"),
        }
    }

    #[test]
    fn normal_return_reports_finished_without_panic() {
        let (ctx, rx, wakes) = ctx(7);
        spawn_job("update-test", ctx, |_| {}).unwrap();
        match rx.recv_timeout(TIMEOUT).unwrap() {
            WorkerMsg::Finished { job, panicked } => {
                assert_eq!(job, 7);
                assert!(!panicked);
            }
            other => panic!("unexpected message {other:?}"),
        }
        // The guard was the last sender; nothing follows Finished.
        expect_disconnect(&rx);
        assert_eq!(wakes.load(Ordering::Acquire), 1);
    }

    #[test]
    fn panic_reports_finished_with_panic() {
        let (ctx, rx, wakes) = ctx(3);
        spawn_job("update-test-panic", ctx, |_| {
            panic!("deliberate test panic")
        })
        .unwrap();
        match rx.recv_timeout(TIMEOUT).unwrap() {
            WorkerMsg::Finished { job, panicked } => {
                assert_eq!(job, 3);
                assert!(panicked);
            }
            other => panic!("unexpected message {other:?}"),
        }
        expect_disconnect(&rx);
        assert_eq!(wakes.load(Ordering::Acquire), 1);
    }

    #[test]
    fn emit_sends_then_wakes_before_finished() {
        let (ctx, rx, wakes) = ctx(1);
        spawn_job("update-test-emit", ctx, |ctx| {
            ctx.emit(Event::UpToDate);
            assert_eq!(std::thread::current().name(), Some("update-test-emit"));
        })
        .unwrap();
        match rx.recv_timeout(TIMEOUT).unwrap() {
            WorkerMsg::Event { job, event } => {
                assert_eq!(job, 1);
                assert_eq!(event, Event::UpToDate);
            }
            other => panic!("unexpected message {other:?}"),
        }
        assert!(matches!(
            rx.recv_timeout(TIMEOUT).unwrap(),
            WorkerMsg::Finished {
                job: 1,
                panicked: false
            }
        ));
        expect_disconnect(&rx);
        assert_eq!(wakes.load(Ordering::Acquire), 2);
    }

    #[test]
    fn emit_on_closed_channel_does_not_wake() {
        let (ctx, rx, wakes) = ctx(1);
        drop(rx);
        ctx.emit(Event::UpToDate);
        assert_eq!(wakes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn cancelled_reflects_the_flag() {
        let (ctx, _rx, _wakes) = ctx(1);
        assert!(!ctx.cancelled());
        ctx.cancel.store(true, Ordering::Release);
        assert!(ctx.cancelled());
        ctx.cancel.store(false, Ordering::Release);
        assert!(!ctx.cancelled());
    }
}

/// The check pipeline driven by a scripted [`Fetch`]; no test opens a socket.
/// The fixture and bundle helpers are shared with the controller's
/// end-to-end shared-staging tests (`app::update`).
#[cfg(all(test, unix))]
pub(crate) mod check_tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::io::Cursor;
    use std::os::unix::fs::PermissionsExt;

    use serde_json::json;

    use super::*;
    use crate::update::checksum::ChecksumError;
    use crate::update::checksum::sha256;
    use crate::update::http::{accept, user_agent};
    use crate::update::release::{archive_name, checksum_name};

    pub(crate) const RUNNING: Version = Version {
        major: 1,
        minor: 2,
        patch: 3,
    };
    pub(crate) const NEW: Version = Version {
        major: 1,
        minor: 3,
        patch: 0,
    };
    const ARCHIVE_URL: &str = "https://github.com/jjunac/cometty/releases/download/v1.3.0/Cometty-1.3.0-macos-universal.zip";
    const CHECKSUM_URL: &str = "https://github.com/jjunac/cometty/releases/download/v1.3.0/Cometty-1.3.0-macos-universal.zip.sha256";
    const TEMPLATE: &[u8] = include_bytes!("../../packaging/macos/Info.plist");

    /// When a [`Tripwire`] body sets the cancel flag.
    #[derive(Clone, Copy, Debug)]
    enum Trip {
        /// After the first read returns data: cancels mid-body.
        FirstRead,
        /// When the body reports end of input: cancels between requests.
        Eof,
    }

    /// A scripted response.
    #[derive(Clone)]
    enum Reply {
        Body(u16, Vec<u8>),
        Fail(Failure),
        /// A 200 whose body sets `flag` at `at`.
        Trip {
            body: Vec<u8>,
            flag: Arc<AtomicBool>,
            at: Trip,
        },
    }

    struct Tripwire {
        data: Cursor<Vec<u8>>,
        flag: Arc<AtomicBool>,
        at: Trip,
    }

    impl Read for Tripwire {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.data.read(buf)?;
            let trip = match self.at {
                Trip::FirstRead => n > 0,
                Trip::Eof => n == 0,
            };
            if trip {
                self.flag.store(true, Ordering::Release);
            }
            Ok(n)
        }
    }

    /// One observed request. A fake sees no wire headers, so `headers`
    /// models what `UreqFetcher` sends for `kind` from the same helpers its
    /// agent is configured with; the agent config itself is asserted in
    /// `http.rs`.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Request {
        url: String,
        kind: RequestKind,
        headers: Vec<(&'static str, String)>,
    }

    /// Serves scripted replies by URL and records every request in order.
    #[derive(Default)]
    struct FakeFetch {
        replies: HashMap<String, Reply>,
        requests: RefCell<Vec<Request>>,
    }

    impl FakeFetch {
        fn with(mut self, url: &str, reply: Reply) -> Self {
            self.replies.insert(url.to_owned(), reply);
            self
        }

        fn kinds(&self) -> Vec<RequestKind> {
            self.requests.borrow().iter().map(|r| r.kind).collect()
        }
    }

    impl Fetch for FakeFetch {
        fn get(&self, url: &str, kind: RequestKind) -> Result<FetchResponse, Failure> {
            self.requests.borrow_mut().push(Request {
                url: url.to_owned(),
                kind,
                headers: vec![
                    ("User-Agent", user_agent()),
                    ("Accept", accept(kind).into()),
                ],
            });
            let reply = self
                .replies
                .get(url)
                .unwrap_or_else(|| panic!("unexpected request for {url}"))
                .clone();
            let (status, body): (u16, Box<dyn Read + Send>) = match reply {
                Reply::Body(status, body) => (status, Box::new(Cursor::new(body))),
                Reply::Fail(failure) => return Err(failure),
                Reply::Trip { body, flag, at } => (
                    200,
                    Box::new(Tripwire {
                        data: Cursor::new(body),
                        flag,
                        at,
                    }),
                ),
            };
            Ok(FetchResponse { status, body })
        }
    }

    /// A temp root holding `staging/` and `Applications/Cometty.app`'s
    /// (writable) parent, removed on drop.
    pub(crate) struct Fixture {
        pub(crate) root: PathBuf,
        cancel: Arc<AtomicBool>,
    }

    impl Fixture {
        pub(crate) fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "cometty-worker-{tag}-{}-{}",
                std::process::id(),
                now_nanos()
            ));
            fs::create_dir_all(root.join("Applications")).unwrap();
            Self {
                root,
                cancel: Arc::new(AtomicBool::new(false)),
            }
        }

        pub(crate) fn staging(&self) -> PathBuf {
            self.root.join("staging")
        }

        fn self_update(&self) -> Mode {
            Mode::SelfUpdate {
                bundle: self.root.join("Applications/Cometty.app"),
            }
        }

        /// Runs the pipeline synchronously and returns every message sent.
        fn run(&self, fetch: &FakeFetch, mode: Mode) -> Vec<WorkerMsg> {
            self.run_patient(fetch, mode, Duration::ZERO)
        }

        /// [`Self::run`] waiting up to `patience` for the staging lock.
        fn run_patient(&self, fetch: &FakeFetch, mode: Mode, patience: Duration) -> Vec<WorkerMsg> {
            let (tx, rx) = mpsc::channel();
            let ctx = JobCtx::new(9, self.cancel.clone(), tx, Arc::new(|| {}));
            let params = CheckParams {
                running: RUNNING,
                mode,
                staging_dir: Some(self.staging()),
                lock_patience: patience,
            };
            run_check(&ctx, fetch, &params);
            drop(ctx);
            rx.try_iter().collect()
        }

        /// Sorted entry names of the staging directory.
        pub(super) fn listing(&self) -> Vec<String> {
            let mut names = list_names(&self.staging()).unwrap();
            names.sort();
            names
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn events(msgs: &[WorkerMsg]) -> Vec<Event> {
        msgs.iter()
            .filter_map(|m| match m {
                WorkerMsg::Event { job: 9, event } => Some(event.clone()),
                _ => None,
            })
            .collect()
    }

    fn event_names(msgs: &[WorkerMsg]) -> Vec<&'static str> {
        events(msgs).iter().map(Event::name).collect()
    }

    fn last_failure(msgs: &[WorkerMsg]) -> Failure {
        match events(msgs).pop() {
            Some(Event::Failed(failure)) => failure,
            other => panic!("expected Failed last, got {other:?}"),
        }
    }

    /// A feed for v1.3.0 whose assets advertise the given sizes.
    fn feed(archive_size: usize, checksum_size: usize) -> Reply {
        let doc = json!({
            "tag_name": "v1.3.0",
            "html_url": "https://github.com/jjunac/cometty/releases/tag/v1.3.0",
            "assets": [
                { "name": archive_name(NEW), "size": archive_size, "browser_download_url": ARCHIVE_URL },
                { "name": checksum_name(NEW), "size": checksum_size, "browser_download_url": CHECKSUM_URL },
            ],
        });
        Reply::Body(200, serde_json::to_vec(&doc).unwrap())
    }

    fn checksum_line(data: &[u8]) -> Vec<u8> {
        let digest = sha256(data);
        format!("{}  {}\n", hex_lower(&digest), archive_name(NEW)).into_bytes()
    }

    /// A fetcher serving the feed, `archive`, and `checksum`, with the feed
    /// advertising their real sizes.
    fn serving(archive: Reply, archive_len: usize, checksum: Vec<u8>) -> FakeFetch {
        FakeFetch::default()
            .with(RELEASE_FEED_URL, feed(archive_len, checksum.len()))
            .with(ARCHIVE_URL, archive)
            .with(CHECKSUM_URL, Reply::Body(200, checksum))
    }

    fn not_a_zip() -> Vec<u8> {
        b"not really a zip archive".repeat(100)
    }

    /// `root/Cometty.app` with an XML plist for `v` and a 0755 executable.
    pub(crate) fn write_bundle(root: &Path, v: Version) {
        let contents = root.join("Cometty.app/Contents");
        fs::create_dir_all(contents.join("MacOS")).unwrap();
        let plist = std::str::from_utf8(TEMPLATE)
            .unwrap()
            .replace("__VERSION__", &v.to_string())
            .replace("__BUILD__", &v.to_string());
        fs::write(contents.join("Info.plist"), plist).unwrap();
        let exe = contents.join("MacOS/cometty");
        fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn feed_request_is_one_get_with_only_user_agent_and_accept() {
        let fx = Fixture::new("feed-headers");
        let fetch = FakeFetch::default().with(RELEASE_FEED_URL, Reply::Body(404, Vec::new()));
        fx.run(&fetch, fx.self_update());
        let requests = fetch.requests.borrow();
        assert_eq!(
            *requests,
            [Request {
                url: RELEASE_FEED_URL.into(),
                kind: RequestKind::Feed,
                headers: vec![
                    (
                        "User-Agent",
                        format!("cometty/{}", crate::update::RUNNING_VERSION)
                    ),
                    ("Accept", "application/vnd.github+json".into()),
                ],
            }]
        );
        assert!(RELEASE_FEED_URL.starts_with("https://api.github.com/"));
    }

    #[test]
    fn not_found_is_up_to_date() {
        let fx = Fixture::new("404");
        let fetch = FakeFetch::default().with(RELEASE_FEED_URL, Reply::Body(404, Vec::new()));
        assert_eq!(events(&fx.run(&fetch, fx.self_update())), [Event::UpToDate]);
    }

    #[test]
    fn server_error_fails_with_status() {
        let fx = Fixture::new("500");
        let fetch = FakeFetch::default().with(RELEASE_FEED_URL, Reply::Body(500, Vec::new()));
        assert_eq!(
            events(&fx.run(&fetch, fx.self_update())),
            [Event::Failed(Failure::Status(500))]
        );
    }

    #[test]
    fn transport_error_fails_without_further_requests() {
        let fx = Fixture::new("timeout");
        let fetch = FakeFetch::default().with(RELEASE_FEED_URL, Reply::Fail(Failure::Timeout));
        assert_eq!(
            events(&fx.run(&fetch, fx.self_update())),
            [Event::Failed(Failure::Timeout)]
        );
        assert_eq!(fetch.kinds(), [RequestKind::Feed]);
    }

    #[test]
    fn oversized_feed_fails_too_large() {
        let fx = Fixture::new("big-feed");
        let body = vec![b' '; FEED_CAP as usize + 1];
        let fetch = FakeFetch::default().with(RELEASE_FEED_URL, Reply::Body(200, body));
        assert_eq!(
            events(&fx.run(&fetch, fx.self_update())),
            [Event::Failed(Failure::TooLarge {
                what: RequestKind::Feed,
                cap: FEED_CAP,
            })]
        );
    }

    #[test]
    fn notify_only_mode_makes_exactly_one_request() {
        let fx = Fixture::new("notify");
        let data = not_a_zip();
        let fetch = serving(
            Reply::Body(200, data.clone()),
            data.len(),
            checksum_line(&data),
        );
        let msgs = fx.run(&fetch, Mode::NotifyOnly(NotifyReason::NotBundled));
        assert_eq!(event_names(&msgs), ["Newer"]);
        assert!(!msgs.iter().any(|m| matches!(m, WorkerMsg::Mode { .. })));
        assert_eq!(fetch.kinds(), [RequestKind::Feed]);
        assert!(!fx.staging().exists(), "notify-only never touches staging");
    }

    /// A staged bundle of `NEW` recording `digest`; returns its extract name.
    fn stage_with_digest(fx: &Fixture, digest: &Digest) -> String {
        let extract = extract_name(NEW, 1, 2);
        write_bundle(&fx.staging().join(&extract), NEW);
        let record = StagedRecord {
            schema: SCHEMA,
            version: NEW,
            sha256: hex_lower(digest),
            extract_dir: extract.clone(),
        };
        fsops::write_json_atomic(&fx.staging().join(STAGED), &record).unwrap();
        extract
    }

    #[test]
    fn a_staged_bundle_with_the_published_digest_needs_no_archive_request() {
        let fx = Fixture::new("already");
        let data = not_a_zip();
        let extract = stage_with_digest(&fx, &sha256(&data));
        let fetch = serving(
            Reply::Body(200, data.clone()),
            data.len(),
            checksum_line(&data),
        );
        let msgs = fx.run(&fetch, fx.self_update());
        assert_eq!(event_names(&msgs), ["Newer", "AlreadyStaged"]);
        assert_eq!(events(&msgs)[1], Event::AlreadyStaged(NEW));
        assert!(msgs.iter().any(|m| matches!(
            m,
            WorkerMsg::Mode {
                job: 9,
                mode: Mode::SelfUpdate { .. }
            }
        )));
        assert_eq!(fetch.kinds(), [RequestKind::Feed, RequestKind::Checksum]);
        assert_eq!(fx.listing(), [extract.as_str(), "lock", STAGED]);
    }

    /// A release re-uploaded under the same tag publishes a new digest: the
    /// stale staged bundle is dropped and the archive is downloaded again.
    #[test]
    fn a_staged_bundle_with_another_digest_is_replaced() {
        let fx = Fixture::new("republished");
        stage_with_digest(&fx, &sha256(b"the first upload"));
        let data = not_a_zip();
        let fetch = serving(
            Reply::Body(200, data.clone()),
            data.len(),
            checksum_line(&data),
        );
        let msgs = fx.run(&fetch, fx.self_update());
        // Not a real zip, so extraction fails after the digest matched.
        assert_eq!(
            event_names(&msgs),
            ["Newer", "DownloadStarted", "VerifyStarted", "Failed"]
        );
        assert!(matches!(last_failure(&msgs), Failure::Extract(_)));
        assert_eq!(
            fetch.kinds(),
            [
                RequestKind::Feed,
                RequestKind::Checksum,
                RequestKind::Archive
            ]
        );
        assert_eq!(fx.listing(), ["lock"], "stale record and bundle removed");
    }

    /// While another job or instance holds the staging lock for the whole
    /// patience window, the job reports only `Newer`, requests no
    /// asset, emits no `Failed`, and leaves the holder's files untouched.
    #[test]
    fn held_lock_past_its_patience_ends_quietly_without_downloads() {
        use std::time::Instant;

        let fx = Fixture::new("busy");
        let held = StagingLock::acquire(&fx.staging(), Duration::ZERO).unwrap();
        let partial = partial_name(NEW);
        fs::write(fx.staging().join(&partial), b"the holder's download").unwrap();

        let data = not_a_zip();
        let fetch = serving(
            Reply::Body(200, data.clone()),
            data.len(),
            checksum_line(&data),
        );
        let patience = Duration::from_millis(300);
        let started = Instant::now();
        let msgs = fx.run_patient(&fetch, fx.self_update(), patience);
        assert!(started.elapsed() >= patience, "waited out its patience");

        assert_eq!(event_names(&msgs), ["Newer"]);
        assert!(msgs.iter().any(|m| matches!(
            m,
            WorkerMsg::Mode {
                job: 9,
                mode: Mode::SelfUpdate { .. }
            }
        )));
        assert_eq!(fetch.kinds(), [RequestKind::Feed]);
        assert_eq!(fx.listing(), [partial.as_str(), "lock"]);
        assert_eq!(
            fs::read(fx.staging().join(&partial)).unwrap(),
            b"the holder's download"
        );
        drop(held);
    }

    #[test]
    fn oversized_archive_deletes_the_partial_and_stale_partials() {
        let fx = Fixture::new("big-archive");
        fs::create_dir_all(fx.staging()).unwrap();
        fs::write(fx.staging().join("download-1.2.9.zip.part"), b"stale").unwrap();
        let data = not_a_zip();
        let advertised = data.len() - 1;
        let fetch = serving(
            Reply::Body(200, data.clone()),
            advertised,
            checksum_line(&data),
        );
        let msgs = fx.run(&fetch, fx.self_update());
        assert_eq!(
            last_failure(&msgs),
            Failure::TooLarge {
                what: RequestKind::Archive,
                cap: advertised as u64,
            }
        );
        assert_eq!(fx.listing(), ["lock"]);
        assert_eq!(
            fetch.kinds(),
            [
                RequestKind::Feed,
                RequestKind::Checksum,
                RequestKind::Archive
            ]
        );
    }

    #[test]
    fn truncated_archive_deletes_the_partial() {
        let fx = Fixture::new("short-archive");
        let data = not_a_zip();
        let fetch = serving(
            Reply::Body(200, data.clone()),
            data.len() + 1,
            checksum_line(&data),
        );
        let msgs = fx.run(&fetch, fx.self_update());
        assert_eq!(event_names(&msgs), ["Newer", "DownloadStarted", "Failed"]);
        assert!(matches!(last_failure(&msgs), Failure::Network(_)));
        assert_eq!(fx.listing(), ["lock"]);
    }

    #[test]
    fn checksum_format_error_fails_before_the_archive_is_requested() {
        let fx = Fixture::new("bad-sum");
        let data = not_a_zip();
        let fetch = serving(
            Reply::Body(200, data.clone()),
            data.len(),
            b"not a checksum\n".to_vec(),
        );
        let msgs = fx.run(&fetch, fx.self_update());
        assert_eq!(event_names(&msgs), ["Newer", "Failed"]);
        assert_eq!(
            last_failure(&msgs),
            Failure::Checksum(ChecksumError::Format)
        );
        let requests = fetch.requests.borrow();
        let order: Vec<(&str, RequestKind)> =
            requests.iter().map(|r| (r.url.as_str(), r.kind)).collect();
        assert_eq!(
            order,
            [
                (RELEASE_FEED_URL, RequestKind::Feed),
                (CHECKSUM_URL, RequestKind::Checksum),
            ]
        );
        assert_eq!(fx.listing(), ["lock"]);
    }

    #[test]
    fn digest_mismatch_deletes_the_archive() {
        let fx = Fixture::new("mismatch");
        let data = not_a_zip();
        let fetch = serving(
            Reply::Body(200, data.clone()),
            data.len(),
            checksum_line(b"other bytes"),
        );
        let msgs = fx.run(&fetch, fx.self_update());
        assert_eq!(
            event_names(&msgs),
            ["Newer", "DownloadStarted", "VerifyStarted", "Failed"]
        );
        let (actual, expected) = (sha256(&data), sha256(b"other bytes"));
        assert_eq!(last_failure(&msgs), Failure::Digest { expected, actual });
        assert_eq!(fx.listing(), ["lock"]);
    }

    #[test]
    fn cancel_after_the_feed_stops_before_the_next_request() {
        let fx = Fixture::new("cancel-feed");
        let data = not_a_zip();
        let Reply::Body(_, feed_body) = feed(data.len(), 100) else {
            unreachable!()
        };
        let feed_reply = Reply::Trip {
            body: feed_body,
            flag: fx.cancel.clone(),
            at: Trip::Eof,
        };
        let fetch = FakeFetch::default()
            .with(RELEASE_FEED_URL, feed_reply)
            .with(ARCHIVE_URL, Reply::Body(200, data));
        let msgs = fx.run(&fetch, fx.self_update());
        assert_eq!(event_names(&msgs), ["Newer"]);
        assert_eq!(fetch.kinds(), [RequestKind::Feed]);
        assert!(!fx.staging().exists());
    }

    #[test]
    fn cancel_mid_archive_removes_the_partial() {
        let fx = Fixture::new("cancel-archive");
        // Larger than one 64 KiB chunk, so the flag is seen mid-body.
        let data = vec![7u8; 200 * 1024];
        let archive = Reply::Trip {
            body: data.clone(),
            flag: fx.cancel.clone(),
            at: Trip::FirstRead,
        };
        let fetch = serving(archive, data.len(), checksum_line(&data));
        let msgs = fx.run(&fetch, fx.self_update());
        assert_eq!(event_names(&msgs), ["Newer", "DownloadStarted"]);
        assert_eq!(
            fetch.kinds(),
            [
                RequestKind::Feed,
                RequestKind::Checksum,
                RequestKind::Archive
            ]
        );
        assert_eq!(fx.listing(), ["lock"]);
    }

    #[test]
    fn unwritable_bundle_parent_downgrades_to_notify_only() {
        let fx = Fixture::new("not-writable");
        let data = not_a_zip();
        let fetch = serving(
            Reply::Body(200, data.clone()),
            data.len(),
            checksum_line(&data),
        );
        let mode = Mode::SelfUpdate {
            bundle: fx.root.join("missing/Cometty.app"),
        };
        let msgs = fx.run(&fetch, mode);
        assert!(msgs.iter().any(|m| matches!(
            m,
            WorkerMsg::Mode {
                mode: Mode::NotifyOnly(NotifyReason::NotWritable),
                ..
            }
        )));
        assert_eq!(fetch.kinds(), [RequestKind::Feed]);
        assert!(!fx.staging().exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn success_path_stages_the_bundle_then_reports_already_staged() {
        use std::process::Command;

        let fx = Fixture::new("success");
        let src = fx.root.join("src");
        write_bundle(&src, NEW);
        let zip = fx.root.join(archive_name(NEW));
        let status = Command::new("/usr/bin/ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(src.join("Cometty.app"))
            .arg(&zip)
            .status()
            .unwrap();
        assert!(status.success());
        let data = fs::read(&zip).unwrap();
        let sum = checksum_line(&data);

        let fetch = serving(Reply::Body(200, data.clone()), data.len(), sum.clone());
        let msgs = fx.run(&fetch, fx.self_update());
        assert_eq!(
            events(&msgs),
            [
                events(&msgs)[0].clone(),
                Event::DownloadStarted,
                Event::VerifyStarted,
                Event::Staged(NEW),
            ]
        );
        assert!(matches!(&events(&msgs)[0], Event::Newer(r) if r.version == NEW));

        let record = read_staged(&fx.staging()).expect("staged.json");
        let digest = sha256(&data);
        assert_eq!(record.version, NEW);
        assert_eq!(record.sha256, hex_lower(&digest));
        assert!(matches!(classify_entry(&record.extract_dir), Entry::Extract(v, _) if v == NEW));
        assert_eq!(
            fx.listing(),
            [record.extract_dir.as_str(), "lock", STAGED],
            "archive removed, exactly one extract kept"
        );
        let facts = fsops::gather_facts(&fx.staging().join(&record.extract_dir)).unwrap();
        assert_eq!(validate_bundle(&facts, NEW), Ok(()));

        // A second check finds the staged bundle and downloads nothing.
        let again = serving(Reply::Body(200, data.clone()), data.len(), sum);
        let msgs = fx.run(&again, fx.self_update());
        assert_eq!(event_names(&msgs), ["Newer", "AlreadyStaged"]);
        assert_eq!(again.kinds(), [RequestKind::Feed, RequestKind::Checksum]);
    }
}

/// The housekeeping job on temp staging directories and bundle parents.
#[cfg(all(test, unix))]
mod housekeeping_tests {
    use super::check_tests::{Fixture, NEW, RUNNING};
    use super::*;

    const OLDER: Version = Version {
        major: 1,
        minor: 2,
        patch: 0,
    };

    fn installed(fx: &Fixture) -> PathBuf {
        fx.root.join("Applications/Cometty.app")
    }

    fn params(fx: &Fixture) -> HousekeepingParams {
        HousekeepingParams {
            running: RUNNING,
            installed: installed(fx),
            staging_dir: fx.staging(),
            lock_patience: Duration::ZERO,
        }
    }

    /// A directory standing in for a bundle; housekeeping never inspects it.
    fn make_bundle(path: &Path) {
        fs::create_dir_all(path.join("Contents/MacOS")).unwrap();
        fs::write(path.join("Contents/MacOS/cometty"), b"exe").unwrap();
    }

    fn staging_with_bundle(tag: &str) -> Fixture {
        let fx = Fixture::new(tag);
        fs::create_dir_all(fx.staging()).unwrap();
        make_bundle(&installed(&fx));
        fx
    }

    /// `staged.json` for `v` plus its extract directory; returns the name.
    fn write_staged(fx: &Fixture, v: Version, nanos: u128) -> String {
        let extract = extract_name(v, 1, nanos);
        make_bundle(&fx.staging().join(&extract).join("Cometty.app"));
        let record = StagedRecord {
            schema: SCHEMA,
            version: v,
            sha256: "ab".repeat(32),
            extract_dir: extract.clone(),
        };
        fsops::write_json_atomic(&fx.staging().join(STAGED), &record).unwrap();
        extract
    }

    fn touch(fx: &Fixture, name: &str) {
        fs::write(fx.staging().join(name), b"data").unwrap();
    }

    #[test]
    fn the_previous_bundle_and_an_interrupted_copy_are_removed() {
        let fx = staging_with_bundle("siblings");
        let (incoming, backup) = sibling_names(&installed(&fx));
        make_bundle(&incoming);
        make_bundle(&backup);
        // The bundle just installed was staged for the running version.
        write_staged(&fx, RUNNING, 2);

        run_housekeeping(&params(&fx));

        assert!(!incoming.exists() && !backup.exists());
        assert!(installed(&fx).join("Contents/MacOS/cometty").exists());
        assert_eq!(fx.listing(), ["lock"], "staged bundle removed");
    }

    #[test]
    fn older_staged_bundles_partials_and_orphans_are_removed() {
        let fx = staging_with_bundle("older");
        write_staged(&fx, OLDER, 2);
        let orphan = extract_name(NEW, 1, 3);
        make_bundle(&fx.staging().join(&orphan).join("Cometty.app"));
        touch(&fx, &partial_name(OLDER));
        touch(&fx, &partial_name(NEW));

        run_housekeeping(&params(&fx));
        assert_eq!(fx.listing(), ["lock"]);

        // A newer staged bundle is kept; a second extract of it is not.
        let kept = write_staged(&fx, NEW, 4);
        let extra = extract_name(NEW, 1, 5);
        fs::create_dir_all(fx.staging().join(&extra)).unwrap();
        run_housekeeping(&params(&fx));
        let mut expected = vec![kept, "lock".into(), STAGED.into()];
        expected.sort();
        assert_eq!(fx.listing(), expected);
    }

    #[test]
    fn unknown_files_survive() {
        let fx = staging_with_bundle("unknown");
        let unknown = [
            "notes.txt",
            ".staged.json.123.tmp",
            "download-1.2.zip.part",
            "Cometty.app",
            "extract-1.3.0",
            "install.json",
        ];
        for name in unknown {
            touch(&fx, name);
        }

        run_housekeeping(&params(&fx));

        let mut expected: Vec<String> = unknown.iter().map(|s| s.to_string()).collect();
        expected.push("lock".into());
        expected.sort();
        assert_eq!(fx.listing(), expected);
    }

    #[test]
    fn a_missing_staging_directory_or_busy_lock_changes_nothing() {
        let fx = Fixture::new("no-staging");
        make_bundle(&installed(&fx));
        let (incoming, _) = sibling_names(&installed(&fx));
        make_bundle(&incoming);
        run_housekeeping(&params(&fx));
        assert!(
            !fx.staging().exists(),
            "the job creates no staging directory"
        );
        assert!(incoming.exists());

        fs::create_dir_all(fx.staging()).unwrap();
        touch(&fx, &partial_name(OLDER));
        let held = StagingLock::acquire(&fx.staging(), Duration::ZERO).unwrap();
        run_housekeeping(&params(&fx));
        assert!(incoming.exists());
        assert_eq!(fx.listing(), [partial_name(OLDER).as_str(), "lock"]);
        drop(held);
    }
}

/// The install job on temp staging directories and bundle parents, with a
/// fake process listing.
#[cfg(all(test, unix))]
mod install_tests {
    use std::cell::Cell;

    use super::check_tests::{Fixture, NEW, write_bundle};
    use super::*;

    const JOB: u64 = 5;

    fn installed(fx: &Fixture) -> PathBuf {
        fx.root.join("Applications/Cometty.app")
    }

    fn params(fx: &Fixture) -> InstallParams {
        InstallParams {
            release: NEW,
            installed: installed(fx),
            staging_dir: fx.staging(),
            lock_patience: Duration::ZERO,
        }
    }

    /// A valid staged bundle for [`NEW`]; returns its extract directory.
    fn stage(fx: &Fixture) -> String {
        let extract = extract_name(NEW, 1, 2);
        write_bundle(&fx.staging().join(&extract), NEW);
        let record = StagedRecord {
            schema: SCHEMA,
            version: NEW,
            sha256: "ab".repeat(32),
            extract_dir: extract.clone(),
        };
        fsops::write_json_atomic(&fx.staging().join(STAGED), &record).unwrap();
        extract
    }

    /// The installed bundle: an older build marked by a `label` file.
    fn install_old(fx: &Fixture) {
        let app = installed(fx);
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::write(app.join("label"), b"old").unwrap();
    }

    /// A process listing with no other Cometty instance.
    fn no_processes() -> Vec<(i32, PathBuf)> {
        Vec::new()
    }

    fn run<L: Fn() -> Vec<(i32, PathBuf)>>(list: &L, params: &InstallParams) -> Vec<WorkerMsg> {
        let (tx, rx) = mpsc::channel();
        let ctx = JobCtx::new(JOB, Arc::new(AtomicBool::new(false)), tx, Arc::new(|| {}));
        run_install(&ctx, list, params);
        drop(ctx);
        rx.try_iter().collect()
    }

    /// The job's only message is `Failed`.
    fn only_failure(msgs: &[WorkerMsg]) -> Failure {
        match msgs {
            [
                WorkerMsg::Event {
                    job: JOB,
                    event: Event::Failed(failure),
                },
            ] => failure.clone(),
            other => panic!("expected exactly one Failed, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_or_mismatched_staged_bundle_fails_without_copying() {
        let fx = Fixture::new("install-unstaged");
        install_old(&fx);
        let msgs = run(&no_processes, &params(&fx));
        assert!(matches!(only_failure(&msgs), Failure::Install(_)));

        // Staged, but not the version the user confirmed.
        let extract = stage(&fx);
        let mut other = params(&fx);
        other.release = Version {
            major: 1,
            minor: 4,
            patch: 0,
        };
        let msgs = run(&no_processes, &other);
        assert!(matches!(only_failure(&msgs), Failure::Install(_)));

        assert_eq!(fx.listing(), [extract.as_str(), "lock", STAGED]);
        assert!(!sibling_names(&installed(&fx)).0.exists());
    }

    #[test]
    fn a_busy_lock_fails_instead_of_finishing_silently() {
        let fx = Fixture::new("install-busy");
        stage(&fx);
        let held = StagingLock::acquire(&fx.staging(), Duration::ZERO).unwrap();
        let msgs = run(&no_processes, &params(&fx));
        assert!(matches!(only_failure(&msgs), Failure::Install(_)));
        drop(held);
    }

    #[test]
    fn an_unwritable_bundle_parent_fails_and_keeps_the_staged_update() {
        let fx = Fixture::new("install-not-writable");
        let extract = stage(&fx);
        let mut params = params(&fx);
        params.installed = fx.root.join("missing/Cometty.app");
        let msgs = run(&no_processes, &params);
        assert!(matches!(only_failure(&msgs), Failure::Install(m) if m.contains("writable")));
        assert_eq!(fx.listing(), [extract.as_str(), "lock", STAGED]);
    }

    /// Another instance of the installed bundle blocks the install before
    /// the copy; this process, other bundles, and look-alike paths do not
    /// count.
    #[test]
    fn other_instances_block_the_install_with_nothing_copied() {
        let fx = Fixture::new("install-blocked");
        install_old(&fx);
        let extract = stage(&fx);
        let params = params(&fx);
        let self_pid = std::process::id() as i32;
        let exe = params.installed.join("Contents/MacOS/cometty");
        let lookalike = fx
            .root
            .join("Applications/Cometty.app2/Contents/MacOS/cometty");
        let listings = Cell::new(0);
        let list = || {
            listings.set(listings.get() + 1);
            vec![
                (self_pid, exe.clone()),
                (self_pid + 1, exe.clone()),
                (self_pid + 2, exe.clone()),
                (self_pid + 3, lookalike.clone()),
                (self_pid + 4, PathBuf::from("/bin/zsh")),
            ]
        };
        let msgs = run(&list, &params);

        assert_eq!(listings.get(), 1);
        assert!(
            matches!(
                msgs.as_slice(),
                [WorkerMsg::Event {
                    job: JOB,
                    event: Event::InstallBlocked { others: 2 },
                }]
            ),
            "InstallBlocked is the only message: {msgs:?}"
        );
        assert!(!sibling_names(&installed(&fx)).0.exists(), "no copy");
        assert_eq!(fs::read(installed(&fx).join("label")).unwrap(), b"old");
        assert_eq!(fx.listing(), [extract.as_str(), "lock", STAGED]);
    }

    /// Off macOS `ditto` is unavailable, so the job fails at the copy with
    /// the installed bundle untouched.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_copy_fails_off_macos_and_changes_nothing() {
        let fx = Fixture::new("install-off-macos");
        install_old(&fx);
        let extract = stage(&fx);
        let msgs = run(&no_processes, &params(&fx));
        assert!(matches!(only_failure(&msgs), Failure::Install(_)));
        assert!(!sibling_names(&installed(&fx)).0.exists());
        assert_eq!(fs::read(installed(&fx).join("label")).unwrap(), b"old");
        assert_eq!(fx.listing(), [extract.as_str(), "lock", STAGED]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_successful_install_swaps_the_bundle_and_reports_installed() {
        let fx = Fixture::new("install-ok");
        install_old(&fx);
        let extract = stage(&fx);
        let params = params(&fx);
        let (incoming, _) = sibling_names(&params.installed);
        // A stale incoming copy is replaced, not merged into.
        fs::create_dir_all(incoming.join("stale")).unwrap();

        let msgs = run(&no_processes, &params);
        assert!(
            matches!(
                msgs.as_slice(),
                [WorkerMsg::Installed { job: JOB, bundle }] if *bundle == params.installed
            ),
            "{msgs:?}"
        );

        // The new bundle is installed; the old one waits at the sibling.
        let facts = fsops::bundle_facts(&params.installed).unwrap();
        assert_eq!(validate_bundle(&facts, NEW), Ok(()));
        assert_eq!(fs::read(incoming.join("label")).unwrap(), b"old");
        assert!(!incoming.join("stale").exists());
        assert_eq!(
            fx.listing(),
            [extract.as_str(), "lock", STAGED],
            "the next launch's housekeeping prunes the staged update"
        );
    }
}
