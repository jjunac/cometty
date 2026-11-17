//! Extracted `Cometty.app` validation rules and minimal `Info.plist` string lookup.
//!
//! [`BundleFacts`] is a pure description of an extraction directory, gathered
//! by `fsops::gather_facts` without following symlinks. [`validate_bundle`]
//! only inspects those facts, so it never touches the filesystem. The rules
//! are listed in `docs/update.md#check-job`.

use crate::update::version::{Version, parse_plain};

/// The only accepted top-level entry of an extraction directory.
pub(crate) const BUNDLE_DIR: &str = "Cometty.app";
/// The `CFBundleIdentifier` every release bundle carries.
pub(crate) const BUNDLE_IDENTIFIER: &str = "com.github.jjunac.cometty";

/// Binary property lists start with this magic; only XML is supported.
const BINARY_PLIST_MAGIC: &[u8] = b"bplist00";

/// What a path is, observed without following symlinks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Dir,
    /// A regular file; `executable` is set when any execute bit is.
    File {
        executable: bool,
    },
    Symlink,
    /// A FIFO, socket, or device node.
    Other,
}

/// The facts `validate_bundle` needs about an extraction directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BundleFacts {
    /// Every top-level entry, by file name.
    pub top_level: Vec<(String, Kind)>,
    /// `Cometty.app/Contents/MacOS/cometty`, or `None` when it is absent.
    pub executable: Option<Kind>,
    /// `Cometty.app/Contents/Info.plist`, read only if it is a regular file
    /// of at most 64 KiB.
    pub plist: Option<Vec<u8>>,
}

/// Why an extracted bundle was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BundleError {
    /// The top level is not exactly one `Cometty.app` directory.
    TopLevel,
    /// `Contents/MacOS/cometty` does not exist.
    MissingExecutable,
    /// `Contents/MacOS/cometty` exists but is not an executable regular file
    /// (a symlink, a non-executable file, a directory, or a special file).
    NotExecutable,
    /// `Contents/Info.plist` is absent, not a regular file, or too large.
    MissingPlist,
    /// `Contents/Info.plist` is a binary property list.
    UnsupportedPlist,
    /// `CFBundleIdentifier` is missing or differs; `found` is the value read.
    Identifier { found: Option<String> },
    /// `CFBundleShortVersionString` is missing or is not the expected
    /// release version; `found` is the value read.
    Version { found: Option<String> },
}

/// Checks the `docs/update.md#check-job` bundle rules against `f` for the
/// release `expected`.
///
/// Checks run in a fixed order (layout, executable, plist), so the error
/// names the first rule that failed.
pub(crate) fn validate_bundle(f: &BundleFacts, expected: Version) -> Result<(), BundleError> {
    match f.top_level.as_slice() {
        [(name, Kind::Dir)] if name == BUNDLE_DIR => {}
        _ => return Err(BundleError::TopLevel),
    }
    match f.executable {
        None => return Err(BundleError::MissingExecutable),
        Some(Kind::File { executable: true }) => {}
        Some(_) => return Err(BundleError::NotExecutable),
    }
    let plist = f.plist.as_deref().ok_or(BundleError::MissingPlist)?;
    if plist.starts_with(BINARY_PLIST_MAGIC) {
        return Err(BundleError::UnsupportedPlist);
    }
    let identifier = plist_string(plist, "CFBundleIdentifier");
    if identifier.as_deref() != Some(BUNDLE_IDENTIFIER) {
        return Err(BundleError::Identifier { found: identifier });
    }
    let version = plist_string(plist, "CFBundleShortVersionString");
    if version.as_deref().and_then(parse_plain) != Some(expected) {
        return Err(BundleError::Version { found: version });
    }
    Ok(())
}

