//! Hyperlink hints + click-to-open (view side).

use winit::keyboard::ModifiersState;
use winit::window::CursorIcon;

use super::App;
use crate::config::LinksConfig;
use crate::links::{HintView, LinkContext, LinkTarget, OpenError, OpenOutcome};

/// True when the configured "open link" modifier is held. `auto` means Cmd
/// on macOS and Ctrl elsewhere; `none` disables hints and click-to-open.
pub fn hint_modifier_held(modifiers: &ModifiersState, config: &LinksConfig) -> bool {
    match config.open_modifier.as_str() {
        "cmd" => modifiers.super_key() && !modifiers.control_key(),
        "ctrl" => modifiers.control_key() && !modifiers.super_key(),
        "alt" => modifiers.alt_key() && !modifiers.control_key(),
        "none" => false,
        _ => {
            if cfg!(target_os = "macos") {
                modifiers.super_key()
            } else {
                modifiers.control_key()
            }
        }
    }
}

impl App {
    /// Hint modifier currently held (opens links on click).
    pub(crate) fn hints_active(&self) -> bool {
        hint_modifier_held(&self.modifiers, &self.config.links)
    }

    /// Link underlines should be painted this frame: `always`, `never`, or
    /// only while the modifier is held (`hover`, the default).
    fn hints_visible(&self) -> bool {
        match self.config.links.underline.as_str() {
            "always" => true,
            "never" => false,
            _ => self.hints_active(),
        }
    }

    /// View-space link runs for this frame, or `None` when underlines are
    /// off. Cached per tab against the grid view version, link settings,
    /// working directory, and modifier-press generation.
    pub(crate) fn link_hint_view(&mut self) -> Option<HintView> {
        if !self.hints_visible() {
            return None;
        }
        let tab = self.tabs.get_mut(self.active)?;
        Some(
            tab.link_hints(
                &self.config.links,
                self.link_hint_generation,
                self.home_dir.as_deref(),
                self.local_host.as_deref(),
            )
            .clone(),
        )
    }

    /// Link or local path under the cursor while the hint modifier is held:
    /// an `OSC 8` hyperlink wins, then a detected plain-text URL or local path.
    pub(crate) fn link_under_cursor(&self) -> Option<LinkTarget> {
        if !self.hints_active() {
            return None;
        }
        let (col, view_row) = self.cell_under_cursor()?;
        let tab = self.active_tab()?;
        let grid = tab.terminal.grid();
        let global = self.view_to_global(view_row)?;
        let row = grid.global_line(global)?;
        let local_host = self.local_host.as_deref();
        let ctx = LinkContext {
            detect_urls: self.config.links.detect_urls,
            open_files: self.config.links.open_files,
            detect_paths: self.config.links.detect_paths,
            cwd: tab.local_cwd(local_host),
            home: self.home_dir.as_deref(),
            local_host,
            platform: crate::links::platform(),
        };
        if let Some(id) = grid.hyperlink_id_at(global, col) {
            let uri = grid.hyperlink_uri(id)?;
            let target = crate::links::resolve_osc8_uri(uri, &ctx);
            if target.is_none() {
                log::debug!("ignoring unopenable OSC 8 link {uri:?}");
            }
            return target;
        }
        crate::links::detected_link_at(&row.materialize(), col, &ctx)
    }

    /// Open the link or local path under the cursor (if any) and report
    /// whether the press was consumed. Rejected targets are ignored quietly
    /// (debug log).
    pub(crate) fn open_link_at_cursor(&mut self) -> bool {
        let Some(target) = self.link_under_cursor() else {
            return false;
        };
        match crate::links::open_target(&target, &self.config.links) {
            Ok(OpenOutcome::Opened(pid)) => log::info!("opened link {target:?} (pid {pid})"),
            Ok(OpenOutcome::Revealed(pid)) => {
                log::info!("revealed executable/bundle path {target:?} (pid {pid})")
            }
            Err(OpenError::Rejected) => log::debug!("refusing to open link {target:?}"),
            Err(OpenError::NotFound) => log::debug!("local path no longer exists: {target:?}"),
            Err(OpenError::Spawn(e)) => log::warn!("failed to open link {target:?}: {e}"),
        }
        true
    }

    /// Pointer cursor over a clickable link, default everywhere else.
    /// Cheap no-op while the icon is unchanged (called on every move).
    pub(crate) fn update_link_cursor(&mut self) {
        let cell = (self.hints_active() && !self.tab_drag.captures_pointer())
            .then(|| self.cell_under_cursor())
            .flatten();
        let pointer = cell.is_some_and(|(col, row)| {
            let generation = self.link_hint_generation;
            let config = &self.config.links;
            let home = self.home_dir.as_deref();
            let local_host = self.local_host.as_deref();
            self.tabs.get_mut(self.active).is_some_and(|tab| {
                tab.link_hints(config, generation, home, local_host)
                    .hit(row, col, 1)
            })
        });
        let desired = if pointer {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        };
        self.set_link_cursor_icon(desired);
    }

    /// Push a cursor icon to the window when it changed.
    pub(crate) fn set_link_cursor_icon(&mut self, desired: CursorIcon) {
        if self.cursor_icon == desired {
            return;
        }
        self.cursor_icon = desired;
        if let Some(w) = self.window.as_ref() {
            w.set_cursor(desired);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LinksConfig;

    fn cfg(mode: &str) -> LinksConfig {
        LinksConfig {
            open_modifier: mode.to_string(),
            ..LinksConfig::default()
        }
    }

    #[test]
    fn modifier_matching_per_mode() {
        let cmd = ModifiersState::SUPER;
        let ctrl = ModifiersState::CONTROL;
        let alt = ModifiersState::ALT;
        assert!(hint_modifier_held(&cmd, &cfg("cmd")));
        assert!(!hint_modifier_held(&ctrl, &cfg("cmd")));
        assert!(hint_modifier_held(&ctrl, &cfg("ctrl")));
        assert!(!hint_modifier_held(&cmd, &cfg("ctrl")));
        assert!(hint_modifier_held(&alt, &cfg("alt")));
        // Ctrl+Alt is a text-composition combo, not a link modifier.
        assert!(!hint_modifier_held(
            &(ModifiersState::CONTROL | ModifiersState::ALT),
            &cfg("alt")
        ));
        assert!(!hint_modifier_held(&ModifiersState::empty(), &cfg("auto")));
        assert!(!hint_modifier_held(&cmd, &cfg("none")));
        assert!(!hint_modifier_held(&ctrl, &cfg("none")));
        // `auto` follows the platform.
        if cfg!(target_os = "macos") {
            assert!(hint_modifier_held(&cmd, &cfg("auto")));
            assert!(!hint_modifier_held(&ctrl, &cfg("auto")));
        } else {
            assert!(hint_modifier_held(&ctrl, &cfg("auto")));
            assert!(!hint_modifier_held(&cmd, &cfg("auto")));
        }
    }
}
