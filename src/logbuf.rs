//! In-memory log ring buffer backing the in-app log panel.
//!
//! [`crate::logging`] pushes every record the panel's record level accepts;
//! [`crate::renderer::log_ui`] renders a filtered view. Storage is bounded
//! twice over: by entry count ([`LogBuffer::set_capacity`]) and by a byte
//! budget derived from it, so a single huge message (shader dumps, `{e:#}`
//! error chains) can't blow the ceiling and evict the whole history.
//!
//! Headless and unit-tested like [`crate::app::settings`]: no window, GPU,
//! or logging-global state is touched here.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use log::{Level, LevelFilter};

/// Per-message cap. Longer messages are truncated on a char boundary with
/// an elision marker: the byte budget must stay meaningful, and evicting
/// every other entry because one record was huge would be surprising.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024;

/// Byte budget granted per configured line: entry overhead + target +
/// message bytes (see [`entry_bytes`]).
const BYTES_PER_LINE: usize = 1024;
/// Hard ceiling for the derived byte budget.
const MAX_BUDGET_BYTES: usize = 16 * 1024 * 1024;
/// Elision marker appended to truncated messages.
const TRUNCATION_MARKER: &str = "… [truncated]";

/// One captured log record.
#[derive(Debug)]
pub struct LogEntry {
    pub id: u64,
    pub level: Level,
    pub target: String,
    pub message: String,
    pub at: SystemTime,
}

/// Bounded FIFO of log entries, oldest evicted first.
pub struct LogBuffer {
    entries: VecDeque<Arc<LogEntry>>,
    max_lines: usize,
    max_bytes: usize,
    /// Sum of [`entry_bytes`] over `entries`.
    bytes: usize,
    dropped: u64,
    next_id: u64,
    generation: u64,
}

impl LogBuffer {
    /// New buffer capped at `max_lines` entries (clamped to at least 1).
    pub fn new(max_lines: usize) -> Self {
        let mut buffer = Self {
            entries: VecDeque::new(),
            max_lines: 1,
            max_bytes: 1,
            bytes: 0,
            dropped: 0,
            next_id: 1,
            generation: 0,
        };
        buffer.set_capacity(max_lines);
        buffer
    }

    /// Resize the ring, dropping oldest entries as needed. Live-applied
    /// from the settings panel; shrinking also releases the spare capacity.
    pub fn set_capacity(&mut self, max_lines: usize) {
        self.max_lines = max_lines.max(1);
        self.max_bytes = self
            .max_lines
            .saturating_mul(BYTES_PER_LINE)
            .min(MAX_BUDGET_BYTES);
        self.evict();
        self.entries.shrink_to_fit();
        self.generation = self.generation.wrapping_add(1);
    }

    /// Append one record, evicting oldest entries past either bound.
    pub fn push(&mut self, level: Level, target: &str, message: String) {
        let message = truncate_message(message);
        let entry = Arc::new(LogEntry {
            id: self.next_id,
            level,
            target: target.to_string(),
            message,
            at: SystemTime::now(),
        });
        self.next_id += 1;
        self.generation = self.generation.wrapping_add(1);
        self.bytes += entry_bytes(&entry);
        self.entries.push_back(entry);
        self.evict();
    }

    /// Entries to display: at most `max_level` verbosity, plus the
    /// case-insensitive `needle` filter over target + message (empty =
    /// everything). O(n) clone of `Arc`s — intended for the panel's
    /// per-frame snapshot while it is open.
    #[cfg(test)]
    pub fn filtered(&self, max_level: LevelFilter, needle: &str) -> Vec<Arc<LogEntry>> {
        let needle = needle.to_ascii_lowercase();
        self.entries
            .iter()
            .filter(|e| level_passes(e.level, max_level) && matches(e, &needle))
            .cloned()
            .collect()
    }

    /// Drop every entry (the panel's `Clear` button).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
        self.dropped = 0;
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn bounds(&self) -> LogBounds {
        LogBounds {
            first: self.entries.front().map_or(self.next_id, |e| e.id),
            next: self.next_id,
            generation: self.generation,
            total: self.len(),
            dropped: self.dropped,
        }
    }

    /// Only newly appended or requested scan slices are cloned under the lock.
    pub fn since(&self, first: u64, limit: usize) -> Vec<Arc<LogEntry>> {
        let base = self.bounds().first;
        let start = first.saturating_sub(base).min(self.entries.len() as u64) as usize;
        self.entries.range(start..).take(limit).cloned().collect()
    }

