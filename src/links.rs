//! Hyperlink and local path model: OSC 8 runs, plain-text URL/path detection,
//! safety guardrails, and opening.
//!
//! Pure, toolkit-free state (like [`crate::search`]) so it stays
//! unit-testable: the grid stores per-cell OSC 8 ids, [`HintView`] projects
//! the link runs of the visible rows for the renderer, and the app resolves
//! clicks against the same helpers.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use crate::config::LinksConfig;
use crate::grid::Cell;

/// Longest URI or local path handed to an opener (bytes). Longer OSC 8
/// payloads are logged and ignored at the grid (see `grid::link`).
pub const MAX_URI_CHARS: usize = 4096;

/// Maximum unique filesystem existence probes performed per [`HintView`] build.
/// Bounds UI-thread `stat()` work even on screens packed with fake path tokens.
pub const MAX_PATH_PROBES: usize = 256;

/// One detected plain-text URL or path on a visual row, in inclusive column
/// space. The last column may be a wide continuation so both halves hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkSpan {
    pub start: usize,
    pub end: usize,
}

/// Resolved local filesystem path with optional 1-based line and column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileTarget {
    pub path: PathBuf,
    pub line: Option<u32>,
    pub col: Option<u32>,
}

/// Resolved clickable target: either a web/remote URI or a local filesystem
/// target. Keeping them distinct guarantees callers of the web-only [`open`]
/// API (such as the updater) can never open `file://` targets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    Url(String),
    File(FileTarget),
}

/// One detected link or path on a visual row, with its column span and target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectedLink {
    pub span: LinkSpan,
    pub target: LinkTarget,
}

/// Detection inputs shared between [`HintView::build_with`] and click
/// resolution (`detected_link_at` / `resolve_osc8_uri`).
#[derive(Clone, Copy, Debug)]
pub struct LinkContext<'a> {
    pub detect_urls: bool,
    pub open_files: bool,
    pub detect_paths: bool,
    pub cwd: Option<&'a str>,
    pub home: Option<&'a str>,
    pub local_host: Option<&'a str>,
    pub platform: Platform,
}

impl<'a> LinkContext<'a> {
    /// Web-only context (no `file://` or local path probing).
    pub fn web_only(detect_urls: bool) -> Self {
        Self {
            detect_urls,
            open_files: false,
            detect_paths: false,
            cwd: None,
            home: None,
            local_host: None,
            platform: platform(),
        }
    }
}

/// URL schemes allowed through the web-only opener ([`sanitize_uri`] /
/// [`open_command`] / [`open`]). Local `file://` links go through the separate
/// [`parse_file_uri`] / [`open_target`] path with host, existence, and
/// executable guardrails.
const ALLOWED_SCHEMES: [&str; 4] = ["http", "https", "ftp", "mailto"];

/// Literal prefixes that start a web URL candidate. `mailto:` needs no `//`;
/// `www.` is accepted without a scheme (normalized to `https://` at open).
const WEB_PREFIXES: [&str; 5] = ["http://", "https://", "ftp://", "mailto:", "www."];

/// Prefixes scanned when local `file://` URLs are also enabled.
const ALL_URL_PREFIXES: [&str; 6] = [
    "http://", "https://", "ftp://", "mailto:", "www.", "file://",
];

/// macOS bundle / launcher / installer extensions that must be revealed in
/// Finder instead of launched. Checked case-insensitively on both the clicked
/// path and its canonical target (including directory bundles like `.app`).
const MACOS_BUNDLE_EXTS: &[&str] = &[
    "app",
    "command",
    "tool",
    "action",
    "workflow",
    "scpt",
    "scptd",
    "applescript",
    "osa",
    "osax",
    "pkg",
    "mpkg",
    "dmg",
    "iso",
    "terminal",
    "inetloc",
    "webloc",
    "fileloc",
    "prefpane",
    "qlgenerator",
    "kext",
    "plugin",
    "bundle",
    "xpc",
    "appex",
    "mobileconfig",
    "jar",
];

/// Linux desktop/installer/executable package extensions revealed instead of
/// launched.
const LINUX_BUNDLE_EXTS: &[&str] = &[
    "desktop",
    "appimage",
    "flatpakref",
    "snap",
    "run",
    "deb",
    "rpm",
    "jar",
];

/// Windows executable, script, shortcut, and installer extensions revealed
/// instead of launched.
const WINDOWS_BUNDLE_EXTS: &[&str] = &[
    "exe", "bat", "cmd", "com", "msi", "msp", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf",
    "wsh", "scr", "pif", "lnk", "hta", "reg", "cpl", "inf", "msc", "url", "jar",
];

/// Cached, bounded filesystem existence prober for one row/view scan.
struct ProbeCache<F> {
    probe: F,
    seen: HashMap<PathBuf, bool>,
    remaining: usize,
}

impl<F: FnMut(&Path) -> bool> ProbeCache<F> {
    fn new(probe: F, limit: usize) -> Self {
        Self {
            probe,
            seen: HashMap::new(),
            remaining: limit,
        }
    }

    fn exists(&mut self, path: &Path) -> bool {
        if let Some(&cached) = self.seen.get(path) {
            return cached;
        }
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        let ok = (self.probe)(path);
        self.seen.insert(path.to_path_buf(), ok);
        ok
    }
}

/// Real filesystem existence check: accepts regular files and directories,
/// rejecting broken symlinks, FIFOs, sockets, and device nodes.
fn fs_path_exists(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() || meta.is_dir())
}

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

/// Build the lead-only haystack and byte->column table for a row.
fn build_haystack(row: &[Cell]) -> (String, Vec<Span>) {
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
    (hay, spans)
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

/// True when `host` (the authority of a `file://host/path` or `OSC 7` URI)
/// refers to the local machine: empty, `localhost`, or matching `local_host`
/// (case-insensitive, allowing macOS `.local` short-name equivalence).
pub fn is_local_file_host(host: &str, local_host: Option<&str>) -> bool {
    if host.is_empty() {
        return true;
    }
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() || host.starts_with('.') {
        return false;
    }
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let Some(local) = local_host.map(|h| h.strip_suffix('.').unwrap_or(h)) else {
        return false;
    };
    if local.is_empty() || local.starts_with('.') {
        return false;
    }
    if host.eq_ignore_ascii_case(local) {
        return true;
    }
    if let Some(short) = strip_dot_local(local)
        && host.eq_ignore_ascii_case(short)
    {
        return true;
    }
    if let Some(short) = strip_dot_local(host)
        && local.eq_ignore_ascii_case(short)
    {
        return true;
    }
    false
}

/// Strip a trailing `.local` suffix (case-insensitive) when the prefix is
/// non-empty.
fn strip_dot_local(host: &str) -> Option<&str> {
    if host.len() <= 6 {
        return None;
    }
    let split = host.len() - 6;
    if host.is_char_boundary(split) && host[split..].eq_ignore_ascii_case(".local") {
        let prefix = &host[..split];
        (!prefix.is_empty()).then_some(prefix)
    } else {
        None
    }
}

/// Decode `%XX` escapes in a `file://` path. Rejects invalid UTF-8 and any
/// ASCII control byte (including NUL `0x00`).
fn percent_decode_path(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            )
        {
            let byte = (hi * 16 + lo) as u8;
            if byte < 0x20 || byte == 0x7F {
                return None;
            }
            out.push(byte);
            i += 3;
            continue;
        }
        let byte = bytes[i];
        if byte < 0x20 || byte == 0x7F {
            return None;
        }
        out.push(byte);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// Parse an optional `#L42`, `#L42C10`, `#L42:10`, `#42`, or `#42:10` fragment.
fn parse_fragment_location(fragment: &str) -> (Option<u32>, Option<u32>) {
    let f = fragment.strip_prefix(['L', 'l']).unwrap_or(fragment);
    if let Some((line_s, col_s)) = f
        .split_once(['C', 'c', ':'])
        .filter(|(a, b)| !a.is_empty() && !b.is_empty())
        && let (Some(line), Some(col)) = (parse_pos_u32(line_s), parse_pos_u32(col_s))
    {
        return (Some(line), Some(col));
    }
    if let Some(line) = parse_pos_u32(f) {
        return (Some(line), None);
    }
    (None, None)
}

fn parse_pos_u32(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = s.parse::<u32>().ok()?;
    (n > 0).then_some(n)
}

/// True when `s` starts with a Windows drive prefix (`C:\...` or `C:/...`).
fn is_windows_drive_path(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() > 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

/// Remove redundant `.` components without altering symlink semantics of `..`.
fn clean_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        if comp != Component::CurDir {
            out.push(comp.as_os_str());
        }
    }
    if out.as_os_str().is_empty() {
        path.to_path_buf()
    } else {
        out
    }
}

