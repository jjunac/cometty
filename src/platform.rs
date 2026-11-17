//! Launch-context helpers shared by the macOS Dock icon and PTY setup.
//!
//! A macOS `.app` always puts `CFBundleExecutable` under
//! `Contents/MacOS`, so the running path is enough to distinguish a
//! LaunchServices/Finder launch from `cargo run`. This is deliberately pure
//! and testable instead of an `NSBundle` query.

#[cfg(target_os = "macos")]
pub(crate) mod titlebar;

#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};

/// True when `path` is the main executable inside a macOS `.app` bundle.
#[cfg(target_os = "macos")]
pub(crate) fn is_app_bundle_executable(path: &Path) -> bool {
    path.to_string_lossy().contains(".app/Contents/MacOS/")
}

/// `/X/Name.app/Contents/MacOS/exe` → `/X/Name.app`; `None` for any other
/// shape.
///
/// Stricter than [`is_app_bundle_executable`]: the executable must sit
/// directly in `Contents/MacOS`, so every path this accepts is also a bundle
/// launch, while a nested helper such as `…/MacOS/sub/exe` has no root.
#[cfg(target_os = "macos")]
pub(crate) fn bundle_root(exe: &Path) -> Option<PathBuf> {
    exe.file_name()?;
    let macos = exe.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    let root = contents.parent()?;
    let name = root.file_name()?.to_str()?;
    (name.len() > ".app".len() && name.ends_with(".app")).then(|| root.to_path_buf())
}

/// True when cometty is running from a macOS `.app` bundle, including
/// translocated bundles.
#[cfg(target_os = "macos")]
pub(crate) fn launched_from_app_bundle() -> bool {
    std::env::current_exe().is_ok_and(|path| is_app_bundle_executable(&path))
}

/// Non-macOS builds have no `.app` launch context.
#[cfg(not(target_os = "macos"))]
pub(crate) fn launched_from_app_bundle() -> bool {
    false
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn detects_app_bundle_layout() {
        assert!(is_app_bundle_executable(Path::new(
            "/Applications/Cometty.app/Contents/MacOS/cometty"
        )));
        assert!(is_app_bundle_executable(Path::new(
            "/private/var/folders/x/AppTranslocation/ABC/d/Cometty.app/Contents/MacOS/cometty"
        )));
        assert!(!is_app_bundle_executable(Path::new(
            "/Users/me/dev/cometty/target/release/cometty"
        )));
        assert!(!is_app_bundle_executable(Path::new(
            "/Applications/Cometty.app/Contents/Resources/cometty"
        )));
    }

    #[test]
    fn bundle_root_strips_the_executable_layout() {
        let root = |p: &str| bundle_root(Path::new(p));
        assert_eq!(
            root("/Applications/Cometty.app/Contents/MacOS/cometty"),
            Some(PathBuf::from("/Applications/Cometty.app"))
        );
        assert_eq!(
            root(
                "/private/var/folders/x/AppTranslocation/ABC/d/Cometty.app/Contents/MacOS/cometty"
            ),
            Some(PathBuf::from(
                "/private/var/folders/x/AppTranslocation/ABC/d/Cometty.app"
            ))
        );
        for other in [
            "/Users/me/dev/cometty/target/release/cometty",
            "/Applications/Cometty.app/Contents/Resources/cometty",
            "/Applications/Cometty.app/Contents/MacOS/sub/cometty",
            "/Applications/Cometty.app/Contents/MacOS/..",
            "/Applications/Cometty.app2/Contents/MacOS/cometty",
            "/Applications/.app/Contents/MacOS/cometty",
            "Contents/MacOS/cometty",
        ] {
            assert_eq!(root(other), None, "{other}");
        }
    }

    #[test]
    fn bundle_root_implies_bundle_detection() {
        for path in [
            "/Applications/Cometty.app/Contents/MacOS/cometty",
            "/Users/me/Applications/Other Name.app/Contents/MacOS/cometty",
        ] {
            let path = Path::new(path);
            assert!(bundle_root(path).is_some());
            assert!(is_app_bundle_executable(path));
        }
    }

    #[test]
    fn test_binary_is_not_in_an_app_bundle() {
        assert!(!launched_from_app_bundle());
    }
}