    pub fn entries_by_id(&self, ids: impl Iterator<Item = u64>) -> Vec<Arc<LogEntry>> {
        let base = self.bounds().first;
        ids.filter_map(|id| {
            id.checked_sub(base)
                .and_then(|i| self.entries.get(i as usize))
                .cloned()
        })
        .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    // Companion of `len` for clippy's `len_without_is_empty`; tests
    // inspect emptiness directly, so silence the binary-crate lint.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries evicted by the ring bounds since the last [`Self::clear`].
    #[cfg(test)]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    fn evict(&mut self) {
        // The `len() > 1` guard keeps a single oversized entry (already
        // capped at `MAX_MESSAGE_BYTES`) instead of emptying the buffer.
        while self.entries.len() > self.max_lines
            || (self.bytes > self.max_bytes && self.entries.len() > 1)
        {
            let Some(front) = self.entries.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(entry_bytes(&front));
            self.dropped += 1;
        }
    }
}

/// `true` when a record at `level` is at most as verbose as `max_level`.
/// Follows [`log::LevelFilter`] semantics: `Off` hides everything.
fn level_passes(level: Level, max_level: LevelFilter) -> bool {
    (level as usize) <= (max_level as usize)
}

/// Approximate heap cost of one entry for the byte budget: the struct
/// itself (two `String` headers, timestamp, level) plus its text. The
/// `Arc` header is small enough to absorb in `BYTES_PER_LINE` slop.
fn entry_bytes(entry: &LogEntry) -> usize {
    std::mem::size_of::<LogEntry>() + entry.target.capacity() + entry.message.capacity()
}