/// Parse a local `file://` URI into a [`FileTarget`], enforcing the local-host
/// allowlist, percent-decoding, control/NUL rejection, and length bounds.
/// Does not touch the filesystem (see [`resolve_file_uri_with_probe`]).
pub fn parse_file_uri(
    uri: &str,
    local_host: Option<&str>,
    platform: Platform,
) -> Option<FileTarget> {
    let trimmed = uri.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_URI_CHARS || trimmed.chars().any(char::is_control)
    {
        return None;
    }
    if trimmed.len() < 7 || !trimmed[..7].eq_ignore_ascii_case("file://") {
        return None;
    }
    let rest = &trimmed[7..];
    let slash = rest.find('/')?;
    let host = &rest[..slash];
    if !is_local_file_host(host, local_host) {
        return None;
    }
    let path_and_tail = &rest[slash..];
    let (without_frag, fragment) = match path_and_tail.split_once('#') {
        Some((p, f)) => (p, Some(f)),
        None => (path_and_tail, None),
    };
    let raw_path = match without_frag.split_once('?') {
        Some((p, _)) => p,
        None => without_frag,
    };
    let mut decoded = percent_decode_path(raw_path)?;
    if decoded.is_empty() || decoded.len() > MAX_URI_CHARS {
        return None;
    }
    if platform == Platform::Windows {
        if decoded.len() > 3
            && decoded.as_bytes()[0] == b'/'
            && decoded.as_bytes()[1].is_ascii_alphabetic()
            && decoded.as_bytes()[2] == b':'
            && matches!(decoded.as_bytes()[3], b'/' | b'\\')
        {
            decoded.remove(0);
        }
        if decoded.starts_with("\\\\") || decoded.starts_with("//") {
            return None;
        }
        if !is_windows_drive_path(&decoded) {
            return None;
        }
    } else if !decoded.starts_with('/') || decoded.starts_with("//") {
        return None;
    }
    let (line, col) = fragment.map_or((None, None), parse_fragment_location);
    Some(FileTarget {
        path: clean_path(Path::new(&decoded)),
        line,
        col,
    })
}

/// Parse a `file://` URI and confirm its resolved path exists locally.
/// Also tolerates trailing `:line[:col]` inside the path when the literal
/// path does not exist (e.g. `file:///path/foo.rs:42:10`).
fn resolve_file_uri_with_prober<F: FnMut(&Path) -> bool>(
    uri: &str,
    local_host: Option<&str>,
    platform: Platform,
    prober: &mut ProbeCache<F>,
) -> Option<FileTarget> {
    let target = parse_file_uri(uri, local_host, platform)?;
    if prober.exists(&target.path) {
        return Some(target);
    }
    if target.line.is_none()
        && let Some(raw) = target.path.to_str()
    {
        let (stripped, line, col) = parse_location_suffix(raw, platform);
        if line.is_some() {
            let candidate = clean_path(Path::new(stripped));
            if prober.exists(&candidate) {
                return Some(FileTarget {
                    path: candidate,
                    line,
                    col,
                });
            }
        }
    }
    None
}

/// Resolve an `OSC 8` hyperlink URI using the active [`LinkContext`].
pub fn resolve_osc8_uri(uri: &str, ctx: &LinkContext<'_>) -> Option<LinkTarget> {
    let mut prober = ProbeCache::new(fs_path_exists, MAX_PATH_PROBES);
    resolve_osc8_uri_with_prober(uri, ctx, &mut prober)
}

fn resolve_osc8_uri_with_prober<F: FnMut(&Path) -> bool>(
    uri: &str,
    ctx: &LinkContext<'_>,
    prober: &mut ProbeCache<F>,
) -> Option<LinkTarget> {
    if let Some(clean) = sanitize_uri(uri) {
        return Some(LinkTarget::Url(clean));
    }
    if ctx.open_files {
        return resolve_file_uri_with_prober(uri, ctx.local_host, ctx.platform, prober)
            .map(LinkTarget::File);
    }
    None
}

/// Detect plain-text web URLs on one visual row, in reading order.
///
/// Matching is per row: a URL that soft-wraps onto the next line is only
/// detected on its visible rows (the grid records no wrap flag).
pub fn detect_urls(row: &[Cell]) -> Vec<LinkSpan> {
    if row.is_empty() {
        return Vec::new();
    }
    let (hay, spans) = build_haystack(row);
    if hay.is_empty() {
        return Vec::new();
    }
    let mut claimed = vec![false; row.len()];
    let mut prober = ProbeCache::new(|_| false, 0);
    let mut links = Vec::new();
    scan_urls_in_haystack(
        &hay,
        &spans,
        &mut claimed,
        &LinkContext::web_only(true),
        &mut prober,
        &mut links,
    );
    links.into_iter().map(|l| l.span).collect()
}

/// Detect plain-text URLs and local paths on one visual row, skipping cells
/// already owned by an `OSC 8` hyperlink.
#[cfg_attr(not(test), allow(dead_code))]
pub fn detect_links_in_row(row: &[Cell], ctx: &LinkContext<'_>) -> Vec<DetectedLink> {
    let mut prober = ProbeCache::new(fs_path_exists, MAX_PATH_PROBES);
    detect_links_in_row_with_prober(row, ctx, &mut prober)
}

fn detect_links_in_row_with_prober<F: FnMut(&Path) -> bool>(
    row: &[Cell],
    ctx: &LinkContext<'_>,
    prober: &mut ProbeCache<F>,
) -> Vec<DetectedLink> {
    if row.is_empty() || (!ctx.detect_urls && !(ctx.open_files && ctx.detect_paths)) {
        return Vec::new();
    }
    let (hay, spans) = build_haystack(row);
    if hay.is_empty() {
        return Vec::new();
    }
    let mut claimed: Vec<bool> = row.iter().map(|c| c.hyperlink.is_some()).collect();
    let mut out = Vec::new();
    if ctx.detect_urls {
        scan_urls_in_haystack(&hay, &spans, &mut claimed, ctx, prober, &mut out);
    }
    if ctx.open_files && ctx.detect_paths {
        scan_paths_in_haystack(&hay, &spans, &mut claimed, ctx, prober, &mut out);
    }
    out.sort_unstable_by_key(|link| (link.span.start, link.span.end));
    out
}

fn scan_urls_in_haystack<F: FnMut(&Path) -> bool>(
    hay: &str,
    spans: &[Span],
    claimed: &mut [bool],
    ctx: &LinkContext<'_>,
    prober: &mut ProbeCache<F>,
    out: &mut Vec<DetectedLink>,
) {
    let prefixes: &[&str] = if ctx.open_files {
        &ALL_URL_PREFIXES
    } else {
        &WEB_PREFIXES
    };
    let lower = hay.to_ascii_lowercase();
    let mut from = 0;
    while from < lower.len() {
        let Some((at, prefix)) = find_prefix(&lower, from, prefixes) else {
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
                let target = if prefix == "file://" {
                    resolve_file_uri_with_prober(trimmed, ctx.local_host, ctx.platform, prober)
                        .map(LinkTarget::File)
                } else {
                    Some(LinkTarget::Url(normalize_url(trimmed)))
                };
                if let Some(target) = target {
                    if let Some((start_col, end_col)) = span_cover(spans, start, trimmed.len())
                        && span_unclaimed(claimed, start_col, end_col)
                    {
                        claim_span(claimed, start_col, end_col);
                        out.push(DetectedLink {
                            span: LinkSpan {
                                start: start_col,
                                end: end_col,
                            },
                            target,
                        });
                    }
                    resume = start + trimmed.len();
                }
            }
        }
        from = resume.max(from + 1);
    }
}

/// Earliest known prefix at or after `from`.
fn find_prefix<'a>(hay: &str, from: usize, prefixes: &[&'a str]) -> Option<(usize, &'a str)> {
    let mut best: Option<(usize, &'a str)> = None;
    for &prefix in prefixes {
        if let Some(rel) = hay[from..].find(prefix) {
            let at = from + rel;
            if best.is_none_or(|(b, _)| at < b) {
                best = Some((at, prefix));
            }
        }
    }
    best
}

fn is_hard_path_delim(c: char) -> bool {
    c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | '"' | '\'' | '`' | '|' | ';')
}

/// Scan unclaimed text on a row for local absolute and relative file paths.
fn scan_paths_in_haystack<F: FnMut(&Path) -> bool>(
    hay: &str,
    spans: &[Span],
    claimed: &mut [bool],
    ctx: &LinkContext<'_>,
    prober: &mut ProbeCache<F>,
    out: &mut Vec<DetectedLink>,
) {
    let mut pos = 0;
    while pos < hay.len() {
        let Some(ch) = hay[pos..].chars().next() else {
            break;
        };
        if ch.is_whitespace() || ch.is_control() {
            pos += ch.len_utf8();
            continue;
        }
        // Quoted same-row path (supports spaces, e.g. `"/Users/me/My Notes.md"`).
        if matches!(ch, '"' | '\'') {
            let inner_start = pos + 1;
            if let Some(rel_close) = hay[inner_start..].find(ch) {
                let inner_end = inner_start + rel_close;
                let inner = &hay[inner_start..inner_end];
                if !inner.is_empty()
                    && inner.len() <= MAX_URI_CHARS
                    && !inner.chars().any(char::is_control)
                    && starts_like_explicit_path(inner, ctx.platform)
                    && let Some(target) = try_resolve_path_slice(inner, ctx, prober)
                {
                    if let Some((start_col, end_col)) = span_cover(spans, inner_start, inner.len())
                        && span_unclaimed(claimed, start_col, end_col)
                    {
                        claim_span(claimed, start_col, end_col);
                        out.push(DetectedLink {
                            span: LinkSpan {
                                start: start_col,
                                end: end_col,
                            },
                            target: LinkTarget::File(target),
                        });
                    }
                    pos = inner_end + 1;
                    continue;
                }
            }
            // Did not resolve as a whole quoted path: step past the opening
            // quote so tokens inside prose quotes still get scanned normally.
            pos += 1;
            continue;
        }
        if is_hard_path_delim(ch) {
            pos += ch.len_utf8();
            continue;
        }
        let token_start = pos;
        let mut token_end = pos;
        for (off, c) in hay[pos..].char_indices() {
            if is_hard_path_delim(c) {
                break;
            }
            token_end = pos + off + c.len_utf8();
        }
        pos = token_end.max(pos + ch.len_utf8());
        if token_end > token_start {
            scan_path_token(
                &hay[token_start..token_end],
                token_start,
                spans,
                claimed,
                ctx,
                prober,
                out,
            );
        }
    }
}

