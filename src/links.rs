//! Hyperlink model: OSC 8 runs, plain-text URL detection, and opening.
//!
//! Pure, toolkit-free state (like [`crate::search`]) so it stays
//! unit-testable: the grid stores per-cell OSC 8 ids, [`HintView`] projects
//! the link runs of the visible rows for the renderer, and the app resolves
//! clicks against the same helpers.

use crate::grid::Cell;

/// Longest URI handed to the OS opener (bytes). Longer OSC 8 payloads are
/// logged and ignored at the grid (see `grid::link`).
pub const MAX_URI_CHARS: usize = 4096;

/// One detected plain-text URL on a visual row, in inclusive column space.
/// The last column may be a wide continuation so both halves hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkSpan {
    pub start: usize,
    pub end: usize,
}

/// URL schemes cometty is willing to hand to the OS opener. Everything else
/// (including payload-free custom schemes) is ignored.
///
/// Note: `file` is deliberately absent. Any terminal output (remote ssh,
/// `curl`, logs) can paint a clickable link, so handing `file://` to the OS
/// opener would let untrusted output open arbitrary local paths.
const ALLOWED_SCHEMES: [&str; 4] = ["http", "https", "ftp", "mailto"];

/// Literal prefixes that start a URL candidate. `mailto:` needs no `//`;
/// `www.` is accepted without a scheme (normalized to `https://` at open).
const PREFIXES: [&str; 5] = ["http://", "https://", "ftp://", "mailto:", "www."];

/// Resolve a column to its cluster lead (mirrors `selection::resolve_lead`:
/// a continuation maps back to the wide lead).
fn resolve_lead(row: &[Cell], x: usize) -> usize {
    if row.is_empty() {
        return 0;
    }
    let mut x = x.min(row.len() - 1);
    if row[x].width == 0 && x > 0 {
        x -= 1;
    }
    x
}

/// One cluster's slice of the per-row haystack.
struct Span {
    start: usize,
    len: usize,
    col: usize,
    width: u8,
}

/// A URL continues while the char is not whitespace/control and not one of
/// the delimiters that commonly wrap URLs in prose (`<https://x>`).
fn is_url_char(c: char) -> bool {
    !c.is_whitespace() && !c.is_control() && !matches!(c, '<' | '>' | '"' | '`')
}

/// Trim trailing sentence punctuation and unbalanced closing brackets:
/// `(https://x.y).` -> `https://x.y`, `https://x.y/a(b)` stays whole.
pub fn trim_url(url: &str) -> &str {
    let mut s = url;
    loop {
        let before = s;
        s = s.trim_end_matches(['.', ',', ';', ':', '!', '?', '\'']);
        while let Some(last) = s.chars().last() {
            let opener = match last {
                ')' => '(',
                ']' => '[',
                '}' => '{',
                _ => break,
            };
            let closes = s.chars().filter(|&c| c == last).count();
            let opens = s.chars().filter(|&c| c == opener).count();
            if closes > opens {
                s = &s[..s.len() - last.len_utf8()];
            } else {
                break;
            }
        }
        if s == before {
            return s;
        }
    }
}

/// Detect plain-text URLs on one visual row, in reading order.
///
/// Matching is per row: a URL that soft-wraps onto the next line is only
/// detected on its visible rows (the grid records no wrap flag).
pub fn detect_urls(row: &[Cell]) -> Vec<LinkSpan> {
    if row.is_empty() {
        return Vec::new();
    }
    // Build the haystack from cluster leads only, keeping a byte->column
    // map so a match can be translated back to cell columns.
    let mut hay = String::new();
    let mut spans: Vec<Span> = Vec::new();
    for (col, cell) in row.iter().enumerate() {
        if cell.width == 0 {
            continue;
        }
        let start = hay.len();
        hay.push(cell.ch);
        if let Some(extra) = cell.extra.as_deref() {
            hay.push_str(extra);
        }
        let len = hay.len() - start;
        if len > 0 {
            spans.push(Span {
                start,
                len,
                col,
                width: cell.width,
            });
        }
    }
    if hay.is_empty() {
        return Vec::new();
    }
    let lower = hay.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while from < lower.len() {
        let Some((at, prefix)) = find_prefix(&lower, from) else {
            break;
        };
        // Word boundary: `xhttps://` / `awww.foo` are not links.
        let boundary = at == 0
            || lower[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_ascii_alphanumeric());
        let start = at;
        let mut end = at + prefix.len();
        // How far the scan resumes: past the whole matched URL on success,
        // else past this prefix, so nested prefixes (`https://www.x`) never
        // produce overlapping second matches.
        let mut resume = at + prefix.len();
        if boundary {
            for (off, c) in hay[end..].char_indices() {
                if !is_url_char(c) {
                    break;
                }
                end = start + prefix.len() + off + c.len_utf8();
            }
            let candidate = &hay[start..end];
            let trimmed = trim_url(candidate);
            // Require content after the prefix (`www.` alone / `http://`).
            if trimmed.len() > prefix.len() {
                if let Some((start_col, end_col)) = span_cover(&spans, start, trimmed.len()) {
                    out.push(LinkSpan {
                        start: start_col,
                        end: end_col,
                    });
                }
                resume = start + trimmed.len();
            }
        }
        from = resume.max(from + 1);
    }
    out
}

