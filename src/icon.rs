//! App window icon embedded from `assets/icon-512.png` (rasterized from `logo.svg`).
//!
//! The PNG is baked into the binary via `include_bytes!` so the single-binary
//! property holds. Decoding failures are non-fatal: callers get `None` and the
//! window is created without a custom icon.

/// Embedded 512x512 PNG rasterized from `logo.svg` via
/// `rsvg-convert -w 512 -h 512 logo.svg -o assets/icon-512.png`.
const ICON_PNG: &[u8] = include_bytes!("../assets/icon-512.png");

/// Decode the embedded PNG to raw RGBA bytes plus dimensions.
/// Returns `None` (and logs) when the asset is corrupt.
pub(crate) fn decode_icon_png() -> Option<(Vec<u8>, u32, u32)> {
    match image::load_from_memory(ICON_PNG) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (w, h) = (rgba.width(), rgba.height());
            Some((rgba.into_raw(), w, h))
        }
        Err(e) => {
            log::warn!("window icon unavailable ({e:#}); continuing without it");
            None
        }
    }
}

/// Build a `winit` window icon from the embedded PNG.
/// Returns `None` (and logs) when decoding or validation fails.
/// No-op on macOS by design (winit ignores it there); the Dock icon is set
/// via [`set_dock_icon`] instead.
pub(crate) fn load_window_icon() -> Option<winit::window::Icon> {
    let (rgba, w, h) = decode_icon_png()?;
    match winit::window::Icon::from_rgba(rgba, w, h) {
        Ok(icon) => Some(icon),
        Err(e) => {
            log::warn!("window icon unavailable ({e:?}); continuing without it");
            None
        }
    }
}

/// Set the macOS Dock tile from the embedded PNG.
///
/// `winit`'s window icon is ignored on macOS (icons come from the app
/// bundle), so this calls `NSApplication::setApplicationIconImage` directly.
/// Works for unbundled runs (`cargo run`); a bundled `.app` uses its
/// `Info.plist`/`Assets.car` icon instead. Non-fatal: logs and returns on
/// any failure. Must run on the main thread (winit's `resumed` qualifies).
#[cfg(target_os = "macos")]
pub(crate) fn set_dock_icon() {
    use objc2::AnyThread as _;

    let Some(mtm) = objc2::MainThreadMarker::new() else {
        log::warn!("dock icon unavailable (not on main thread)");
        return;
    };
    let data = objc2_foundation::NSData::with_bytes(ICON_PNG);
    let Some(image) = objc2_app_kit::NSImage::initWithData(objc2_app_kit::NSImage::alloc(), &data)
    else {
        log::warn!("dock icon unavailable (image decode failed)");
        return;
    };
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    // SAFETY: `image` is a valid non-null `NSImage`; passing `Some` (never
    // `None`, which would restore the default icon).
    unsafe { app.setApplicationIconImage(Some(&image)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_to_square_rgba() {
        let (rgba, w, h) = decode_icon_png().expect("embedded icon must decode");
        assert_eq!((w, h), (512, 512));
        assert_eq!(rgba.len(), w as usize * h as usize * 4);
        // Not fully transparent: the logo paints a dark rounded square.
        assert!(rgba.chunks_exact(4).any(|px| px[3] > 0));
    }

    #[test]
    fn builds_winit_icon() {
        assert!(load_window_icon().is_some());
    }
}