fn starts_like_explicit_path(s: &str, platform: Platform) -> bool {
    if s.starts_with("//") || s.starts_with("\\\\") {
        return false;
    }
    s.starts_with('/')
        || s.starts_with("~/")
        || s.starts_with("./")
        || s.starts_with("../")
        || (platform == Platform::Windows
            && (is_windows_drive_path(s)
                || s.starts_with("~\\")
                || s.starts_with(".\\")
                || s.starts_with("..\\")))
}

/// Trim surrounding wrappers (`(foo)`, `[foo]`) and trailing prose
/// punctuation (`.`, `,`, `!`, `?`, `:`, unbalanced closing brackets),
/// returning the relative `(start, end)` byte range inside `raw`.
fn trim_path_token(raw: &str) -> (usize, usize) {
    let mut start = 0;
    let mut end = raw.len();
    while start < end {
        let ch = raw[start..end].chars().next().unwrap();
        if matches!(ch, '(' | '[' | '{') {
            start += ch.len_utf8();
        } else {
            break;
        }
    }
    loop {
        let before = end;
        let s = &raw[start..end];
        let trimmed = s.trim_end_matches(['.', ',', '!', '?', ':']);
        end = start + trimmed.len();
        while start < end {
            let s = &raw[start..end];
            let Some(last) = s.chars().last() else {
                break;
            };
            let opener = match last {
                ')' => '(',
                ']' => '[',
                '}' => '{',
                _ => break,
            };
            let closes = s.chars().filter(|&c| c == last).count();
            let opens = s.chars().filter(|&c| c == opener).count();
            if closes > opens {
                end -= last.len_utf8();
            } else {
                break;
            }
        }
        if end == before {
            break;
        }
    }
    (start, end)
}

/// Scan one whitespace-delimited token for one or more local paths, handling
/// `key=path` flags and `:`-separated path lists (`PATH`, `error:file:42`).
fn scan_path_token<F: FnMut(&Path) -> bool>(
    raw: &str,
    base_offset: usize,
    spans: &[Span],
    claimed: &mut [bool],
    ctx: &LinkContext<'_>,
    prober: &mut ProbeCache<F>,
    out: &mut Vec<DetectedLink>,
) {
    let (rel_start, rel_end) = trim_path_token(raw);
    if rel_start >= rel_end {
        return;
    }
    let trimmed = &raw[rel_start..rel_end];
    let offset = base_offset + rel_start;

    // 1. Try the whole trimmed token first (`src/links.rs:42:10`, `/etc/hosts`).
    if let Some(target) = try_resolve_path_slice(trimmed, ctx, prober) {
        emit_file_span(offset, trimmed.len(), target, spans, claimed, out);
        return;
    }

    // 2. Flag value after `=` (`--config=/Users/me/config.toml`, `FILE=src/links.rs:10`).
    if let Some(eq) = trimmed.find('=') {
        let rhs_start = eq + 1;
        if rhs_start < trimmed.len() {
            scan_path_token(
                &trimmed[rhs_start..],
                offset + rhs_start,
                spans,
                claimed,
                ctx,
                prober,
                out,
            );
            return;
        }
    }

    // 3. Colon-separated segments (`error:src/links.rs:42:10`, `/usr/local/bin:/usr/bin`).
    if trimmed.contains(':') {
        let parts = split_colon_segments(trimmed, ctx.platform);
        if parts.len() > 1 {
            let mut i = 0;
            while i < parts.len() {
                let (seg_start, seg_end) = parts[i];
                // Try attaching `:line:col` or `:line` from the following segments.
                if i + 2 < parts.len()
                    && parse_pos_u32(&trimmed[parts[i + 1].0..parts[i + 1].1]).is_some()
                    && parse_pos_u32(&trimmed[parts[i + 2].0..parts[i + 2].1]).is_some()
                {
                    let combined = &trimmed[seg_start..parts[i + 2].1];
                    if let Some(target) = try_resolve_path_slice(combined, ctx, prober) {
                        emit_file_span(
                            offset + seg_start,
                            combined.len(),
                            target,
                            spans,
                            claimed,
                            out,
                        );
                        i += 3;
                        continue;
                    }
                }
                if i + 1 < parts.len()
                    && parse_pos_u32(&trimmed[parts[i + 1].0..parts[i + 1].1]).is_some()
                {
                    let combined = &trimmed[seg_start..parts[i + 1].1];
                    if let Some(target) = try_resolve_path_slice(combined, ctx, prober) {
                        emit_file_span(
                            offset + seg_start,
                            combined.len(),
                            target,
                            spans,
                            claimed,
                            out,
                        );
                        i += 2;
                        continue;
                    }
                }
                let seg = &trimmed[seg_start..seg_end];
                let (s0, s1) = trim_path_token(seg);
                if s0 < s1 {
                    let sub = &seg[s0..s1];
                    if let Some(target) = try_resolve_path_slice(sub, ctx, prober) {
                        emit_file_span(
                            offset + seg_start + s0,
                            sub.len(),
                            target,
                            spans,
                            claimed,
                            out,
                        );
                    }
                }
                i += 1;
            }
        }
    }
}

/// Split `s` on `:` into `(start, end)` byte ranges without breaking a Windows
/// drive prefix (`C:\...` or `C:/...`).
fn split_colon_segments(s: &str, platform: Platform) -> Vec<(usize, usize)> {
    let bytes = s.as_bytes();
    let mut parts = Vec::new();
    let mut seg_start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' {
            let is_win_drive = platform == Platform::Windows
                && i == seg_start + 1
                && bytes[seg_start].is_ascii_alphabetic()
                && bytes.get(i + 1).is_some_and(|&b| b == b'\\' || b == b'/');
            if !is_win_drive {
                parts.push((seg_start, i));
                seg_start = i + 1;
            }
        }
        i += 1;
    }
    parts.push((seg_start, bytes.len()));
    parts
}

fn emit_file_span(
    byte_start: usize,
    byte_len: usize,
    target: FileTarget,
    spans: &[Span],
    claimed: &mut [bool],
    out: &mut Vec<DetectedLink>,
) {
    if let Some((start_col, end_col)) = span_cover(spans, byte_start, byte_len)
        && span_unclaimed(claimed, start_col, end_col)
    {
        claim_span(claimed, start_col, end_col);
        out.push(DetectedLink {
            span: LinkSpan {
                start: start_col,
                end: end_col,
            },
            target: LinkTarget::File(target),
        });
    }
}

/// Parse an optional trailing location suffix: `:line:col`, `:line`,
/// `(line,col)`, or `(line)`.
pub fn parse_location_suffix(token: &str, platform: Platform) -> (&str, Option<u32>, Option<u32>) {
    // Style 1: `path(line,col)` or `path(line)` (TypeScript, C#, MSVC).
    if let Some(without_close) = token.strip_suffix(')')
        && let Some(open) = without_close.rfind('(')
        && open > 0
    {
        let inside = &without_close[open + 1..];
        let base = &without_close[..open];
        if let Some((l_s, c_s)) = inside.split_once(',') {
            if let (Some(line), Some(col)) = (parse_pos_u32(l_s), parse_pos_u32(c_s)) {
                return (base, Some(line), Some(col));
            }
        } else if let Some(line) = parse_pos_u32(inside) {
            return (base, Some(line), None);
        }
    }

    // Style 2: `path:line:col` or `path:line` (rustc, cargo, ripgrep, gcc).
    if let Some((prefix, last)) = token.rsplit_once(':')
        && !prefix.is_empty()
        && let Some(num_last) = parse_pos_u32(last)
    {
        let drive_colon = platform == Platform::Windows
            && prefix.len() == 1
            && prefix.as_bytes()[0].is_ascii_alphabetic();
        if !drive_colon {
            if let Some((base, mid)) = prefix.rsplit_once(':')
                && !base.is_empty()
                && let Some(num_mid) = parse_pos_u32(mid)
            {
                let base_drive = platform == Platform::Windows
                    && base.len() == 1
                    && base.as_bytes()[0].is_ascii_alphabetic();
                if !base_drive {
                    return (base, Some(num_mid), Some(num_last));
                }
            }
            return (prefix, Some(num_last), None);
        }
    }

    (token, None, None)
}