/// Case-insensitive (ASCII) substring match over target + message.
/// `needle_lower` must already be ASCII-lowercased.
fn matches(entry: &LogEntry, needle_lower: &str) -> bool {
    contains(entry.target.as_bytes(), needle_lower.as_bytes())
        || contains(entry.message.as_bytes(), needle_lower.as_bytes())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

fn truncate_message(mut message: String) -> String {
    if message.len() <= MAX_MESSAGE_BYTES {
        return message;
    }
    let mut cut = MAX_MESSAGE_BYTES;
    while cut > 0 && !message.is_char_boundary(cut) {
        cut -= 1;
    }
    message.truncate(cut);
    message.push_str(TRUNCATION_MARKER);
    message.shrink_to_fit();
    message
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LogBounds {
    pub first: u64,
    pub next: u64,
    pub generation: u64,
    pub total: usize,
    pub dropped: u64,
}

/// Display index holds IDs, not Arcs: evicted messages are never pinned by
/// a hidden panel/cache. Query changes scan bounded slices outside the ring
/// mutex; unchanged frames only read metadata and acquire visible entries.
#[derive(Default)]
pub struct LogIndex {
    pub ids: VecDeque<u64>,
    level: Option<LevelFilter>,
    needle: String,
    next: u64,
    generation: Option<u64>,
    pub bounds: LogBounds,
    pub scanning: bool,
}

impl LogIndex {
    pub fn refresh(
        &mut self,
        buffer: &std::sync::Mutex<LogBuffer>,
        level: LevelFilter,
        needle: &str,
    ) {
        let level_changed = self.level != Some(level);
        let query_changed = !self.needle.eq_ignore_ascii_case(needle);
        let (bounds, entries) = {
            let ring = buffer.lock().unwrap_or_else(|p| p.into_inner());
            let bounds = ring.bounds();
            if level_changed || query_changed {
                self.ids.clear();
                self.next = bounds.first;
                self.level = Some(level);
                self.needle = needle.to_ascii_lowercase();
                self.generation = None;
            }
            self.next = self.next.max(bounds.first);
            let entries = if self.generation == Some(bounds.generation) && self.next == bounds.next
            {
                Vec::new()
            } else {
                ring.since(self.next, 8192)
            };
            (bounds, entries)
        };
        while self.ids.front().is_some_and(|&id| id < bounds.first) {
            self.ids.pop_front();
        }
        let start = std::time::Instant::now();
        let mut processed = 0usize;
        for entry in entries {
            if level_passes(entry.level, level) && matches(&entry, &self.needle) {
                self.ids.push_back(entry.id);
            }
            self.next = entry.id + 1;
            processed += 1;
            // Always make progress even on a loaded/slow runner; ordinary
            // batches finish well below the deadline.
            if processed >= 256 && start.elapsed() >= Duration::from_millis(2) {
                break;
            }
        }
        self.bounds = bounds;
        self.generation = Some(bounds.generation);
        self.scanning = self.next < bounds.next;
    }

    pub fn visible(
        &self,
        buffer: &std::sync::Mutex<LogBuffer>,
        range: std::ops::Range<usize>,
    ) -> Vec<Arc<LogEntry>> {
        buffer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entries_by_id(self.ids.range(range).copied())
    }

    pub fn copy_snapshot(&self, buffer: &std::sync::Mutex<LogBuffer>) -> Vec<Arc<LogEntry>> {
        let entries = buffer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .since(0, usize::MAX);
        entries
            .into_iter()
            .filter(|e| {
                level_passes(e.level, self.level.unwrap_or(LevelFilter::Off))
                    && matches(e, &self.needle)
            })
            .collect()
    }
}

/// Wall-clock time of day as `HH:MM:SS.mmm`, in UTC.
///
/// UTC matches the timestamps `env_logger` writes to stderr, so the panel
/// and the terminal log line up. The date is deliberately omitted: the
/// buffer only ever holds the recent past, and day rollover wraps.
pub fn format_hhmmss_mmm(at: SystemTime) -> String {
    let since_epoch = at.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let secs = since_epoch.as_secs();
    let (hours, minutes, seconds) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    format!(
        "{hours:02}:{minutes:02}:{seconds:02}.{:03}",
        since_epoch.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(buffer: &LogBuffer) -> Vec<String> {
        buffer.entries.iter().map(|e| e.message.clone()).collect()
    }

    fn push(buffer: &mut LogBuffer, level: Level, message: &str) {
        buffer.push(level, "cometty::test", message.to_string());
    }

    #[test]
    fn ring_evicts_oldest_and_counts_dropped() {
        let mut buffer = LogBuffer::new(3);
        for i in 0..5 {
            push(&mut buffer, Level::Info, &format!("msg {i}"));
        }
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.dropped(), 2);
        assert_eq!(messages(&buffer), ["msg 2", "msg 3", "msg 4"]);
    }

    #[test]
    fn byte_budget_evicts_before_line_cap() {
        // 4 lines grant a 4 KiB budget, but each entry also pays its
        // struct overhead, so four 1 KiB payloads no longer fit: one is
        // evicted even though the line cap would keep four.
        let mut buffer = LogBuffer::new(4);
        for i in 0..4 {
            push(
                &mut buffer,
                Level::Info,
                &format!("{i}{}", "x".repeat(1000)),
            );
        }
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.dropped(), 1);
        assert!(!messages(&buffer)[0].starts_with('0'));
    }

    #[test]
    fn oversized_message_is_truncated_not_evicting() {
        let mut buffer = LogBuffer::new(1);
        push(&mut buffer, Level::Info, &"x".repeat(MAX_MESSAGE_BYTES * 2));
        // The single entry survives (guard in `evict`) and is truncated.
        assert_eq!(buffer.len(), 1);
        let message = &messages(&buffer)[0];
        assert!(message.ends_with(TRUNCATION_MARKER));
        assert!(message.len() <= MAX_MESSAGE_BYTES + TRUNCATION_MARKER.len());
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        // 'é' is 2 bytes: a naive cut at `MAX_MESSAGE_BYTES` would split it.
        let mut buffer = LogBuffer::new(1);
        push(&mut buffer, Level::Info, &"é".repeat(MAX_MESSAGE_BYTES));
        let message = &messages(&buffer)[0];
        assert!(message.ends_with(TRUNCATION_MARKER));
        assert!(message.len() <= MAX_MESSAGE_BYTES + TRUNCATION_MARKER.len());
    }

    #[test]
    fn filtered_applies_level_and_text() {
        let mut buffer = LogBuffer::new(16);
        push(&mut buffer, Level::Error, "pty spawn failed");
        push(&mut buffer, Level::Info, "shell exited (tab 0)");
        push(&mut buffer, Level::Debug, "renderer init");
        push(&mut buffer, Level::Trace, "grid version bumped");

        assert_eq!(buffer.filtered(LevelFilter::Trace, "").len(), 4);
        assert_eq!(buffer.filtered(LevelFilter::Info, "").len(), 2);
        assert_eq!(buffer.filtered(LevelFilter::Off, "").len(), 0);
        // Text filter is case-insensitive and spans target + message.
        let hits = buffer.filtered(LevelFilter::Trace, "SHELL");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].message, "shell exited (tab 0)");
        assert_eq!(
            buffer.filtered(LevelFilter::Trace, "cometty::test").len(),
            4
        );
    }

    #[test]
    fn clear_resets_entries_and_dropped() {
        let mut buffer = LogBuffer::new(2);
        for i in 0..4 {
            push(&mut buffer, Level::Info, &format!("msg {i}"));
        }
        assert_eq!(buffer.dropped(), 2);
        buffer.clear();
        assert!(buffer.is_empty());
        assert_eq!(buffer.dropped(), 0);
    }

    #[test]
    fn capacity_shrink_keeps_newest_and_releases_memory() {
        let mut buffer = LogBuffer::new(8);
        for i in 0..8 {
            push(&mut buffer, Level::Info, &format!("msg {i}"));
        }
        buffer.set_capacity(2);
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.dropped(), 6);
        assert_eq!(messages(&buffer), ["msg 6", "msg 7"]);
    }

    #[test]
    fn time_of_day_is_utc_with_millis() {
        assert_eq!(
            format_hhmmss_mmm(UNIX_EPOCH),
            "00:00:00.000",
            "epoch is midnight UTC"
        );
        let one_hour_one_minute = UNIX_EPOCH + Duration::from_millis(3_661_500);
        assert_eq!(format_hhmmss_mmm(one_hour_one_minute), "01:01:01.500");
        let next_day = UNIX_EPOCH + Duration::from_secs(86_400 + 61);
        assert_eq!(format_hhmmss_mmm(next_day), "00:01:01.000");
    }

    #[test]
    fn log_index_is_incremental_bounded_and_never_pins_evicted_entries() {
        let buffer = std::sync::Mutex::new(LogBuffer::new(4));
        {
            let mut ring = buffer.lock().unwrap();
            for i in 0..4 {
                ring.push(Level::Info, "cometty::test", format!("first {i}"));
            }
        }
        let mut index = LogIndex::default();
        index.refresh(&buffer, LevelFilter::Info, "first");
        assert_eq!(index.ids.len(), 4);
        assert!(!index.scanning);
        let unchanged = index.bounds.generation;
        index.refresh(&buffer, LevelFilter::Info, "first");
        assert_eq!(index.bounds.generation, unchanged);
        assert_eq!(index.ids.len(), 4);
        {
            let mut ring = buffer.lock().unwrap();
            ring.push(Level::Info, "cometty::test", "first 4".into());
            ring.push(Level::Warn, "cometty::test", "other".into());
        }
        index.refresh(&buffer, LevelFilter::Info, "first");
        assert_eq!(
            index.ids.len(),
            3,
            "evicted IDs drop; only newcomers are scanned"
        );
        let visible = index.visible(&buffer, 0..2);
        assert_eq!(visible.len(), 2);
        assert!(visible[0].message.starts_with("first"));
        // Query change rescans from the current first ID, not stale Arcs.
        index.refresh(&buffer, LevelFilter::Info, "other");
        assert_eq!(index.ids.len(), 1);
        assert_eq!(index.copy_snapshot(&buffer).len(), 1);
        // A level change is a full filter reset as well.
        index.refresh(&buffer, LevelFilter::Error, "");
        assert!(index.ids.is_empty());
    }

    #[test]
    fn log_index_scans_in_batches_without_duplicates_or_gaps() {
        let buffer = std::sync::Mutex::new(LogBuffer::new(20_000));
        {
            let mut ring = buffer.lock().unwrap();
            for i in 0..20_000 {
                let level = if i % 7 == 0 {
                    Level::Warn
                } else {
                    Level::Debug
                };
                ring.push(level, "cometty::test", format!("entry {i}"));
            }
        }
        let mut index = LogIndex::default();
        let mut passes = 0;
        loop {
            index.refresh(&buffer, LevelFilter::Debug, "entry");
            passes += 1;
            if !index.scanning {
                break;
            }
            assert!(passes < 100, "batch slicing makes progress");
        }
        assert!(passes > 1, "large rings are split into bounded slices");
        assert_eq!(index.ids.len(), 20_000);
        assert!(index.ids.iter().is_sorted());
        let duplicate_free: std::collections::HashSet<_> = index.ids.iter().collect();
        assert_eq!(duplicate_free.len(), 20_000);
        assert_eq!(index.visible(&buffer, 19_998..20_000).len(), 2);
    }

    #[test]
    fn zero_capacity_is_clamped_to_one() {
        let mut buffer = LogBuffer::new(0);
        push(&mut buffer, Level::Info, "only");
        assert_eq!(buffer.len(), 1);
    }
}
