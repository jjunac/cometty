//! The update state machine, its transitions, and the derived notice
//! (`docs/update.md#state-machine`).

use std::fmt;

use super::bundle::BundleError;
use super::checksum::{ChecksumError, Digest, hex_lower};
use super::limits::RequestKind;
use super::mode::{Mode, NotifyReason};
use super::release::Release;
use super::version::Version;

// ---------------------------------------------------------------------------
// Failure
// ---------------------------------------------------------------------------

/// Why an update job ended in `Failed`.
///
/// The `Display` text is the reason recorded under the `cometty::update` log
/// target and shown in the Updates settings status line and the notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    // Transport.
    /// A response status other than 200 (or 404 on the release feed).
    Status(u16),
    /// The release metadata is not valid JSON or lacks a required field.
    Parse(String),
    /// A connect, read, or global timeout expired.
    Timeout,
    /// DNS, TLS, connection, or socket I/O failure.
    Network(String),
    /// A redirect pointed at a non-https URL.
    InsecureRedirect,
    /// The redirect limit was exceeded.
    Redirects,
    /// Any other HTTP client error.
    Http(String),
    /// The body for `what` exceeded `cap` bytes.
    TooLarge { what: RequestKind, cap: u64 },

    // Verification.
    /// The checksum file was rejected.
    Checksum(ChecksumError),
    /// The archive's SHA-256 differs from the published digest.
    Digest { expected: Digest, actual: Digest },

    // Staging.
    /// The extracted bundle failed validation.
    Bundle(BundleError),
    /// The extraction tool failed.
    Extract(String),
    /// A staging file system operation failed.
    Io(String),

    // Install.
    /// A worker thread could not be started.
    Spawn(String),
    /// An update worker thread panicked.
    WorkerPanicked,
    /// The install job could not copy or swap the bundle.
    Install(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Status(code) => write!(f, "server returned HTTP status {code}"),
            Self::Parse(msg) => write!(f, "invalid release metadata: {msg}"),
            Self::Timeout => f.write_str("request timed out"),
            Self::Network(msg) => write!(f, "network error: {msg}"),
            Self::InsecureRedirect => f.write_str("refused a redirect to a non-https URL"),
            Self::Redirects => f.write_str("too many redirects"),
            Self::Http(msg) => write!(f, "HTTP error: {msg}"),
            Self::TooLarge { what, cap } => {
                write!(f, "{} exceeded the {cap}-byte limit", request_label(*what))
            }
            Self::Checksum(err) => match err {
                ChecksumError::Format => {
                    f.write_str("checksum file is not a single `<sha256>  <name>` line")
                }
                ChecksumError::NameMismatch { found } => {
                    write!(f, "checksum file names {found:?}, not the archive")
                }
            },
            Self::Digest { expected, actual } => write!(
                f,
                "SHA-256 mismatch: expected {}, got {}",
                hex_lower(expected),
                hex_lower(actual)
            ),
            Self::Bundle(err) => write!(f, "invalid bundle: {}", bundle_reason(err)),
            Self::Extract(msg) => write!(f, "extraction failed: {msg}"),
            Self::Io(msg) => write!(f, "file system error: {msg}"),
            Self::Spawn(msg) => write!(f, "could not start: {msg}"),
            Self::WorkerPanicked => f.write_str("update worker panicked"),
            Self::Install(msg) => write!(f, "install failed: {msg}"),
        }
    }
}

impl From<ChecksumError> for Failure {
    fn from(err: ChecksumError) -> Self {
        Self::Checksum(err)
    }
}

impl From<BundleError> for Failure {
    fn from(err: BundleError) -> Self {
        Self::Bundle(err)
    }
}

/// What a capped request was fetching, for log lines.
fn request_label(kind: RequestKind) -> &'static str {
    match kind {
        RequestKind::Feed => "release metadata",
        RequestKind::Checksum => "checksum file",
        RequestKind::Archive => "archive",
    }
}