/// Cheap syntactic filter before probing the filesystem. Prevents English
/// words, version numbers, IP addresses, and comment markers from hitting
/// `stat()` or lighting up prose.
fn looks_like_path_candidate(raw: &str, platform: Platform) -> bool {
    if raw.is_empty()
        || raw.len() > MAX_URI_CHARS
        || raw.chars().any(char::is_control)
        || matches!(raw, "/" | "." | "..")
        || raw.starts_with("//")
        || raw.starts_with("\\\\")
        || raw.contains("://")
    {
        return false;
    }
    if raw.starts_with('/') {
        return raw.bytes().any(|b| b != b'/');
    }
    if platform == Platform::Windows && is_windows_drive_path(raw) {
        return true;
    }
    if raw.starts_with("~/") && raw.len() > 2 {
        return true;
    }
    if platform == Platform::Windows && raw.starts_with("~\\") && raw.len() > 2 {
        return true;
    }
    if (raw.starts_with("./") && raw.len() > 2) || (raw.starts_with("../") && raw.len() > 3) {
        return true;
    }
    if platform == Platform::Windows
        && ((raw.starts_with(".\\") && raw.len() > 2) || (raw.starts_with("..\\") && raw.len() > 3))
    {
        return true;
    }
    if raw.contains('/') {
        return raw.bytes().any(|b| b != b'/');
    }
    if platform == Platform::Windows && raw.contains('\\') {
        return raw.bytes().any(|b| b != b'\\');
    }
    // Single-component dotfile (`.gitignore`, `.env`).
    if let Some(rest) = raw.strip_prefix('.') {
        return !rest.is_empty()
            && !rest.starts_with('.')
            && rest
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
            && rest.bytes().any(|b| b.is_ascii_alphanumeric());
    }
    // Single-component filename with an alphabetic extension (`Cargo.toml`,
    // `README.md`, `video.mp4`), rejecting numeric tokens (`v1.2.3`, `127.0.0.1`).
    if let Some((stem, ext)) = raw.rsplit_once('.') {
        return !stem.is_empty()
            && stem.chars().any(|c| c.is_ascii_alphanumeric())
            && !ext.is_empty()
            && ext.len() <= 16
            && ext.bytes().all(|b| b.is_ascii_alphanumeric())
            && ext.bytes().any(|b| b.is_ascii_alphabetic());
    }
    false
}

/// Resolve one trimmed candidate slice (possibly with `:line[:col]` or
/// `(line,col)` suffix) against `ctx` and verify it exists via `prober`.
fn try_resolve_path_slice<F: FnMut(&Path) -> bool>(
    token: &str,
    ctx: &LinkContext<'_>,
    prober: &mut ProbeCache<F>,
) -> Option<FileTarget> {
    let (stripped, line, col) = parse_location_suffix(token, ctx.platform);
    // When a location suffix is present, check the stripped path first (1 stat
    // for the common compiler/grep case); fall back to the literal token only
    // if the stripped path does not exist (handles literal `foo:42` filenames).
    if line.is_some()
        && let Some(path) = resolve_candidate_path(stripped, ctx, prober)
    {
        return Some(FileTarget { path, line, col });
    }
    resolve_candidate_path(token, ctx, prober).map(|path| FileTarget {
        path,
        line: None,
        col: None,
    })
}

/// Expand a candidate path (`~`, absolute, or relative to `ctx.cwd`), check
/// existence via `prober`, and fall back to stripping a `git diff` `a/`/`b/`
/// prefix when the literal path does not exist.
fn resolve_candidate_path<F: FnMut(&Path) -> bool>(
    raw: &str,
    ctx: &LinkContext<'_>,
    prober: &mut ProbeCache<F>,
) -> Option<PathBuf> {
    if !looks_like_path_candidate(raw, ctx.platform) {
        return None;
    }
    let resolved = expand_path(raw, ctx)?;
    if prober.exists(&resolved) {
        return Some(clean_path(&resolved));
    }
    // `git diff` prefixes paths with `a/` and `b/`. Check literal first so a
    // real `a/` or `b/` directory always wins.
    if let Some(rest) = raw.strip_prefix("a/").or_else(|| raw.strip_prefix("b/"))
        && !rest.is_empty()
        && let Some(cwd) = ctx.cwd
    {
        let alt = PathBuf::from(cwd).join(rest);
        if prober.exists(&alt) {
            return Some(clean_path(&alt));
        }
    }
    None
}

fn expand_path(raw: &str, ctx: &LinkContext<'_>) -> Option<PathBuf> {
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| {
        (ctx.platform == Platform::Windows)
            .then(|| raw.strip_prefix("~\\"))
            .flatten()
    }) {
        let home = ctx.home.filter(|h| !h.is_empty())?;
        return Some(PathBuf::from(home).join(rest));
    }
    if raw.starts_with('/') || (ctx.platform == Platform::Windows && is_windows_drive_path(raw)) {
        return Some(PathBuf::from(raw));
    }
    let cwd = ctx.cwd.filter(|c| !c.is_empty())?;
    Some(PathBuf::from(cwd).join(raw))
}

fn span_unclaimed(claimed: &[bool], start_col: usize, end_col: usize) -> bool {
    claimed
        .get(start_col..=end_col)
        .is_some_and(|slice| slice.iter().all(|&c| !c))
}

fn claim_span(claimed: &mut [bool], start_col: usize, end_col: usize) {
    if let Some(slice) = claimed.get_mut(start_col..=end_col) {
        slice.fill(true);
    }
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
#[cfg_attr(not(test), allow(dead_code))]
pub fn url_text(row: &[Cell], span: LinkSpan) -> String {
    row.iter()
        .take(span.end + 1)
        .skip(span.start)
        .filter(|c| c.width != 0)
        .map(|c| c.cluster())
        .collect()
}

/// The detected web URL covering `col`, if any (continuations resolve to their
/// lead). OSC 8 cells are checked by the caller first.
///
/// Scheme-less `www.` detections are normalized to `https://` so the
/// `detected_url_at -> sanitize_uri -> open_command` round-trip succeeds
/// instead of underlining a link whose click is then swallowed as rejected.
#[cfg_attr(not(test), allow(dead_code))]
pub fn detected_url_at(row: &[Cell], col: usize) -> Option<String> {
    let col = resolve_lead(row, col);
    detect_urls(row)
        .into_iter()
        .find(|s| col >= s.start && col <= s.end)
        .map(|s| normalize_url(&url_text(row, s)))
}

/// The detected plain-text URL or local path covering `col`, if any
/// (continuations resolve to their lead). `OSC 8` cells are checked by the
/// caller first and skipped here.
pub fn detected_link_at(row: &[Cell], col: usize, ctx: &LinkContext<'_>) -> Option<LinkTarget> {
    let mut prober = ProbeCache::new(fs_path_exists, MAX_PATH_PROBES);
    detected_link_at_with_prober(row, col, ctx, &mut prober)
}

fn detected_link_at_with_prober<F: FnMut(&Path) -> bool>(
    row: &[Cell],
    col: usize,
    ctx: &LinkContext<'_>,
    prober: &mut ProbeCache<F>,
) -> Option<LinkTarget> {
    let col = resolve_lead(row, col);
    detect_links_in_row_with_prober(row, ctx, prober)
        .into_iter()
        .find(|link| col >= link.span.start && col <= link.span.end)
        .map(|link| link.target)
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

/// Per-row link runs in view space: OSC 8 links plus detected web URLs not
/// already covered by one. Inclusive `(x0, x1)` columns, sorted.
#[cfg_attr(not(test), allow(dead_code))]
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

/// Link/URL/path underline runs for the visible rows (row 0 = top of screen).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HintView {
    pub rows: Vec<Vec<(usize, usize)>>,
}

impl HintView {
    /// Web-only builder (no filesystem probes), retained for unit tests and
    /// callers without tab CWD context.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn build(grid: &crate::grid::Grid, detect_urls: bool) -> Self {
        Self::build_with_probe(grid, &LinkContext::web_only(detect_urls), |_| false)
    }

    /// Build visible link runs for `grid` using `ctx` (OSC 8 hyperlinks,
    /// plain-text URLs, and existing local paths). Filesystem probes are
    /// deduplicated and capped at [`MAX_PATH_PROBES`] per build.
    pub fn build_with(grid: &crate::grid::Grid, ctx: &LinkContext<'_>) -> Self {
        Self::build_with_probe(grid, ctx, fs_path_exists)
    }

    /// [`Self::build_with`] with an explicit existence probe (testable seam).
    pub fn build_with_probe(
        grid: &crate::grid::Grid,
        ctx: &LinkContext<'_>,
        probe: impl FnMut(&Path) -> bool,
    ) -> Self {
        let mut prober = ProbeCache::new(probe, MAX_PATH_PROBES);
        let mut osc8_allowed: HashMap<u16, bool> = HashMap::new();
        let rows = grid
            .view_rows()
            .iter()
            .map(|row| {
                let cells = row.materialize();
                let mut runs: Vec<(usize, usize)> = Vec::new();
                let mut x = 0;
                while x < cells.len() {
                    let Some(id) = cells[x].hyperlink else {
                        x += 1;
                        continue;
                    };
                    let start = x;
                    let mut end = x;
                    x += 1;
                    while x < cells.len() && cells[x].hyperlink == Some(id) {
                        end = x;
                        x += 1;
                    }
                    let openable = *osc8_allowed.entry(id).or_insert_with(|| {
                        grid.hyperlink_uri(id).is_some_and(|uri| {
                            resolve_osc8_uri_with_prober(uri, ctx, &mut prober).is_some()
                        })
                    });
                    if openable {
                        runs.push((start, end));
                    }
                }
                for link in detect_links_in_row_with_prober(&cells, ctx, &mut prober) {
                    runs.push((link.span.start, link.span.end));
                }
                runs.sort_unstable();
                runs
            })
            .collect();
        Self { rows }
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

/// OS opener families (kept explicit so command builders are testable without
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
/// opener) and validate the scheme against [`ALLOWED_SCHEMES`]. `None` = refuse.
///
/// Strictly web-only (`http`, `https`, `ftp`, `mailto`). `file://` URIs are
/// handled separately through [`parse_file_uri`] and [`open_target`].
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

/// Argv for opening a web `uri` on `platform`, or `None` when refused.
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
/// / environment-variable expansion). Controls/newlines are already stripped,
/// so only these printable metacharacters are checked.
fn contains_cmd_metachars(uri: &str) -> bool {
    uri.chars()
        .any(|c| matches!(c, '&' | '|' | '<' | '>' | '(' | ')' | '^' | '%' | '!' | '"'))
}

/// True when `ext` (without leading dot) is a dangerous bundle, launcher,
/// installer, or executable extension on `platform`.
pub fn is_bundle_or_launcher_ext(ext: &str, platform: Platform) -> bool {
    let list = match platform {
        Platform::Macos => MACOS_BUNDLE_EXTS,
        Platform::Linux => LINUX_BUNDLE_EXTS,
        Platform::Windows => WINDOWS_BUNDLE_EXTS,
    };
    list.iter().any(|&danger| ext.eq_ignore_ascii_case(danger))
}

fn path_has_dangerous_ext(path: &Path, platform: Platform) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| is_bundle_or_launcher_ext(ext, platform))
}

