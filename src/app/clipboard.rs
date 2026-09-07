//! Clipboard copy/paste paths.

use super::App;

impl App {
    pub(crate) fn copy_selection(&mut self) {
        let text = {
            let Some(tab) = self.active_tab() else {
                return;
            };
            let Some(sel) = tab.selection.as_ref() else {
                return;
            };
            let grid = tab.terminal.grid();
            crate::selection::extract_text(sel, |g| {
                grid.global_line_cells(g).map(|row| {
                    row.into_iter()
                        .map(|c| crate::selection::SelCell {
                            text: c.cluster(),
                            width: c.width,
                        })
                        .collect()
                })
            })
        };
        let Some(text) = text else { return };
        if text.is_empty() {
            return;
        }
        if self.clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(cb) => self.clipboard = Some(cb),
                Err(e) => {
                    log::warn!("clipboard unavailable: {e:#}");
                    return;
                }
            }
        }
        if let Some(cb) = self.clipboard.as_mut()
            && let Err(e) = cb.set_text(text)
        {
            log::warn!("clipboard copy failed: {e:#}");
        }
    }

    pub(crate) fn paste_from_clipboard(&mut self) {
        if self.clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(cb) => self.clipboard = Some(cb),
                Err(e) => {
                    log::warn!("clipboard unavailable: {e:#}");
                    return;
                }
            }
        }
        let text = self.clipboard.as_mut().and_then(|cb| cb.get_text().ok());
        let Some(text) = text else { return };
        // Normalize all line endings to CR, the terminal's Enter byte.
        let normalized = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\n', "\r");
        let enabled = self
            .active_tab()
            .is_some_and(|t| t.terminal.bracketed_paste());
        let bytes = crate::input::wrap_bracketed_paste(&normalized, enabled);
        if let Some(tab) = self.active_tab() {
            tab.pty.write(bytes);
        }
    }
}
