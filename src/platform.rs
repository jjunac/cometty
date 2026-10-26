//! Launch-context helpers shared by the macOS Dock icon and PTY setup.
//!
//! A macOS `.app` always puts `CFBundleExecutable` under
//! `Contents/MacOS`, so the running path is enough to distinguish a
//! LaunchServices/Finder launch from `cargo run`. This is deliberately pure
//! and testable instead of an `NSBundle` query.

#[cfg(target_os = "macos")]
use std::path::Path;

/// True when `path` is the main executable inside a macOS `.app` bundle.
#[cfg(target_os = "macos")]
pub(crate) fn is_app_bundle_executable(path: &Path) -> bool {
    path.to_string_lossy().contains(".app/Contents/MacOS/")
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
    fn test_binary_is_not_in_an_app_bundle() {
        assert!(!launched_from_app_bundle());
    }
}
