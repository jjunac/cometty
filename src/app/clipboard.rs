//! Clipboard copy/paste paths (selection shortcuts + OSC 52).

use super::App;
use crate::term::ClipboardOp;

/// Normalize pasted text to the terminal's Enter byte: CRLF and lone CR
/// both become CR, then lone LF becomes CR. Pure seam for unit tests; the
/// bracketed-paste wrap stays in `input::wrap_bracketed_paste`.
pub(crate) fn normalize_paste(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r")
}

impl App {
    /// Lazily created system clipboard handle. `None` when the platform
    /// clipboard is unavailable (already logged).
    fn ensure_clipboard(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(cb) => self.clipboard = Some(cb),
                Err(e) => {
                    log::warn!("clipboard unavailable: {e:#}");
                    return None;
                }
            }
        }
        self.clipboard.as_mut()
    }

    /// Write `text` to the system clipboard. Used by the copy shortcut and
    /// by `OSC 52` writes.
    pub(crate) fn set_clipboard_text(&mut self, text: &str) -> bool {
        let Some(cb) = self.ensure_clipboard() else {
            return false;
        };
        match cb.set_text(text.to_string()) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("clipboard write failed: {e:#}");
                false
            }
        }
    }

    /// Read the system clipboard, if available.
    pub(crate) fn clipboard_text(&mut self) -> Option<String> {
        let cb = self.ensure_clipboard()?;
        match cb.get_text() {
            Ok(text) => Some(text),
            Err(e) => {
                log::debug!("clipboard read failed: {e:#}");
                None
            }
        }
    }

    /// Apply queued `OSC 52` requests from [`super::pty_io::App::drain_pty`].
    ///
    /// Writes are gated by `[clipboard] osc52` (`off` drops them), reads
    /// only work in `read-write` — a `?` request is how an app would
    /// exfiltrate the clipboard, so it is opt-in and logged at debug when
    /// denied. Replies go to the tab that asked.
    pub(crate) fn handle_clipboard_ops(&mut self, ops: Vec<(usize, ClipboardOp)>) {
        let write_ok = self.config.clipboard.allows_write();
        let read_ok = self.config.clipboard.allows_read();
        let mut denied_read_logged = false;
        for (tab, op) in ops {
            match op {
                ClipboardOp::Set(text) => {
                    if write_ok {
                        log::debug!("OSC 52 clipboard write ({} bytes)", text.len());
                        self.set_clipboard_text(&text);
                    } else {
                        log::debug!(
                            "OSC 52 write ignored (osc52 = {})",
                            self.config.clipboard.osc52
                        );
                    }
                }
                ClipboardOp::Query => {
                    if !read_ok {
                        if !denied_read_logged {
                            denied_read_logged = true;
                            log::debug!(
                                "OSC 52 read ignored (osc52 = {})",
                                self.config.clipboard.osc52
                            );
                        }
                        continue;
                    }
                    let Some(text) = self.clipboard_text() else {
                        continue;
                    };
                    if text.len() > crate::term::MAX_CLIPBOARD_BYTES {
                        log::warn!(
                            "OSC 52 read reply skipped: {} bytes (limit {})",
                            text.len(),
                            crate::term::MAX_CLIPBOARD_BYTES
                        );
                        continue;
                    }
                    log::debug!("OSC 52 clipboard read reply ({} bytes)", text.len());
                    if let Some(tab) = self.tabs.get_mut(tab) {
                        tab.pty.write(crate::term::osc52_reply(&text));
                    }
                }
            }
        }
    }

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
        self.set_clipboard_text(&text);
    }

    pub(crate) fn paste_from_clipboard(&mut self) {
        let Some(text) = self.clipboard_text() else {
            return;
        };
        let normalized = normalize_paste(&text);
        let enabled = self
            .active_tab()
            .is_some_and(|t| t.terminal.bracketed_paste());
        let bytes = crate::input::wrap_bracketed_paste(&normalized, enabled);
        if let Some(tab) = self.active_tab() {
            tab.pty.write(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_normalizes_all_line_endings_to_cr() {
        assert_eq!(normalize_paste("a\nb"), "a\rb");
        assert_eq!(normalize_paste("a\r\nb"), "a\rb");
        assert_eq!(normalize_paste("a\rb"), "a\rb");
        assert_eq!(normalize_paste("a\r\nb\nc\rd"), "a\rb\rc\rd");
        assert_eq!(normalize_paste("plain"), "plain");
        assert_eq!(normalize_paste(""), "");
        // CRLF must collapse to one CR, not two.
        assert_eq!(normalize_paste("\r\n").chars().count(), 1);
    }

    #[test]
    fn clipboard_policy_gates_osc52() {
        // `off` drops writes, `write` allows them, only `read-write`
        // answers read queries (exfiltration guard).
        let off = crate::config::ClipboardConfig {
            osc52: "off".to_string(),
        };
        assert!(!off.allows_write() && !off.allows_read());
        let write = crate::config::ClipboardConfig {
            osc52: "write".to_string(),
        };
        assert!(write.allows_write() && !write.allows_read());
        let rw = crate::config::ClipboardConfig {
            osc52: "read-write".to_string(),
        };
        assert!(rw.allows_write() && rw.allows_read());
    }
}