/// The rule a rejected bundle broke, for log lines.
fn bundle_reason(err: &BundleError) -> String {
    match err {
        BundleError::TopLevel => "top level is not exactly one Cometty.app".into(),
        BundleError::MissingExecutable => "Contents/MacOS/cometty is missing".into(),
        BundleError::NotExecutable => {
            "Contents/MacOS/cometty is not an executable regular file".into()
        }
        BundleError::MissingPlist => "Contents/Info.plist is missing or unreadable".into(),
        BundleError::UnsupportedPlist => "Contents/Info.plist is a binary plist".into(),
        BundleError::Identifier { found } => format!("CFBundleIdentifier is {found:?}"),
        BundleError::Version { found } => format!("CFBundleShortVersionString is {found:?}"),
    }
}

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

/// Why the last Restart did not install the staged update. Shown in the
/// Ready notice, which keeps Restart available for a retry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InstallIssue {
    /// Other instances run from the installed bundle.
    Blocked { others: usize },
    /// The install job failed; nothing was replaced.
    Failed(Failure),
}

/// Where the updater is in its check → download → verify → stage → install
/// pipeline. Only [`transition`] moves between states.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UpdateState {
    /// No check has run since launch or since the setting was disabled.
    Idle,
    /// A check job is fetching the release feed.
    Checking,
    /// The latest release is not newer, or the feed has no release.
    UpToDate,
    /// A newer release exists; self-update may continue to download it.
    Available(Release),
    /// The archive for this release is streaming to the staging directory.
    Downloading(Release),
    /// The archive is being checked, extracted, and validated.
    Verifying(Release),
    /// A validated bundle is staged. `issue` says why the last Restart did
    /// not install it.
    Staged {
        version: Version,
        issue: Option<InstallIssue>,
    },
    /// The install job is copying and swapping the bundle; success quits.
    Installing(Version),
    /// The last job failed; the next scheduled check may retry. `release` is
    /// the newer release the job had found, if any.
    Failed {
        failure: Failure,
        release: Option<Release>,
    },
}

impl UpdateState {
    /// Staged or Installing: no checks run until relaunch.
    pub(crate) fn suspends_checks(&self) -> bool {
        matches!(self, Self::Staged { .. } | Self::Installing(_))
    }
}

/// An input to [`transition`], sent by an update job or the controller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    /// The controller spawned a check job.
    CheckStarted,
    /// The feed has no release newer than the running version.
    UpToDate,
    /// The feed has a strictly newer release.
    Newer(Release),
    /// The archive download began.
    DownloadStarted,
    /// A valid staged bundle with the published digest already exists.
    AlreadyStaged(Version),
    /// The download finished and verification began.
    VerifyStarted,
    /// The bundle for this version was validated and committed.
    Staged(Version),
    /// The user chose Restart.
    InstallRequested,
    /// Other instances run from the installed bundle.
    InstallBlocked { others: usize },
    /// The current job failed.
    Failed(Failure),
    /// The setting was turned off.
    Disabled,
}

/// A state/event pair the transition table does not list, by variant name.
/// The state is left unchanged; the controller logs the pair at `debug`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Invalid {
    pub state: &'static str,
    pub event: &'static str,
}

impl UpdateState {
    /// The variant name, for [`Invalid`] and log lines.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Checking => "Checking",
            Self::UpToDate => "UpToDate",
            Self::Available(_) => "Available",
            Self::Downloading(_) => "Downloading",
            Self::Verifying(_) => "Verifying",
            Self::Staged { .. } => "Staged",
            Self::Installing(_) => "Installing",
            Self::Failed { .. } => "Failed",
        }
    }
}