/// Classify a local target by inspecting both the clicked path and its
/// canonicalized target (catching symlinks to `.app` bundles or executables).
/// Returns `(is_bundle_or_launcher, is_executable_regular_file)`.
fn classify_local_file(
    path: &Path,
    canonical: Option<&Path>,
    meta: &std::fs::Metadata,
    platform: Platform,
) -> (bool, bool) {
    let is_bundle = path_has_dangerous_ext(path, platform)
        || canonical.is_some_and(|c| path_has_dangerous_ext(c, platform));
    #[cfg(unix)]
    let is_exec_file = {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && (meta.permissions().mode() & 0o111) != 0
    };
    #[cfg(not(unix))]
    let is_exec_file = {
        let _ = meta;
        false
    };
    (is_bundle, is_exec_file)
}

/// Validate a local path string before passing it to an OS opener or custom
/// command: must be absolute, non-empty, within [`MAX_URI_CHARS`], free of
/// control bytes, and never a Windows UNC path.
fn validated_local_path(path: &Path, platform: Platform) -> Option<&str> {
    let s = path.to_str()?;
    if s.is_empty() || s.len() > MAX_URI_CHARS || s.chars().any(char::is_control) {
        return None;
    }
    if platform == Platform::Windows {
        if s.starts_with("\\\\") || s.starts_with("//") || !is_windows_drive_path(s) {
            return None;
        }
    } else if !s.starts_with('/') || s.starts_with("//") {
        return None;
    }
    Some(s)
}

/// Action planned for a local file click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileAction {
    Open,
    Reveal,
}

/// Argv for safely revealing `path` in the platform file manager instead of
/// executing it.
pub fn reveal_file_command(path: &Path, platform: Platform) -> Option<Vec<String>> {
    let path_str = validated_local_path(path, platform)?;
    Some(match platform {
        Platform::Macos => vec!["open".to_string(), "-R".to_string(), path_str.to_string()],
        Platform::Linux => {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(path);
            let parent_str = validated_local_path(parent, platform)?;
            vec!["xdg-open".to_string(), parent_str.to_string()]
        }
        Platform::Windows => vec!["explorer.exe".to_string(), format!("/select,{path_str}")],
    })
}

/// Argv for opening a safe local path with the platform default opener.
pub fn default_open_file_command(path: &Path, platform: Platform) -> Option<Vec<String>> {
    let path_str = validated_local_path(path, platform)?;
    if platform == Platform::Windows && contains_cmd_metachars(path_str) {
        return None;
    }
    Some(match platform {
        Platform::Macos => vec!["open".to_string(), path_str.to_string()],
        Platform::Linux => vec!["xdg-open".to_string(), path_str.to_string()],
        Platform::Windows => vec![
            "cmd".to_string(),
            "/C".to_string(),
            "start".to_string(),
            String::new(),
            path_str.to_string(),
        ],
    })
}

/// Build argv from a configured `[links] file_opener` template.
///
/// The template is split into argv tokens *first* (using
/// [`crate::pty::split_command_args`], no shell interpolation), and
/// `{path}`, `{line}`, `{col}` are expanded within each argument token
/// afterwards. `argv[0]` is never substituted, and `{path}` is replaced last
/// so filenames containing spaces, quotes, shell metacharacters, or literal
/// `{line}`/`{col}` text can never inject extra arguments or be re-expanded.
pub fn custom_file_opener_command(
    template: &str,
    target: &FileTarget,
    platform: Platform,
) -> Option<Vec<String>> {
    let path_str = validated_local_path(&target.path, platform)?;
    let tokens = crate::pty::split_command_args(template);
    let (program, rest) = tokens.split_first()?;
    if program.is_empty() || program.chars().any(char::is_control) {
        return None;
    }
    let mut argv = Vec::with_capacity(tokens.len() + 1);
    argv.push(program.clone());
    let mut saw_path = false;
    for token in rest {
        if token.contains("{path}") {
            saw_path = true;
        }
        let Some(expanded) = expand_opener_arg(token, path_str, target.line, target.col) else {
            continue;
        };
        argv.push(expanded);
    }
    if !saw_path {
        argv.push(path_str.to_string());
    }
    Some(argv)
}

/// Expand `{path}`, `{line}`, `{col}` inside one already-split argv token.
/// Returns `None` when a standalone optional location arg (`+{line}`,
/// `+{line}:{col}`, `--line={line}`) should be omitted because no line exists.
fn expand_opener_arg(
    token: &str,
    path_str: &str,
    line: Option<u32>,
    col: Option<u32>,
) -> Option<String> {
    if !token.contains("{path}") {
        if token == "+{line}:{col}" {
            let l = line?;
            return Some(match col {
                Some(c) => format!("+{l}:{c}"),
                None => format!("+{l}"),
            });
        }
        if token == "+{line}" {
            return Some(format!("+{}", line?));
        }
        if let Some(prefix) = token.strip_suffix("={line}")
            && !prefix.is_empty()
            && !prefix.contains("{col}")
        {
            return Some(format!("{prefix}={}", line?));
        }
    }
    // Expand `:line:col` / `:line` / `:col` segments first on the template
    // token (before substituting `{path}`, so a filename literally containing
    // `{line}` or `{col}` is never touched).
    let mut s = token.to_string();
    if s.contains(":{line}:{col}") {
        let replacement = match (line, col) {
            (Some(l), Some(c)) => format!(":{l}:{c}"),
            (Some(l), None) => format!(":{l}"),
            (None, _) => String::new(),
        };
        s = s.replace(":{line}:{col}", &replacement);
    }
    if s.contains(":{line}") {
        let replacement = match line {
            Some(l) => format!(":{l}"),
            None => String::new(),
        };
        s = s.replace(":{line}", &replacement);
    }
    if s.contains(":{col}") {
        let replacement = match col {
            Some(c) => format!(":{c}"),
            None => String::new(),
        };
        s = s.replace(":{col}", &replacement);
    }
    if s.contains("{line}") {
        s = s.replace("{line}", &line.unwrap_or(1).to_string());
    }
    if s.contains("{col}") {
        s = s.replace("{col}", &col.unwrap_or(1).to_string());
    }
    if s.contains("{path}") {
        s = s.replace("{path}", path_str);
    }
    Some(s)
}

/// Decide the action (`Open` vs `Reveal`) and argv for opening a local file.
///
/// - Dangerous bundles/launchers (`.app`, `.command`, `.pkg`, `.dmg`,
///   `.desktop`, `.exe`, …) always reveal in the file manager.
/// - With no custom `file_opener`, executable regular files (`+x` on Unix)
///   also reveal in the file manager instead of launching.
/// - With a custom `file_opener`, regular files (including `+x` scripts) and
///   plain directories open in the configured command.
pub fn plan_file_open(
    target: &FileTarget,
    file_opener: &str,
    is_bundle: bool,
    is_exec_file: bool,
    platform: Platform,
) -> Option<(FileAction, Vec<String>)> {
    let custom = !file_opener.trim().is_empty();
    if is_bundle || (!custom && is_exec_file) {
        return reveal_file_command(&target.path, platform).map(|argv| (FileAction::Reveal, argv));
    }
    if custom {
        return custom_file_opener_command(file_opener, target, platform)
            .map(|argv| (FileAction::Open, argv));
    }
    default_open_file_command(&target.path, platform).map(|argv| (FileAction::Open, argv))
}

