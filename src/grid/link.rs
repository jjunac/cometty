//! OSC 8 hyperlink storage: interned URI table + current-link state.
//!
//! The grid owns the table because cells only carry a small `u16` id, and
//! because scrollback rows outlive any per-sequence state in `Terminal`.
//! Ids are never reused, so a cell's link stays resolvable after the table
//! fills up or the link is ended.

use super::Grid;

/// Longest URI stored per link, in bytes. Longer `OSC 8` payloads are
/// rejected (logged) so hostile output cannot grow the table without bound.
pub const MAX_HYPERLINK_URI: usize = 4096;
/// Distinct URIs kept per grid. Once full, new links are dropped with a
/// one-shot warning; existing cells keep working.
pub const MAX_HYPERLINKS: usize = 1024;
/// Total interned URI bytes per grid: a hostile stream can stay under the
/// count cap with four-kilobyte URIs, so the table also has a byte budget.
pub const MAX_HYPERLINK_BYTES: usize = 1 << 18;

impl Grid {
    /// Set the hyperlink applied to subsequently printed cells.
    /// `None` / an empty URI ends the current link (`OSC 8 ; ; ST`).
    /// URIs over [`MAX_HYPERLINK_URI`] and table overflow are ignored
    /// (logged once); the previous link state is replaced either way.
    pub fn set_hyperlink(&mut self, uri: Option<&str>) {
        self.current_hyperlink = match uri {
            Some(uri) if !uri.is_empty() => self.intern_hyperlink(uri),
            _ => None,
        };
    }

    /// The terminal can reject a raw/decoded URI before allocating its text.
    /// Keep the same clear-current-link and one-shot warning policy as an
    /// oversized URI passed directly to `set_hyperlink`.
    pub fn reject_oversized_hyperlink(&mut self, bytes: usize) {
        self.current_hyperlink = None;
        self.warn_hyperlink_once(format!(
            "OSC 8 link ignored: {bytes} bytes (limit {MAX_HYPERLINK_URI})"
        ));
    }

    /// The current OSC 8 link id, if a link is open (test/inspection).
    #[allow(dead_code)]
    pub fn current_hyperlink(&self) -> Option<u16> {
        self.current_hyperlink
    }

    /// Resolve an interned URI.
    pub fn hyperlink_uri(&self, id: u16) -> Option<&str> {
        self.hyperlinks.get(id as usize).map(|s| s.as_ref())
    }

    /// Lead-resolving hyperlink id for a cell on a global line
    /// (`0` = oldest scrollback row), or `None`.
    pub fn hyperlink_id_at(&self, global: usize, col: usize) -> Option<u16> {
        let row = self.global_line(global)?;
        let mut x = col.min(row.len().checked_sub(1)?);
        if row[x].width == 0 && x > 0 {
            x -= 1;
        }
        row.get(x).and_then(|c| c.hyperlink)
    }

    fn intern_hyperlink(&mut self, uri: &str) -> Option<u16> {
        if let Some(id) = self.hyperlink_index.get(uri) {
            return Some(*id);
        }
        if uri.len() > MAX_HYPERLINK_URI {
            self.reject_oversized_hyperlink(uri.len());
            return None;
        }
        if self.hyperlinks.len() >= MAX_HYPERLINKS {
            self.warn_hyperlink_once(format!(
                "OSC 8 link ignored: table full ({MAX_HYPERLINKS} URIs)"
            ));
            return None;
        }
        if self.hyperlink_bytes + uri.len() > MAX_HYPERLINK_BYTES {
            self.warn_hyperlink_once(format!(
                "OSC 8 link ignored: table byte budget reached ({MAX_HYPERLINK_BYTES} bytes)"
            ));
            return None;
        }
        let id = self.hyperlinks.len() as u16;
        let boxed: Box<str> = uri.into();
        self.hyperlink_bytes += boxed.len();
        self.hyperlinks.push(boxed.clone());
        self.hyperlink_index.insert(boxed, id);
        Some(id)
    }