impl Event {
    /// The variant name, for [`Invalid`] and log lines.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::CheckStarted => "CheckStarted",
            Self::UpToDate => "UpToDate",
            Self::Newer(_) => "Newer",
            Self::DownloadStarted => "DownloadStarted",
            Self::AlreadyStaged(_) => "AlreadyStaged",
            Self::VerifyStarted => "VerifyStarted",
            Self::Staged(_) => "Staged",
            Self::InstallRequested => "InstallRequested",
            Self::InstallBlocked { .. } => "InstallBlocked",
            Self::Failed(_) => "Failed",
            Self::Disabled => "Disabled",
        }
    }
}

/// Applies `e` to `s` following the transition table in
/// `docs/update.md#state-machine`. Every pair not in the table returns
/// `Err(Invalid)`.
pub(crate) fn transition(s: &UpdateState, e: &Event) -> Result<UpdateState, Invalid> {
    use Event as E;
    use UpdateState as S;
    let staged = |version: Version, issue| S::Staged { version, issue };
    let next = match (s, e) {
        (S::Idle | S::UpToDate | S::Available(_) | S::Failed { .. }, E::CheckStarted) => {
            S::Checking
        }
        (S::Checking, E::UpToDate) => S::UpToDate,
        (S::Checking, E::Newer(release)) => S::Available(release.clone()),
        (S::Available(release), E::DownloadStarted) => S::Downloading(release.clone()),
        (S::Available(release), E::AlreadyStaged(v)) if *v == release.version => staged(*v, None),
        (S::Downloading(release), E::VerifyStarted) => S::Verifying(release.clone()),
        (S::Verifying(release), E::Staged(v)) if *v == release.version => staged(*v, None),
        (S::Staged { version, .. }, E::InstallRequested) => S::Installing(*version),
        (S::Installing(v), E::InstallBlocked { others }) => {
            staged(*v, Some(InstallIssue::Blocked { others: *others }))
        }
        // A failed install keeps the staged update so Restart can retry.
        (S::Installing(v), E::Failed(failure)) => {
            staged(*v, Some(InstallIssue::Failed(failure.clone())))
        }
        (S::Checking, E::Failed(failure)) => S::Failed {
            failure: failure.clone(),
            release: None,
        },
        (S::Available(r) | S::Downloading(r) | S::Verifying(r), E::Failed(failure)) => S::Failed {
            failure: failure.clone(),
            release: Some(r.clone()),
        },
        // Installing ignores Disabled: it is short and ends in quitting, and
        // the install job keeps its handle.
        (state, E::Disabled) if !matches!(state, S::Installing(_)) => S::Idle,
        _ => {
            return Err(Invalid {
                state: s.name(),
                event: e.name(),
            });
        }
    };
    Ok(next)
}

// ---------------------------------------------------------------------------
// Notice model
// ---------------------------------------------------------------------------

/// Shown with every staged update: replacing the bundle can reset the TCC
/// grants macOS keyed to the old one (`docs/update.md#trust-model`).
pub(crate) const PRIVACY_LINE: &str =
    "macOS may ask again for permissions you granted Cometty (for example Full Disk Access).";

/// Shown when a bundle launch cannot update in place.
pub(crate) const MOVE_HINT: &str =
    "Move Cometty to a writable Applications folder to update in place.";

/// What the notice shows, derived from the state by [`notice`]. The renderer
/// draws it; the controller compares two values to decide whether a repaint
/// is needed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UpdateNotice {
    /// A validated bundle is staged: Restart to update · Later. Always
    /// carries [`PRIVACY_LINE`]; `issue` explains why the last Restart did
    /// not install it.
    Ready {
        version: Version,
        issue: Option<InstallIssue>,
    },
    /// The install job is copying and swapping the bundle; no actions.
    Preparing { version: Version },
    /// A newer release this launch will not install: View release · Later.
    /// `hint` is [`MOVE_HINT`] when moving the bundle would enable
    /// self-update; `failure` is why the automatic download failed.
    Available {
        version: Version,
        url: String,
        hint: Option<&'static str>,
        failure: Option<Failure>,
    },
}