/// Earliest known prefix at or after `from`.
fn find_prefix(hay: &str, from: usize) -> Option<(usize, &'static str)> {
    let mut best: Option<(usize, &'static str)> = None;
    for prefix in PREFIXES {
        if let Some(rel) = hay[from..].find(prefix) {
            let at = from + rel;
            if best.is_none_or(|(b, _)| at < b) {
                best = Some((at, prefix));
            }
        }
    }
    best
}

/// Map a byte range to inclusive cell columns, covering the trailing wide
/// continuation so both halves hit-test.
fn span_cover(spans: &[Span], start: usize, len: usize) -> Option<(usize, usize)> {
    let end = start + len;
    let first = spans
        .iter()
        .find(|s| s.start <= start && start < s.start + s.len)?;
    let last = spans
        .iter()
        .rev()
        .find(|s| s.start < end && end <= s.start + s.len)?;
    let mut end_col = last.col;
    if last.width == 2 {
        end_col += 1;
    }
    Some((first.col, end_col))
}

/// Text of a detected span (cluster leads only; continuations skipped).
pub fn url_text(row: &[Cell], span: LinkSpan) -> String {
    row.iter()
        .take(span.end + 1)
        .skip(span.start)
        .filter(|c| c.width != 0)
        .map(|c| c.cluster())
        .collect()
}

/// The detected URL covering `col`, if any (continuations resolve to their
/// lead). OSC 8 cells are checked by the caller first.
///
/// Scheme-less `www.` detections are normalized to `https://` so the
/// `detected_url_at -> sanitize_uri -> open_command` round-trip succeeds
/// instead of underlining a link whose click is then swallowed as rejected.
pub fn detected_url_at(row: &[Cell], col: usize) -> Option<String> {
    let col = resolve_lead(row, col);
    detect_urls(row)
        .into_iter()
        .find(|s| col >= s.start && col <= s.end)
        .map(|s| normalize_url(&url_text(row, s)))
}

/// Prepend `https://` to scheme-less `www.` detections (case-insensitive);
/// all other text passes through unchanged.
fn normalize_url(url: &str) -> String {
    if url.len() >= 4 && url[..4].eq_ignore_ascii_case("www.") {
        format!("https://{url}")
    } else {
        url.to_string()
    }
}

/// Per-row link runs in view space: OSC 8 links plus detected URLs not
/// already covered by one. Inclusive `(x0, x1)` columns, sorted.
pub fn hint_runs(row: &[Cell], with_urls: bool) -> Vec<(usize, usize)> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut x = 0;
    while x < row.len() {
        let Some(id) = row[x].hyperlink else {
            x += 1;
            continue;
        };
        let start = x;
        let mut end = x;
        x += 1;
        while x < row.len() && row[x].hyperlink == Some(id) {
            end = x;
            x += 1;
        }
        runs.push((start, end));
    }
    if with_urls {
        for span in detect_urls(row) {
            if row[span.start..=span.end]
                .iter()
                .any(|c| c.hyperlink.is_some())
            {
                continue;
            }
            runs.push((span.start, span.end));
        }
    }
    runs.sort_unstable();
    runs
}

/// Link/URL underline runs for the visible rows (row 0 = top of screen).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HintView {
    pub rows: Vec<Vec<(usize, usize)>>,
}

