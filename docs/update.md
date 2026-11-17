# Updates

Sources: [`update.rs`](../src/update.rs) and [`update/`](../src/update) (pure
helpers: `version`, `release`, `checksum`, `bundle`, `staging`, `mode`, `state`,
`schedule`, `limits`; I/O: `http`, `worker`, `fsops`, `procs`),
[`app/update.rs`](../src/app/update.rs), and
[`renderer/update_ui.rs`](../src/renderer/update_ui.rs). Event-loop wiring is in
[architecture](architecture.md#update-workers); the setting is in
[configuration](configuration.md#updates); release naming and the manual
checklist are in [development](development.md#reaching-the-updater).

Every launch with `[update] enabled` checks GitHub's latest published release.
A launch from a replaceable `Cometty.app` also downloads, verifies, and stages a
newer release in the background. It installs it only after the user chooses
**Restart to update**: the install job swaps the bundle in place, and the app
quits and reopens it. Every other launch only announces the release. All
records use the `cometty::update` log target. Most of this page describes the
implementation; the [last section](#test-coverage-and-open-work) separates
modeled tests, unautomated behavior, measured results, and open work.

## Trust model

Integrity rests on **HTTPS to GitHub plus a SHA-256 digest published in the same
release**. Nothing else authenticates an update:

- The checksum asset and the archive come from the same release. Whoever can
  publish a release on `jjunac/cometty` (the maintainer account, or a CI token
  that may write releases) can replace both together. The digest detects
  corruption, truncation, and a mismatched upload, not a malicious publisher.
- TLS uses rustls with ring and the bundled `webpki-roots` Mozilla roots. The
  system and enterprise trust stores are not consulted.
- Release artifacts are **ad-hoc signed and not notarized** (the default
  `--identity -` in `scripts/bundle-macos.sh`, which CI uses). The updater does
  not check a code signature, team ID, or notarization ticket. Bundle validation
  checks only structure and `Info.plist` identity and version.
- The archive is written by Cometty, extracted by `ditto -x -k`, and copied by
  `ditto`. None of these steps sets **`com.apple.quarantine`**, and the release
  pipeline strips extended attributes before signing. Gatekeeper therefore does
  not assess the update on first launch. The prompt a browser download of the
  same zip would trigger never appears.
- An ad-hoc signature identifies code by its hash, so every release is a new
  code identity to macOS privacy (TCC). The staged-update notice says that
  granted permissions may be requested again.

Because a publisher who can ship a malicious archive can equally ship a
malicious binary inside a well-formed `Cometty.app`, the updater does not parse
or police the archive's entries. `ditto` extracts into a fresh directory inside
the staging directory, and bundle validation rejects a wrong or corrupted
artifact. A signature check that closes the same-release gap would compare the
archive digest against a detached signature made with a key held outside the
GitHub release pipeline. It would run right after the digest comparison in
[check step 8](#check-job), before extraction.

The release page opened by a notify-only notice is the feed's `html_url` only if
it starts with `https://github.com/jjunac/cometty/releases/` and is printable,
non-space ASCII. Otherwise it is `…/releases/latest`. Opening still goes through
`links::open`'s scheme allowlist and detached spawn.

## Mode selection

`mode::resolve_mode` runs once in `UpdateController::new`. Its inputs are
`platform::launched_from_app_bundle()` (the same check that selects the
login-shell default), `current_exe()`, and `staging::default_dir()`. The UI
thread does no filesystem probe. The first failing rule wins, in this order:

| Mode | Condition | Feed check | Download, stage, install | Launch housekeeping | Notice for a newer release |
| --- | --- | --- | --- | --- | --- |
| `NotifyOnly(NotBundled)` | Not a bundle launch, or the executable is not directly in `<Name>.app/Contents/MacOS/`; always off macOS | yes | no | no | Available |
| `NotifyOnly(Translocated)` | A bundle path component is `AppTranslocation` | yes | no | no | Available + move hint |
| `NotifyOnly(NoStagingDir)` | No usable `HOME` | yes | no | no | Available |
| `SelfUpdate { bundle }` | Everything else | yes | yes | yes | Ready / Preparing; Available if the assets were rejected or the download failed |
| `NotifyOnly(NotWritable)` | Set later by a check job whose `access(parent, W_OK)` failed (including `EROFS`) | yes | no | already ran | Available + move hint |

- A check job probes writability (`fsops::parent_writable`) in step 3 and sends
  `WorkerMsg::Mode`, which the controller adopts. The install job probes again
  and fails with a reason instead of changing the mode.
- If `CARGO_PKG_VERSION` is not `X.Y.Z`, the updater is inert: one warning, no
  deadline, and no jobs.
- Restart is accepted only in `Staged` state and `SelfUpdate` mode.

## Jobs and the staging lock

Check and install jobs are detached, named threads (`update-check`,
`update-install`) started by `worker::spawn_job`:

- A drop guard armed inside the thread sends `Finished { panicked }` after the
  job returns or unwinds. A failed spawn reports nothing, and the caller handles
  it.
- The `JoinHandle` is dropped. Nothing joins or waits for a job, and the process
  may exit mid-job.
- These are not `Startup::spawn_worker` workers, and the PTY supervisor does not
  own them.

Housekeeping is a plain detached `update-housekeeping` thread with no job ID or
channel: it reports only through the log.

`JobCtx::emit` sends a `WorkerMsg` and wakes the loop with
`UserEvent::UpdateWorker` through the shared proxy `Arc`. The cancel flag is
checked before every request and before each read of at most 64 KiB. A read in
progress is not interrupted; its timeout bounds it. Each `UreqFetcher` builds at
most one agent per request kind and drops them with the job, so no pooled socket
outlives a job.

**Lock.** `fsops::StagingLock::acquire` creates the staging directory (mode
0700) and opens `lock` (0600). It retries `File::try_lock` every **250 ms**, for
up to **45 s**. `flock` belongs to the open file description, so it serializes
jobs within one process and across instances. On a busy lock:

| Holder that gave up | Result |
| --- | --- |
| Check job | Ends quietly (`debug`); state stays `Available`, which shows no notice in self-update mode; next check in 24 h. The holder is another instance's job, which stages the release for both |
| Housekeeping | Skipped for this launch |
| Install job | `Staged` with the failure: the Ready notice explains it and Restart can retry |

### Check job

Generic over `F: Fetch` ([`worker.rs`](../src/update/worker.rs)). `RemoveOnDrop`
guards make every failure or cancellation remove what the step created before
`Failed` is emitted. A cancelled job emits nothing further.

1. GET the feed. A 404 emits `UpToDate`. Any status other than 200 or 404 is
   `Failed(Status)`. The body (≤ 1 MiB) is parsed and classified. A draft, a
   prerelease, a tag that is not `vX.Y.Z` (logged at `info`), or a version that
   is not newer emits `UpToDate`. Otherwise the job emits `Newer(release)`.
2. Stops when cancelled, when the mode is notify-only, or when asset selection
   failed (missing, duplicate, archive > 256 MiB, checksum > 1 KiB, or a
   non-HTTPS URL; logged at `info`).
3. Probes writability, sends `Mode`, and stops unless `SelfUpdate`.
4. Takes the lock. A busy lock ends the job quietly.
5. GETs the checksum (status 200, ≤ 1 KiB, in memory) and parses it. It must be
   exactly `<64 hex>  <archive name>` with an optional final `\n`. `*`, `\r`,
   extra spaces, and extra lines are rejected. A different name is
   `NameMismatch`. Hex case does not matter, because digits decode to bytes. A
   bad checksum file fails here, before any archive byte is requested.
6. If `staged.json` names this version, has the current schema, records **the
   published digest**, and points at a canonical `extract-<v>-…` directory whose
   bundle validates, the job emits `AlreadyStaged(v)` and stops. The record read
   is capped at 64 KiB. A record for this version with another digest means the
   release was re-uploaded under the same tag (`release.yml` uploads with
   `--clobber`); it is logged and replaced.
7. Emits `DownloadStarted`, removes every partial, every unreferenced extract,
   and a staged record for this same version (`plan_cleanup`; a staged bundle
   of another version stays until this one commits), and streams the archive
   into `download-<v>.zip.part` (0600), hashing it as it is written. The status
   must be 200, the body is capped at `min(advertised, 256 MiB)`, and it must
   reach exactly the advertised size. A short body is `Failure::Network`.
   Write errors are `Failure::Io`. The archive file is removed on every exit,
   including after the commit.
8. Emits `VerifyStarted` and compares the streamed digest with the published
   one; a mismatch is `Failure::Digest` with both digests. It then creates
   `extract-<v>-<pid>-<nanos>/`, runs `/usr/bin/ditto -x -k` into it (a failure
   removes it and is `Failure::Extract`), and gathers bundle facts without
   following symlinks. `validate_bundle` requires:
   - exactly one top-level `Cometty.app` directory (a leftover `__MACOSX` fails)
   - a regular executable `Contents/MacOS/cometty`
   - an XML `Contents/Info.plist` (≤ 64 KiB; binary plists rejected) with
     `CFBundleIdentifier = com.github.jjunac.cometty` and a
     `CFBundleShortVersionString` equal to the release
9. Writes `staged.json` atomically. This is the **commit point**. The job then
   removes other extracts and the archive, and emits `Staged(v)`.

### Housekeeping job

Housekeeping needs no network. It is started at the end of `resumed`, after the
initial tabs spawn, in `SelfUpdate` mode only, whether or not checks are
enabled. A missing staging directory or a busy lock skips it. It creates
nothing.

1. Takes the lock.
2. `plan_cleanup` removes:
   - staged bundles at or below the running version
   - every partial download (downloads never resume)
   - extracts that no valid `staged.json` references
   - `staged.json` itself when it is invalid or stale
3. Removes `.<name>.cometty-new` and `.<name>.cometty-old` next to the bundle.
   After an install one of them holds the previous version; otherwise they are
   leftovers of an interrupted copy. Both paths are derived from this launch's
   bundle path, never read from a file.

Housekeeping does not re-arm the check schedule.

### Install job

The install job starts on "Restart to update". `InstallRequested` moves the state
to `Installing` and keeps the schedule suspended.

1. Takes the lock and re-applies the step 6 rules (except the digest, which it
   does not refetch) to `staged.json` and its bundle.
2. Counts other processes executing from `<bundle>/Contents/MacOS/`, excluding
   its own pid. If there are any, it emits `InstallBlocked { others }` with
   nothing copied. The state returns to `Staged` with the blocked issue.
3. Probes writability; a read-only folder is a failure with that reason.
4. Removes a stale incoming sibling, runs `/usr/bin/ditto <staged>/Cometty.app
   <parent>/.<name>.cometty-new`, and revalidates the copy without following
   symlinks. `ditto` preserves the signature, modes, and xattrs, and works
   across volumes, so the swap below is a rename on one volume.
5. Swaps the copy into place with `fsops::replace_bundle`:
   - `renamex_np(incoming, installed, RENAME_SWAP)`. The installed path never
     disappears, and the old bundle ends up at the incoming name.
   - On `ENOTSUP`/`EINVAL` only (a volume without the swap): remove a stale
     `.cometty-old`, rename installed → `.cometty-old`, then incoming →
     installed. If the second rename fails, the old bundle is moved back; if
     that also fails, the error names where it is.
   - Any other error, including App Management `EPERM` on macOS 13+, moves
     nothing.
6. Emits `WorkerMsg::Installed { bundle }`.

Steps 3–5 fail with `Failure::Install`, after removing the incoming copy. Every
failure returns the state to `Staged` with the reason, so the notice explains it
and **Restart to update** retries.

**Quit and relaunch.** macOS keeps a running process's mapped executable when
its bundle is renamed, so the app keeps running from the old bundle at the
sibling name. On `Installed` the controller records the bundle and the UI thread
calls `event_loop.exit()`. `exiting()` saves the session, shuts PTYs down within
the shared deadline, and flushes `ConfigSaver`. Last, `relaunch_after_update`
releases the session lock (so the new instance restores the snapshot) and starts
`/usr/bin/open -n <bundle>` without waiting for it. `-n` starts a fresh instance
even though LaunchServices still lists the exiting one. If `open` cannot be
started, the update is still installed; the warning says to start Cometty again.
The new version's housekeeping then deletes the previous bundle.

## State machine

[`state.rs`](../src/update/state.rs) `transition`. Every pair not listed returns
`Err(Invalid)`, is logged at `debug`, and leaves the state unchanged.

| From | Event | To |
| --- | --- | --- |
| Idle, UpToDate, Available, Failed | CheckStarted | Checking |
| Checking | UpToDate | UpToDate |
| Checking | Newer(r) | Available(r) |
| Available(r) | DownloadStarted | Downloading(r) |
| Available(r) | AlreadyStaged(v), v = r.version | Staged { v, None } |
| Downloading(r) | VerifyStarted | Verifying(r) |
| Verifying(r) | Staged(v), v = r.version | Staged { v, None } |
| Staged { v, _ } | InstallRequested | Installing(v) |
| Installing(v) | InstallBlocked { n } | Staged { v, Blocked(n) } |
| Installing(v) | Failed(f) | Staged { v, Failed(f) } |
| Checking | Failed(f) | Failed { f, None } |
| Available(r), Downloading(r), Verifying(r) | Failed(f) | Failed { f, Some(r) } |
| any except Installing | Disabled | Idle |

A successful install leaves the state `Installing` until the process exits.

`Failure` groups:

- transport: `Status`, `Parse`, `Timeout`, `Network`, `InsecureRedirect`,
  `Redirects`, `Http`, `TooLarge`
- verification: `Checksum`, `Digest`
- staging: `Bundle`, `Extract`, `Io`
- install: `Spawn`, `WorkerPanicked`, `Install`

Every failure is logged at `warn` and shown in the Updates settings status line.
A failure after a newer release was found keeps that release, so the notice
still announces it.

Controller rules ([`app/update.rs`](../src/app/update.rs)):

- **Stale messages.** Every message carries a job ID. Messages from a superseded
  or abandoned job are dropped.
- **Missing results.** `Finished` with no terminal message while the state is
  Checking, Downloading, Verifying, or Installing applies
  `Failed(WorkerPanicked)`. `Installed` is the install job's terminal message.
- **Schedule.** The end of a check or install job re-arms the daily schedule,
  whatever the outcome. A spawn error is `Failed(Spawn)` and also re-arms it.
- **Disabling.** It clears the deadline, then:
  - in `Installing`: nothing more; the install job keeps its handle, so its
    `Installed` is not discarded
  - otherwise: cancels and abandons a check job and applies `Disabled`
- **Repaints.** `apply_event` requests a repaint only when `notice()` changes. A
  drained batch repaints at most once.
- **Scheduler.**
  - Deadlines are `(Instant, SystemTime)` pairs: 5 s after construction, 1 s
    after enabling, and 24 h after each finished check or install job.
  - A check is due when either clock passes its half. On macOS `Instant` stops
    during sleep, so the first loop turn after waking runs an overdue check.
  - It is suspended while Staged or Installing, so "Later" leaves checks off
    until relaunch.
  - It contributes one deadline to `about_to_wait` and adds no timer.

**Notice** (`state::notice`, drawn by `update_ui`). It is a bottom-right
`egui::Area` that clears the scrollbar track and is at most 360 pt wide:

| State | Notice | Lines | Buttons |
| --- | --- | --- | --- |
| Staged { v, issue }, not dismissed | Ready | "Cometty v is ready." plus the privacy line; a blocked issue adds "Quit the other Cometty window…" (one) or "Quit the other N Cometty windows…", a failed one "The update could not be installed: …" | Restart to update · Later |
| Installing(v) | Preparing | "Preparing Cometty v…" | none |
| Available(r), not dismissed, and notify-only or assets rejected | Available | "Cometty v is available." plus the move hint for Translocated/NotWritable | View release · Later |
| Failed { f, Some(r) }, not dismissed | Available | "Cometty v is available." plus "The update could not be downloaded: …" | View release · Later |
| anything else, including a failed feed check | none | | |

The buttons are `ui.interact(.., Sense::CLICK)`, and the labels are not
selectable. Clicking never takes egui focus or IME from the terminal.

- Presses inside the notice count as chrome: no selection, mouse report, or link
  open.
- `ChromeHover::UpdateNotice(target)` makes pointer motion repaint only when the
  hovered target (body, Restart, Later, Open) changes.
- A visible notice prevents `should_skip_overlay` from skipping the egui pass.
- "Later" hides that version until relaunch.

## Staging directory layout

`${XDG_STATE_HOME:-$HOME/.local/state}/cometty/updates/`. It uses the session
snapshot's base rules through the shared `session::state_base`, and a relative
`XDG_STATE_HOME` is ignored.

```text
lock                         flock: every update job
download-<v>.zip.part        archive while it streams (at most one)
extract-<v>-<pid>-<nanos>/   extraction directory containing Cometty.app
staged.json                  StagedRecord { schema, version, sha256, extract_dir }
.<name>.<pid>.tmp            interrupted atomic write (kept as Unknown)
```

Next to the installed bundle are the siblings `.<name>.cometty-new` (the
incoming copy, then the previous bundle after a swap) and `.<name>.cometty-old`
(the previous bundle after the rename fallback). They are hidden and do not end
in `.app`, so LaunchServices does not register them.

- **Atomic writes.** `write_json_atomic` writes `.<name>.<pid>.tmp` (0600),
  fsyncs it, renames it over the target, and fsyncs the directory best effort.
- **Unknown names.** Only canonical names are recognized. Anything else,
  including temp files and near misses, is `Unknown` and never removed.
- **Never cleaned up.** `lock` is never removed.
- **Commit.** An extract without a valid `staged.json` is partial. At most one
  extract survives a cleanup.
- **Interrupted runs.** An exit during a download leaves at most one partial. An
  exit during extraction leaves an unrecorded extract. An exit during the
  install copy leaves an incoming sibling. Each is removed by the next download
  or launch housekeeping.

## Multiple instances

**Policy.** The install job replaces the bundle only when no other process runs
an executable from it. A surviving instance would:

- receive LaunchServices' activation instead of the new version
- keep running from a previous bundle that the next launch deletes
- race on the staging directory

`procs::executables()` lists `(pid, path)` with `proc_listallpids`, then
`proc_pidpath` for each pid. `count_instances` matches by path components, so
`Cometty.app2` is not `Cometty.app`. If others run, the Ready notice shows the
blocked line, Restart stays available, and Cometty keeps running.

Instances share one staging directory under the lock. A second instance finds a
valid `staged.json` with the published digest and goes to `Staged` without
downloading.

Limits:

- **Unreadable processes.** Detection is best effort. Processes whose path
  cannot be read (exited, zombies, or other users' processes when
  `proc_pidpath` is denied, as with Fast User Switching) are not counted. Such a
  process keeps running its mapped old binary after the swap.
- **Late launches.** The staging lock does not stop Finder or the Dock. An
  instance launched between the count and the swap is not seen. It runs the old
  binary from the sibling, which the relaunched app then deletes.
- **Path matching.** Detection matches the installed path only. Copies of
  Cometty elsewhere, or a translocated launch of the same bundle, are not
  counted, and they are not replaced either.

## HTTP limits

[`limits.rs`](../src/update/limits.rs) holds the table, which a unit test
asserts. [`http.rs`](../src/update/http.rs) turns it into one `ureq` agent per
kind. Dependencies are `ureq =3.5.2` (`default-features = false`, `rustls`
feature: ring + webpki-roots), `serde_json =1.0.154`, and `sha2 =0.11.1`, all
recorded in `Cargo.lock`.

| Kind | Accept | Resolve / connect | Global | Per read | Body cap |
| --- | --- | --- | --- | --- | --- |
| Feed (`api.github.com/repos/jjunac/cometty/releases/latest`) | `application/vnd.github+json` | 10 s / 10 s | 30 s | none | 1 MiB |
| Checksum asset | `application/octet-stream` | 10 s / 10 s | 30 s | none | 1 KiB |
| Archive asset | `application/octet-stream` | 10 s / 10 s | 15 min | 30 s | min(advertised size, 256 MiB); short body fails |

Agent configuration:

- `https_only(true)` applies to every redirect hop.
- `max_redirects(5)` with `max_redirects_will_error(true)`.
- `http_status_as_error(false)`, so the updater classifies status codes itself.
- `User-Agent: cometty/<version>` and `Accept` are the only identifying headers.
  No `Accept-Encoding`, cookies, or authentication are sent.
- ureq's default proxy-from-environment behavior is kept.

`copy_capped` reads at most 64 KiB at a time, checks the cancel flag before each
read, and refuses the first byte past the cap.

| Error | Failure |
| --- | --- |
| An `http` URL or redirect | `InsecureRedirect` |
| More than 5 redirects | `Redirects` |
| Any timeout | `Timeout` |
| DNS, TLS, proxy, connection, or socket error, including a body read error | `Network` |
| Any other ureq error | `Http` |
| A body over its cap | `TooLarge` |

Each of these failures is retried at the next daily check.

## Test coverage and open work

**Modeled tests** (`cargo test --locked update::`, `app::update::`,
`renderer::update_ui::`). All are offline; no test opens a socket.

- **Pure helpers.** Examples and fixtures cover versions, release metadata (a
  realistic GitHub fixture with unknown fields, plus newer, equal, older, draft,
  prerelease, bad-tag, missing, duplicate, and oversized assets), checksum
  lines, streaming SHA-256, plist and bundle rules, cleanup plans, instance
  counting, mode resolution, the scheduler, and the limits table. An exhaustive
  `UpdateState × Event` test covers every transition.
- **Swap.** The rename fallback, its move-back, and non-fallback errnos run on
  real temporary directories with an injected swap result.
- **Transport.** `classify_error` is tested on constructed `ureq` errors, and
  the agent configuration is inspected for its headers.
- **Pipelines.** A scripted `FakeFetch` drives the check, housekeeping, and
  install jobs on temporary staging directories. The tests cover:
  - request discipline: one feed GET with only `User-Agent` and `Accept`, one
    request in notify-only mode, checksum before archive, and no archive for a
    staged release with the published digest
  - re-download of a staged release whose digest changed
  - cleanup after oversize, truncation, checksum and digest errors, and
    cancellation
  - the quiet busy lock and the writability downgrade
  - install blocking and failure paths, with a fake process lister
- **macOS-only integration.** These run real `/usr/bin/ditto` and
  `renamex_np` on temporary directories. They cover:
  - the full check success path
  - a ditto round trip that keeps a symlink, mode, and xattr and leaves no
    `__MACOSX`
  - a corrupt archive that leaves no extraction directory
  - `RENAME_SWAP` of two directories and a full install job swap
  - `procs` listing the test process
- **Controller and UI.** Tests cover stale IDs, `WorkerPanicked`, disable
  semantics, failure notices, and repaint-only-on-notice-change. A headless egui
  pass checks that clicking Restart leaves focus empty. Pointer-repaint and
  chrome-press routing and the overlay skip rule are tested too.
- **Non-macOS builds.** The non-macOS paths (`ditto`/`open` stubs, an empty
  process list, the rename fallback as the only swap) are exercised only by the
  Linux CI run.

**Not automated.** These are in the [manual
checklist](development.md#manual-update-checklist):

- live GitHub requests
- a real LaunchServices relaunch and session restore across an update
- the TCC re-prompt
- App Management `EPERM` on macOS 13+
- Gatekeeper's handling of the unquarantined update
- the move hint for translocated or read-only copies
- multi-instance blocking with real Dock-launched instances

**Measured results.** None are recorded. The idle claim (no thread, socket, or
timer between checks beyond one loop deadline), the binary-size cost of the
rustls stack, and download/verify timings are design expectations until
`make bench-idle` and a size comparison are recorded as
[development](development.md#manual-update-checklist) describes.

**Open work.**

- A detached-signature check (see the [trust model](#trust-model)).
- `platform-verifier` instead of bundled roots for enterprise trust stores.