    fn warn_hyperlink_once(&mut self, message: String) {
        if !self.hyperlink_overflow_warned {
            self.hyperlink_overflow_warned = true;
            log::warn!("{message}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn grid() -> Grid {
        Grid::new(10, 3, Theme::default())
    }

    #[test]
    fn interns_and_resolves_uris() {
        let mut g = grid();
        g.set_hyperlink(Some("https://a.b"));
        g.put_char('x');
        g.set_hyperlink(Some("https://a.b"));
        g.put_char('y');
        g.set_hyperlink(None);
        g.put_char('z');
        assert_eq!(g.current_hyperlink(), None);
        let row = g.global_line(0).unwrap();
        let id = row[0].hyperlink.expect("x is linked");
        assert_eq!(row[1].hyperlink, Some(id), "same URI reuses the id");
        assert_eq!(row[2].hyperlink, None, "ended link stamps nothing");
        assert_eq!(g.hyperlink_uri(id), Some("https://a.b"));
    }

    #[test]
    fn empty_uri_ends_the_link() {
        let mut g = grid();
        g.set_hyperlink(Some("https://a.b"));
        g.put_char('x');
        g.set_hyperlink(Some(""));
        g.put_char('y');
        let row = g.global_line(0).unwrap();
        assert!(row[0].hyperlink.is_some());
        assert!(row[1].hyperlink.is_none());
    }

    #[test]
    fn wide_continuation_inherits_the_link() {
        let mut g = grid();
        g.set_hyperlink(Some("https://a.b"));
        g.put_char('中');
        let row = g.global_line(0).unwrap();
        assert_eq!(row[1].width, 0);
        assert_eq!(row[0].hyperlink, row[1].hyperlink);
        assert_eq!(g.hyperlink_id_at(0, 1), row[0].hyperlink);
    }

    #[test]
    fn erase_clears_links_but_wide_placeholder_keeps_them() {
        let mut g = grid();
        g.set_hyperlink(Some("https://a.b"));
        g.put_char('x');
        g.put_char('y');
        g.set_cursor(0, 2);
        g.erase_in_line(2);
        let row = g.global_line(2).unwrap();
        assert!(row.iter().all(|c| c.hyperlink.is_none()));
    }

    #[test]
    fn oversized_and_overflow_links_are_dropped() {
        let mut g = grid();
        let huge = format!("https://x/{}", "a".repeat(MAX_HYPERLINK_URI));
        g.set_hyperlink(Some(&huge));
        g.put_char('x');
        assert_eq!(g.global_line(0).unwrap()[0].hyperlink, None);
        assert_eq!(g.current_hyperlink(), None);
        // The table fills up; further links are dropped without panicking.
        for i in 0..MAX_HYPERLINKS + 8 {
            g.set_hyperlink(Some(&format!("https://x/{i}")));
            g.put_char('y');
        }
        assert!(g.hyperlinks.len() <= MAX_HYPERLINKS);
        // Existing entries still resolve.
        g.set_hyperlink(Some("https://x/0"));
        assert!(g.current_hyperlink().is_some());
    }

    #[test]
    fn byte_budget_caps_the_table() {
        let mut g = grid();
        let chunk = "a".repeat(MAX_HYPERLINK_URI - 16);
        for i in 0..MAX_HYPERLINK_BYTES / chunk.len() + 4 {
            g.set_hyperlink(Some(&format!("https://x/{i}/{chunk}")));
        }
        assert!(g.hyperlink_bytes <= MAX_HYPERLINK_BYTES);
    }

    #[test]
    fn hyperlink_id_at_out_of_range_is_none() {
        let g = grid();
        assert_eq!(g.hyperlink_id_at(99, 0), None);
        assert_eq!(g.hyperlink_id_at(0, 99), None);
    }
}
