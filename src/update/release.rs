//! Release feed metadata parsing, status/release classification, and asset
//! selection (`docs/update.md#check-job`).
//!
//! Everything here is pure: the check job fetches the feed body (capped at
//! `limits::FEED_CAP`) and hands the bytes and status to these helpers.

use std::fmt;

use serde::Deserialize;

use super::limits::{ARCHIVE_CAP, CHECKSUM_CAP};
use super::state::Failure;
use super::version::{self, Version};

/// Prefix an `html_url` must carry to be offered as the release page.
const RELEASES_PREFIX: &str = "https://github.com/jjunac/cometty/releases/";
/// Release page used when the feed's `html_url` is missing or untrusted.
pub(crate) const LATEST_RELEASE_URL: &str = "https://github.com/jjunac/cometty/releases/latest";

// ---------------------------------------------------------------------------
// Wire format
// ---------------------------------------------------------------------------

/// The subset of the release feed document the updater reads.
///
/// serde ignores every other field. `tag_name` and `assets` are required, so
/// a document without them is a parse error.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct RawRelease {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub html_url: Option<String>,
    pub assets: Vec<RawAsset>,
}

/// One entry of the release `assets` list.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct RawAsset {
    pub name: String,
    pub size: u64,
    pub browser_download_url: String,
}

// ---------------------------------------------------------------------------
// Feed status
// ---------------------------------------------------------------------------

/// What to do with a release feed response, by HTTP status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusClass {
    /// 200: read and parse the body.
    Parse,
    /// 404: the repository has no published release.
    UpToDate,
    /// Any other status fails the check.
    Failed(u16),
}

pub(crate) fn classify_status(status: u16) -> StatusClass {
    match status {
        200 => StatusClass::Parse,
        404 => StatusClass::UpToDate,
        code => StatusClass::Failed(code),
    }
}

/// Parses a release feed body; invalid JSON or a missing required field is
/// `Failure::Parse` with serde's message.
pub(crate) fn parse_release(body: &[u8]) -> Result<RawRelease, Failure> {
    serde_json::from_slice(body).map_err(|err| Failure::Parse(err.to_string()))
}

// ---------------------------------------------------------------------------
// Release classification
// ---------------------------------------------------------------------------

/// The result of comparing the latest release with the running version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CheckOutcome {
    UpToDate(UpToDateReason),
    Newer(Release),
}

/// Why a parsed release is not offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UpToDateReason {
    /// `draft` is true. Checked first.
    Draft,
    /// `prerelease` is true.
    Prerelease,
    /// `tag_name` is not `vX.Y.Z`; the worker logs it at `info`.
    BadTag(String),
    /// The release version is equal to or older than the running one.
    NotNewer,
}

/// A release strictly newer than the running version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Release {
    pub version: Version,
    /// The page opened from a Notify_Only notice; see [`release_page_url`].
    pub page_url: String,
    /// The download pair, or why this release falls back to Notify_Only
    /// behavior.
    pub assets: Result<SelectedAssets, AssetProblem>,
}

/// One selected release asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Asset {
    pub name: String,
    /// The advertised size in bytes; the archive download cap is derived from it.
    pub size: u64,
    /// An `https` download URL.
    pub url: String,
}

/// The archive and its checksum file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SelectedAssets {
    pub archive: Asset,
    pub checksum: Asset,
}

pub(crate) fn classify_release(raw: RawRelease, running: Version) -> CheckOutcome {
    if raw.draft {
        return CheckOutcome::UpToDate(UpToDateReason::Draft);
    }
    if raw.prerelease {
        return CheckOutcome::UpToDate(UpToDateReason::Prerelease);
    }
    let Some(version) = version::parse_tag(&raw.tag_name) else {
        return CheckOutcome::UpToDate(UpToDateReason::BadTag(raw.tag_name));
    };
    if version <= running {
        return CheckOutcome::UpToDate(UpToDateReason::NotNewer);
    }
    CheckOutcome::Newer(Release {
        version,
        page_url: release_page_url(raw.html_url.as_deref()),
        assets: select_assets(&raw.assets, version),
    })
}