/// Returns the `<string>` value that follows `<key>key</key>` in an XML plist.
///
/// Only whitespace may separate the two elements. Key and value text are
/// unescaped (`&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;`); any other entity,
/// a non-string value, non-UTF-8 input, or a binary plist gives `None`. The
/// first matching key wins. This is not a general plist parser: release
/// bundles always carry the flat XML dictionary from `packaging/macos`.
pub(crate) fn plist_string(xml: &[u8], key: &str) -> Option<String> {
    if xml.starts_with(BINARY_PLIST_MAGIC) {
        return None;
    }
    let text = std::str::from_utf8(xml).ok()?;
    let mut rest = text;
    loop {
        let (raw_key, after) = element(rest, "<key>", "</key>")?;
        rest = after;
        if unescape(raw_key).as_deref() != Some(key) {
            continue;
        }
        let value = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let raw_value = value.strip_prefix("<string>")?;
        let (raw_value, _) = raw_value.split_once("</string>")?;
        return unescape(raw_value);
    }
}

/// Finds the next `open…close` element in `s`, returning its raw text and
/// the remainder after `close`.
fn element<'a>(s: &'a str, open: &str, close: &str) -> Option<(&'a str, &'a str)> {
    let (_, after_open) = s.split_once(open)?;
    after_open.split_once(close)
}

