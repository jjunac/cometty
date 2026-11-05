//! Keep macOS's implicit titlebar drag from stealing app-owned tab gestures.
//!
//! A transparent/full-size titlebar still lets AppKit synthesize a window drag
//! before winit dispatches the press. Report the content view as opaque for that
//! specific hit test, like a native titlebar control. Empty-chrome drags remain
//! explicit `Window::drag_window` calls after the app's own hit test.

use std::ffi::CStr;
use std::sync::OnceLock;

use anyhow::{Context, ensure};
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
use objc2::{MainThreadMarker, msg_send, sel};
use objc2_app_kit::NSView;
use objc2_foundation::NSRect;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

type ViewClass = OnceLock<Option<&'static AnyClass>>;
static VIEW_CLASS: ViewClass = OnceLock::new();

/// Install once, on the UI thread, before renderer/IME/accessibility setup.
/// Do not disable `NSWindow.isMovable`: that also breaks system window tiling
/// and display-reconfiguration behavior, not just unwanted titlebar drags.
pub(crate) fn install(window: &winit::window::Window) -> anyhow::Result<()> {
    let _main_thread =
        MainThreadMarker::new().context("titlebar setup requires the main thread")?;
    let handle = window.window_handle()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        anyhow::bail!("expected an AppKit window handle");
    };
    // SAFETY: winit's live AppKit handle supplies an NSView. `window` and its
    // borrowed handle keep it alive; all access is on the main thread.
    let view = unsafe { handle.ns_view.cast::<AnyObject>().as_ref() };
    install_on_view(view, &VIEW_CLASS, c"ComettyAppOwnedTitlebarView")?;
    // SAFETY: this is the same live NSView from the borrowed winit handle.
    // Invalidate once so AppKit also refreshes any cached titlebar move region.
    unsafe { handle.ns_view.cast::<NSView>().as_ref() }.setNeedsDisplay(true);
    Ok(())
}

fn install_on_view(view: &AnyObject, slot: &ViewClass, name: &CStr) -> anyhow::Result<()> {
    let original = view.class();
    let subclass = slot
        .get_or_init(|| {
            let mut class = ClassBuilder::new(name, original)?;
            // SAFETY: AppKit's selector has no explicit arguments and returns
            // NSRect. The receiver inherits NSView's `bounds` method. No ivars,
            // deallocator, event handlers or GPU/IME methods are replaced.
            unsafe {
                class.add_method(
                    sel!(_opaqueRectForWindowMoveWhenInTitlebar),
                    opaque_titlebar_rect as extern "C" fn(_, Sel) -> NSRect,
                );
            }
            Some(class.register())
        })
        .context("could not register the app-owned titlebar view")?;
    if original == subclass {
        return Ok(());
    }
    ensure!(
        subclass.superclass() == Some(original),
        "unexpected titlebar view class"
    );
    ensure!(
        subclass.instance_size() == original.instance_size(),
        "titlebar subclass changed the view layout"
    );
    // SAFETY: this is an ivar-free direct subclass of the current class with
    // exactly the same instance size. Its only override has the same ABI and
    // changes only AppKit's titlebar move hit test. Setup runs on the main thread
    // before other integrations can subclass this particular view. The base
    // winit class is not modified, so unrelated views/windows are unaffected.
    let previous = unsafe { AnyObject::set_class(view, subclass) };
    assert_eq!(
        previous, original,
        "titlebar view class changed during setup"
    );
    Ok(())
}

extern "C" fn opaque_titlebar_rect(view: &AnyObject, _: Sel) -> NSRect {
    // This AppKit hook is undocumented. Returning current bounds marks the
    // content as app-owned without disabling window movement globally. Keep it
    // isolated here and recheck it when upgrading winit or supported macOS.
    // SAFETY: installed only on an NSView subclass; `bounds` returns NSRect.
    unsafe { msg_send![view, bounds] }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::ClassType;
    use objc2::rc::Retained;
    use objc2::runtime::NSObject;
    use objc2_foundation::{NSPoint, NSSize};

    extern "C" fn bounds(_: &AnyObject, _: Sel) -> NSRect {
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0))
    }

    extern "C" fn empty_rect(_: &AnyObject, _: Sel) -> NSRect {
        NSRect::ZERO
    }

    #[test]
    fn titlebar_override_preserves_layout_inheritance_and_unrelated_views() {
        // A windowless Objective-C stand-in exercises the production subclass
        // installation/dispatch without creating NSApplication/NSView or needing
        // a display. The actual AppKit drag arbitration still needs a GUI pass.
        let mut class = ClassBuilder::new(c"ComettyTitlebarTestBase", NSObject::class()).unwrap();
        class.add_ivar::<usize>(c"comettyTestState");
        // SAFETY: both zero-argument methods return NSRect with the declared ABI.
        unsafe {
            class.add_method(sel!(bounds), bounds as extern "C" fn(_, Sel) -> NSRect);
            class.add_method(
                sel!(_opaqueRectForWindowMoveWhenInTitlebar),
                empty_rect as extern "C" fn(_, Sel) -> NSRect,
            );
        }
        let base = class.register();
        // SAFETY: the base inherits NSObject's owning `new` constructor.
        let view: Retained<NSObject> = unsafe { msg_send![base, new] };
        let other: Retained<NSObject> = unsafe { msg_send![base, new] };
        let state = base.instance_variable(c"comettyTestState").unwrap();
        // SAFETY: the base defines this usize ivar on both live objects.
        unsafe { state.load_ptr::<usize>(&view).write(0xface) };
        let slot = ViewClass::new();
        install_on_view(&view, &slot, c"ComettyTitlebarTestOwned").unwrap();
        let owned = view.class();
        assert_eq!(owned.superclass(), Some(base));
        assert_eq!(owned.instance_size(), base.instance_size());
        assert_eq!(other.class(), base);
        // SAFETY: the subclass has the same instance layout and inherited ivar.
        assert_eq!(unsafe { state.load_ptr::<usize>(&view).read() }, 0xface);
        // SAFETY: the selectors and return types match the registered methods.
        let opaque: NSRect = unsafe { msg_send![&*view, _opaqueRectForWindowMoveWhenInTitlebar] };
        let unrelated: NSRect =
            unsafe { msg_send![&*other, _opaqueRectForWindowMoveWhenInTitlebar] };
        assert_eq!(opaque, bounds(&view, sel!(bounds)));
        assert_eq!(unrelated, NSRect::ZERO);
        install_on_view(&view, &slot, c"ComettyTitlebarTestOwned").unwrap();
        assert_eq!(view.class(), owned, "installation is idempotent");
        let incompatible = NSObject::new();
        assert!(install_on_view(&incompatible, &slot, c"ComettyTitlebarTestOwned").is_err());
        assert_eq!(
            incompatible.class(),
            NSObject::class(),
            "unexpected bases are left untouched"
        );
    }
}