// ---------------------------------------------------------------------------
// Asset selection
// ---------------------------------------------------------------------------

/// Why a newer release cannot be downloaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AssetProblem {
    /// No asset carries this exact name.
    Missing(String),
    /// More than one asset carries this exact name.
    Duplicate(String),
    /// The archive's advertised size, over `ARCHIVE_CAP`.
    ArchiveTooLarge(u64),
    /// The checksum's advertised size, over `CHECKSUM_CAP`.
    ChecksumTooLarge(u64),
    /// The rejected `browser_download_url`.
    NotHttps(String),
}

impl fmt::Display for AssetProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(name) => write!(f, "release asset {name} is missing"),
            Self::Duplicate(name) => write!(f, "release asset {name} is listed more than once"),
            Self::ArchiveTooLarge(size) => {
                write!(
                    f,
                    "archive size {size} exceeds the {ARCHIVE_CAP}-byte limit"
                )
            }
            Self::ChecksumTooLarge(size) => {
                write!(
                    f,
                    "checksum size {size} exceeds the {CHECKSUM_CAP}-byte limit"
                )
            }
            Self::NotHttps(url) => write!(f, "asset URL {url} does not use https"),
        }
    }
}

/// `Cometty-<v>-macos-universal.zip`, the archive name for `v`.
pub(crate) fn archive_name(v: Version) -> String {
    format!("Cometty-{v}-macos-universal.zip")
}

/// `<archive>.sha256`, the checksum file name for `v`.
pub(crate) fn checksum_name(v: Version) -> String {
    format!("{}.sha256", archive_name(v))
}

/// Selects the archive and checksum file for `v` by exact name
///.
///
/// Checks run in a fixed order, so the reported problem is deterministic:
/// archive name, checksum name, archive size, checksum size, archive URL,
/// checksum URL. Sizes are the advertised `size` values.
pub(crate) fn select_assets(
    assets: &[RawAsset],
    v: Version,
) -> Result<SelectedAssets, AssetProblem> {
    let archive = find_unique(assets, &archive_name(v))?;
    let checksum = find_unique(assets, &checksum_name(v))?;
    if archive.size > ARCHIVE_CAP {
        return Err(AssetProblem::ArchiveTooLarge(archive.size));
    }
    if checksum.size > CHECKSUM_CAP {
        return Err(AssetProblem::ChecksumTooLarge(checksum.size));
    }
    for asset in [archive, checksum] {
        if !is_https(&asset.browser_download_url) {
            return Err(AssetProblem::NotHttps(asset.browser_download_url.clone()));
        }
    }
    Ok(SelectedAssets {
        archive: selected(archive),
        checksum: selected(checksum),
    })
}

fn find_unique<'a>(assets: &'a [RawAsset], name: &str) -> Result<&'a RawAsset, AssetProblem> {
    let mut matches = assets.iter().filter(|a| a.name == name);
    let first = matches
        .next()
        .ok_or_else(|| AssetProblem::Missing(name.to_owned()))?;
    if matches.next().is_some() {
        return Err(AssetProblem::Duplicate(name.to_owned()));
    }
    Ok(first)
}

fn selected(raw: &RawAsset) -> Asset {
    Asset {
        name: raw.name.clone(),
        size: raw.size,
        url: raw.browser_download_url.clone(),
    }
}

/// Whether `url` has the `https` scheme (case-insensitive) and a non-empty
/// host.
///
/// The host is the authority after any `user@` prefix; an authority that is
/// empty or only a port (`https://:443/`) is rejected.
pub(crate) fn is_https(url: &str) -> bool {
    const SCHEME: &str = "https://";
    let Some(scheme) = url.get(..SCHEME.len()) else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case(SCHEME) {
        return false;
    }
    let rest = &url[SCHEME.len()..];
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or_default();
    !host.is_empty() && !host.starts_with(':')
}