/// Replaces the five predefined XML entities. Character text must not
/// contain a raw `<`, and any other `&` sequence is rejected.
fn unescape(raw: &str) -> Option<String> {
    if raw.contains('<') {
        return None;
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let entity = &rest[amp..];
        let (ch, len) = [
            ('&', "&amp;"),
            ('<', "&lt;"),
            ('>', "&gt;"),
            ('"', "&quot;"),
            ('\'', "&apos;"),
        ]
        .into_iter()
        .find(|(_, name)| entity.starts_with(name))
        .map(|(ch, name)| (ch, name.len()))?;
        out.push(ch);
        rest = &entity[len..];
    }
    out.push_str(rest);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: &[u8] = include_bytes!("../../packaging/macos/Info.plist");

    fn v(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    /// The shipped template with the placeholders `bundle-macos.sh` fills.
    fn plist_for(version: &str) -> Vec<u8> {
        let text = std::str::from_utf8(TEMPLATE).expect("template is UTF-8");
        text.replace("__VERSION__", version)
            .replace("__BUILD__", version)
            .into_bytes()
    }

    fn valid_facts(version: &str) -> BundleFacts {
        BundleFacts {
            top_level: vec![(BUNDLE_DIR.to_owned(), Kind::Dir)],
            executable: Some(Kind::File { executable: true }),
            plist: Some(plist_for(version)),
        }
    }

    #[test]
    fn shipped_template_validates() {
        assert_eq!(validate_bundle(&valid_facts("1.2.3"), v(1, 2, 3)), Ok(()));
    }

    #[test]
    fn plist_string_reads_template_keys() {
        let plist = plist_for("1.2.3");
        assert_eq!(
            plist_string(&plist, "CFBundleIdentifier").as_deref(),
            Some(BUNDLE_IDENTIFIER)
        );
        assert_eq!(
            plist_string(&plist, "CFBundleShortVersionString").as_deref(),
            Some("1.2.3")
        );
        assert_eq!(
            plist_string(&plist, "NSHumanReadableCopyright").as_deref(),
            Some("Copyright © 2026 jjunac")
        );
        // A `<true/>` value is not a string.
        assert_eq!(plist_string(&plist, "NSHighResolutionCapable"), None);
        assert_eq!(plist_string(&plist, "Missing"), None);
    }

    #[test]
    fn plist_string_unescapes_entities() {
        let xml = b"<dict>\n\t<key>a&amp;b</key>\r\n  <string>&lt;x&gt; &quot;y&quot; &apos;z&apos; &amp;amp;</string>\n</dict>";
        assert_eq!(
            plist_string(xml, "a&b").as_deref(),
            Some("<x> \"y\" 'z' &amp;")
        );
        // The raw, escaped key text does not match.
        assert_eq!(plist_string(xml, "a&amp;b"), None);
    }

    #[test]
    fn plist_string_rejects_malformed_values() {
        // Unknown entity.
        assert_eq!(
            plist_string(b"<key>k</key><string>&#65;</string>", "k"),
            None
        );
        // Something other than whitespace between key and string.
        assert_eq!(
            plist_string(b"<key>k</key><!-- c --><string>v</string>", "k"),
            None
        );
        // Unterminated string.
        assert_eq!(plist_string(b"<key>k</key><string>v", "k"), None);
        // Non-UTF-8 input.
        assert_eq!(
            plist_string(b"<key>k</key><string>\xff</string>", "k"),
            None
        );
        // Empty string is a value.
        assert_eq!(
            plist_string(b"<key>k</key><string></string>", "k").as_deref(),
            Some("")
        );
    }

    #[test]
    fn wrong_identifier_is_rejected() {
        let mut facts = valid_facts("1.2.3");
        let text = String::from_utf8(facts.plist.take().unwrap()).unwrap();
        facts.plist = Some(
            text.replace(BUNDLE_IDENTIFIER, "com.example.other")
                .into_bytes(),
        );
        assert_eq!(
            validate_bundle(&facts, v(1, 2, 3)),
            Err(BundleError::Identifier {
                found: Some("com.example.other".to_owned())
            })
        );
    }

    #[test]
    fn wrong_version_is_rejected() {
        assert_eq!(
            validate_bundle(&valid_facts("1.2.4"), v(1, 2, 3)),
            Err(BundleError::Version {
                found: Some("1.2.4".to_owned())
            })
        );
        // Non-canonical spellings do not parse to the expected version.
        assert_eq!(
            validate_bundle(&valid_facts("1.02.3"), v(1, 2, 3)),
            Err(BundleError::Version {
                found: Some("1.02.3".to_owned())
            })
        );
        // The unsubstituted template has no valid version.
        let mut facts = valid_facts("1.2.3");
        facts.plist = Some(TEMPLATE.to_vec());
        assert_eq!(
            validate_bundle(&facts, v(1, 2, 3)),
            Err(BundleError::Version {
                found: Some("__VERSION__".to_owned())
            })
        );
    }

    #[test]
    fn missing_plist_is_rejected() {
        let mut facts = valid_facts("1.2.3");
        facts.plist = None;
        assert_eq!(
            validate_bundle(&facts, v(1, 2, 3)),
            Err(BundleError::MissingPlist)
        );
    }

    #[test]
    fn binary_plist_is_rejected() {
        let mut facts = valid_facts("1.2.3");
        let mut binary = b"bplist00".to_vec();
        binary.extend_from_slice(&plist_for("1.2.3"));
        assert_eq!(plist_string(&binary, "CFBundleIdentifier"), None);
        facts.plist = Some(binary);
        assert_eq!(
            validate_bundle(&facts, v(1, 2, 3)),
            Err(BundleError::UnsupportedPlist)
        );
    }

    #[test]
    fn executable_must_be_an_executable_regular_file() {
        let mut facts = valid_facts("1.2.3");
        for kind in [
            Kind::Symlink,
            Kind::File { executable: false },
            Kind::Dir,
            Kind::Other,
        ] {
            facts.executable = Some(kind);
            assert_eq!(
                validate_bundle(&facts, v(1, 2, 3)),
                Err(BundleError::NotExecutable),
                "{kind:?}"
            );
        }
        facts.executable = None;
        assert_eq!(
            validate_bundle(&facts, v(1, 2, 3)),
            Err(BundleError::MissingExecutable)
        );
    }

    #[test]
    fn top_level_must_be_exactly_one_bundle_directory() {
        let mut facts = valid_facts("1.2.3");
        facts.top_level.push(("__MACOSX".to_owned(), Kind::Dir));
        assert_eq!(
            validate_bundle(&facts, v(1, 2, 3)),
            Err(BundleError::TopLevel)
        );
        for top_level in [
            vec![],
            vec![(BUNDLE_DIR.to_owned(), Kind::Symlink)],
            vec![("Other.app".to_owned(), Kind::Dir)],
        ] {
            facts.top_level = top_level;
            assert_eq!(
                validate_bundle(&facts, v(1, 2, 3)),
                Err(BundleError::TopLevel)
            );
        }
    }
}