impl UpdateNotice {
    /// The first line of the notice.
    pub(crate) fn headline(&self) -> String {
        match self {
            Self::Ready { version, .. } => format!("Cometty {version} is ready."),
            Self::Preparing { version } => format!("Preparing Cometty {version}…"),
            Self::Available { version, .. } => format!("Cometty {version} is available."),
        }
    }

    /// The privacy statement, present exactly for `Ready`.
    pub(crate) fn privacy_line(&self) -> Option<&'static str> {
        matches!(self, Self::Ready { .. }).then_some(PRIVACY_LINE)
    }
}

/// The notice for `s` in `mode`, or `None` when nothing should be shown.
///
/// - `Staged` gives `Ready` unless "Later" dismissed that version.
/// - `Installing` gives `Preparing`.
/// - `Available` gives `Available` only when this launch will not download
///   it, because the mode is notify-only or the assets were rejected. In
///   self-update mode with both assets the download starts at once and no
///   notice shows.
/// - `Failed` with a release gives `Available` with the failure, so a
///   failed download still announces the release.
/// - Every other state gives `None`. Dismissed versions never show.
pub(crate) fn notice(
    s: &UpdateState,
    mode: &Mode,
    dismissed: Option<Version>,
) -> Option<UpdateNotice> {
    let available = |release: &Release, failure: Option<&Failure>| {
        let hint = matches!(
            mode,
            Mode::NotifyOnly(NotifyReason::Translocated | NotifyReason::NotWritable)
        )
        .then_some(MOVE_HINT);
        UpdateNotice::Available {
            version: release.version,
            url: release.page_url.clone(),
            hint,
            failure: failure.cloned(),
        }
    };
    let notice = match s {
        UpdateState::Staged { version, issue } => UpdateNotice::Ready {
            version: *version,
            issue: issue.clone(),
        },
        UpdateState::Installing(version) => {
            return Some(UpdateNotice::Preparing { version: *version });
        }
        UpdateState::Available(release) => {
            let downloads = matches!(mode, Mode::SelfUpdate { .. }) && release.assets.is_ok();
            if downloads {
                return None;
            }
            available(release, None)
        }
        UpdateState::Failed {
            failure,
            release: Some(release),
        } => available(release, Some(failure)),
        _ => return None,
    };
    let version = match &notice {
        UpdateNotice::Ready { version, .. } | UpdateNotice::Available { version, .. } => *version,
        UpdateNotice::Preparing { version } => *version,
    };
    (dismissed != Some(version)).then_some(notice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::release::{Asset, AssetProblem, SelectedAssets};

    #[test]
    fn display_includes_status_cap_and_both_digests() {
        assert!(Failure::Status(503).to_string().contains("503"));

        let cap = 2 * 1024 * 1024;
        let too_large = Failure::TooLarge {
            what: RequestKind::Feed,
            cap,
        }
        .to_string();
        assert!(too_large.contains(&cap.to_string()), "{too_large}");

        let expected = Digest([0xab; 32]);
        let actual = Digest([0x01; 32]);
        let digest = Failure::Digest { expected, actual }.to_string();
        assert!(digest.contains(&"ab".repeat(32)), "{digest}");
        assert!(digest.contains(&"01".repeat(32)), "{digest}");
    }

    // --- State machine ----------------------------------------------------

    const V: Version = Version {
        major: 1,
        minor: 2,
        patch: 0,
    };
    const OTHER: Version = Version {
        major: 1,
        minor: 3,
        patch: 0,
    };

    fn release() -> Release {
        Release {
            version: V,
            page_url: "https://github.com/jjunac/cometty/releases/tag/v1.2.0".into(),
            assets: Err(AssetProblem::Missing(
                "Cometty-1.2.0-macos-universal.zip".into(),
            )),
        }
    }

    fn staged(issue: Option<InstallIssue>) -> UpdateState {
        UpdateState::Staged { version: V, issue }
    }

    fn blocked(others: usize) -> Option<InstallIssue> {
        Some(InstallIssue::Blocked { others })
    }

    fn install_failed() -> Option<InstallIssue> {
        Some(InstallIssue::Failed(Failure::WorkerPanicked))
    }

    fn failed(release: Option<Release>) -> UpdateState {
        UpdateState::Failed {
            failure: Failure::WorkerPanicked,
            release,
        }
    }

    /// One value per state variant (several for `Staged` and `Failed`, to
    /// cover their fields).
    fn states() -> Vec<UpdateState> {
        vec![
            UpdateState::Idle,
            UpdateState::Checking,
            UpdateState::UpToDate,
            UpdateState::Available(release()),
            UpdateState::Downloading(release()),
            UpdateState::Verifying(release()),
            staged(None),
            staged(blocked(2)),
            staged(install_failed()),
            UpdateState::Installing(V),
            failed(None),
            failed(Some(release())),
        ]
    }

    /// One value per event variant, plus version-mismatched `AlreadyStaged`
    /// and `Staged`, which the table rejects.
    fn events() -> Vec<Event> {
        vec![
            Event::CheckStarted,
            Event::UpToDate,
            Event::Newer(release()),
            Event::DownloadStarted,
            Event::AlreadyStaged(V),
            Event::AlreadyStaged(OTHER),
            Event::VerifyStarted,
            Event::Staged(V),
            Event::Staged(OTHER),
            Event::InstallRequested,
            Event::InstallBlocked { others: 3 },
            Event::Failed(Failure::WorkerPanicked),
            Event::Disabled,
        ]
    }

    /// The documented transition table, spelled out for the sample values.
    fn table() -> Vec<(UpdateState, Event, UpdateState)> {
        use Event as E;
        use UpdateState as S;
        let fail = || E::Failed(Failure::WorkerPanicked);
        let mut rows = vec![
            // Check results.
            (S::Checking, E::UpToDate, S::UpToDate),
            (S::Checking, E::Newer(release()), S::Available(release())),
            // Download, reuse, verification, staging.
            (
                S::Available(release()),
                E::DownloadStarted,
                S::Downloading(release()),
            ),
            (S::Available(release()), E::AlreadyStaged(V), staged(None)),
            (
                S::Downloading(release()),
                E::VerifyStarted,
                S::Verifying(release()),
            ),
            (S::Verifying(release()), E::Staged(V), staged(None)),
            // Install: success quits outside the table.
            (
                S::Installing(V),
                E::InstallBlocked { others: 3 },
                staged(blocked(3)),
            ),
            (S::Installing(V), fail(), staged(install_failed())),
            // Failures keep the release they had found.
            (S::Checking, fail(), failed(None)),
            (S::Available(release()), fail(), failed(Some(release()))),
            (S::Downloading(release()), fail(), failed(Some(release()))),
            (S::Verifying(release()), fail(), failed(Some(release()))),
        ];
        for from in [
            S::Idle,
            S::UpToDate,
            S::Available(release()),
            failed(None),
            failed(Some(release())),
        ] {
            rows.push((from, E::CheckStarted, S::Checking));
        }
        for from in [staged(None), staged(blocked(2)), staged(install_failed())] {
            rows.push((from, E::InstallRequested, S::Installing(V)));
        }
        // Disabled from every state except Installing.
        for from in states() {
            if !matches!(from, S::Installing(_)) {
                rows.push((from, E::Disabled, S::Idle));
            }
        }
        rows
    }

    /// Fails to compile when a variant is added, so the samples stay complete.
    fn state_variant(s: &UpdateState) -> usize {
        match s {
            UpdateState::Idle => 0,
            UpdateState::Checking => 1,
            UpdateState::UpToDate => 2,
            UpdateState::Available(_) => 3,
            UpdateState::Downloading(_) => 4,
            UpdateState::Verifying(_) => 5,
            UpdateState::Staged { .. } => 6,
            UpdateState::Installing(_) => 7,
            UpdateState::Failed { .. } => 8,
        }
    }

    fn event_variant(e: &Event) -> usize {
        match e {
            Event::CheckStarted => 0,
            Event::UpToDate => 1,
            Event::Newer(_) => 2,
            Event::DownloadStarted => 3,
            Event::AlreadyStaged(_) => 4,
            Event::VerifyStarted => 5,
            Event::Staged(_) => 6,
            Event::InstallRequested => 7,
            Event::InstallBlocked { .. } => 8,
            Event::Failed(_) => 9,
            Event::Disabled => 10,
        }
    }

    #[test]
    fn samples_cover_every_variant() {
        let mut seen: Vec<usize> = states().iter().map(state_variant).collect();
        seen.dedup();
        assert_eq!(seen, (0..=8).collect::<Vec<_>>());
        let mut seen: Vec<usize> = events().iter().map(event_variant).collect();
        seen.dedup();
        assert_eq!(seen, (0..=10).collect::<Vec<_>>());
    }

    #[test]
    fn transition_table_is_exhaustive() {
        let (states, events, table) = (states(), events(), table());
        for (i, (from, event, _)) in table.iter().enumerate() {
            assert!(states.contains(from), "row {i}: unknown state {from:?}");
            assert!(events.contains(event), "row {i}: unknown event {event:?}");
            let dupes = table
                .iter()
                .filter(|(f, e, _)| f == from && e == event)
                .count();
            assert_eq!(dupes, 1, "row {i} is listed {dupes} times");
        }

        let mut listed = 0;
        for from in &states {
            for event in &events {
                let row = table.iter().find(|(f, e, _)| f == from && e == event);
                let got = transition(from, event);
                match row {
                    Some((_, _, to)) => {
                        listed += 1;
                        assert_eq!(got.as_ref(), Ok(to), "{from:?} × {event:?}");
                    }
                    None => assert_eq!(
                        got,
                        Err(Invalid {
                            state: from.name(),
                            event: event.name(),
                        }),
                        "{from:?} × {event:?} should be rejected"
                    ),
                }
            }
        }
        assert_eq!(listed, table.len());
    }

    #[test]
    fn already_staged_or_staged_with_another_version_is_rejected() {
        let available = UpdateState::Available(release());
        assert!(transition(&available, &Event::AlreadyStaged(OTHER)).is_err());
        let verifying = UpdateState::Verifying(release());
        assert!(transition(&verifying, &Event::Staged(OTHER)).is_err());
    }

    // --- Notice model -----------------------------------------------------

    fn self_update() -> Mode {
        Mode::SelfUpdate {
            bundle: "/Applications/Cometty.app".into(),
        }
    }

    /// Every mode: self-update plus each notify-only reason.
    fn modes() -> Vec<Mode> {
        vec![
            self_update(),
            Mode::NotifyOnly(NotifyReason::NotBundled),
            Mode::NotifyOnly(NotifyReason::Translocated),
            Mode::NotifyOnly(NotifyReason::NotWritable),
            Mode::NotifyOnly(NotifyReason::NoStagingDir),
        ]
    }

    /// Whether `mode` should carry the move hint.
    fn wants_hint(mode: &Mode) -> bool {
        matches!(
            mode,
            Mode::NotifyOnly(NotifyReason::Translocated | NotifyReason::NotWritable)
        )
    }

    /// `release()` with both assets selected.
    fn downloadable() -> Release {
        let asset = |name: &str| Asset {
            name: name.into(),
            size: 1,
            url: format!("https://github.com/jjunac/cometty/releases/download/v1.2.0/{name}"),
        };
        Release {
            assets: Ok(SelectedAssets {
                archive: asset("Cometty-1.2.0-macos-universal.zip"),
                checksum: asset("Cometty-1.2.0-macos-universal.zip.sha256"),
            }),
            ..release()
        }
    }

    #[test]
    fn staged_is_ready_with_the_privacy_line_and_its_issue_in_every_mode() {
        for mode in modes() {
            for issue in [None, blocked(2), install_failed()] {
                let got = notice(&staged(issue.clone()), &mode, None);
                assert_eq!(
                    got,
                    Some(UpdateNotice::Ready { version: V, issue }),
                    "{mode:?}"
                );
                let got = got.unwrap();
                assert_eq!(got.privacy_line(), Some(PRIVACY_LINE));
                assert_eq!(got.headline(), "Cometty 1.2.0 is ready.");
            }
        }
    }

    #[test]
    fn available_shows_only_when_this_launch_will_not_download() {
        // Self-update with both assets: the download starts, no notice.
        let state = UpdateState::Available(downloadable());
        assert_eq!(notice(&state, &self_update(), None), None);

        // Notify-only with both assets, or any mode with an asset problem.
        let url = release().page_url;
        for (release, mode) in modes()
            .into_iter()
            .skip(1)
            .map(|m| (downloadable(), m))
            .chain(modes().into_iter().map(|m| (release(), m)))
        {
            let got = notice(&UpdateState::Available(release), &mode, None);
            let hint = wants_hint(&mode).then_some(MOVE_HINT);
            assert_eq!(
                got,
                Some(UpdateNotice::Available {
                    version: V,
                    url: url.clone(),
                    hint,
                    failure: None,
                }),
                "{mode:?}"
            );
            let got = got.unwrap();
            assert_eq!(got.privacy_line(), None);
            assert_eq!(got.headline(), "Cometty 1.2.0 is available.");
        }
    }

    #[test]
    fn a_failed_download_still_announces_the_release() {
        let failure = Failure::Network("proxy refused".into());
        let state = UpdateState::Failed {
            failure: failure.clone(),
            release: Some(downloadable()),
        };
        assert_eq!(
            notice(&state, &self_update(), None),
            Some(UpdateNotice::Available {
                version: V,
                url: release().page_url,
                hint: None,
                failure: Some(failure),
            })
        );
        assert_eq!(notice(&state, &self_update(), Some(V)), None, "dismissed");
    }

    #[test]
    fn dismissed_hides_only_the_matching_version() {
        let notify = Mode::NotifyOnly(NotifyReason::NotBundled);
        let available = UpdateState::Available(release());
        for (state, mode) in [(staged(None), self_update()), (available, notify)] {
            assert_eq!(notice(&state, &mode, Some(V)), None, "{state:?}");
            assert!(notice(&state, &mode, Some(OTHER)).is_some(), "{state:?}");
            assert!(notice(&state, &mode, None).is_some(), "{state:?}");
        }
    }

    #[test]
    fn installing_is_preparing_even_when_dismissed() {
        for mode in modes() {
            for dismissed in [None, Some(V), Some(OTHER)] {
                let got = notice(&UpdateState::Installing(V), &mode, dismissed);
                assert_eq!(got, Some(UpdateNotice::Preparing { version: V }));
                let got = got.unwrap();
                assert_eq!(got.headline(), "Preparing Cometty 1.2.0…");
                assert_eq!(got.privacy_line(), None);
            }
        }
    }

    #[test]
    fn idle_checking_in_progress_and_feed_failures_show_nothing() {
        let states = [
            UpdateState::Idle,
            UpdateState::Checking,
            UpdateState::UpToDate,
            UpdateState::Downloading(downloadable()),
            UpdateState::Verifying(downloadable()),
            failed(None),
        ];
        for state in states {
            for mode in modes() {
                assert_eq!(notice(&state, &mode, None), None, "{state:?} {mode:?}");
            }
        }
    }

    #[test]
    fn only_staged_and_installing_suspend_checks() {
        for state in states() {
            let expected = matches!(
                state,
                UpdateState::Staged { .. } | UpdateState::Installing(_)
            );
            assert_eq!(state.suspends_checks(), expected, "{state:?}");
        }
    }
}
