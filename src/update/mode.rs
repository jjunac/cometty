//! Updater mode resolution, the bundle's sibling names, and instance counting
//! (`docs/update.md#mode-selection`, `#multiple-instances`).

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// Why this launch only notifies instead of updating itself in place.
// Off macOS `resolve_mode` only yields `NotBundled`; the other reasons stay
// so the UI's match and the tests compile on every platform.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NotifyReason {
    /// Not a `.app` launch (`cargo run`, `cargo install`, non-macOS).
    NotBundled,
    /// The bundle runs from a Gatekeeper App Translocation mount.
    Translocated,
    /// The directory holding the bundle is not writable by this user. Only a
    /// worker's probe reports this; launch-time resolution never does.
    NotWritable,
    /// No staging directory could be derived (no `HOME`).
    NoStagingDir,
}

/// How the updater behaves for this launch.
// `SelfUpdate` is only constructed on macOS (see `resolve_mode`).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Download, verify, stage, and install into `bundle` on confirmed restart.
    SelfUpdate { bundle: PathBuf },
    /// Only report that a release exists.
    NotifyOnly(NotifyReason),
}

/// Resolves the launch mode from path and environment facts only, checking
/// `NotBundled`, `Translocated`, then `NoStagingDir`. Writability is probed
/// later by the workers (see `docs/update.md#mode-selection`).
///
/// `bundled` is `platform::launched_from_app_bundle()`, so the updater and
/// the login-shell default agree on what a bundle launch is. Non-macOS
/// builds always resolve to `NotBundled`.
pub(crate) fn resolve_mode(bundled: bool, exe: &Path, staging_dir: Option<&Path>) -> Mode {
    #[cfg(target_os = "macos")]
    {
        let Some(bundle) = bundled.then(|| crate::platform::bundle_root(exe)).flatten() else {
            return Mode::NotifyOnly(NotifyReason::NotBundled);
        };
        if is_translocated(&bundle) {
            return Mode::NotifyOnly(NotifyReason::Translocated);
        }
        if staging_dir.is_none() {
            return Mode::NotifyOnly(NotifyReason::NoStagingDir);
        }
        Mode::SelfUpdate { bundle }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (bundled, exe, staging_dir);
        Mode::NotifyOnly(NotifyReason::NotBundled)
    }
}

/// Any path component equal to `AppTranslocation`.
// Called only by the macOS `resolve_mode` branch; tested everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn is_translocated(path: &Path) -> bool {
    path.components()
        .any(|c| matches!(c, Component::Normal(name) if name == "AppTranslocation"))
}

/// `(incoming, backup)` next to `installed`: `.<name>.cometty-new` and
/// `.<name>.cometty-old`. Hidden and not ending in `.app`, so LaunchServices
/// never registers them as a second Cometty.
pub(crate) fn sibling_names(installed: &Path) -> (PathBuf, PathBuf) {
    let name = installed.file_name().unwrap_or_default();
    let sibling = |suffix: &str| {
        let mut hidden = OsString::from(".");
        hidden.push(name);
        hidden.push(suffix);
        installed.with_file_name(hidden)
    };
    (sibling(".cometty-new"), sibling(".cometty-old"))
}

