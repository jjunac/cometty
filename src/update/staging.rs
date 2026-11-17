//! Staging directory names, the staged record, and cleanup planning.
//!
//! Layout of `${XDG_STATE_HOME:-$HOME/.local/state}/cometty/updates/`
//! (see `docs/update.md#staging-directory-layout`):
//!
//! ```text
//! lock                         flock held by every update job
//! download-<v>.zip.part        archive being streamed (at most one)
//! extract-<v>-<pid>-<nanos>/   extraction directory containing Cometty.app
//! staged.json                  StagedRecord: its presence defines the staged update
//! ```
//!
//! Everything here is pure: names, the record, and [`classify_entry`]. The
//! worker lists the directory and applies the plan.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::version::{Version, parse_plain};

/// On-disk schema of [`StagedRecord`].
pub(crate) const SCHEMA: u32 = 1;

/// The `flock` file serializing every writer of the directory.
pub(crate) const LOCK: &str = "lock";
/// The [`StagedRecord`]; written last, so it is the staging commit point.
pub(crate) const STAGED: &str = "staged.json";

const PARTIAL_PREFIX: &str = "download-";
const PARTIAL_SUFFIX: &str = ".zip.part";
const EXTRACT_PREFIX: &str = "extract-";

/// `${XDG_STATE_HOME:-$HOME/.local/state}/cometty/updates`, with the same
/// base rules as the session snapshot; `None` without a usable `HOME`.
pub(crate) fn default_dir() -> Option<PathBuf> {
    dir_from(
        std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

fn dir_from(state_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    Some(
        crate::session::state_base(state_home, home)?
            .join("cometty")
            .join("updates"),
    )
}

/// `download-<v>.zip.part`: the archive while it streams.
pub(crate) fn partial_name(v: Version) -> String {
    format!("{PARTIAL_PREFIX}{v}{PARTIAL_SUFFIX}")
}

/// `extract-<v>-<pid>-<nanos>`: unique per job, so a stale directory from an
/// interrupted run never collides with a new extraction.
pub(crate) fn extract_name(v: Version, pid: u32, nanos: u128) -> String {
    format!("{EXTRACT_PREFIX}{v}-{pid}-{nanos}")
}

/// `staged.json`: a verified, extracted bundle ready to install.
#[derive(Serialize, Deserialize, PartialEq, Eq, Clone, Debug)]
pub(crate) struct StagedRecord {
    pub schema: u32,
    pub version: Version,
    /// Lowercase hex SHA-256 of the verified archive. A check compares it
    /// with the published digest, so a re-uploaded release is downloaded
    /// again.
    pub sha256: String,
    /// Bare name of the `extract-*` directory holding `Cometty.app`.
    pub extract_dir: String,
}

/// One staging directory entry, by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    /// `download-<v>.zip.part`.
    Partial(Version),
    /// `extract-<v>-<pid>-<nanos>`, with its full name.
    Extract(Version, String),
    Staged,
    Lock,
    /// Anything this module would not have produced; never removed.
    Unknown(String),
}

/// Classify a bare directory entry name. Only canonical names (those
/// [`partial_name`] and [`extract_name`] produce) are recognized; anything
/// else, including near misses, is `Unknown`.
pub(crate) fn classify_entry(name: &str) -> Entry {
    match name {
        LOCK => return Entry::Lock,
        STAGED => return Entry::Staged,
        _ => {}
    }
    if let Some(v) = name
        .strip_prefix(PARTIAL_PREFIX)
        .and_then(|rest| rest.strip_suffix(PARTIAL_SUFFIX))
        .and_then(parse_plain)
    {
        return Entry::Partial(v);
    }
    if let Some(v) = parse_extract(name) {
        return Entry::Extract(v, name.to_string());
    }
    Entry::Unknown(name.to_string())
}

/// The version of a canonical `extract-<v>-<pid>-<nanos>` name. Versions
/// contain no `-`, so the three fields split unambiguously; the name must
/// round-trip through [`extract_name`] (no leading zeros, signs, or overflow).
fn parse_extract(name: &str) -> Option<Version> {
    let mut fields = name.strip_prefix(EXTRACT_PREFIX)?.split('-');
    let v = parse_plain(fields.next()?)?;
    let pid: u32 = fields.next()?.parse().ok()?;
    let nanos: u128 = fields.next()?.parse().ok()?;
    if fields.next().is_some() || extract_name(v, pid, nanos) != name {
        return None;
    }
    Some(v)
}

/// The names in `listing` (bare entries of the staging directory) to remove,
/// in listing order. The caller holds the lock, so no download is running.
///
/// - `staged` is the `staged.json` the caller still trusts, `None` when it
///   is absent, unreadable, or superseded. It is kept only when its schema is
///   current, its version is newer than `running`, and its `extract_dir` is
///   a listed canonical extract of that version. Otherwise `staged.json` is
///   removed with every extract.
/// - Every extract except the kept record's is removed, so at most one
///   remains: an extraction without a record is partial.
/// - Every partial download is removed; downloads never resume.
/// - `lock` and `Unknown` names are never listed. `Unknown` includes
///   interrupted atomic-write temp files, which are deliberately left behind
///   rather than matched by a looser pattern.
pub(crate) fn plan_cleanup(
    listing: &[String],
    staged: Option<&StagedRecord>,
    running: Version,
) -> Vec<String> {
    let entries: Vec<(&String, Entry)> = listing.iter().map(|n| (n, classify_entry(n))).collect();
    let keep_extract = staged
        .filter(|r| r.schema == SCHEMA && r.version > running)
        .and_then(|r| {
            entries.iter().find_map(|(name, entry)| match entry {
                Entry::Extract(v, _) if *v == r.version && **name == r.extract_dir => {
                    Some(name.as_str())
                }
                _ => None,
            })
        });
    entries
        .iter()
        .filter(|(name, entry)| match entry {
            Entry::Partial(_) => true,
            Entry::Extract(..) => Some(name.as_str()) != keep_extract,
            Entry::Staged => keep_extract.is_none(),
            Entry::Lock | Entry::Unknown(_) => false,
        })
        .map(|(name, _)| (*name).clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn dir_uses_the_session_state_base() {
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(
            dir_from(Some("/state".into()), home.clone()),
            Some(PathBuf::from("/state/cometty/updates"))
        );
        assert_eq!(
            dir_from(Some("relative".into()), home.clone()),
            Some(PathBuf::from("/home/u/.local/state/cometty/updates")),
            "a relative XDG_STATE_HOME is ignored"
        );
        assert_eq!(
            dir_from(Some("/state".into()), None),
            Some("/state/cometty/updates".into())
        );
        assert_eq!(dir_from(None, None), None, "no HOME, no directory");
        assert_eq!(dir_from(Some("relative".into()), None), None);
    }

    #[test]
    fn classifies_every_layout_name() {
        assert_eq!(classify_entry("lock"), Entry::Lock);
        assert_eq!(classify_entry("staged.json"), Entry::Staged);
        assert_eq!(
            classify_entry("download-1.2.3.zip.part"),
            Entry::Partial(v(1, 2, 3))
        );
        assert_eq!(
            classify_entry("extract-0.10.0-4242-1793000000123456789"),
            Entry::Extract(
                v(0, 10, 0),
                "extract-0.10.0-4242-1793000000123456789".into()
            )
        );
    }

    #[test]
    fn name_helpers_round_trip() {
        let version = v(2, 0, 17);
        assert_eq!(partial_name(version), "download-2.0.17.zip.part");
        assert_eq!(
            classify_entry(&partial_name(version)),
            Entry::Partial(version)
        );
        let name = extract_name(version, u32::MAX, u128::MAX);
        assert_eq!(classify_entry(&name), Entry::Extract(version, name.clone()));
        let name = extract_name(version, 0, 0);
        assert_eq!(name, "extract-2.0.17-0-0");
        assert_eq!(classify_entry(&name), Entry::Extract(version, name.clone()));
    }

    #[test]
    fn unknown_names_stay_unknown() {
        for name in [
            "",
            "Lock",
            "staged.json.tmp",
            ".staged.json",
            "install.json",
            "install.log",
            "download-1.2.zip.part",
            "download-1.2.3.zip",
            "download-01.2.3.zip.part",
            "download-v1.2.3.zip.part",
            "download-.zip.part",
            "extract-1.2.3",
            "extract-1.2.3-1",
            "extract-1.2.3-1-2-3",
            "extract-1.2.3-01-2",
            "extract-1.2.3-1-02",
            "extract-1.2.3-+1-2",
            "extract-1.2.3-4294967296-2",
            "extract-1.2-1-2",
            "extract-1.2.3--2",
            "Cometty.app",
            ".DS_Store",
        ] {
            assert_eq!(
                classify_entry(name),
                Entry::Unknown(name.to_string()),
                "{name:?} must be unknown"
            );
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn staged(version: Version, extract_dir: &str) -> StagedRecord {
        StagedRecord {
            schema: SCHEMA,
            version,
            sha256: "00".repeat(32),
            extract_dir: extract_dir.into(),
        }
    }

    /// Names that `plan_cleanup` must never list, whatever else is present.
    const PROTECTED: [&str; 4] = [LOCK, "staged.json.tmp", "Cometty.app", "extract-1.2.3"];

    fn with_protected(list: &[&str]) -> Vec<String> {
        let mut all = names(list);
        all.extend(names(&PROTECTED));
        all
    }

    #[test]
    fn cleanup_removes_an_older_or_current_staged_record() {
        let running = v(1, 2, 0);
        for version in [v(1, 1, 9), running] {
            let extract = extract_name(version, 1, 2);
            let listing = with_protected(&[STAGED, &extract, &partial_name(version)]);
            assert_eq!(
                plan_cleanup(&listing, Some(&staged(version, &extract)), running),
                names(&[STAGED, &extract, &partial_name(version)]),
                "{version} is not newer than {running}"
            );
        }
    }

    #[test]
    fn cleanup_keeps_a_newer_staged_record() {
        let running = v(1, 2, 0);
        let extract = extract_name(v(1, 3, 0), 1, 2);
        let listing = with_protected(&[STAGED, &extract]);
        let record = staged(v(1, 3, 0), &extract);
        assert!(plan_cleanup(&listing, Some(&record), running).is_empty());
    }

    #[test]
    fn cleanup_removes_orphaned_extracts() {
        let running = v(1, 0, 0);
        let kept = extract_name(v(2, 0, 0), 5, 6);
        let orphan_same_version = extract_name(v(2, 0, 0), 5, 7);
        let orphan_newer = extract_name(v(3, 0, 0), 1, 1);
        let listing = with_protected(&[&orphan_same_version, STAGED, &kept, &orphan_newer]);
        assert_eq!(
            plan_cleanup(&listing, Some(&staged(v(2, 0, 0), &kept)), running),
            names(&[&orphan_same_version, &orphan_newer])
        );

        // No record: every extract is an interrupted extraction, and an
        // unreadable or superseded staged.json goes with them.
        assert_eq!(
            plan_cleanup(&listing, None, running),
            names(&[&orphan_same_version, STAGED, &kept, &orphan_newer])
        );

        // A record whose directory is missing, of another version, or of
        // another schema references nothing.
        for record in [
            staged(v(2, 0, 0), &extract_name(v(2, 0, 0), 9, 9)),
            staged(v(3, 0, 0), &kept),
            StagedRecord {
                schema: SCHEMA + 1,
                ..staged(v(2, 0, 0), &kept)
            },
        ] {
            assert_eq!(
                plan_cleanup(&listing, Some(&record), running),
                names(&[&orphan_same_version, STAGED, &kept, &orphan_newer]),
                "{record:?}"
            );
        }
    }

    #[test]
    fn cleanup_removes_every_partial() {
        let running = v(1, 0, 0);
        let all = [
            partial_name(v(0, 9, 0)),
            partial_name(v(1, 2, 0)),
            partial_name(running),
        ];
        let listing = with_protected(&all.each_ref().map(String::as_str));
        assert_eq!(plan_cleanup(&listing, None, running), all);
    }

    #[test]
    fn cleanup_never_lists_protected_names() {
        assert!(plan_cleanup(&names(&PROTECTED), None, v(0, 0, 0)).is_empty());
    }

    #[test]
    fn record_round_trips_through_json() {
        let staged = StagedRecord {
            schema: SCHEMA,
            version: v(1, 2, 3),
            sha256: "ab".repeat(32),
            extract_dir: extract_name(v(1, 2, 3), 7, 9),
        };
        let json = serde_json::to_vec_pretty(&staged).unwrap();
        assert_eq!(
            serde_json::from_slice::<StagedRecord>(&json).unwrap(),
            staged
        );
    }
}
