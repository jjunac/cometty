//! Release version parsing (`vX.Y.Z` tags and plain `X.Y.Z`) and numeric
//! ordering (`docs/update.md#check-job`).

use std::fmt;

use serde::{Deserialize, Serialize};

/// A `major.minor.patch` release version.
///
/// The derived `Ord` compares fields in declaration order: major, then minor,
/// then patch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

/// Parses a release `tag_name` of the exact form `vX.Y.Z`.
pub(crate) fn parse_tag(tag: &str) -> Option<Version> {
    parse_plain(tag.strip_prefix('v')?)
}

/// Parses a plain `X.Y.Z` version (`CARGO_PKG_VERSION`, plist values).
///
/// Each component is a non-empty run of ASCII digits with no leading zero
/// unless it is exactly `0`, and fits in a `u64`. Nothing else is accepted, so
/// `parse_plain(s).map(|v| v.to_string()) == Some(s)` for every accepted `s`.
pub(crate) fn parse_plain(s: &str) -> Option<Version> {
    let mut parts = s.split('.');
    let major = parse_component(parts.next()?)?;
    let minor = parse_component(parts.next()?)?;
    let patch = parse_component(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some(Version {
        major,
        minor,
        patch,
    })
}

/// One canonical decimal run. `u64::from_str` alone would accept a leading
/// `+` and leading zeros, so the digits are checked first.
fn parse_component(run: &str) -> Option<u64> {
    let bytes = run.as_bytes();
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if bytes.len() > 1 && bytes[0] == b'0' {
        return None;
    }
    run.parse().ok()
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::RUNNING_VERSION;

    fn v(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn running_version_parses_and_round_trips() {
        let parsed = parse_plain(RUNNING_VERSION).expect("CARGO_PKG_VERSION is X.Y.Z");
        assert_eq!(parsed.to_string(), RUNNING_VERSION);
    }

    #[test]
    fn accepts_canonical_tags() {
        assert_eq!(parse_tag("v0.2.0"), Some(v(0, 2, 0)));
        assert_eq!(parse_tag("v10.0.1"), Some(v(10, 0, 1)));
        assert_eq!(
            parse_tag("v18446744073709551615.0.0"),
            Some(v(u64::MAX, 0, 0))
        );
        assert_eq!(parse_plain("1.2.3"), Some(v(1, 2, 3)));
    }

    #[test]
    fn rejects_non_canonical_tags() {
        for tag in [
            "v01.2.3",
            "v1.02.3",
            "v1.2.03",
            "v1.2",
            "v1.2.3.4",
            "v1.2.3-rc1",
            "v1.2.3+build",
            " v1.2.3",
            "v1.2.3 ",
            "v 1.2.3",
            "1.2.3",
            "V1.2.3",
            "vv1.2.3",
            "v+1.2.3",
            "v-1.2.3",
            "v1..3",
            "v1.2.",
            "v.1.2.3",
            "v",
            "",
            "v1.2.18446744073709551616",
            "v١.2.3",
        ] {
            assert_eq!(parse_tag(tag), None, "{tag:?} must be rejected");
        }
    }

    #[test]
    fn plain_rejects_tag_prefix() {
        assert_eq!(parse_plain("v1.2.3"), None);
    }

    #[test]
    fn display_is_dotted_triple() {
        assert_eq!(v(10, 0, 1).to_string(), "10.0.1");
    }

    #[test]
    fn orders_by_major_then_minor_then_patch() {
        assert!(v(2, 0, 0) > v(1, 99, 99));
        assert!(v(1, 3, 0) > v(1, 2, 99));
        assert!(v(1, 2, 4) > v(1, 2, 3));
        assert!(v(0, 10, 0) > v(0, 9, 0));
        assert_eq!(v(1, 2, 3).cmp(&v(1, 2, 3)), std::cmp::Ordering::Equal);
    }

    #[test]
    fn serde_round_trips() {
        let version = v(1, 2, 3);
        let json = serde_json::to_string(&version).unwrap();
        assert_eq!(serde_json::from_str::<Version>(&json).unwrap(), version);
    }
}