/// Entries of `list` (`(pid, executable path)`) whose pid is not `exclude`
/// and whose path lies strictly under `<bundle>/Contents/MacOS/`.
///
/// Matching is by path components, never string prefix, so
/// `/A/Cometty.app2/Contents/MacOS/cometty` is not under `/A/Cometty.app`.
pub(crate) fn count_instances(list: &[(i32, PathBuf)], bundle: &Path, exclude: i32) -> usize {
    let macos = bundle.join("Contents").join("MacOS");
    list.iter()
        .filter(|(pid, _)| *pid != exclude)
        .filter(|(_, path)| {
            path.strip_prefix(&macos).is_ok_and(|rest| {
                rest.components().next().is_some()
                    && rest.components().all(|c| matches!(c, Component::Normal(_)))
            })
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSTALLED: &str = "/Applications/Cometty.app/Contents/MacOS/cometty";
    const TRANSLOCATED: &str =
        "/private/var/folders/x/AppTranslocation/ABC/d/Cometty.app/Contents/MacOS/cometty";
    const STAGING: &str = "/Users/me/.local/state/cometty/updates";

    fn resolve(bundled: bool, exe: &str, staging: bool) -> Mode {
        resolve_mode(bundled, Path::new(exe), staging.then(|| Path::new(STAGING)))
    }

    fn notify(reason: NotifyReason) -> Mode {
        Mode::NotifyOnly(reason)
    }

    #[test]
    fn translocation_matches_whole_components_only() {
        assert!(is_translocated(Path::new(TRANSLOCATED)));
        assert!(!is_translocated(Path::new(INSTALLED)));
        assert!(!is_translocated(Path::new(
            "/Users/me/AppTranslocationX/Cometty.app"
        )));
        assert!(!is_translocated(Path::new("/Users/me/MyAppTranslocation")));
    }

    #[test]
    fn non_bundle_path_is_not_bundled() {
        let dev = "/Users/me/dev/cometty/target/release/cometty";
        assert_eq!(resolve(true, dev, true), notify(NotifyReason::NotBundled));
        assert_eq!(resolve(false, dev, true), notify(NotifyReason::NotBundled));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn installed_bundle_self_updates() {
        assert_eq!(
            resolve(true, INSTALLED, true),
            Mode::SelfUpdate {
                bundle: PathBuf::from("/Applications/Cometty.app"),
            }
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn each_failing_condition_gives_its_reason_in_order() {
        // The bundled flag is authoritative even for a bundle-shaped path.
        assert_eq!(
            resolve(false, INSTALLED, true),
            notify(NotifyReason::NotBundled)
        );
        assert_eq!(
            resolve(
                true,
                "/Applications/Cometty.app/Contents/MacOS/sub/cometty",
                true
            ),
            notify(NotifyReason::NotBundled)
        );
        assert_eq!(
            resolve(true, TRANSLOCATED, true),
            notify(NotifyReason::Translocated)
        );
        // Translocated beats a missing staging dir.
        assert_eq!(
            resolve(true, TRANSLOCATED, false),
            notify(NotifyReason::Translocated)
        );
        assert_eq!(
            resolve(true, INSTALLED, false),
            notify(NotifyReason::NoStagingDir)
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_is_never_bundled() {
        assert_eq!(
            resolve(true, INSTALLED, true),
            notify(NotifyReason::NotBundled)
        );
    }

    #[test]
    fn sibling_names_are_hidden_non_app_siblings() {
        let (incoming, backup) = sibling_names(Path::new("/Users/me/Apps/Other Name.app"));
        assert_eq!(
            incoming,
            PathBuf::from("/Users/me/Apps/.Other Name.app.cometty-new")
        );
        assert_eq!(
            backup,
            PathBuf::from("/Users/me/Apps/.Other Name.app.cometty-old")
        );
    }

    fn procs(entries: &[(i32, &str)]) -> Vec<(i32, PathBuf)> {
        entries
            .iter()
            .map(|&(pid, path)| (pid, PathBuf::from(path)))
            .collect()
    }

    #[test]
    fn instances_match_by_components_not_prefix() {
        let list = procs(&[
            (10, "/A/Cometty.app/Contents/MacOS/cometty"),
            (11, "/A/Cometty.app/Contents/MacOS/helper/sub"),
            (12, "/A/Cometty.app2/Contents/MacOS/cometty"),
            (13, "/A/Cometty.app/Contents/Resources/cometty"),
            (14, "/A/Cometty.app/Contents/MacOS"),
            (15, "/A/Cometty.app/Contents/MacOS/../MacOS/cometty"),
            (16, "/B/Cometty.app/Contents/MacOS/cometty"),
        ]);
        assert_eq!(count_instances(&list, Path::new("/A/Cometty.app"), 0), 2);
        assert_eq!(count_instances(&list, Path::new("/A/Cometty.app2"), 0), 1);
        assert_eq!(count_instances(&[], Path::new("/A/Cometty.app"), 0), 0);
    }

    #[test]
    fn own_pid_is_excluded() {
        let list = procs(&[
            (10, "/A/Cometty.app/Contents/MacOS/cometty"),
            (20, "/A/Cometty.app/Contents/MacOS/cometty"),
        ]);
        let bundle = Path::new("/A/Cometty.app");
        assert_eq!(count_instances(&list, bundle, 10), 1);
        assert_eq!(count_instances(&list, bundle, 99), 2);
    }
}