#[derive(Debug)]
pub enum OpenError {
    /// URI or path failed validation (scheme not allowed / remote host /
    /// local files disabled / empty / oversized).
    Rejected,
    /// Local path does not exist (or is not a regular file / directory).
    NotFound,
    Spawn(std::io::Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected => write!(f, "target not allowed"),
            Self::NotFound => write!(f, "file not found"),
            Self::Spawn(e) => write!(f, "{e}"),
        }
    }
}

/// Outcome of opening a [`LinkTarget`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenOutcome {
    /// Opened in the browser, default app, or custom `file_opener`.
    Opened(u32),
    /// Revealed in the platform file manager instead of executing.
    Revealed(u32),
}

/// Spawn `argv` detached with null stdio. When `macos_path_fallback` is true
/// and `argv[0]` is a bare program name missing from launchd's minimal `PATH`,
/// also checks standard macOS user binary directories so `code` / `zed` /
/// `cursor` / `subl` work when launched from `Cometty.app`.
fn spawn_detached(argv: &[String], macos_path_fallback: bool) -> Result<u32, OpenError> {
    let (program, args) = argv.split_first().ok_or(OpenError::Rejected)?;
    let spawn = |prog: &Path| {
        let mut command = std::process::Command::new(prog);
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command.spawn()
    };
    match spawn(Path::new(program)) {
        Ok(child) => Ok(child.id()),
        Err(err)
            if macos_path_fallback
                && cfg!(target_os = "macos")
                && err.kind() == std::io::ErrorKind::NotFound
                && is_bare_program_name(program) =>
        {
            for dir in macos_fallback_bin_dirs() {
                let candidate = dir.join(program);
                if candidate.is_file() {
                    return spawn(&candidate)
                        .map(|child| child.id())
                        .map_err(OpenError::Spawn);
                }
            }
            Err(OpenError::Spawn(err))
        }
        Err(err) => Err(OpenError::Spawn(err)),
    }
}

fn is_bare_program_name(program: &str) -> bool {
    !program.is_empty()
        && !matches!(program, "." | "..")
        && !program.contains('/')
        && !program.contains('\\')
}

fn macos_fallback_bin_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    if let Some(home) = crate::procinfo::home() {
        dirs.push(PathBuf::from(home).join(".local/bin"));
    }
    dirs
}

/// Hand a web `uri` to the platform opener, detached. Returns the spawned pid.
/// Strictly web-only (`http`, `https`, `ftp`, `mailto`).
pub fn open(uri: &str) -> Result<u32, OpenError> {
    let argv = open_command(uri, platform()).ok_or(OpenError::Rejected)?;
    spawn_detached(&argv, false)
}

/// Open a resolved [`LinkTarget`] according to `config` and local-file safety
/// guardrails.
pub fn open_target(target: &LinkTarget, config: &LinksConfig) -> Result<OpenOutcome, OpenError> {
    match target {
        LinkTarget::Url(uri) => open(uri).map(OpenOutcome::Opened),
        LinkTarget::File(file) => open_file(file, config),
    }
}