impl HintView {
    pub fn build(grid: &crate::grid::Grid, detect_urls: bool) -> Self {
        Self {
            rows: grid
                .view_rows()
                .iter()
                .map(|row| hint_runs(row, detect_urls))
                .collect(),
        }
    }

    /// True when the view cell (`x`, spanning 1 or 2 columns) is underlined.
    /// Mirror of `SearchView::hit`: inclusive ranges, wide-aware.
    pub fn hit(&self, view_row: usize, x: usize, span: usize) -> bool {
        let Some(runs) = self.rows.get(view_row) else {
            return false;
        };
        let right = x + span;
        runs.iter().any(|&(start, end)| start < right && x <= end)
    }

    /// Inspection helper (the renderer just never hits an empty run list).
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.rows.iter().all(|r| r.is_empty())
    }
}

/// OS opener families (kept explicit so `open_command` is testable without
/// `cfg` branching).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Macos,
    Linux,
    Windows,
}

pub fn platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::Macos
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else {
        Platform::Linux
    }
}

/// Strip control characters (ESC/BEL could smuggle sequences into an
/// opener) and validate the scheme. `None` = refuse.
pub fn sanitize_uri(uri: &str) -> Option<String> {
    let cleaned: String = uri.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_URI_CHARS {
        return None;
    }
    let scheme = trimmed.split_once(':')?.0.to_ascii_lowercase();
    if !ALLOWED_SCHEMES.contains(&scheme.as_str()) {
        return None;
    }
    Some(trimmed.to_string())
}

/// Argv for opening `uri` on `platform`, or `None` when refused.
/// The URI is passed as a single argument (no shell), so no escaping is
/// needed on macOS/Linux (`open`/`xdg-open` exec directly). On Windows the
/// launcher is `cmd /C start`, which *is* a shell: Rust's argv quoting only
/// quotes args containing space/tab/newline/quote, so cmd metacharacters
/// (`& | < > ( ) ^ % !`) in an attacker-controlled URI would be interpreted
/// as command separators. URIs containing them are refused on Windows.
/// Validation guarantees an allowlisted scheme, so the URI can never look
/// like an option to the opener.
pub fn open_command(uri: &str, platform: Platform) -> Option<Vec<String>> {
    let uri = sanitize_uri(uri)?;
    if platform == Platform::Windows && contains_cmd_metachars(&uri) {
        return None;
    }
    Some(match platform {
        Platform::Macos => vec!["open".to_string(), uri],
        Platform::Linux => vec!["xdg-open".to_string(), uri],
        Platform::Windows => vec![
            "cmd".to_string(),
            "/C".to_string(),
            "start".to_string(),
            String::new(),
            uri,
        ],
    })
}

/// True when `uri` contains characters `cmd.exe` interprets even inside a
/// quoted argv element (command separators, redirection, grouping, delayed
/// / environment-variable expansion). Controls/newlines are already stripped
/// by `sanitize_uri`, so only these printable metacharacters are checked.
fn contains_cmd_metachars(uri: &str) -> bool {
    uri.chars()
        .any(|c| matches!(c, '&' | '|' | '<' | '>' | '(' | ')' | '^' | '%' | '!' | '"'))
}

#[derive(Debug)]
pub enum OpenError {
    /// URI failed validation (scheme not allowed / empty / oversized).
    Rejected,
    Spawn(std::io::Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected => write!(f, "scheme not allowed"),
            Self::Spawn(e) => write!(f, "{e}"),
        }
    }
}