/// The release page offered by a Notify_Only notice.
///
/// `html_url` is kept only when it lies under this repository's GitHub
/// releases and contains nothing but printable, non-space ASCII; otherwise
/// the `/releases/latest` page is used. Opening still goes through
/// `links::open`'s scheme allowlist.
pub(crate) fn release_page_url(html_url: Option<&str>) -> String {
    match html_url {
        Some(url)
            if url.starts_with(RELEASES_PREFIX) && url.bytes().all(|b| b.is_ascii_graphic()) =>
        {
            url.to_owned()
        }
        _ => LATEST_RELEASE_URL.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// A realistic GitHub "latest release" response for v1.3.0, including
    /// unknown fields at the top level, in nested objects, and in assets.
    const LATEST_FIXTURE: &str = include_str!("testdata/release-latest.json");

    const RUNNING: Version = v(1, 2, 3);
    const NEW: Version = v(1, 3, 0);

    const fn v(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    fn url(name: &str) -> String {
        format!("https://github.com/jjunac/cometty/releases/download/v1.3.0/{name}")
    }

    fn asset(name: &str, size: u64, url: &str) -> Value {
        json!({ "name": name, "size": size, "browser_download_url": url })
    }

    /// Archive and checksum assets for `NEW`, with the given sizes.
    fn pair(archive_size: u64, checksum_size: u64) -> Vec<Value> {
        let (zip, sum) = (archive_name(NEW), checksum_name(NEW));
        vec![
            asset(&zip, archive_size, &url(&zip)),
            asset(&sum, checksum_size, &url(&sum)),
        ]
    }

    /// A minimal feed document without `html_url`.
    fn feed(tag: &str, draft: bool, prerelease: bool, assets: Vec<Value>) -> Vec<u8> {
        let doc = json!({
            "tag_name": tag, "draft": draft, "prerelease": prerelease, "assets": assets,
        });
        serde_json::to_vec(&doc).unwrap()
    }

    fn check(body: &[u8]) -> CheckOutcome {
        classify_release(parse_release(body).expect("fixture parses"), RUNNING)
    }

    /// The `assets` result of a newer release, failing on any other outcome.
    fn newer_assets(body: &[u8]) -> Result<SelectedAssets, AssetProblem> {
        match check(body) {
            CheckOutcome::Newer(release) => {
                assert_eq!(release.version, NEW);
                release.assets
            }
            other => panic!("expected Newer, got {other:?}"),
        }
    }

    fn expected_assets(archive_size: u64, checksum_size: u64) -> SelectedAssets {
        let (zip, sum) = (archive_name(NEW), checksum_name(NEW));
        SelectedAssets {
            archive: Asset {
                url: url(&zip),
                name: zip,
                size: archive_size,
            },
            checksum: Asset {
                url: url(&sum),
                name: sum,
                size: checksum_size,
            },
        }
    }

    // -- Feed status and parsing ------------------------------------------

    #[test]
    fn status_classification() {
        assert_eq!(classify_status(200), StatusClass::Parse);
        assert_eq!(classify_status(404), StatusClass::UpToDate);
        for code in [204, 301, 403, 429, 500, 503] {
            assert_eq!(classify_status(code), StatusClass::Failed(code));
        }
    }

    #[test]
    fn invalid_or_incomplete_metadata_is_a_parse_failure() {
        let bodies: [&[u8]; 6] = [
            b"",
            b"not json",
            br#"{"tag_name": "v1.3.0", "assets": []"#,
            br#"{"assets": []}"#,
            br#"{"tag_name": "v1.3.0"}"#,
            br#"{"tag_name": "v1.3.0", "assets": [{"name": "x", "size": -1, "browser_download_url": "https://a/"}]}"#,
        ];
        for body in bodies {
            assert!(
                matches!(parse_release(body), Err(Failure::Parse(_))),
                "{:?}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn optional_fields_default() {
        let raw = parse_release(br#"{"tag_name": "v1.3.0", "html_url": null, "assets": []}"#)
            .expect("parses");
        assert_eq!(
            raw,
            RawRelease {
                tag_name: "v1.3.0".into(),
                draft: false,
                prerelease: false,
                html_url: None,
                assets: vec![],
            }
        );
    }

    // -- Release classification -------------------------------------------

    #[test]
    fn newer_release_selects_both_assets() {
        assert_eq!(
            check(LATEST_FIXTURE.as_bytes()),
            CheckOutcome::Newer(Release {
                version: NEW,
                page_url: "https://github.com/jjunac/cometty/releases/tag/v1.3.0".into(),
                assets: Ok(expected_assets(12_345_678, 100)),
            })
        );
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let minimal = json!({
            "tag_name": "v1.3.0",
            "draft": false,
            "prerelease": false,
            "html_url": "https://github.com/jjunac/cometty/releases/tag/v1.3.0",
            "assets": [
                asset(&archive_name(NEW), 12_345_678, &url(&archive_name(NEW))),
                asset(&checksum_name(NEW), 100, &url(&checksum_name(NEW))),
            ],
        });
        let minimal = serde_json::to_vec(&minimal).unwrap();
        assert_eq!(
            parse_release(LATEST_FIXTURE.as_bytes()).unwrap(),
            parse_release(&minimal).unwrap()
        );
        assert_eq!(check(LATEST_FIXTURE.as_bytes()), check(&minimal));
    }

    #[test]
    fn missing_html_url_falls_back_to_latest_page() {
        match check(&feed("v1.3.0", false, false, pair(10, 100))) {
            CheckOutcome::Newer(release) => assert_eq!(release.page_url, LATEST_RELEASE_URL),
            other => panic!("expected Newer, got {other:?}"),
        }
    }

    #[test]
    fn equal_release_is_up_to_date() {
        assert_eq!(
            check(&feed("v1.2.3", false, false, pair(10, 100))),
            CheckOutcome::UpToDate(UpToDateReason::NotNewer)
        );
    }

    #[test]
    fn older_release_is_up_to_date() {
        for tag in ["v1.2.2", "v1.1.9", "v0.99.99"] {
            assert_eq!(
                check(&feed(tag, false, false, pair(10, 100))),
                CheckOutcome::UpToDate(UpToDateReason::NotNewer),
                "{tag}"
            );
        }
    }

    #[test]
    fn draft_is_up_to_date() {
        assert_eq!(
            check(&feed("v1.3.0", true, false, pair(10, 100))),
            CheckOutcome::UpToDate(UpToDateReason::Draft)
        );
        // Draft wins over prerelease.
        assert_eq!(
            check(&feed("v1.3.0", true, true, pair(10, 100))),
            CheckOutcome::UpToDate(UpToDateReason::Draft)
        );
    }

    #[test]
    fn prerelease_is_up_to_date() {
        assert_eq!(
            check(&feed("v1.3.0", false, true, pair(10, 100))),
            CheckOutcome::UpToDate(UpToDateReason::Prerelease)
        );
    }

    #[test]
    fn malformed_tag_is_up_to_date() {
        for tag in [
            "1.3.0",
            "v1.3",
            "v1.3.0-rc.1",
            "v01.3.0",
            "V1.3.0",
            " v1.3.0",
            "",
        ] {
            assert_eq!(
                check(&feed(tag, false, false, pair(10, 100))),
                CheckOutcome::UpToDate(UpToDateReason::BadTag(tag.into())),
                "{tag:?}"
            );
        }
    }

    // -- Asset selection --------------------------------------------------

    #[test]
    fn missing_archive() {
        let assets = vec![pair(10, 100).remove(1)];
        assert_eq!(
            newer_assets(&feed("v1.3.0", false, false, assets)),
            Err(AssetProblem::Missing(archive_name(NEW)))
        );
        // An archive for another version does not count.
        let old = archive_name(RUNNING);
        let mut assets = pair(10, 100);
        assets[0] = asset(&old, 10, &url(&old));
        assert_eq!(
            newer_assets(&feed("v1.3.0", false, false, assets)),
            Err(AssetProblem::Missing(archive_name(NEW)))
        );
    }

    #[test]
    fn missing_checksum() {
        let assets = vec![pair(10, 100).remove(0)];
        assert_eq!(
            newer_assets(&feed("v1.3.0", false, false, assets)),
            Err(AssetProblem::Missing(checksum_name(NEW)))
        );
    }

    #[test]
    fn oversized_archive() {
        assert_eq!(
            newer_assets(&feed("v1.3.0", false, false, pair(ARCHIVE_CAP + 1, 100))),
            Err(AssetProblem::ArchiveTooLarge(ARCHIVE_CAP + 1))
        );
        assert_eq!(
            newer_assets(&feed("v1.3.0", false, false, pair(ARCHIVE_CAP, 100))),
            Ok(expected_assets(ARCHIVE_CAP, 100))
        );
    }

    #[test]
    fn oversized_checksum() {
        assert_eq!(
            newer_assets(&feed("v1.3.0", false, false, pair(10, CHECKSUM_CAP + 1))),
            Err(AssetProblem::ChecksumTooLarge(CHECKSUM_CAP + 1))
        );
        assert_eq!(
            newer_assets(&feed("v1.3.0", false, false, pair(10, CHECKSUM_CAP))),
            Ok(expected_assets(10, CHECKSUM_CAP))
        );
    }

    #[test]
    fn non_https_url() {
        let insecure = "http://github.com/jjunac/cometty/releases/download/v1.3.0/x.zip";
        let mut assets = pair(10, 100);
        assets[0]["browser_download_url"] = json!(insecure);
        assert_eq!(
            newer_assets(&feed("v1.3.0", false, false, assets)),
            Err(AssetProblem::NotHttps(insecure.into()))
        );

        let mut assets = pair(10, 100);
        assets[1]["browser_download_url"] = json!("ftp://example.com/x.sha256");
        assert_eq!(
            newer_assets(&feed("v1.3.0", false, false, assets)),
            Err(AssetProblem::NotHttps("ftp://example.com/x.sha256".into()))
        );
    }

    #[test]
    fn duplicate_asset() {
        let mut assets = pair(10, 100);
        assets.push(assets[1].clone());
        assert_eq!(
            newer_assets(&feed("v1.3.0", false, false, assets)),
            Err(AssetProblem::Duplicate(checksum_name(NEW)))
        );
    }

    #[test]
    fn asset_problem_display_names_the_detail() {
        let name = archive_name(NEW);
        assert!(
            AssetProblem::Missing(name.clone())
                .to_string()
                .contains(&name)
        );
        assert!(
            AssetProblem::ArchiveTooLarge(999)
                .to_string()
                .contains("999")
        );
        assert!(
            AssetProblem::NotHttps("http://x/".into())
                .to_string()
                .contains("http://x/")
        );
    }

    #[test]
    fn https_scheme_detection() {
        for url in [
            "https://github.com/a",
            "HTTPS://github.com",
            "HttpS://h?q",
            "https://user@host:443/p",
        ] {
            assert!(is_https(url), "{url}");
        }
        for url in [
            "",
            "http://github.com/a",
            "https:/github.com",
            "https://",
            "https:///path",
            "https://:443/",
            "https://user@/p",
            "ftp://github.com",
            " https://github.com",
            "httpsé://x",
        ] {
            assert!(!is_https(url), "{url}");
        }
    }

    // -- Release page ------------------------------------------------------

    #[test]
    fn release_page_url_keeps_repository_pages_only() {
        let valid = "https://github.com/jjunac/cometty/releases/tag/v1.3.0";
        assert_eq!(release_page_url(Some(valid)), valid);
        for foreign in [
            "https://evil.example/jjunac/cometty/releases/tag/v1.3.0",
            "https://github.com/jjunac/cometty-evil/releases/tag/v1.3.0",
            "https://github.com.evil.example/jjunac/cometty/releases/",
            "http://github.com/jjunac/cometty/releases/tag/v1.3.0",
            "https://github.com/jjunac/cometty/releases/tag/v1 x",
            "",
        ] {
            assert_eq!(
                release_page_url(Some(foreign)),
                LATEST_RELEASE_URL,
                "{foreign}"
            );
        }
        assert_eq!(release_page_url(None), LATEST_RELEASE_URL);
    }
}