fn open_file(target: &FileTarget, config: &LinksConfig) -> Result<OpenOutcome, OpenError> {
    if !config.open_files {
        return Err(OpenError::Rejected);
    }
    let meta = std::fs::metadata(&target.path).map_err(|_| OpenError::NotFound)?;
    if !meta.is_file() && !meta.is_dir() {
        return Err(OpenError::Rejected);
    }
    let canonical = std::fs::canonicalize(&target.path).ok();
    let plat = platform();
    let (is_bundle, is_exec_file) =
        classify_local_file(&target.path, canonical.as_deref(), &meta, plat);
    let (action, argv) = plan_file_open(target, &config.file_opener, is_bundle, is_exec_file, plat)
        .ok_or(OpenError::Rejected)?;
    let use_macos_fallback = action == FileAction::Open && !config.file_opener.trim().is_empty();
    let pid = spawn_detached(&argv, use_macos_fallback)?;
    Ok(match action {
        FileAction::Open => OpenOutcome::Opened(pid),
        FileAction::Reveal => OpenOutcome::Revealed(pid),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

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

    fn mock_ctx<'a>(cwd: Option<&'a str>, home: Option<&'a str>) -> LinkContext<'a> {
        LinkContext {
            detect_urls: true,
            open_files: true,
            detect_paths: true,
            cwd,
            home,
            local_host: Some("MacBook.local"),
            platform: Platform::Macos,
        }
    }

    fn detect_mock(text: &str, ctx: &LinkContext<'_>, existing: &[&str]) -> Vec<DetectedLink> {
        let set: HashSet<PathBuf> = existing.iter().map(PathBuf::from).collect();
        let mut prober = ProbeCache::new(|p: &Path| set.contains(&clean_path(p)), MAX_PATH_PROBES);
        detect_links_in_row_with_prober(&cells(text), ctx, &mut prober)
    }

    #[test]
    fn detects_schemes_and_www() {
        assert_eq!(spans("see https://example.com now"), vec![(4, 22)]);
        assert_eq!(spans("HTTP://EXAMPLE.COM"), vec![(0, 17)]);
        assert_eq!(spans("www.example.com"), vec![(0, 14)]);
        assert_eq!(spans("ftp://host/file"), vec![(0, 14)]);
        assert_eq!(spans("mailto:a@b.c"), vec![(0, 11)]);
        // Web-only `detect_urls` never emits `file://`.
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
        assert!(sanitize_uri("file:///tmp/x").is_none());
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
    fn open_command_per_platform_stays_web_only() {
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
        // The web-only `open_command` (also used by the updater) refuses
        // `file://` on every platform.
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
        for platform in [Platform::Macos, Platform::Linux, Platform::Windows] {
            assert!(
                open_command(&url, platform).is_some(),
                "normalized www. URL must open on {platform:?}"
            );
        }
        let row = cells("WWW.EXAMPLE.COM");
        assert_eq!(
            detected_url_at(&row, 0).as_deref(),
            Some("https://WWW.EXAMPLE.COM")
        );
    }

    #[test]
    fn local_file_host_matching() {
        assert!(is_local_file_host("", Some("MacBook.local")));
        assert!(is_local_file_host("localhost", Some("MacBook.local")));
        assert!(is_local_file_host("LOCALHOST", None));
        assert!(is_local_file_host("MacBook.local", Some("MacBook.local")));
        assert!(is_local_file_host("macbook.local", Some("MacBook.local")));
        assert!(is_local_file_host("macbook", Some("MacBook.local")));
        assert!(is_local_file_host("macbook.local", Some("macbook")));
        assert!(!is_local_file_host("remote-server", Some("MacBook.local")));
        assert!(!is_local_file_host(
            "evil.example.com",
            Some("MacBook.local")
        ));
        assert!(!is_local_file_host("MacBook.local", None));
        assert!(!is_local_file_host(".", Some("MacBook.local")));
        assert!(!is_local_file_host("..", Some("MacBook.local")));
        assert!(!is_local_file_host(".local", Some(".local")));
    }

    #[test]
    fn parse_file_uri_handles_hosts_decoding_fragments_and_rejects_bad() {
        let local = Some("MacBook.local");
        assert_eq!(
            parse_file_uri("file:///Users/me/my%20file.txt", local, Platform::Macos),
            Some(FileTarget {
                path: PathBuf::from("/Users/me/my file.txt"),
                line: None,
                col: None,
            })
        );
        assert_eq!(
            parse_file_uri("file://localhost/tmp/a.rs#L42C10", local, Platform::Macos),
            Some(FileTarget {
                path: PathBuf::from("/tmp/a.rs"),
                line: Some(42),
                col: Some(10),
            })
        );
        assert_eq!(
            parse_file_uri("FILE://macbook/tmp/a.rs#42", local, Platform::Macos),
            Some(FileTarget {
                path: PathBuf::from("/tmp/a.rs"),
                line: Some(42),
                col: None,
            })
        );
        // Remote host refused.
        assert_eq!(
            parse_file_uri("file://remote.box/etc/passwd", local, Platform::Macos),
            None
        );
        // Encoded NUL / control bytes refused.
        assert_eq!(
            parse_file_uri("file:///tmp/a%00b", local, Platform::Macos),
            None
        );
        assert_eq!(
            parse_file_uri("file:///tmp/a%0Ab", local, Platform::Macos),
            None
        );
        // Double-slash authority/UNC-like refused.
        assert_eq!(
            parse_file_uri("file:////host/share", local, Platform::Macos),
            None
        );
        // Windows drive URI strips leading slash; UNC refused.
        assert_eq!(
            parse_file_uri("file:///C:/Users/me/a.rs", local, Platform::Windows),
            Some(FileTarget {
                path: PathBuf::from("C:/Users/me/a.rs"),
                line: None,
                col: None,
            })
        );
        assert_eq!(
            parse_file_uri("file://server/share/a.rs", local, Platform::Windows),
            None
        );
    }

    #[test]
    fn detects_local_paths_file_urls_locations_and_skips_prose() {
        let ctx = mock_ctx(Some("/work/repo"), Some("/Users/me"));
        let existing = &[
            "/etc/hosts",
            "/Users/me/notes.md",
            "/Users/me/My Notes.md",
            "/work/repo/src/links.rs",
            "/work/repo/README.md",
            "/work/repo/.gitignore",
            "/work/repo/docs",
            "/work/repo/docs/",
            "/work/other/Cargo.toml",
            "/usr/local/bin",
            "/usr/bin",
        ];

        // Absolute, home, explicit relative, implicit relative, dotfile, extension.
        let found = detect_mock("open /etc/hosts now", &ctx, existing);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].span.start, found[0].span.end), (5, 14));
        assert_eq!(
            found[0].target,
            LinkTarget::File(FileTarget {
                path: PathBuf::from("/etc/hosts"),
                line: None,
                col: None,
            })
        );

        let found = detect_mock("see ~/notes.md.", &ctx, existing);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].span.start, found[0].span.end), (4, 13));
        assert_eq!(
            found[0].target,
            LinkTarget::File(FileTarget {
                path: PathBuf::from("/Users/me/notes.md"),
                line: None,
                col: None,
            })
        );

        // Compiler output with :line:col and trailing `:`.
        let found = detect_mock("  --> src/links.rs:42:10:", &ctx, existing);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].span.start, found[0].span.end), (6, 23));
        assert_eq!(
            found[0].target,
            LinkTarget::File(FileTarget {
                path: PathBuf::from("/work/repo/src/links.rs"),
                line: Some(42),
                col: Some(10),
            })
        );

        // TypeScript/MSVC `(line,col)` format.
        let found = detect_mock("src/links.rs(42,10): error", &ctx, existing);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].span.start, found[0].span.end), (0, 18));
        assert_eq!(
            found[0].target,
            LinkTarget::File(FileTarget {
                path: PathBuf::from("/work/repo/src/links.rs"),
                line: Some(42),
                col: Some(10),
            })
        );

        // Explicit `./` and `../` plus single-file `README.md`, `.gitignore`, `docs/`.
        let found = detect_mock("./src/links.rs README.md .gitignore docs/", &ctx, existing);
        assert_eq!(found.len(), 4);
        assert_eq!(
            found[0].target,
            LinkTarget::File(FileTarget {
                path: PathBuf::from("/work/repo/src/links.rs"),
                line: None,
                col: None,
            })
        );
        assert_eq!(
            found[1].target,
            LinkTarget::File(FileTarget {
                path: PathBuf::from("/work/repo/README.md"),
                line: None,
                col: None,
            })
        );
        assert_eq!(
            found[2].target,
            LinkTarget::File(FileTarget {
                path: PathBuf::from("/work/repo/.gitignore"),
                line: None,
                col: None,
            })
        );

        // `git diff` a/ and b/ prefixes fall back to stripped path.
        let found = detect_mock("+++ b/src/links.rs", &ctx, existing);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].target,
            LinkTarget::File(FileTarget {
                path: PathBuf::from("/work/repo/src/links.rs"),
                line: None,
                col: None,
            })
        );

        // Quoted path with spaces.
        let found = detect_mock("File \"/Users/me/My Notes.md\", line 5", &ctx, existing);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].span.start, found[0].span.end), (6, 26));
        assert_eq!(
            found[0].target,
            LinkTarget::File(FileTarget {
                path: PathBuf::from("/Users/me/My Notes.md"),
                line: None,
                col: None,
            })
        );

        // Flag value after `=` and colon-separated PATH.
        let found = detect_mock(
            "--config=/etc/hosts PATH=/usr/local/bin:/usr/bin",
            &ctx,
            existing,
        );
        assert_eq!(found.len(), 3);
        assert_eq!(
            found[0].target,
            LinkTarget::File(FileTarget {
                path: PathBuf::from("/etc/hosts"),
                line: None,
                col: None,
            })
        );
        assert_eq!(
            found[1].target,
            LinkTarget::File(FileTarget {
                path: PathBuf::from("/usr/local/bin"),
                line: None,
                col: None,
            })
        );
        assert_eq!(
            found[2].target,
            LinkTarget::File(FileTarget {
                path: PathBuf::from("/usr/bin"),
                line: None,
                col: None,
            })
        );

        // Prefix before colon (`error:src/links.rs:42`).
        let found = detect_mock("error:src/links.rs:42", &ctx, existing);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].span.start, found[0].span.end), (6, 20));
        assert_eq!(
            found[0].target,
            LinkTarget::File(FileTarget {
                path: PathBuf::from("/work/repo/src/links.rs"),
                line: Some(42),
                col: None,
            })
        );

        // Local `file://` URL detected; remote `file://` URL skipped.
        let found = detect_mock(
            "file:///etc/hosts file://remote-box/etc/hosts",
            &ctx,
            existing,
        );
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].span.start, found[0].span.end), (0, 16));

        // Web URL does NOT also match `/etc/hosts` inside its path!
        let found = detect_mock("https://example.com/etc/hosts", &ctx, existing);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].target,
            LinkTarget::Url("https://example.com/etc/hosts".to_string())
        );

        // Prose, bare single-word `docs` (no `/` or `.`), comments `//etc/hosts`,
        // bare `/`, version numbers, and nonexistent paths are NOT matched.
        assert!(
            detect_mock(
                "see docs and //etc/hosts or / or v1.2.3 or read/write or missing/file.rs",
                &ctx,
                existing
            )
            .is_empty()
        );
    }

    #[test]
    fn wide_cells_and_quoted_prose_keep_accurate_columns() {
        let ctx = mock_ctx(Some("/work/repo"), Some("/Users/me"));
        let existing: HashSet<PathBuf> = [PathBuf::from("/work/repo/src/links.rs")]
            .into_iter()
            .collect();
        let mut row = cells("中 \"see src/links.rs:7\"");
        row[0].width = 2;
        row.insert(
            1,
            Cell {
                width: 0,
                ..Default::default()
            },
        );
        let mut prober = ProbeCache::new(|p: &Path| existing.contains(&clean_path(p)), 64);
        let found = detect_links_in_row_with_prober(&row, &ctx, &mut prober);
        assert_eq!(found.len(), 1);
        assert_eq!(url_text(&row, found[0].span), "src/links.rs:7");
        assert_eq!(
            found[0].target,
            LinkTarget::File(FileTarget {
                path: PathBuf::from("/work/repo/src/links.rs"),
                line: Some(7),
                col: None,
            })
        );
    }

    #[test]
    fn probe_cache_deduplicates_and_caps_work() {
        let mut calls = 0usize;
        let mut prober = ProbeCache::new(
            |_p: &Path| {
                calls += 1;
                false
            },
            3,
        );
        assert!(!prober.exists(Path::new("/a")));
        assert!(!prober.exists(Path::new("/a"))); // cached hit
        assert!(!prober.exists(Path::new("/b")));
        assert!(!prober.exists(Path::new("/c")));
        assert!(!prober.exists(Path::new("/d"))); // over cap -> skipped
        assert_eq!(calls, 3);
    }

    #[test]
    fn osc8_hint_view_filters_remote_and_disallowed_schemes() {
        let mut g = crate::grid::Grid::new(40, 1, crate::theme::Theme::default());
        g.set_hyperlink(Some("file:///etc/hosts"));
        for ch in "local".chars() {
            g.put_char(ch);
        }
        g.set_hyperlink(None);
        g.put_char(' ');
        g.set_hyperlink(Some("file://remote.example/etc/hosts"));
        for ch in "remote".chars() {
            g.put_char(ch);
        }
        g.set_hyperlink(None);
        g.put_char(' ');
        g.set_hyperlink(Some("javascript:alert(1)"));
        for ch in "evil".chars() {
            g.put_char(ch);
        }
        g.set_hyperlink(None);

        let ctx = mock_ctx(Some("/work"), Some("/Users/me"));
        let view = HintView::build_with_probe(&g, &ctx, |p| p == Path::new("/etc/hosts"));
        // Only the local existing file:// OSC 8 run is underlined.
        assert_eq!(view.rows[0], vec![(0, 4)]);

        // When `open_files = false`, even local file:// OSC 8 is hidden.
        let mut off = ctx;
        off.open_files = false;
        let view = HintView::build_with_probe(&g, &off, |p| p == Path::new("/etc/hosts"));
        assert!(view.rows[0].is_empty());
    }

    #[test]
    fn custom_file_opener_substitutes_safely_and_handles_optional_line_col() {
        let target_full = FileTarget {
            path: PathBuf::from("/Users/me/My Notes {line}.md"),
            line: Some(42),
            col: Some(10),
        };
        let target_line = FileTarget {
            path: PathBuf::from("/work/src/links.rs"),
            line: Some(42),
            col: None,
        };
        let target_plain = FileTarget {
            path: PathBuf::from("/work/src/links.rs"),
            line: None,
            col: None,
        };

        // VS Code / Zed `{path}:{line}:{col}` style.
        assert_eq!(
            custom_file_opener_command(
                "code -g {path}:{line}:{col}",
                &target_full,
                Platform::Macos
            )
            .unwrap(),
            vec!["code", "-g", "/Users/me/My Notes {line}.md:42:10"]
        );
        assert_eq!(
            custom_file_opener_command(
                "code -g {path}:{line}:{col}",
                &target_line,
                Platform::Macos
            )
            .unwrap(),
            vec!["code", "-g", "/work/src/links.rs:42"]
        );
        assert_eq!(
            custom_file_opener_command(
                "code -g {path}:{line}:{col}",
                &target_plain,
                Platform::Macos
            )
            .unwrap(),
            vec!["code", "-g", "/work/src/links.rs"]
        );

        // `+{line}` drops cleanly when no line is present.
        assert_eq!(
            custom_file_opener_command("nvim +{line} {path}", &target_line, Platform::Macos)
                .unwrap(),
            vec!["nvim", "+42", "/work/src/links.rs"]
        );
        assert_eq!(
            custom_file_opener_command("nvim +{line} {path}", &target_plain, Platform::Macos)
                .unwrap(),
            vec!["nvim", "/work/src/links.rs"]
        );

        // Standalone `{line}` defaults to 1 so `--line` never dangles.
        assert_eq!(
            custom_file_opener_command("idea --line {line} {path}", &target_plain, Platform::Macos)
                .unwrap(),
            vec!["idea", "--line", "1", "/work/src/links.rs"]
        );

        // No `{path}` placeholder -> path appended as final argument.
        assert_eq!(
            custom_file_opener_command(
                "open -a \"Visual Studio Code\"",
                &target_plain,
                Platform::Macos
            )
            .unwrap(),
            vec!["open", "-a", "Visual Studio Code", "/work/src/links.rs"]
        );

        // Shell metacharacters / spaces in filename stay inside a single arg.
        let scary = FileTarget {
            path: PathBuf::from("/tmp/a $(touch pwned); rm -rf .txt"),
            line: Some(3),
            col: None,
        };
        let argv =
            custom_file_opener_command("zed {path}:{line}:{col}", &scary, Platform::Macos).unwrap();
        assert_eq!(argv.len(), 2);
        assert_eq!(argv[0], "zed");
        assert_eq!(argv[1], "/tmp/a $(touch pwned); rm -rf .txt:3");
    }

    #[test]
    fn guardrails_plan_reveal_for_executables_and_bundles() {
        let app = FileTarget {
            path: PathBuf::from("/Applications/Calculator.app"),
            line: None,
            col: None,
        };
        let script = FileTarget {
            path: PathBuf::from("/work/scripts/release.sh"),
            line: Some(12),
            col: None,
        };
        let doc = FileTarget {
            path: PathBuf::from("/work/README.md"),
            line: None,
            col: None,
        };

        // Bundles ALWAYS reveal, even with a custom `file_opener`.
        assert_eq!(
            plan_file_open(&app, "", true, false, Platform::Macos),
            Some((
                FileAction::Reveal,
                vec![
                    "open".into(),
                    "-R".into(),
                    "/Applications/Calculator.app".into()
                ]
            ))
        );
        assert_eq!(
            plan_file_open(&app, "code -g {path}", true, false, Platform::Macos),
            Some((
                FileAction::Reveal,
                vec![
                    "open".into(),
                    "-R".into(),
                    "/Applications/Calculator.app".into()
                ]
            ))
        );

        // Executable regular file: reveals under OS default, opens in custom editor.
        assert_eq!(
            plan_file_open(&script, "", false, true, Platform::Macos),
            Some((
                FileAction::Reveal,
                vec![
                    "open".into(),
                    "-R".into(),
                    "/work/scripts/release.sh".into()
                ]
            ))
        );
        assert_eq!(
            plan_file_open(&script, "", false, true, Platform::Linux),
            Some((
                FileAction::Reveal,
                vec!["xdg-open".into(), "/work/scripts".into()]
            ))
        );
        assert_eq!(
            plan_file_open(
                &script,
                "code -g {path}:{line}",
                false,
                true,
                Platform::Macos
            ),
            Some((
                FileAction::Open,
                vec![
                    "code".into(),
                    "-g".into(),
                    "/work/scripts/release.sh:12".into()
                ]
            ))
        );

        // Safe non-executable doc opens directly under OS default.
        assert_eq!(
            plan_file_open(&doc, "", false, false, Platform::Macos),
            Some((
                FileAction::Open,
                vec!["open".into(), "/work/README.md".into()]
            ))
        );
        assert_eq!(
            plan_file_open(&doc, "", false, false, Platform::Linux),
            Some((
                FileAction::Open,
                vec!["xdg-open".into(), "/work/README.md".into()]
            ))
        );

        // Windows: `.js` is dangerous there, safe on macOS/Linux.
        assert!(is_bundle_or_launcher_ext("js", Platform::Windows));
        assert!(!is_bundle_or_launcher_ext("js", Platform::Macos));
        assert!(!is_bundle_or_launcher_ext("js", Platform::Linux));
        let win_exe = FileTarget {
            path: PathBuf::from(r"C:\Users\me\tool.exe"),
            line: None,
            col: None,
        };
        assert_eq!(
            plan_file_open(&win_exe, "", true, false, Platform::Windows),
            Some((
                FileAction::Reveal,
                vec![
                    "explorer.exe".into(),
                    r"/select,C:\Users\me\tool.exe".into()
                ]
            ))
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_bundle_or_executable_is_caught_by_canonical_check() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = std::env::temp_dir().join(format!(
            "cometty-links-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // 1. Symlink `harmless.txt -> Fake.app` (a directory bundle).
        let app_dir = dir.join("Fake.app");
        std::fs::create_dir_all(&app_dir).unwrap();
        let link_to_app = dir.join("harmless.txt");
        symlink(&app_dir, &link_to_app).unwrap();
        let meta = std::fs::metadata(&link_to_app).unwrap();
        let canon = std::fs::canonicalize(&link_to_app).ok();
        let (is_bundle, _) =
            classify_local_file(&link_to_app, canon.as_deref(), &meta, Platform::Macos);
        assert!(
            is_bundle,
            "symlink to .app bundle must trigger bundle guardrail"
        );

        // 2. Symlink `notes.md -> exec_bin` (+x regular file).
        let exec_bin = dir.join("exec_bin");
        std::fs::write(&exec_bin, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&exec_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link_to_exec = dir.join("notes.md");
        symlink(&exec_bin, &link_to_exec).unwrap();
        let meta = std::fs::metadata(&link_to_exec).unwrap();
        let canon = std::fs::canonicalize(&link_to_exec).ok();
        let (_, is_exec) =
            classify_local_file(&link_to_exec, canon.as_deref(), &meta, Platform::Macos);
        assert!(
            is_exec,
            "symlink to +x binary must trigger executable guardrail"
        );

        // 3. Regular non-executable file passes both guardrails.
        let plain = dir.join("plain.txt");
        std::fs::write(&plain, b"hello").unwrap();
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
        let meta = std::fs::metadata(&plain).unwrap();
        let canon = std::fs::canonicalize(&plain).ok();
        let (is_bundle, is_exec) =
            classify_local_file(&plain, canon.as_deref(), &meta, Platform::Macos);
        assert!(!is_bundle && !is_exec);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn real_fs_detection_hint_view_and_open_guards() {
        let dir = std::env::temp_dir().join(format!(
            "cometty-links-fs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let sub = dir.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        let file = sub.join("hello.rs");
        std::fs::write(&file, b"fn main() {}\n").unwrap();

        let dir_str = dir.to_str().unwrap();
        let file_uri = format!("file://localhost{}", file.to_str().unwrap());
        let local_host = crate::procinfo::hostname();
        let ctx = LinkContext {
            detect_urls: true,
            open_files: true,
            detect_paths: true,
            cwd: Some(dir_str),
            home: None,
            local_host: local_host.as_deref(),
            platform: platform(),
        };

        let line = format!("see src/hello.rs:1:4 and missing/nope.rs and {file_uri}");
        let row = cells(&line);
        let found = detect_links_in_row(&row, &ctx);
        assert_eq!(found.len(), 2);
        assert_eq!(
            found[0].target,
            LinkTarget::File(FileTarget {
                path: file.clone(),
                line: Some(1),
                col: Some(4),
            })
        );
        assert_eq!(
            found[1].target,
            LinkTarget::File(FileTarget {
                path: file.clone(),
                line: None,
                col: None,
            })
        );
        assert_eq!(
            detected_link_at(&row, 5, &ctx),
            Some(found[0].target.clone())
        );
        assert_eq!(detected_link_at(&row, 26, &ctx), None);

        // `HintView::build_with` uses the real filesystem probe cleanly.
        let mut g =
            crate::grid::Grid::new(line.chars().count() + 4, 1, crate::theme::Theme::default());
        for ch in line.chars() {
            g.put_char(ch);
        }
        let view = HintView::build_with(&g, &ctx);
        assert_eq!(view.rows[0].len(), 2);

        // `open_target` enforces `open_files = false` and missing-file checks
        // before spawning any external process.
        let disabled = LinksConfig {
            open_files: false,
            ..LinksConfig::default()
        };
        assert!(matches!(
            open_target(&found[0].target, &disabled),
            Err(OpenError::Rejected)
        ));
        let missing = LinkTarget::File(FileTarget {
            path: dir.join("does-not-exist.rs"),
            line: None,
            col: None,
        });
        assert!(matches!(
            open_target(&missing, &LinksConfig::default()),
            Err(OpenError::NotFound)
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