/// Hand `uri` to the platform opener, detached. Returns the spawned pid.
pub fn open(uri: &str) -> Result<u32, OpenError> {
    let argv = open_command(uri, platform()).ok_or(OpenError::Rejected)?;
    let mut command = std::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = command.spawn().map_err(OpenError::Spawn)?;
    Ok(child.id())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(text: &str) -> Vec<Cell> {
        text.chars()
            .map(|ch| Cell {
                ch,
                ..Default::default()
            })
            .collect()
    }

    fn spans(text: &str) -> Vec<(usize, usize)> {
        detect_urls(&cells(text))
            .into_iter()
            .map(|s| (s.start, s.end))
            .collect()
    }

    #[test]
    fn detects_schemes_and_www() {
        assert_eq!(spans("see https://example.com now"), vec![(4, 22)]);
        assert_eq!(spans("HTTP://EXAMPLE.COM"), vec![(0, 17)]);
        assert_eq!(spans("www.example.com"), vec![(0, 14)]);
        assert_eq!(spans("ftp://host/file"), vec![(0, 14)]);
        assert_eq!(spans("mailto:a@b.c"), vec![(0, 11)]);
        // `file://` is not an openable link (untrusted output could point it
        // at arbitrary local paths), so it is not detected either: detecting
        // it would underline a link whose click is then swallowed as rejected.
        assert!(spans("file:///tmp/x").is_empty());
        // A nested prefix stays one link, not two overlapping ones.
        assert_eq!(spans("https://www.example.com"), vec![(0, 22)]);
    }

    #[test]
    fn rejects_non_urls_and_prefix_soup() {
        assert!(spans("plain text").is_empty());
        // No word boundary: part of a larger token.
        assert!(spans("xhttps://example.com").is_empty());
        assert!(spans("awww.example.com").is_empty());
        // Bare scheme with nothing after it.
        assert!(spans("http://").is_empty());
        assert!(spans("https:// ").is_empty());
    }

    #[test]
    fn trims_sentence_punctuation_and_brackets() {
        assert_eq!(spans("(https://example.com)."), vec![(1, 19)]);
        assert_eq!(spans("see https://x.y, then"), vec![(4, 14)]);
        // Balanced brackets survive; unbalanced closers are trimmed.
        assert_eq!(spans("https://x.y/a(b)"), vec![(0, 15)]);
        assert_eq!(spans("https://x.y/a(b))"), vec![(0, 15)]);
        assert_eq!(trim_url("https://x.y/a(b)"), "https://x.y/a(b)");
        assert_eq!(trim_url("https://x.y!"), "https://x.y");
    }

    #[test]
    fn multiple_urls_and_wide_cells() {
        let row = cells("https://a.b and https://c.d");
        let found = detect_urls(&row);
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].start, found[0].end), (0, 10));
        assert_eq!((found[1].start, found[1].end), (16, 26));
        // A wide cell before the URL shifts columns but not bytes.
        let mut wide = cells("中 https://a.b");
        wide[0].width = 2;
        wide.insert(
            1,
            Cell {
                width: 0,
                ..Default::default()
            },
        );
        let found = detect_urls(&wide);
        assert_eq!((found[0].start, found[0].end), (3, 13));
        assert_eq!(url_text(&wide, found[0]), "https://a.b");
    }

    #[test]
    fn detected_url_at_resolves_continuations() {
        let row = cells("https://a.b tail");
        assert_eq!(detected_url_at(&row, 3).as_deref(), Some("https://a.b"));
        assert_eq!(detected_url_at(&row, 0).as_deref(), Some("https://a.b"));
        assert_eq!(detected_url_at(&row, 12), None, "outside the URL");
        // A wide cell before the URL shifts columns but not bytes; a click
        // anywhere in the URL (continuation included) resolves to its lead.
        let mut wide = cells("中https://a.b");
        wide[0].width = 2;
        wide.insert(
            1,
            Cell {
                width: 0,
                ..Default::default()
            },
        );
        assert_eq!(detected_url_at(&wide, 2).as_deref(), Some("https://a.b"));
        assert_eq!(detected_url_at(&wide, 3).as_deref(), Some("https://a.b"));
    }

    #[test]
    fn hint_runs_merge_osc8_and_prefer_it_over_detection() {
        let mut row = cells("link https://a.b");
        // OSC 8 on the word `link`.
        for c in row.iter_mut().take(4) {
            c.hyperlink = Some(7);
        }
        let runs = hint_runs(&row, true);
        assert_eq!(runs, vec![(0, 3), (5, 15)]);
        // The URL itself is an OSC 8 link: detection must not duplicate it.
        let mut row2 = cells("https://a.b");
        for c in row2.iter_mut() {
            c.hyperlink = Some(1);
        }
        assert_eq!(hint_runs(&row2, true), vec![(0, 10)]);
        // URL detection can be disabled.
        assert_eq!(hint_runs(&row, false), vec![(0, 3)]);
    }

    #[test]
    fn hint_view_hit_covers_wide_runs() {
        let view = HintView {
            rows: vec![vec![(2, 4)]],
        };
        assert!(!view.hit(0, 1, 1));
        assert!(view.hit(0, 2, 1));
        assert!(view.hit(0, 4, 1));
        assert!(!view.hit(0, 5, 1));
        assert!(!view.hit(1, 2, 1));
        // Wide cell starting inside the run is hit from both halves.
        assert!(view.hit(0, 4, 2));
        assert!(!view.is_empty());
        assert!(HintView::default().is_empty());
    }

    #[test]
    fn sanitize_requires_allowlisted_scheme() {
        assert_eq!(
            sanitize_uri("  https://x.y  ").as_deref(),
            Some("https://x.y")
        );
        assert!(sanitize_uri("javascript:alert(1)").is_none());
        assert!(sanitize_uri("vscode://file/x").is_none());
        assert!(sanitize_uri("").is_none());
        assert!(sanitize_uri("no-scheme").is_none());
        // Control bytes are stripped, not rejected outright.
        assert_eq!(
            sanitize_uri("https://x\u{7}.y").as_deref(),
            Some("https://x.y")
        );
        // Oversized URIs are refused.
        let huge = format!("https://x.y/{}", "a".repeat(MAX_URI_CHARS));
        assert!(sanitize_uri(&huge).is_none());
    }

    #[test]
    fn open_command_per_platform() {
        assert_eq!(
            open_command("https://x.y", Platform::Macos).unwrap(),
            vec!["open", "https://x.y"]
        );
        assert_eq!(
            open_command("https://x.y", Platform::Linux).unwrap(),
            vec!["xdg-open", "https://x.y"]
        );
        assert_eq!(
            open_command("https://x.y", Platform::Windows).unwrap(),
            vec!["cmd", "/C", "start", "", "https://x.y"]
        );
        assert!(open_command("javascript:x", Platform::Linux).is_none());
        // `file://` is refused on every platform: untrusted terminal output
        // must not open arbitrary local paths.
        assert!(open_command("file:///tmp/x", Platform::Macos).is_none());
        assert!(open_command("file:///tmp/x", Platform::Linux).is_none());
        assert!(open_command("file:///tmp/x", Platform::Windows).is_none());
    }

    #[test]
    fn windows_refuses_cmd_metachars_others_accept() {
        // `&` would chain a second command under `cmd /C start`.
        assert!(open_command("https://x.y/&calc", Platform::Windows).is_none());
        for evil in [
            "https://x.y/a|b",
            "https://x.y/a>b",
            "https://x.y/a<b",
            "https://x.y/(b)",
            "https://x.y/a^b",
            "https://x.y/%PATH%",
            "https://x.y/a!b",
            "https://x.y/a\"b",
        ] {
            assert!(
                open_command(evil, Platform::Windows).is_none(),
                "{evil} must be refused on Windows"
            );
            // macOS/Linux exec the opener directly (no shell), so the same
            // URI stays openable there.
            assert!(
                open_command(evil, Platform::Macos).is_some(),
                "{evil} must stay openable on macOS"
            );
            assert!(
                open_command(evil, Platform::Linux).is_some(),
                "{evil} must stay openable on Linux"
            );
        }
        // Benign query/fragment characters still pass on Windows.
        assert!(open_command("https://x.y/a?b=c#frag", Platform::Windows).is_some());
        // Fail-closed tradeoff: `&` is legitimate in multi-param queries
        // (`?a=1&b=2`) but indistinguishable from cmd chaining, so such URLs
        // are refused on Windows (copy-paste still works).
        assert!(open_command("https://x.y/a?a=1&b=2", Platform::Windows).is_none());
    }

    #[test]
    fn www_detection_round_trips_to_open() {
        let row = cells("see www.example.com/x now");
        let url = detected_url_at(&row, 5).expect("www. must be detected");
        assert_eq!(url, "https://www.example.com/x");
        // The normalized URL must survive validation and produce an opener
        // argv on every platform (previously it was rejected for no scheme,
        // swallowing the click).
        for platform in [Platform::Macos, Platform::Linux, Platform::Windows] {
            assert!(
                open_command(&url, platform).is_some(),
                "normalized www. URL must open on {platform:?}"
            );
        }
        // Case-insensitive prefix still normalizes.
        let row = cells("WWW.EXAMPLE.COM");
        assert_eq!(
            detected_url_at(&row, 0).as_deref(),
            Some("https://WWW.EXAMPLE.COM")
        );
    }
}
