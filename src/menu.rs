//! Native OS menu bar: app menu with Settings + Logs entries.
//!
//! The panels themselves are egui (see [`crate::renderer::settings_ui`] and
//! [`crate::renderer::log_ui`]); this module only owns the platform menu
//! that opens them. Only macOS attaches a global menu today
//! (`init_for_nsapp`); other platforms keep the `Ctrl+,` / `Ctrl+Shift+L`
//! shortcuts until window-menu integration lands.
//! Construction/install failures are non-fatal: the app stays fully usable
//! without the menu.

use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};

/// Owned native menu. Must stay alive for the app lifetime: dropping it
/// unregisters the platform menu. Muda handles are main-thread only
/// (`!Send`), which is fine — [`crate::app::App`] never crosses threads
/// (worker threads only clone the winit event-loop proxy).
pub struct NativeMenu {
    menu: Menu,
    settings_item: MenuItem,
    logs_item: MenuItem,
}

impl NativeMenu {
    pub fn new() -> muda::Result<Self> {
        let menu = Menu::new();
        let app_submenu = Submenu::new("Cometty", true);
        #[cfg(target_os = "macos")]
        let settings_accelerator = Accelerator::new(Some(Modifiers::SUPER), Code::Comma);
        #[cfg(not(target_os = "macos"))]
        let settings_accelerator = Accelerator::new(Some(Modifiers::CONTROL), Code::Comma);
        #[cfg(target_os = "macos")]
        let logs_accelerator =
            Accelerator::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyL);
        #[cfg(not(target_os = "macos"))]
        let logs_accelerator =
            Accelerator::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyL);
        let settings_item = MenuItem::with_id(
            "cometty-settings",
            "Settings…",
            true,
            Some(settings_accelerator),
        );
        let logs_item = MenuItem::with_id("cometty-logs", "Logs…", true, Some(logs_accelerator));
        app_submenu.append_items(&[
            &PredefinedMenuItem::about(
                None,
                Some(AboutMetadata {
                    name: Some("Cometty".to_string()),
                    version: Some(env!("CARGO_PKG_VERSION").to_string()),
                    ..Default::default()
                }),
            ),
            &PredefinedMenuItem::separator(),
            &settings_item,
            &logs_item,
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(None),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::quit(None),
        ])?;
        menu.append(&app_submenu)?;
        Ok(Self {
            menu,
            settings_item,
            logs_item,
        })
    }

    /// Attach to the platform menu bar. Main thread only; no-op off macOS.
    /// Must run after winit's event-loop startup (e.g. from `resumed`):
    /// winit installs its own default menu first, which would clobber an
    /// earlier install.
    pub fn install(&self) {
        #[cfg(target_os = "macos")]
        self.menu.init_for_nsapp();
    }

    pub fn settings_id(&self) -> &muda::MenuId {
        self.settings_item.id()
    }

    pub fn logs_id(&self) -> &muda::MenuId {
        self.logs_item.id()
    }
}
