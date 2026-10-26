mod parser;

use base64::Engine as _;
use parser::BoundedParser;
use vte::Params;

use crate::grid::{CursorShape, CursorStyle, Grid, MouseMode};
use crate::theme::Theme;

/// Clipboard request decoded from `OSC 52`. `Terminal` never touches the
/// system clipboard itself; the app drains these after each feed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardOp {
    /// `OSC 52 ; target ; base64` — write the decoded text.
    Set(String),
    /// `OSC 52 ; target ; ?` — reply with clipboard contents when allowed.
    Query,
}

/// Largest decoded `OSC 52` payload accepted, in bytes. Anything bigger is
/// logged and dropped (clipboard writes are not a bulk transfer channel).
pub const MAX_CLIPBOARD_BYTES: usize = 1 << 20;

pub struct Terminal {
    grid: Grid,
    parser: BoundedParser,
    // Pending wrap: true when cursor is past right margin and next printable should wrap first.
    pending_wrap: bool,
    // Last OSC 0/1/2 title set by the shell; empty when none seen.
    // Drives tab-bar labels (falls back to `Tab N`).
    title: String,
    // Last OSC 7 working directory set by the shell, when it reports one.
    // Fallback source for the `$cwd` label variable.
    cwd: Option<String>,
    // Monotonic count of OSC 7 reports, so the tab label can tell a live
    // report (a running job's own directory) from one left behind by a
    // finished job.
    cwd_version: u64,
    max_title_chars: usize,
    // Last graphic character emitted via `print`, for `REP` (`CSI n b`).
    // Control sequences never clear it; only a new graphic updates it.
    last_graphic: Option<char>,
    // Bytes queued for the PTY (DA / CPR / DSR / DECRQM replies).
    // Drained by `take_response` in the app's PTY loop.
    pending_response: Vec<u8>,
    // Clipboard requests queued by OSC 52, drained by the app (which owns
    // the arboard handle and the OSC 52 policy).
    pending_clipboard: Vec<ClipboardOp>,
}

/// OSC numbers cometty interprets today (window/icon title + cwd);
/// everything else is logged and ignored.
const TITLE_OSC: [&[u8]; 3] = [b"0", b"1", b"2"];
/// Every OSC number cometty acts on; the rest are marked unhandled in logs.
const HANDLED_OSC: [&[u8]; 6] = [b"0", b"1", b"2", b"7", b"8", b"52"];
/// Payload characters shown in an OSC debug line before truncating: OSC 52
/// base64 blobs and OSC 8 URLs can be kilobytes long.
const OSC_SUMMARY_CHARS: usize = 120;

/// Borrow the OSC number and split payload. Handlers stream the fragments;
/// ignored sequences never materialize their payload. Preserve
/// the distinction between a bare number and an explicitly empty payload.
struct Osc<'a> {
    number: &'a [u8],
    payload: Option<OscPayload<'a>>,
}

#[derive(Clone, Copy)]
struct OscPayload<'a> {
    first: &'a [u8],
    rest: &'a [&'a [u8]],
}

impl<'a> Osc<'a> {
    fn parse(params: &'a [&'a [u8]]) -> Option<Self> {
        let (number, payload) = match params {
            [] => return None,
            [single] => match single.iter().position(|&b| b == b';') {
                Some(i) => (
                    &single[..i],
                    Some(OscPayload {
                        first: &single[i + 1..],
                        rest: &[],
                    }),
                ),
                None => (*single, None),
            },
            [number, first, rest @ ..] => (*number, Some(OscPayload { first, rest })),
        };
        Some(Self { number, payload })
    }
}

impl<'a> OscPayload<'a> {
    fn bytes(&self) -> impl Iterator<Item = u8> + '_ {
        self.first.iter().copied().chain(
            self.rest
                .iter()
                .flat_map(|piece| std::iter::once(b';').chain(piece.iter().copied())),
        )
    }

    fn len(&self) -> usize {
        self.rest
            .iter()
            .fold(self.first.len(), |len, piece| len + 1 + piece.len())
    }

    /// Split at the first payload separator without joining large fragments.
    fn split_once(self) -> Option<(&'a [u8], Self)> {
        if let Some(i) = self.first.iter().position(|&b| b == b';') {
            return Some((
                &self.first[..i],
                Self {
                    first: &self.first[i + 1..],
                    rest: self.rest,
                },
            ));
        }
        let (first, rest) = self.rest.split_first()?;
        Some((self.first, Self { first, rest }))
    }

    fn chars(&self) -> impl Iterator<Item = char> + '_ {
        // Fragments have an ASCII ';' between them, so no valid UTF-8 scalar
        // crosses a fragment boundary. Lossy decoding each piece is equivalent
        // to decoding the joined bytes, without allocating that joined text.
        lossy_chars(self.first).chain(
            self.rest
                .iter()
                .flat_map(|piece| std::iter::once(';').chain(lossy_chars(piece))),
        )
    }
}

fn lossy_chars(bytes: &[u8]) -> impl Iterator<Item = char> + '_ {
    bytes.utf8_chunks().flat_map(|chunk| {
        chunk
            .valid()
            .chars()
            .chain((!chunk.invalid().is_empty()).then_some('\u{fffd}'))
    })
}

/// Extract an `OSC 0/1/2` window-title update from vte's split params.
///
/// Embedded control characters (newline, BEL, ESC, …) are stripped: titles
/// flow into the tab bar and the OS window title, where controls could break
/// layout or smuggle sequences.
fn title_payload(payload: OscPayload<'_>, max_chars: usize) -> String {
    let mut chars = payload
        .chars()
        .filter(|c| !c.is_control())
        .skip_while(|c| c.is_whitespace());
    let mut title: String = chars.by_ref().take(max_chars.max(1)).collect();
    // Trim *before* truncation, as before. A whitespace suffix of our prefix
    // stays if a non-space occurs later in the original title; otherwise it
    // really was trailing whitespace and should be removed.
    if title.ends_with(char::is_whitespace) && !chars.any(|c| !c.is_whitespace()) {
        title.truncate(title.trim_end().len());
    }
    title
}

/// Extract an `OSC 7` working-directory update (`file://host/path`).
///
/// The authority is ignored (empty, `localhost`, or a remote host name —
/// cometty only ever shows a local shell's directory). Non-`file` schemes
/// and relative paths are rejected.
fn cwd_payload(payload: OscPayload<'_>, max_chars: usize) -> Option<String> {
    let mut chars = payload.chars();
    if !chars.by_ref().take(7).eq("file://".chars()) {
        return None;
    }
    chars.find(|&c| c == '/')?;
    let path_bytes = std::iter::once('/').chain(chars).flat_map(|c| {
        let mut bytes = [0; 4];
        let len = c.encode_utf8(&mut bytes).len();
        bytes.into_iter().take(len)
    });
    let max_chars = max_chars.max(1);
    // Four decoded bytes per retained scalar suffice, even with invalid UTF-8.
    // Never decode/copy a megabyte-long suffix to keep a 256-character label.
    let decoded: Vec<u8> = percent_decoded(path_bytes)
        .take(max_chars.saturating_mul(4))
        .collect();
    Some(
        String::from_utf8_lossy(&decoded)
            .chars()
            .take(max_chars)
            .collect(),
    )
}

/// Decode `%XX` escapes (paths with spaces, non-UTF-8 bytes, …). Invalid
/// escapes are kept literally.
fn percent_decoded(bytes: impl Iterator<Item = u8>) -> impl Iterator<Item = u8> {
    let mut bytes = bytes.peekable();
    let mut pending = None;
    std::iter::from_fn(move || {
        if let Some(byte) = pending.take() {
            return Some(byte);
        }
        let byte = bytes.next()?;
        if byte == b'%' {
            let hex = |b: &u8| (*b as char).to_digit(16);
            if let Some(hi) = bytes.peek().and_then(hex) {
                let first = bytes.next().unwrap();
                if let Some(lo) = bytes.peek().and_then(hex) {
                    bytes.next();
                    return Some((hi * 16 + lo) as u8);
                }
                // The first hex digit is literal, but leave the next byte
                // unconsumed: it could itself begin another `%XX` escape.
                pending = Some(first);
            }
        }
        Some(byte)
    })
}

#[derive(Debug, PartialEq, Eq)]
enum HyperlinkOp {
    End,
    Set(String),
    TooLong(usize),
}

/// Extract an `OSC 8` hyperlink update: `params;URI`.
///
/// Empty/oversized URIs end the current link; a missing `;` is malformed and
/// ignored. The `id=` params are not interpreted: only the URI identifies it.
fn hyperlink_payload(payload: OscPayload<'_>) -> Option<HyperlinkOp> {
    let (_, uri) = payload.split_once()?;
    let len = uri.len();
    if len == 0 {
        return Some(HyperlinkOp::End);
    }
    if len > crate::grid::MAX_HYPERLINK_URI {
        return Some(HyperlinkOp::TooLong(len));
    }
    // Lossy UTF-8 can expand bytes. Check the final size before allocating it.
    let text_len = uri.chars().map(char::len_utf8).sum();
    if text_len > crate::grid::MAX_HYPERLINK_URI {
        return Some(HyperlinkOp::TooLong(text_len));
    }
    let mut text = String::with_capacity(text_len);
    text.extend(uri.chars());
    Some(HyperlinkOp::Set(text))
}

/// Extract an `OSC 52` clipboard operation: `target;payload`.
///
/// Targets: `c` (clipboard), `p` (primary), `s` (selection), `0..7` (cut
/// buffers) and empty all map to the system clipboard; unknown targets are
/// ignored. `?` is a read request; anything else must be base64 text.
/// Invalid base64 or text exceeding `max_bytes` (including UTF-8 replacement
/// expansion) is dropped with a size-only warning.
fn clipboard_payload(payload: OscPayload<'_>, max_bytes: usize) -> Option<ClipboardOp> {
    let (target, data) = payload.split_once()?;
    let known_target = target.is_empty()
        || target == b"c"
        || target == b"p"
        || target == b"s"
        || (target.len() == 1 && (b'0'..=b'7').contains(&target[0]));
    if !known_target {
        log::debug!("OSC 52 unknown target ignored ({} bytes)", target.len());
        return None;
    }
    if data.first == b"?" && data.rest.is_empty() {
        return Some(ClipboardOp::Query);
    }
    // tmux wraps long base64 payloads with newlines; whitespace is not data.
    let encoded_len = data.bytes().filter(|b| !b.is_ascii_whitespace()).count();
    if encoded_len == 0 {
        return Some(ClipboardOp::Set(String::new()));
    }
    // Bound the decode: 4 base64 chars per 3 bytes (plus padding).
    if encoded_len > max_bytes.div_ceil(3).saturating_mul(4) {
        log::warn!(
            "OSC 52 payload ignored: {} bytes encoded (limit {max_bytes})",
            encoded_len
        );
        return None;
    }
    let mut filtered = Vec::with_capacity(encoded_len);
    filtered.extend(data.bytes().filter(|b| !b.is_ascii_whitespace()));
    let engine = base64::engine::general_purpose::STANDARD;
    let decoded = match engine
        .decode(&filtered)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&filtered))
    {
        Ok(decoded) => decoded,
        Err(e) => {
            log::warn!("OSC 52 payload is not valid base64 ({e})");
            return None;
        }
    };
    if decoded.len() > max_bytes {
        log::warn!(
            "OSC 52 payload ignored: {} bytes decoded (limit {max_bytes})",
            decoded.len()
        );
        return None;
    }
    let text = match String::from_utf8(decoded) {
        Ok(text) => text, // Reuse the decode allocation for valid text.
        Err(error) => {
            let text_len: usize = lossy_chars(error.as_bytes()).map(char::len_utf8).sum();
            if text_len > max_bytes {
                log::warn!(
                    "OSC 52 payload ignored: {text_len} bytes after UTF-8 conversion (limit {max_bytes})"
                );
                return None;
            }
            String::from_utf8_lossy(error.as_bytes()).into_owned()
        }
    };
    Some(ClipboardOp::Set(text))
}

/// `OSC 52` reply carrying clipboard `text` (ST-terminated, `c` target).
pub fn osc52_reply(text: &str) -> Vec<u8> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let mut out = Vec::with_capacity(encoded.len() + 8);
    out.extend_from_slice(b"\x1b]52;c;");
    out.extend_from_slice(encoded.as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

/// Well-known OSC numbers, for readable log lines. Numbers cometty does
/// not interpret are still named here when the meaning is standard.
fn osc_name(num: &[u8]) -> Option<&'static str> {
    Some(match num {
        b"0" | b"1" | b"2" => "title",
        b"4" => "palette",
        b"7" => "cwd",
        b"8" => "hyperlink",
        b"9" | b"777" => "notification",
        b"10" | b"11" | b"12" => "colors",
        b"52" => "clipboard",
        b"104" => "reset colors",
        b"133" => "prompt marks",
        b"1337" => "iTerm2",
        _ => return None,
    })
}

/// One-line rendering of a detected OSC sequence for the log panel, e.g.
/// `2 (title): nvim | ~/dev` or `133 (prompt marks, unhandled): A`.
/// Long payloads are truncated and control bytes escaped so a record stays
/// a single, readable line.
fn decoded_osc_summary(osc: &Osc<'_>) -> String {
    let num = osc.number;
    let mut summary = String::new();
    // Even a malformed, megabyte-long OSC number must have a bounded log.
    append_osc_preview(&mut summary, num.iter().copied(), num.len());
    summary.push_str(" (");
    summary.push_str(osc_name(num).unwrap_or("unknown"));
    if !HANDLED_OSC.contains(&num) {
        summary.push_str(", unhandled");
    }
    summary.push(')');
    let Some(payload) = osc.payload else {
        return summary;
    };
    summary.push_str(": ");
    // Clipboard payloads are private: never echo their contents.
    if num == b"52" {
        summary.push_str(&format!("<redacted, {} bytes>", payload.len()));
        return summary;
    }
    append_osc_preview(&mut summary, payload.bytes(), payload.len());
    summary
}

fn append_osc_preview(summary: &mut String, bytes: impl Iterator<Item = u8>, len: usize) {
    // Four bytes per scalar, plus one scalar to detect truncation. UTF-8
    // split across vte fragments remains intact in this bounded prefix.
    let prefix: Vec<u8> = bytes.take((OSC_SUMMARY_CHARS + 1) * 4).collect();
    let text = String::from_utf8_lossy(&prefix);
    for (shown, ch) in text.chars().enumerate() {
        if shown == OSC_SUMMARY_CHARS {
            summary.push_str(&format!("… ({len} bytes)"));
            break;
        }
        // Keep one line: control bytes (BEL, ESC, CR) become escapes.
        if ch.is_control() {
            summary.extend(ch.escape_debug());
        } else {
            summary.push(ch);
        }
    }
}

impl Terminal {
    #[allow(dead_code)]
    pub fn new(cols: usize, rows: usize, theme: Theme) -> Self {
        Self::new_with_config(cols, rows, theme, &crate::config::Config::default())
    }

    pub fn new_with_config(
        cols: usize,
        rows: usize,
        theme: Theme,
        config: &crate::config::Config,
    ) -> Self {
        Self {
            grid: Grid::new_with_config(cols, rows, theme, config),
            parser: BoundedParser::new(),
            pending_wrap: false,
            title: String::new(),
            cwd: None,
            cwd_version: 0,
            max_title_chars: config.terminal.max_title_chars.max(1),
            last_graphic: None,
            pending_response: Vec::new(),
            pending_clipboard: Vec::new(),
        }
    }

    /// Last `OSC 0/1/2` title reported by the shell, or empty.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Last `OSC 7` working directory reported by the shell, when any.
    /// Source for the tab label's `$cwd` alongside the kernel-reported
    /// shell cwd (see `app::tab::preferred_cwd`).
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// Monotonic count of `OSC 7` reports; see [`Self::cwd`].
    pub fn cwd_version(&self) -> u64 {
        self.cwd_version
    }

    #[allow(dead_code)]
    pub fn theme(&self) -> Theme {
        self.grid.theme()
    }

    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    pub fn cols(&self) -> usize {
        self.grid.cols()
    }

    pub fn rows(&self) -> usize {
        self.grid.rows()
    }

    pub fn cursor_visible(&self) -> bool {
        self.grid.cursor_enabled()
    }

    pub fn cursor_style(&self) -> CursorStyle {
        self.grid.cursor_style()
    }

    #[allow(dead_code)]
    pub fn bracketed_paste(&self) -> bool {
        self.grid.bracketed_paste()
    }

    pub fn cursor_app_mode(&self) -> bool {
        self.grid.cursor_app_mode()
    }

    pub fn keypad_app_mode(&self) -> bool {
        self.grid.keypad_app_mode()
    }

    pub fn mouse_mode(&self) -> MouseMode {
        self.grid.mouse_mode()
    }

    pub fn mouse_sgr(&self) -> bool {
        self.grid.mouse_sgr()
    }

    pub fn focus_report(&self) -> bool {
        self.grid.focus_report()
    }

    pub fn in_sync(&self) -> bool {
        self.grid.in_sync()
    }

    #[allow(dead_code)]
    pub fn is_alt(&self) -> bool {
        self.grid.is_alt()
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.grid.resize(cols, rows);
        self.pending_wrap = false;
        self.last_graphic = None;
    }

    /// History plus primary screen as replayable VT text; see
    /// [`Grid::dump_primary`].
    pub fn dump_scrollback(&self) -> Vec<u8> {
        self.grid.dump_primary()
    }

    /// Feed a saved dump into a fresh terminal. It is not program output:
    /// any reply or clipboard request it provokes is dropped (a tampered
    /// file must not type into the shell), and an unterminated string or
    /// open style/link ends with it.
    pub fn replay(&mut self, dump: &[u8]) {
        self.feed(dump);
        self.feed(b"\x1b\\\x1b[0m\x1b]8;;\x1b\\");
        self.pending_response = Vec::new();
        self.pending_clipboard = Vec::new();
    }

    /// Take queued PTY reply bytes (DA / CPR / DSR / DECRQM).
    /// The app writes the result back via `PtySession::write`.
    pub fn take_response(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending_response)
    }

    /// Take queued `OSC 52` clipboard requests. The app owns the system
    /// clipboard and the read/write policy, so the terminal only queues.
    pub fn take_clipboard(&mut self) -> Vec<ClipboardOp> {
        std::mem::take(&mut self.pending_clipboard)
    }

    fn push_response(&mut self, bytes: &[u8]) {
        self.pending_response.extend_from_slice(bytes);
    }

    /// Cursor position report values, 1-based. With origin mode the row is
    /// relative to the scroll-region top (matching `H` / `d` handling).
    fn cpr_position(&self) -> (usize, usize) {
        let cursor = self.grid.cursor();
        let row = if self.grid.origin_mode() {
            cursor
                .y
                .saturating_sub(self.grid.scroll_region().0)
                .saturating_add(1)
        } else {
            cursor.y.saturating_add(1)
        };
        (row, cursor.x.saturating_add(1))
    }

    /// Live-apply settings changes to an existing session: theme plus
    /// terminal tuning. Shell/cwd/term intentionally stay with the
    /// spawned process (new tabs pick those up).
    pub fn apply_config(&mut self, theme: Theme, config: &crate::config::Config) {
        self.grid.set_theme(theme);
        self.grid.apply_terminal_config(&config.terminal);
        self.max_title_chars = config.terminal.max_title_chars.max(1);
        let keep = self.title.chars().count().min(self.max_title_chars);
        if keep < self.title.chars().count() {
            self.title = self.title.chars().take(keep).collect();
        }
        if let Some(cwd) = self.cwd.as_ref()
            && cwd.chars().count() > self.max_title_chars
        {
            self.cwd = Some(cwd.chars().take(self.max_title_chars).collect());
        }
    }

    pub fn scroll_offset(&self) -> usize {
        self.grid.scroll_offset()
    }

    pub fn scroll_by(&mut self, delta: isize) -> bool {
        let changed = self.grid.scroll_by(delta);
        if changed {
            self.pending_wrap = false;
        }
        changed
    }

    pub fn scroll_to_top(&mut self) -> bool {
        self.grid.scroll_to_top()
    }

    pub fn scroll_to_bottom(&mut self) -> bool {
        self.grid.scroll_to_bottom()
    }

    pub fn scroll_to_offset(&mut self, offset: usize) -> bool {
        let changed = self.grid.scroll_to_offset(offset);
        if changed {
            self.pending_wrap = false;
        }
        changed
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        // vte::Parser::advance borrows both parser and performer; swap parser
        // out to satisfy the borrow checker while preserving state.
        let mut parser = std::mem::replace(&mut self.parser, BoundedParser::new());
        parser.advance(self, bytes);
        self.parser = parser;
    }

    fn param_at(params: &Params, idx: usize, default: u16) -> u16 {
        let mut i = 0;
        for group in params.iter() {
            for &v in group.iter() {
                if i == idx {
                    return if v == 0 { default } else { v };
                }
                i += 1;
            }
        }
        default
    }

    fn params_iter(params: &Params) -> impl Iterator<Item = u16> + '_ {
        params.iter().flat_map(|p| p.iter().copied())
    }

    fn params_is_empty(params: &Params) -> bool {
        Self::params_iter(params).next().is_none()
    }

    fn set_private_mode(&mut self, mode: u16, set: bool) {
        match mode {
            1 => self.grid.set_cursor_app_mode(set),
            4 => self.grid.set_insert_mode(set),
            6 => self.grid.set_origin_mode(set),
            7 => {
                self.grid.set_auto_wrap(set);
                if !set {
                    self.pending_wrap = false;
                }
            }
            25 => self.grid.set_cursor_enabled(set),
            66 => self.grid.set_keypad_app_mode(set),
            1000 => self.grid.set_mouse_press(set),
            1002 => self.grid.set_mouse_drag(set),
            1003 => self.grid.set_mouse_any(set),
            1004 => self.grid.set_focus_report(set),
            1006 => self.grid.set_mouse_sgr(set),
            2004 => self.grid.set_bracketed_paste(set),
            2026 => {
                if set {
                    self.grid.sync_begin();
                } else {
                    self.grid.sync_end();
                }
            }
            47 | 1047 => {
                if set {
                    self.grid.enter_alt(false);
                } else {
                    self.grid.exit_alt();
                }
                self.pending_wrap = false;
            }
            1048 => {
                if set {
                    self.grid.save_cursor();
                } else {
                    self.grid.restore_cursor();
                }
            }
            1049 => {
                if set {
                    self.grid.save_cursor();
                    self.grid.enter_alt(true);
                } else {
                    self.grid.exit_alt();
                    self.grid.restore_cursor();
                }
                self.pending_wrap = false;
            }
            _ => {}
        }
    }

    fn set_cursor_style(&mut self, ps: u16) {
        let style = match ps {
            0 | 1 => CursorStyle {
                shape: CursorShape::Block,
                blinking: true,
            },
            2 => CursorStyle {
                shape: CursorShape::Block,
                blinking: false,
            },
            3 => CursorStyle {
                shape: CursorShape::Underline,
                blinking: true,
            },
            4 => CursorStyle {
                shape: CursorShape::Underline,
                blinking: false,
            },
            5 => CursorStyle {
                shape: CursorShape::Bar,
                blinking: true,
            },
            6 => CursorStyle {
                shape: CursorShape::Bar,
                blinking: false,
            },
            _ => return,
        };
        self.grid.set_cursor_style(style);
    }
}

impl vte::Perform for Terminal {
    fn print(&mut self, c: char) {
        // DECAWM off: no pending wrap; Grid clamps to the last column.
        if !self.grid.auto_wrap() {
            self.pending_wrap = false;
        }
        // Pending wrap clamps the cursor onto the last cell: a combining /
        // ZWJ continuation must attach there, not wrap and not attach one
        // cell back. Preserve pending state for the next advancing char.
        if self.pending_wrap {
            let cur = self.grid.cursor();
            if self.grid.append_to_cell(cur.x, cur.y, c) {
                return;
            }
            self.grid.newline();
            self.pending_wrap = false;
            // Fall through to write `c` fresh (it wasn't a continuation of
            // the margin cell). Combining at a fresh line start attaches via
            // the normal path below if applicable.
        }
        // Combining / ZWJ / flag continuations attach to the previous
        // cluster without wrapping or moving the cursor.
        if self.grid.append_to_prev(c) {
            return;
        }
        if c != '\0' && !crate::grid::unicode::is_joining_modifier(c) {
            self.last_graphic = Some(c);
        }
        if !self.pending_wrap {
            // Pending case already newlined above; otherwise resolve an
            // overfull cursor (e.g. after a direct grid write).
            self.grid.handle_wrap_if_needed();
        }
        // After newline, cursor.x is 0.
        self.grid.put_fresh_char(c);
        if self.grid.auto_wrap() && self.grid.cursor().x >= self.grid.cols() {
            self.pending_wrap = true;
            // Clamp visual cursor to last column until wrap resolves.
            let y = self.grid.cursor().y;
            self.grid.set_cursor(self.grid.cols().saturating_sub(1), y);
            // put_char already advanced x to cols; set_cursor above fixes it.
            // Re-advance pending state: next print will newline.
            // But set_cursor cleared the >=cols condition; keep pending_wrap.
            // Note: set_cursor snaps off continuations, so a wide lead at
            // the margin keeps the visual cursor on its lead cell.
        } else if !self.grid.auto_wrap() {
            self.pending_wrap = false;
        }
    }

    fn execute(&mut self, byte: u8) {
        self.pending_wrap = false;
        match byte {
            b'\n' | 0x0B | 0x0C => self.grid.newline(),
            b'\r' => self.grid.carriage_return(),
            0x08 => self.grid.backspace(),
            b'\t' => self.grid.tab(),
            0x07 => {} // bell: ignore for v1
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        if !intermediates.is_empty() {
            if intermediates == [b'?'] && (action == 'h' || action == 'l') {
                let set = action == 'h';
                self.pending_wrap = false;
                for mode in Self::params_iter(params) {
                    self.set_private_mode(mode, set);
                }
            } else if intermediates == [b' '] && action == 'q' {
                // DECSCUSR: `CSI Ps SP q` cursor shape + blink.
                self.pending_wrap = false;
                self.set_cursor_style(Self::param_at(params, 0, 0));
            } else if intermediates == [b'?'] && action == 'n' {
                // DECXCPR: `CSI ? 6 n` extended cursor position report.
                self.pending_wrap = false;
                if Self::params_iter(params).next().unwrap_or(0) == 6 {
                    let (row, col) = self.cpr_position();
                    let reply = format!("\x1b[?{row};{col}R");
                    self.push_response(reply.as_bytes());
                }
            } else if intermediates == [b'$'] && action == 'p' {
                // DECRQM (ANSI): `CSI Ps $ p`.
                self.pending_wrap = false;
                let mode = Self::params_iter(params).next().unwrap_or(0);
                let val = self.grid.query_ansi_mode(mode);
                let reply = format!("\x1b[{mode};{val}$y");
                self.push_response(reply.as_bytes());
            } else if intermediates == [b'?', b'$'] && action == 'p' {
                // DECRQM (private): `CSI ? Ps $ p` stub.
                self.pending_wrap = false;
                let mode = Self::params_iter(params).next().unwrap_or(0);
                let val = self.grid.query_private_mode(mode);
                let reply = format!("\x1b[?{mode};{val}$y");
                self.push_response(reply.as_bytes());
            } else if intermediates == [b'>'] && action == 'c' {
                // Secondary DA: report a minimal xterm-like identity.
                self.pending_wrap = false;
                self.push_response(b"\x1b[>0;95;0c");
            }
            return;
        }
        self.pending_wrap = false;
        match action {
            'h' | 'l' => {
                // SM / RM without `?`: only IRM (4) is supported.
                let set = action == 'h';
                for mode in Self::params_iter(params) {
                    if mode == 4 {
                        self.grid.set_insert_mode(set);
                    }
                }
            }
            'r' => {
                // DECSTBM: `CSI top;bottom r` (1-based). Empty = full reset.
                if Self::params_is_empty(params) {
                    self.grid.reset_scroll_region();
                    self.grid.home_cursor();
                } else {
                    let rows = self.grid.rows();
                    let top = Self::param_at(params, 0, 1);
                    let bottom = Self::param_at(params, 1, rows.max(1) as u16);
                    let top0 = top.saturating_sub(1) as usize;
                    let bottom0 = bottom.saturating_sub(1) as usize;
                    // Invalid (`top >= bottom`) is ignored.
                    self.grid.set_scroll_region(top0, bottom0);
                }
            }
            'A' => {
                let n = Self::param_at(params, 0, 1) as isize;
                self.grid.move_cursor(0, -n);
            }
            'B' | 'e' => {
                let n = Self::param_at(params, 0, 1) as isize;
                self.grid.move_cursor(0, n);
            }
            'C' | 'a' => {
                let n = Self::param_at(params, 0, 1) as isize;
                self.grid.move_cursor(n, 0);
            }
            'D' => {
                let n = Self::param_at(params, 0, 1) as isize;
                self.grid.move_cursor(-n, 0);
            }
            'E' => {
                let n = Self::param_at(params, 0, 1) as isize;
                let y = self.grid.cursor().y as isize + n;
                let y = y.clamp(0, self.grid.rows().saturating_sub(1) as isize) as usize;
                self.grid.set_cursor(0, y);
            }
            'F' => {
                let n = Self::param_at(params, 0, 1) as isize;
                let y = self.grid.cursor().y as isize - n;
                let y = y.clamp(0, self.grid.rows().saturating_sub(1) as isize) as usize;
                self.grid.set_cursor(0, y);
            }
            'G' | '`' => {
                let col = Self::param_at(params, 0, 1).saturating_sub(1) as usize;
                let y = self.grid.cursor().y;
                self.grid.set_cursor(col, y);
            }
            'd' => {
                let mut row = Self::param_at(params, 0, 1).saturating_sub(1) as usize;
                if self.grid.origin_mode() {
                    row = row.saturating_add(self.grid.scroll_region().0);
                }
                let x = self.grid.cursor().x;
                self.grid.set_cursor(x, row);
            }
            'H' | 'f' => {
                let mut row = Self::param_at(params, 0, 1).saturating_sub(1) as usize;
                let col = Self::param_at(params, 1, 1).saturating_sub(1) as usize;
                if self.grid.origin_mode() {
                    row = row.saturating_add(self.grid.scroll_region().0);
                }
                self.grid.set_cursor(col, row);
            }
            'J' => {
                let mode = Self::param_at(params, 0, 0);
                self.grid.erase_in_display(mode);
            }
            'K' => {
                let mode = Self::param_at(params, 0, 0);
                self.grid.erase_in_line(mode);
            }
            'm' => {
                let groups: Vec<Vec<u16>> = params.iter().map(|p| p.to_vec()).collect();
                self.grid.sgr_groups(&groups);
            }
            's' => self.grid.save_cursor(),
            'u' => self.grid.restore_cursor(),
            '@' => {
                // ICH: insert blanks at the cursor, shifting the row right.
                let n = Self::param_at(params, 0, 1) as usize;
                self.grid.insert_chars(n);
            }
            'P' => {
                // DCH: delete chars at the cursor, shifting the row left.
                let n = Self::param_at(params, 0, 1) as usize;
                self.grid.delete_chars(n);
            }
            'X' => {
                // ECH: erase chars at the cursor without shifting.
                let n = Self::param_at(params, 0, 1) as usize;
                self.grid.erase_chars(n);
            }
            'b' => {
                // REP: repeat the last graphic character.
                let n = Self::param_at(params, 0, 1) as usize;
                let n = n.clamp(1, 16384);
                if let Some(c) = self.last_graphic {
                    // Route through `print` so wrap / wide / insert-mode
                    // handling matches a normally typed character.
                    // `last_graphic` stays `c`, so chained REPs repeat it.
                    for _ in 0..n {
                        self.print(c);
                    }
                    // `print` may leave a pending wrap behind; keep it so
                    // a following char wraps exactly like typed output.
                }
            }
            'c' => {
                // Primary DA: VT100 with advanced video (`?1;2`).
                self.push_response(b"\x1b[?1;2c");
            }
            'n' => {
                let ps = Self::params_iter(params).next().unwrap_or(0);
                match ps {
                    5 => self.push_response(b"\x1b[0n"),
                    6 => {
                        let (row, col) = self.cpr_position();
                        let reply = format!("\x1b[{row};{col}R");
                        self.push_response(reply.as_bytes());
                    }
                    _ => {}
                }
            }
            'L' => {
                let n = Self::param_at(params, 0, 1) as usize;
                self.grid.insert_lines(n);
            }
            'M' => {
                let n = Self::param_at(params, 0, 1) as usize;
                self.grid.delete_lines(n);
            }
            'S' => {
                let n = Self::param_at(params, 0, 1) as usize;
                self.grid.scroll_up(n);
            }
            'T' => {
                let n = Self::param_at(params, 0, 1) as usize;
                self.grid.scroll_down(n);
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        self.pending_wrap = false;
        if intermediates.is_empty() {
            match byte {
                b'M' => self.grid.reverse_index(),
                b'7' => self.grid.save_cursor(),
                b'8' => self.grid.restore_cursor(),
                b'=' => self.grid.set_keypad_app_mode(true),
                b'>' => self.grid.set_keypad_app_mode(false),
                b'c' => {
                    // Full reset: back to main buffer with default modes.
                    self.grid.exit_alt();
                    self.grid.set_cursor_enabled(true);
                    self.grid.set_bracketed_paste(false);
                    self.grid.set_cursor_app_mode(false);
                    self.grid.set_keypad_app_mode(false);
                    self.grid.reset_mouse_and_focus();
                    self.grid.set_cursor_style(CursorStyle::default());
                    self.grid.reset_margins_and_modes();
                    // Reset pen before clearing so BCE erase uses default bg.
                    self.grid.sgr(&[0]);
                    self.grid.set_hyperlink(None);
                    self.grid.clear_all();
                    self.grid.set_cursor(0, 0);
                    self.title.clear();
                    self.cwd = None;
                    self.last_graphic = None;
                }
                _ => {}
            }
        }
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        let Some(osc) = Osc::parse(params) else {
            log::debug!(
                "OSC no parameters [{}]",
                if bell_terminated { "BEL" } else { "ST" }
            );
            return;
        };
        // Log every OSC at debug, handled or not: shells emit plenty that
        // cometty ignores today (hyperlinks, prompt marks, clipboard, …)
        // and the panel should answer "did the app even see it?".
        log::debug!(
            "OSC {} [{}]",
            decoded_osc_summary(&osc),
            if bell_terminated { "BEL" } else { "ST" }
        );
        // Tracked OSC numbers: titles (0/1/2) feed tab labels, cwd (7) the
        // `$cwd` fallback, 8 the per-cell hyperlink state, 52 clipboard.
        let Some(payload) = osc.payload else {
            return;
        };
        match osc.number {
            num if TITLE_OSC.contains(&num) => {
                self.title = title_payload(payload, self.max_title_chars)
            }
            b"7" => {
                if let Some(cwd) = cwd_payload(payload, self.max_title_chars) {
                    self.cwd = Some(cwd);
                    self.cwd_version += 1;
                }
            }
            b"8" => match hyperlink_payload(payload) {
                Some(HyperlinkOp::End) => self.grid.set_hyperlink(None),
                Some(HyperlinkOp::Set(uri)) => self.grid.set_hyperlink(Some(&uri)),
                Some(HyperlinkOp::TooLong(bytes)) => self.grid.reject_oversized_hyperlink(bytes),
                None => {}
            },
            b"52" => {
                if let Some(op) = clipboard_payload(payload, MAX_CLIPBOARD_BYTES) {
                    self.pending_clipboard.push(op);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    // Keep the parser-shape tests readable while production routes once.
    fn osc_title(params: &[&[u8]], max: usize) -> Option<String> {
        let osc = Osc::parse(params)?;
        TITLE_OSC.contains(&osc.number).then_some(())?;
        Some(title_payload(osc.payload?, max))
    }
    fn osc_cwd(params: &[&[u8]], max: usize) -> Option<String> {
        let osc = Osc::parse(params)?;
        (osc.number == b"7").then_some(())?;
        cwd_payload(osc.payload?, max)
    }
    fn osc_hyperlink(params: &[&[u8]]) -> Option<Option<String>> {
        let osc = Osc::parse(params)?;
        (osc.number == b"8").then_some(())?;
        hyperlink_payload(osc.payload?).map(|op| match op {
            HyperlinkOp::End | HyperlinkOp::TooLong(_) => None,
            HyperlinkOp::Set(uri) => Some(uri),
        })
    }
    fn osc_clipboard(params: &[&[u8]], max: usize) -> Option<ClipboardOp> {
        let osc = Osc::parse(params)?;
        (osc.number == b"52").then_some(())?;
        clipboard_payload(osc.payload?, max)
    }
    fn osc_summary(params: &[&[u8]]) -> String {
        Osc::parse(params).map_or_else(|| "no parameters".into(), |osc| decoded_osc_summary(&osc))
    }

    #[test]
    fn ignored_osc_dispatch_does_not_copy_payloads() {
        let long = vec![b'x'; 1 << 20];
        let params: [&[u8]; 2] = [b"999", &long];
        let osc = Osc::parse(&params).unwrap();
        let (calls, _, bytes) = crate::perf_bench::allocation_counts(|| {
            let summary = decoded_osc_summary(&osc);
            assert!(summary.len() < 250);
        });
        assert!(
            calls < 20 && bytes < 4096,
            "summary must not copy a megabyte"
        );
    }

    #[test]
    fn decoded_osc_preserves_fragment_boundaries_and_bounded_private_summaries() {
        assert_eq!(
            osc_clipboard(&[b"52", b"c", b"aG k=\n"], 10),
            Some(ClipboardOp::Set("hi".into()))
        );
        assert_eq!(osc_clipboard(&[b"52;c;?"], 10), Some(ClipboardOp::Query));
        assert_eq!(osc_clipboard(&[b"52", b"unknown", b"aGk="], 10), None);
        assert_eq!(osc_clipboard(&[b"52", b"c", b"aGk="], 1), None);
        assert_eq!(osc_clipboard(&[b"52"], 10), None);
        assert_eq!(
            osc_hyperlink(&[b"8", b"id=1", b"https://host/a", b"b"]),
            Some(Some("https://host/a;b".into()))
        );
        let payload = "中".repeat(200);
        let summary = osc_summary(&[b"999", payload.as_bytes()]);
        assert!(summary.ends_with("… (600 bytes)"));
        assert!(!summary.contains('\u{fffd}'));
        let number = vec![b'9'; 1 << 20];
        assert!(osc_summary(&[&number]).len() < 200);
        assert!(!osc_summary(&[b"52", b"c", b"secret"]).contains("secret"));
    }

    #[test]
    fn streaming_labels_match_full_transformations_without_prefix_trim_changes() {
        let cases: &[&[&[u8]]] = &[
            &[b" \x00abc     "],
            &[b"abc    suffix"],
            &["\u{2003}abc \u{2003}x\u{2003}".as_bytes()],
            &[b"\xff \xe4\xb8", b"\xad end \n"],
            &[b"file://host/a%20b%2Fc%%41%1%42%2%"],
            &[b"file:///", "é中".as_bytes(), b"%FF%C3%A9%F0%9F%92%A5"],
            &[b"file://host/\xff\xe4\xb8", b"%41"],
            &[b"file://host/abc   "],
            &[b"file://host"],
            &[b"http://host/path"],
            &[b"file:/", b"/path"],
            &[b"", b""],
        ];
        for pieces in cases {
            let payload = OscPayload {
                first: pieces[0],
                rest: &pieces[1..],
            };
            let joined: Vec<u8> = payload.bytes().collect();
            for max in [0, 1, 2, 3, 4, 5, 6, 9, 16, 256] {
                let cleaned: String = String::from_utf8_lossy(&joined)
                    .chars()
                    .filter(|c| !c.is_control())
                    .collect();
                let expected: String = cleaned.trim().chars().take(max.max(1)).collect();
                assert_eq!(
                    title_payload(payload, max),
                    expected,
                    "{pieces:?}, max={max}"
                );
                assert_eq!(
                    cwd_payload(payload, max),
                    reference_cwd(&joined, max),
                    "{pieces:?}, max={max}"
                );
            }
        }
        // All decoded byte values, including UTF-8 split at the bounded-prefix
        // edge, must match the old decode-entire-path-then-truncate semantics.
        let uri = format!(
            "file:///{}",
            (0..=255).map(|b| format!("%{b:02X}")).collect::<String>()
        );
        let payload = OscPayload {
            first: uri.as_bytes(),
            rest: &[],
        };
        for max in 1..=260 {
            assert_eq!(
                cwd_payload(payload, max),
                reference_cwd(uri.as_bytes(), max)
            );
        }
    }

    fn reference_cwd(bytes: &[u8], max: usize) -> Option<String> {
        let uri = String::from_utf8_lossy(bytes);
        let rest = uri.strip_prefix("file://")?;
        let path = &rest.as_bytes()[rest.find('/')?..];
        let mut decoded = Vec::new();
        let mut i = 0;
        while i < path.len() {
            if path[i] == b'%' && i + 2 < path.len() {
                let hi = (path[i + 1] as char).to_digit(16);
                let lo = (path[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    decoded.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
            }
            decoded.push(path[i]);
            i += 1;
        }
        Some(
            String::from_utf8_lossy(&decoded)
                .chars()
                .take(max.max(1))
                .collect(),
        )
    }

    #[test]
    fn handler_allocations_follow_retained_limits_not_raw_payload_size() {
        let long = vec![b'x'; 1 << 20];
        let title = OscPayload {
            first: &long,
            rest: &[],
        };
        let stats = crate::perf_bench::allocation_stats(|| {
            assert_eq!(title_payload(title, 16).len(), 16);
        });
        assert!(stats.largest_request < 4096 && stats.live_delta == 0);

        let uri = format!("file://host/{}", "%E4%B8%AD".repeat(1 << 17));
        let cwd = OscPayload {
            first: uri.as_bytes(),
            rest: &[],
        };
        let stats = crate::perf_bench::allocation_stats(|| {
            assert_eq!(cwd_payload(cwd, 16).unwrap().chars().count(), 16);
        });
        assert!(stats.largest_request < 4096 && stats.live_delta == 0);

        let pieces: [&[u8]; 2] = [b"https://host/", &long];
        let link = OscPayload {
            first: b"id=1",
            rest: &pieces,
        };
        assert_eq!(
            crate::perf_bench::allocation_counts(|| {
                assert!(matches!(
                    hyperlink_payload(link),
                    Some(HyperlinkOp::TooLong(_))
                ));
            }),
            (0, 0, 0)
        );

        let pieces = [long.as_slice()];
        let clipboard = OscPayload {
            first: b"c",
            rest: &pieces,
        };
        assert_eq!(
            crate::perf_bench::allocation_counts(|| {
                assert_eq!(clipboard_payload(clipboard, 16), None);
            }),
            (0, 0, 0),
            "reject encoded size before joining/filtering/decoding"
        );
    }

    #[test]
    fn oversized_raw_oscs_never_apply_partial_supported_updates() {
        let chunk = [b'x'; 8192];
        for prefix in [
            b"0;".as_slice(),
            b"1;",
            b"2;",
            b"7;file:///",
            b"8;;https://host/",
            b"52;c;",
            b"999;",
        ] {
            for end in [b"\x07".as_slice(), b"\x1b\\", b"\x18", b"\x1a"] {
                let mut t = test_terminal(20, 2);
                t.feed(b"\x1b]2;kept\x07\x1b]7;file:///kept\x07\x1b]8;;https://kept\x07");
                let link = t.grid.current_hyperlink();
                let cwd_version = t.cwd_version();
                t.feed(b"\x1b]");
                t.feed(prefix);
                for _ in 0..=parser::MAX_RAW_OSC_BYTES / chunk.len() {
                    t.feed(&chunk);
                }
                t.feed(end);
                assert_eq!(t.title(), "kept", "prefix={prefix:?}, end={end:?}");
                assert_eq!(t.cwd(), Some("/kept"));
                assert_eq!(t.cwd_version(), cwd_version);
                assert_eq!(t.grid.current_hyperlink(), link);
                assert!(t.take_clipboard().is_empty());
                t.feed(b"\x1b]2;after\x07\x1b[31mOK");
                assert_eq!(t.title(), "after");
                assert_eq!(t.grid.cell(0, 0).unwrap().ch, 'O');
                assert_eq!(t.grid.cell(1, 0).unwrap().ch, 'K');
            }
        }
    }

    #[test]
    fn oversized_hyperlink_clears_current_link_without_interning_prefix() {
        let mut t = test_terminal(10, 2);
        t.feed(b"\x1b]8;;https://kept\x07");
        assert!(t.grid.current_hyperlink().is_some());
        t.feed(b"\x1b]8;id=1;https://host/");
        t.feed(&[b'x'; crate::grid::MAX_HYPERLINK_URI]);
        t.feed(b"\x07X");
        assert!(t.grid.current_hyperlink().is_none());
        assert!(t.grid.cell(0, 0).unwrap().hyperlink.is_none());
        let invalid = [0xff; crate::grid::MAX_HYPERLINK_URI / 2];
        let pieces = [invalid.as_slice()];
        assert!(
            matches!(
                hyperlink_payload(OscPayload {
                    first: b"",
                    rest: &pieces
                }),
                Some(HyperlinkOp::TooLong(_))
            ),
            "lossy URI expansion must respect the same byte cap"
        );
    }

    #[test]
    fn osc52_full_one_megabyte_transfer_still_works_with_chunked_wrapping() {
        let mut t = test_terminal(2, 1);
        let text = "a".repeat(MAX_CLIPBOARD_BYTES);
        let encoded = base64::engine::general_purpose::STANDARD.encode(&text);
        t.feed(b"\x1b]52;c;");
        for chunk in encoded.as_bytes().chunks(76) {
            t.feed(chunk);
            t.feed(b"\n ");
        }
        t.feed(b"\x1b");
        t.feed(b"\\OK");
        assert_eq!(t.take_clipboard(), vec![ClipboardOp::Set(text)]);
        assert_eq!(t.grid.cell(0, 0).unwrap().ch, 'O');
        assert_eq!(t.grid.cell(1, 0).unwrap().ch, 'K');
    }

    #[test]
    fn osc52_lossy_utf8_expansion_cannot_bypass_final_text_limit() {
        assert_eq!(osc_clipboard(&[b"52", b"c", b"/w=="], 2), None);
        assert_eq!(
            osc_clipboard(&[b"52", b"c", b"/w=="], 3),
            Some(ClipboardOp::Set("\u{fffd}".into()))
        );
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(vec![0xff; MAX_CLIPBOARD_BYTES]);
        let pieces = [encoded.as_bytes()];
        let payload = OscPayload {
            first: b"c",
            rest: &pieces,
        };
        let stats = crate::perf_bench::allocation_stats(|| {
            assert_eq!(clipboard_payload(payload, MAX_CLIPBOARD_BYTES), None);
        });
        assert_eq!(stats.live_delta, 0);
        assert!(
            stats.largest_request <= encoded.len(),
            "no expanded 3 MiB text allocation"
        );
        assert!(stats.peak_delta <= (encoded.len() + MAX_CLIPBOARD_BYTES + 2) as i64);
    }

    fn feed_str(t: &mut Terminal, s: &str) {
        t.feed(s.as_bytes());
    }

    fn test_terminal(cols: usize, rows: usize) -> Terminal {
        Terminal::new(cols, rows, Theme::default())
    }

    /// Reference dispatcher retains the old append-aware grid write after
    /// Terminal's own continuation check. Compare against the new single
    /// decision/fresh-write path, including wrap-driven cursor changes.
    struct Reference<'a>(&'a mut Terminal);

    impl vte::Perform for Reference<'_> {
        fn print(&mut self, c: char) {
            let t = &mut self.0;
            if !t.grid.auto_wrap() {
                t.pending_wrap = false;
            }
            if t.pending_wrap {
                let cur = t.grid.cursor();
                if t.grid.append_to_cell(cur.x, cur.y, c) {
                    return;
                }
                t.grid.newline();
                t.pending_wrap = false;
            }
            if t.grid.append_to_prev(c) {
                return;
            }
            if c != '\0' && !crate::grid::unicode::is_joining_modifier(c) {
                t.last_graphic = Some(c);
            }
            t.grid.handle_wrap_if_needed();
            t.grid.put_char(c);
            if t.grid.auto_wrap() && t.grid.cursor().x >= t.grid.cols() {
                t.pending_wrap = true;
                let y = t.grid.cursor().y;
                t.grid.set_cursor(t.grid.cols().saturating_sub(1), y);
            } else if !t.grid.auto_wrap() {
                t.pending_wrap = false;
            }
        }
        fn execute(&mut self, byte: u8) {
            self.0.execute(byte);
        }
        fn csi_dispatch(
            &mut self,
            params: &Params,
            intermediates: &[u8],
            ignore: bool,
            action: char,
        ) {
            self.0.csi_dispatch(params, intermediates, ignore, action);
        }
        fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
            self.0.esc_dispatch(intermediates, ignore, byte);
        }
        fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
            self.0.osc_dispatch(params, bell_terminated);
        }
    }

    fn assert_same_terminal(actual: &Terminal, expected: &Terminal) {
        assert_eq!(actual.grid.cursor(), expected.grid.cursor());
        assert_eq!(actual.grid.pen(), expected.grid.pen());
        assert_eq!(actual.grid.total_lines(), expected.grid.total_lines());
        for line in 0..expected.grid.total_lines() {
            assert_eq!(
                actual.grid.global_line(line),
                expected.grid.global_line(line)
            );
        }
        assert_eq!(actual.grid.scroll_region(), expected.grid.scroll_region());
        assert_eq!(actual.grid.cursor_style(), expected.grid.cursor_style());
        assert_eq!(actual.grid.version, expected.grid.version);
        assert_eq!(actual.grid.content_version, expected.grid.content_version);
        assert_eq!(actual.pending_wrap, expected.pending_wrap);
        assert_eq!(actual.last_graphic, expected.last_graphic);
        assert_eq!(actual.pending_response, expected.pending_response);
        assert_eq!(actual.pending_clipboard, expected.pending_clipboard);
        assert_eq!(actual.title, expected.title);
        assert_eq!(actual.cwd, expected.cwd);
    }

    #[test]
    fn mixed_unicode_and_controls_match_reference_at_every_byte_split() {
        let corpora = [
            "abcdefghijk\r\nxyz\t!\x08?",
            "abce\u{301}\u{301}X 中\u{301}Y\r\n \u{301}Z",
            "\u{600}ab\u{600}cde\r\n\u{600}xyz",
            "👨\u{200d}👩\u{200d}👧 🇺🇸🇨🇦 #\u{fe0f}\u{20e3}👍🏽xyz",
            "abcd\x1b[2D\x1b[4h中\u{301}Z\x1b[4l\x1b[?7labcdef\x1b[?7hX",
            "top\r\nmain\x1b[?1049h中abc\u{301}\x1b[2Jx\x1b[?1049lZ\x1b[2;3r\x1b[3;1Hx\r\ny",
            "\x1b]2;a;b\x07\x1b]7;file:///tmp/a%20b\x1b\\\x1b]8;;https://a/b;c\x07中e\u{301}\x1b]8;;\x07X\x1b]52;c;aGk=\x1b\\\x1b[6n",
        ];
        for cols in [1, 2, 5, 9] {
            for corpus in corpora {
                let bytes = corpus.as_bytes();
                let mut reference = test_terminal(cols, 3);
                let mut parser = vte::Parser::new();
                parser.advance(&mut Reference(&mut reference), bytes);
                for split in 0..=bytes.len() {
                    let mut actual = test_terminal(cols, 3);
                    actual.feed(&bytes[..split]);
                    actual.feed(&bytes[split..]);
                    assert_same_terminal(&actual, &reference);
                }
                let mut bytewise = test_terminal(cols, 3);
                for byte in bytes {
                    bytewise.feed(std::slice::from_ref(byte));
                }
                assert_same_terminal(&bytewise, &reference);
            }
        }
    }

    #[test]
    fn prints_simple_text() {
        let mut t = test_terminal(10, 5);
        feed_str(&mut t, "hi");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, 'h');
        assert_eq!(t.grid().cell(1, 0).unwrap().ch, 'i');
    }

    #[test]
    fn apply_config_updates_theme_and_truncates_title() {
        let mut t = test_terminal(10, 5);
        feed_str(&mut t, "\x1b]0;hello\x07");
        assert_eq!(t.title(), "hello");
        let mut cfg = crate::config::Config::default();
        cfg.theme.name = "vscode".to_string();
        cfg.terminal.max_title_chars = 3;
        t.apply_config(Theme::vscode(), &cfg);
        assert_eq!(t.theme(), Theme::vscode());
        assert_eq!(t.title(), "hel");
    }

    #[test]
    fn handles_crlf() {
        let mut t = test_terminal(10, 5);
        feed_str(&mut t, "a\r\nb");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, 'a');
        assert_eq!(t.grid().cell(0, 1).unwrap().ch, 'b');
    }

    #[test]
    fn handles_sgr_color() {
        let mut t = test_terminal(10, 5);
        feed_str(&mut t, "\x1b[31mR\x1b[0mN");
        let r = t.grid().cell(0, 0).unwrap();
        let n = t.grid().cell(1, 0).unwrap();
        let theme = t.theme();
        assert_eq!(r.ch, 'R');
        assert_eq!(r.fg, theme.ansi(1));
        assert_eq!(n.ch, 'N');
        assert_eq!(n.fg, theme.foreground);
    }

    #[test]
    fn handles_cup_and_ed() {
        let mut t = test_terminal(10, 5);
        feed_str(&mut t, "hello");
        feed_str(&mut t, "\x1b[1;1H");
        assert_eq!(t.grid().cursor().x, 0);
        feed_str(&mut t, "\x1b[2J");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, ' ');
    }

    #[test]
    fn handles_wrap() {
        let mut t = test_terminal(3, 2);
        feed_str(&mut t, "abcd");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, 'a');
        assert_eq!(t.grid().cell(2, 0).unwrap().ch, 'c');
        assert_eq!(t.grid().cell(0, 1).unwrap().ch, 'd');
    }

    #[test]
    fn handles_truecolor() {
        let mut t = test_terminal(10, 2);
        feed_str(&mut t, "\x1b[38;2;10;20;30mX");
        let c = t.grid().cell(0, 0).unwrap();
        assert_eq!((c.fg.r, c.fg.g, c.fg.b), (10, 20, 30));
    }

    #[test]
    fn alt_screen_1049_swaps_and_restores() {
        let mut t = test_terminal(5, 3);
        feed_str(&mut t, "hi");
        feed_str(&mut t, "\x1b[?1049h");
        assert!(t.is_alt());
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, ' ');
        feed_str(&mut t, "X");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, 'X');
        assert_eq!(t.grid().scrollback_len(), 0);
        feed_str(&mut t, "\x1b[?1049l");
        assert!(!t.is_alt());
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, 'h');
        assert_eq!(t.grid().cell(1, 0).unwrap().ch, 'i');
    }

    #[test]
    fn alt_screen_1047_and_cursor_save_restore() {
        let mut t = test_terminal(5, 3);
        feed_str(&mut t, "ab");
        feed_str(&mut t, "\x1b[?1048h\x1b[?1047h");
        assert!(t.is_alt());
        feed_str(&mut t, "\x1b[?1047l\x1b[?1048l");
        assert!(!t.is_alt());
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, 'a');
    }

    #[test]
    fn decset_cursor_and_bracketed_modes() {
        let mut t = test_terminal(5, 3);
        assert!(t.cursor_visible());
        assert!(!t.bracketed_paste());
        feed_str(&mut t, "\x1b[?25l");
        assert!(!t.cursor_visible());
        feed_str(&mut t, "\x1b[?25h");
        assert!(t.cursor_visible());
        feed_str(&mut t, "\x1b[?2004h");
        assert!(t.bracketed_paste());
        feed_str(&mut t, "\x1b[?2004l");
        assert!(!t.bracketed_paste());
    }

    #[test]
    fn decckm_and_keypad_app_modes() {
        let mut t = test_terminal(5, 3);
        assert!(!t.cursor_app_mode());
        assert!(!t.keypad_app_mode());
        feed_str(&mut t, "\x1b[?1h");
        assert!(t.cursor_app_mode());
        feed_str(&mut t, "\x1b[?1l");
        assert!(!t.cursor_app_mode());
        feed_str(&mut t, "\x1b[?66h");
        assert!(t.keypad_app_mode());
        feed_str(&mut t, "\x1b[?66l");
        assert!(!t.keypad_app_mode());
        // DECKPAM / DECKPNM legacy sequences.
        feed_str(&mut t, "\x1b=");
        assert!(t.keypad_app_mode());
        feed_str(&mut t, "\x1b>");
        assert!(!t.keypad_app_mode());
        // Full reset clears both.
        feed_str(&mut t, "\x1b[?1h\x1b=\x1bc");
        assert!(!t.cursor_app_mode());
        assert!(!t.keypad_app_mode());
    }

    #[test]
    fn unknown_private_mode_ignored() {
        let mut t = test_terminal(5, 3);
        feed_str(&mut t, "ok\x1b[?9999h");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, 'o');
        assert!(!t.is_alt());
    }

    #[test]
    fn cnl_and_cpl_move_to_column_zero() {
        let mut t = test_terminal(5, 4);
        feed_str(&mut t, "\x1b[3;3H"); // row 3, col 3 (1-based)
        feed_str(&mut t, "\x1b[1E");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (0, 3));
        feed_str(&mut t, "\x1b[2F");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (0, 1));
        // Clamped at the margins, never panics.
        feed_str(&mut t, "\x1b[99F");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (0, 0));
        feed_str(&mut t, "\x1b[99E");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (0, 3));
    }

    #[test]
    fn csi_params_index_without_alloc() {
        // Empty, zero, and multi-param forms all resolve without a heap
        // Vec per query (see `param_at` / `params_iter`).
        let mut t = test_terminal(5, 4);
        feed_str(&mut t, "\x1b[H");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (0, 0));
        feed_str(&mut t, "\x1b[0;0H");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (0, 0));
        feed_str(&mut t, "\x1b[2;3H");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (2, 1));
        // Subparam form (`:`) flattens the same way.
        feed_str(&mut t, "\x1b[3:4H");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (3, 2));
        // Relative moves default a missing/zero count to 1.
        feed_str(&mut t, "\x1b[1;1H\x1b[A");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (0, 0));
        feed_str(&mut t, "\x1b[1;1H\x1b[0C");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (1, 0));
    }

    #[test]
    fn insert_and_delete_lines_shift_rows() {
        let mut t = test_terminal(3, 3);
        feed_str(&mut t, "a\r\nb\r\nc");
        feed_str(&mut t, "\x1b[1;1H\x1b[1L");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, ' ');
        assert_eq!(t.grid().cell(0, 1).unwrap().ch, 'a');
        feed_str(&mut t, "\x1b[1;1H\x1b[1M");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, 'a');
    }

    #[test]
    fn ed1_and_el_modes_clear() {
        let mut t = test_terminal(4, 2);
        feed_str(&mut t, "abcd\r\nwxyz\x1b[1;3H");
        feed_str(&mut t, "\x1b[1K");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, ' ');
        assert_eq!(t.grid().cell(2, 0).unwrap().ch, ' ');
        assert_eq!(t.grid().cell(3, 0).unwrap().ch, 'd');
        feed_str(&mut t, "\x1b[1J");
        assert_eq!(t.grid().cell(3, 0).unwrap().ch, 'd');
        assert_eq!(t.grid().cell(0, 1).unwrap().ch, 'w');
    }

    #[test]
    fn malformed_extended_sgr_is_ignored() {
        let mut t = test_terminal(5, 2);
        let before = t.grid().pen();
        // Truncated tails must not panic or change the pen.
        // (Note: leftover values that ARE real codes, e.g. the `1` in
        // `38;2;1`, still apply as usual — only the truncated prefix is skipped.)
        feed_str(&mut t, "\x1b[38;5mX\x1b[38;2;10mY\x1b[48mZ");
        assert_eq!(t.grid().pen(), before);
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, 'X');
    }

    #[test]
    fn full_sgr_attrs_set_and_reset() {
        use crate::grid::UnderlineStyle;
        let mut t = test_terminal(5, 2);
        feed_str(&mut t, "\x1b[1;2;3;7;9;53m");
        let pen = t.grid().pen();
        assert!(pen.bold && pen.dim && pen.italic);
        assert!(pen.inverse && pen.strikethrough && pen.overline);
        // 21 clears bold only (vte CancelBold); 22 clears bold+dim.
        feed_str(&mut t, "\x1b[21m");
        assert!(!t.grid().pen().bold);
        assert!(t.grid().pen().dim);
        feed_str(&mut t, "\x1b[1;2m\x1b[22m");
        assert!(!t.grid().pen().bold);
        assert!(!t.grid().pen().dim);
        feed_str(&mut t, "\x1b[23;27;29;55m");
        let pen = t.grid().pen();
        assert!(!pen.italic && !pen.inverse && !pen.strikethrough && !pen.overline);
        // Underline single via `4`, cleared by `24` (color persists).
        feed_str(&mut t, "\x1b[58;2;1;2;3m\x1b[4m");
        assert_eq!(t.grid().pen().underline, UnderlineStyle::Single);
        feed_str(&mut t, "\x1b[24m");
        assert_eq!(t.grid().pen().underline, UnderlineStyle::None);
        assert!(t.grid().pen().underline_color.is_some());
        feed_str(&mut t, "\x1b[59m");
        assert!(t.grid().pen().underline_color.is_none());
        // Full reset clears everything.
        feed_str(&mut t, "\x1b[1;2;3;7;9;53m\x1b[58;5;200m\x1b[0m");
        assert_eq!(t.grid().pen(), t.grid().pen().clone());
        let pen = t.grid().pen();
        assert!(!pen.bold && !pen.dim && !pen.italic);
        assert!(!pen.inverse && !pen.strikethrough && !pen.overline);
        assert_eq!(pen.underline, UnderlineStyle::None);
        assert!(pen.underline_color.is_none());
    }

    #[test]
    fn underline_styles_colon_vs_semicolon() {
        use crate::grid::UnderlineStyle;
        let mut t = test_terminal(5, 2);
        for (seq, style) in [
            ("\x1b[4:0m", UnderlineStyle::None),
            ("\x1b[4:1m", UnderlineStyle::Single),
            ("\x1b[4:2m", UnderlineStyle::Double),
            ("\x1b[4:3m", UnderlineStyle::Curly),
            ("\x1b[4:4m", UnderlineStyle::Dotted),
            ("\x1b[4:5m", UnderlineStyle::Dashed),
            ("\x1b[4m", UnderlineStyle::Single),
        ] {
            feed_str(&mut t, "\x1b[0m");
            feed_str(&mut t, seq);
            assert_eq!(t.grid().pen().underline, style, "seq={seq:?}");
        }
        // `4;3` is two codes (underline + italic), not curly.
        feed_str(&mut t, "\x1b[0m\x1b[4;3m");
        assert_eq!(t.grid().pen().underline, UnderlineStyle::Single);
        assert!(t.grid().pen().italic);
    }

    #[test]
    fn extended_colon_colors_and_colorspace() {
        let mut t = test_terminal(5, 2);
        feed_str(&mut t, "\x1b[38:5:200m");
        assert_eq!(t.grid().pen().fg, t.grid().extended_palette(200));
        feed_str(&mut t, "\x1b[0m\x1b[38:2:10:20:30m");
        assert_eq!(t.grid().pen().fg, crate::theme::Rgb::new(10, 20, 30));
        feed_str(&mut t, "\x1b[0m\x1b[48:2:40:50:60m");
        assert_eq!(t.grid().pen().bg, crate::theme::Rgb::new(40, 50, 60));
        // Colorspace id skipped: `38:2:<cs>:r:g:b`.
        feed_str(&mut t, "\x1b[0m\x1b[38:2:0:11:22:33m");
        assert_eq!(t.grid().pen().fg, crate::theme::Rgb::new(11, 22, 33));
        // Underline color both forms + reset.
        feed_str(&mut t, "\x1b[58:2:1:2:3m");
        assert_eq!(
            t.grid().pen().underline_color,
            Some(crate::theme::Rgb::new(1, 2, 3))
        );
        feed_str(&mut t, "\x1b[59m");
        assert!(t.grid().pen().underline_color.is_none());
        feed_str(&mut t, "\x1b[58;5;124m");
        assert!(t.grid().pen().underline_color.is_some());
    }

    #[test]
    fn inverse_and_dim_affect_effective_colors() {
        let mut t = test_terminal(5, 2);
        feed_str(&mut t, "\x1b[31m\x1b[44m");
        let fg = t.grid().pen().fg;
        let bg = t.grid().pen().bg;
        feed_str(&mut t, "A");
        let plain = t.grid().cell(0, 0).unwrap();
        assert_eq!(plain.effective_fg(), fg);
        assert_eq!(plain.effective_bg(), bg);
        feed_str(&mut t, "\x1b[7mB");
        let inv = t.grid().cell(1, 0).unwrap();
        assert!(inv.inverse);
        assert_eq!(inv.effective_fg(), bg);
        assert_eq!(inv.effective_bg(), fg);
        feed_str(&mut t, "\x1b[27m\x1b[2mC");
        let dim = t.grid().cell(2, 0).unwrap();
        assert!(dim.dim);
        assert_eq!(dim.effective_fg(), dim.fg.dimmed());
    }

    #[test]
    fn erase_clears_full_sgr_attrs() {
        let mut t = test_terminal(4, 2);
        feed_str(&mut t, "\x1b[1;2;3;7;9;53m\x1b[4:3m\x1b[58;5;200m");
        feed_str(&mut t, "\x1b[2J");
        let c = t.grid().cell(0, 0).unwrap();
        assert!(!c.bold && !c.dim && !c.italic);
        assert!(!c.inverse && !c.strikethrough && !c.overline);
        assert_eq!(c.underline, crate::grid::UnderlineStyle::None);
        assert!(c.underline_color.is_none());
    }

    #[test]
    fn osc_title_is_stored_for_tab_labels() {
        let mut t = test_terminal(10, 2);
        assert_eq!(t.title(), "");
        feed_str(&mut t, "\x1b]0;nvim | ~/dev\x07");
        assert_eq!(t.title(), "nvim | ~/dev");
        // Icon title (1) and window title (2) also update.
        feed_str(&mut t, "\x1b]2;second\x07");
        assert_eq!(t.title(), "second");
        // Titles containing ';' survive vte's param splitting.
        feed_str(&mut t, "\x1b]0;a;b\x07");
        assert_eq!(t.title(), "a;b");
    }

    #[test]
    fn osc_non_title_is_ignored() {
        let mut t = test_terminal(10, 2);
        feed_str(&mut t, "\x1b]0;kept\x07");
        // OSC 4 (palette) must not clobber the title.
        feed_str(&mut t, "\x1b]4;1;red\x07");
        assert_eq!(t.title(), "kept");
    }

    #[test]
    fn osc_title_parser_shapes() {
        assert_eq!(osc_title(&[], 256), None);
        assert_eq!(osc_title(&[b"4", b"1;red"], 256), None);
        assert_eq!(osc_title(&[b"0", b"hi"], 256), Some("hi".to_string()));
        assert_eq!(
            osc_title(&[b"2;split;title"], 256),
            Some("split;title".to_string())
        );
        assert_eq!(osc_title(&[b"0"], 256), None);
    }

    #[test]
    fn osc_title_strips_control_chars() {
        // Embedded newline / BEL / ESC must not reach the tab bar or OS
        // window title (layout breakage / sequence smuggling).
        assert_eq!(
            osc_title(&[b"0", b"evil\ntitle"], 256),
            Some("eviltitle".to_string())
        );
        assert_eq!(osc_title(&[b"0", b"a\x07b"], 256), Some("ab".to_string()));
        assert_eq!(
            osc_title(&[b"2", b"\x1b]0;smuggled"], 256),
            Some("]0;smuggled".to_string())
        );
        // A title of only controls collapses to empty (stored, not ignored:
        // the shell explicitly set it).
        assert_eq!(osc_title(&[b"0", b"\x07\x07"], 256), Some("".to_string()));
        // Legitimate titles with `;`, `|` and unicode survive.
        assert_eq!(
            osc_title(&[b"0", "nvim | ~/dév".as_bytes()], 256),
            Some("nvim | ~/dév".to_string())
        );
        // End-to-end: fed sequence lands sanitized in the stored title.
        let mut t = test_terminal(10, 2);
        feed_str(&mut t, "\x1b]0;evil\ntitle\x07");
        assert_eq!(t.title(), "eviltitle");
    }

    #[test]
    fn osc_cwd_parser_shapes() {
        assert_eq!(
            osc_cwd(&[b"7", b"file://localhost/Users/x/dev"], 256),
            Some("/Users/x/dev".to_string())
        );
        assert_eq!(
            osc_cwd(&[b"7", b"file:///tmp"], 256),
            Some("/tmp".to_string())
        );
        // Percent escapes decode; spaces survive.
        assert_eq!(
            osc_cwd(&[b"7", b"file://host/tmp/a%20b%2Fc"], 256),
            Some("/tmp/a b/c".to_string())
        );
        // Non-file schemes, relative paths, and other OSC numbers are ignored.
        assert_eq!(osc_cwd(&[b"7", b"http://host/tmp"], 256), None);
        assert_eq!(osc_cwd(&[b"7", b"file://host"], 256), None);
        assert_eq!(osc_cwd(&[b"0", b"hi"], 256), None);
        assert_eq!(osc_cwd(&[b"7"], 256), None);
        // Truncation mirrors titles.
        assert_eq!(
            osc_cwd(&[b"7", b"file://host/abcde"], 3),
            Some("/ab".to_string())
        );
    }

    #[test]
    fn osc_cwd_is_stored_for_labels() {
        let mut t = test_terminal(10, 2);
        assert_eq!(t.cwd(), None);
        assert_eq!(t.cwd_version(), 0);
        feed_str(&mut t, "\x1b]7;file://localhost/Users/x/dev\x07");
        assert_eq!(t.cwd(), Some("/Users/x/dev"));
        assert_eq!(t.cwd_version(), 1);
        // A bare OSC 7 keeps the last directory instead of clearing it.
        feed_str(&mut t, "\x1b]7\x07");
        assert_eq!(t.cwd(), Some("/Users/x/dev"));
        assert_eq!(t.cwd_version(), 1);
        // Full reset (ESC c) drops it along with the title; the version
        // stays monotonic so a later report still counts as fresh.
        feed_str(&mut t, "\x1bc");
        assert_eq!(t.cwd(), None);
        assert_eq!(t.cwd_version(), 1);
        assert_eq!(t.title(), "");
    }

    #[test]
    fn osc_summary_names_and_payload() {
        assert_eq!(osc_summary(&[b"2", b"nvim"]), "2 (title): nvim");
        assert_eq!(osc_summary(&[b"0"]), "0 (title)");
        assert_eq!(
            osc_summary(&[b"4", b"1", b"index"]),
            "4 (palette, unhandled): 1;index"
        );
        assert_eq!(
            osc_summary(&[b"133", b"A"]),
            "133 (prompt marks, unhandled): A"
        );
        assert_eq!(
            osc_summary(&[b"9999", b"x"]),
            "9999 (unknown, unhandled): x"
        );
        assert_eq!(osc_summary(&[]), "no parameters");
        // Unsplit single-param form, as seen with some terminators.
        assert_eq!(
            osc_summary(&[b"7;file://host/tmp"]),
            "7 (cwd): file://host/tmp"
        );
    }

    #[test]
    fn osc_summary_is_one_line_and_truncated() {
        let long = vec![b'x'; 4096];
        let params: [&[u8]; 3] = [b"52", b"c", &long];
        let summary = osc_summary(&params);
        assert!(
            summary.starts_with("52 (clipboard): <redacted"),
            "clipboard contents never reach the log: {summary}"
        );
        assert!(summary.contains("bytes>"), "redaction reports length");
        assert!(summary.len() < 200, "bounded line, got {}", summary.len());

        // Control bytes escape instead of breaking the log row.
        let escaped = osc_summary(&[b"9", b"a\nb\x07c"]);
        assert!(!escaped.contains('\n'));
        assert!(escaped.contains("\\n"));
    }

    #[test]
    fn osc8_hyperlink_applies_to_printed_cells() {
        let mut t = test_terminal(20, 2);
        feed_str(&mut t, "\x1b]8;;https://example.com\x07cli\x1b]8;;\x07ck");
        let row = t.grid().global_line(0).unwrap();
        let id = row.get(0).unwrap().hyperlink.expect("c is linked");
        assert_eq!(t.grid().hyperlink_uri(id), Some("https://example.com"));
        assert_eq!(row.get(1).unwrap().hyperlink, Some(id));
        assert_eq!(row.get(2).unwrap().hyperlink, Some(id));
        assert_eq!(
            row.get(3).unwrap().hyperlink,
            None,
            "empty URI ends the link"
        );
        // `id=` params are accepted and ignored; BEL or ST both work.
        feed_str(&mut t, "\x1b]8;id=42;https://a.b\x1b\\x");
        let row = t.grid().global_line(0).unwrap();
        assert!(row.get(5).unwrap().hyperlink.is_some());
        // SGR does not clear an active hyperlink (spec behavior).
        feed_str(&mut t, "\x1b]8;;https://c.d\x07\x1b[0my");
        let row = t.grid().global_line(0).unwrap();
        assert!(
            row.get(6).unwrap().hyperlink.is_some(),
            "SGR 0 keeps the link open"
        );
        // Full reset (ESC c) drops the link state and clears cells.
        feed_str(&mut t, "\x1bc");
        feed_str(&mut t, "z");
        let row = t.grid().global_line(0).unwrap();
        assert_eq!(row.get(0).unwrap().hyperlink, None);
    }

    #[test]
    fn osc8_hyperlink_parser_shapes() {
        assert_eq!(osc_hyperlink(&[]), None);
        assert_eq!(osc_hyperlink(&[b"0", b"x"]), None);
        assert_eq!(
            osc_hyperlink(&[b"8", b";https://a.b"]),
            Some(Some("https://a.b".to_string()))
        );
        assert_eq!(
            osc_hyperlink(&[b"8", b"id=7;https://a.b"]),
            Some(Some("https://a.b".to_string()))
        );
        assert_eq!(osc_hyperlink(&[b"8", b";"]), Some(None));
        // Malformed (no separator) is ignored rather than treated as a URI.
        assert_eq!(osc_hyperlink(&[b"8", b"id=7"]), None);
        assert_eq!(osc_hyperlink(&[b"7", b"file:///tmp"]), None);
    }

    #[test]
    fn osc8_hyperlink_shows_in_hint_view() {
        let mut t = test_terminal(24, 2);
        feed_str(
            &mut t,
            "\x1b]8;;https://example.com\x07link\x1b]8;;\x07 plain https://a.b",
        );
        let view = crate::links::HintView::build(t.grid(), true);
        // OSC 8 run (cols 0..3) and the detected URL (cols 11..21).
        assert_eq!(view.rows[0], vec![(0, 3), (11, 21)]);
        assert!(view.hit(0, 2, 1));
        assert!(!view.hit(0, 4, 1));
        // `detect_urls = false` keeps only the OSC 8 run.
        let view = crate::links::HintView::build(t.grid(), false);
        assert_eq!(view.rows[0], vec![(0, 3)]);
    }

    #[test]
    fn osc52_set_decode_and_queue() {
        let mut t = test_terminal(10, 2);
        // "hi" -> aGk=
        feed_str(&mut t, "\x1b]52;c;aGk=\x07");
        assert_eq!(t.take_clipboard(), vec![ClipboardOp::Set("hi".to_string())]);
        // Whitespace/newlines inside the payload are tolerated (tmux).
        feed_str(&mut t, "\x1b]52;c;YW\n\tI=\x07");
        assert_eq!(t.take_clipboard(), vec![ClipboardOp::Set("ab".to_string())]);
        // Unknown targets are ignored; an empty payload clears the clipboard.
        feed_str(&mut t, "\x1b]52;z;YQ==\x07");
        feed_str(&mut t, "\x1b]52;c;\x07");
        assert_eq!(t.take_clipboard(), vec![ClipboardOp::Set(String::new())]);
        // Invalid base64 is dropped.
        feed_str(&mut t, "\x1b]52;c;!!!not-base64!!!\x07");
        assert!(t.take_clipboard().is_empty());
    }

    #[test]
    fn osc52_query_and_reply_roundtrip() {
        let mut t = test_terminal(10, 2);
        feed_str(&mut t, "\x1b]52;c;?\x07");
        assert_eq!(t.take_clipboard(), vec![ClipboardOp::Query]);
        let reply = osc52_reply("hi");
        assert_eq!(reply, b"\x1b]52;c;aGk=\x1b\\".to_vec());
    }

    #[test]
    fn osc52_oversized_payload_is_dropped() {
        let mut t = test_terminal(10, 2);
        // 4/3 bytes encoded per input byte: exceed the cap without decoding.
        let encoded = "A".repeat(MAX_CLIPBOARD_BYTES / 3 * 4 + 8);
        feed_str(&mut t, &format!("\x1b]52;c;{encoded}\x07"));
        assert!(t.take_clipboard().is_empty());
    }

    #[test]
    fn sgr_underline_is_stored_on_cells() {
        use crate::grid::{CursorShape, CursorStyle, UnderlineStyle};
        let mut t = test_terminal(5, 2);
        feed_str(&mut t, "\x1b[4mU\x1b[24mN");
        assert_eq!(
            t.grid().cell(0, 0).unwrap().underline,
            UnderlineStyle::Single
        );
        assert_eq!(t.grid().cell(1, 0).unwrap().underline, UnderlineStyle::None);
        // Default cursor style is blinking block.
        assert_eq!(t.cursor_style(), CursorStyle::default());
        assert_eq!(
            t.cursor_style(),
            CursorStyle {
                shape: CursorShape::Block,
                blinking: true,
            }
        );
    }

    #[test]
    fn decscusr_sets_shape_and_blink() {
        use crate::grid::CursorShape;
        let mut t = test_terminal(5, 2);
        feed_str(&mut t, "\x1b[0 q");
        assert_eq!(t.cursor_style().shape, CursorShape::Block);
        assert!(t.cursor_style().blinking);
        feed_str(&mut t, "\x1b[2 q");
        assert_eq!(t.cursor_style().shape, CursorShape::Block);
        assert!(!t.cursor_style().blinking);
        feed_str(&mut t, "\x1b[3 q");
        assert_eq!(t.cursor_style().shape, CursorShape::Underline);
        assert!(t.cursor_style().blinking);
        feed_str(&mut t, "\x1b[4 q");
        assert_eq!(t.cursor_style().shape, CursorShape::Underline);
        assert!(!t.cursor_style().blinking);
        feed_str(&mut t, "\x1b[5 q");
        assert_eq!(t.cursor_style().shape, CursorShape::Bar);
        assert!(t.cursor_style().blinking);
        feed_str(&mut t, "\x1b[6 q");
        assert_eq!(t.cursor_style().shape, CursorShape::Bar);
        assert!(!t.cursor_style().blinking);
        // Unknown Ps is ignored, keeping the previous style.
        feed_str(&mut t, "\x1b[9 q");
        assert_eq!(t.cursor_style().shape, CursorShape::Bar);
        assert!(!t.cursor_style().blinking);
    }

    #[test]
    fn decscusr_save_restore_and_reset() {
        use crate::grid::CursorShape;
        let mut t = test_terminal(5, 2);
        feed_str(&mut t, "\x1b[5 q");
        feed_str(&mut t, "\x1b7");
        feed_str(&mut t, "\x1b[3 q");
        assert_eq!(t.cursor_style().shape, CursorShape::Underline);
        feed_str(&mut t, "\x1b8");
        assert_eq!(t.cursor_style().shape, CursorShape::Bar);
        // Full reset restores blinking block.
        feed_str(&mut t, "\x1bc");
        assert_eq!(t.cursor_style(), crate::grid::CursorStyle::default());
    }

    #[test]
    fn erase_uses_pen_bg_and_reset_clears_to_default() {
        let mut t = test_terminal(4, 2);
        let theme = t.theme();
        feed_str(&mut t, "\x1b[41m\x1b[2J");
        let c = t.grid().cell(0, 0).unwrap();
        assert_eq!(c.bg, theme.ansi(1));
        assert_eq!(c.fg, theme.foreground);
        // Full reset clears pen first, so BCE fill lands on default bg.
        feed_str(&mut t, "\x1bc");
        let c = t.grid().cell(0, 0).unwrap();
        assert_eq!(c.bg, theme.background);
    }

    #[test]
    fn wide_emoji_feed_occupies_two_cols() {
        let mut t = test_terminal(6, 2);
        feed_str(&mut t, "😀");
        let lead = t.grid().cell(0, 0).unwrap();
        assert_eq!(lead.width, 2);
        assert_eq!(lead.ch, '😀');
        assert_eq!(t.grid().cell(1, 0).unwrap().width, 0);
    }

    #[test]
    fn combining_feed_attaches_without_wrap() {
        let mut t = test_terminal(3, 2);
        feed_str(&mut t, "abc");
        // Pending wrap is set (cursor past margin, clamped to last col).
        feed_str(&mut t, "\u{0301}");
        // Combining attaches to `c`, no newline.
        assert_eq!(t.grid().cell(2, 0).unwrap().cluster(), "c\u{0301}");
        assert_eq!(t.grid().cell(0, 1).unwrap().ch, ' ');
    }

    #[test]
    fn zwj_feed_stays_in_one_cell() {
        let mut t = test_terminal(8, 2);
        feed_str(&mut t, "👨\u{200D}👩\u{200D}👧");
        let lead = t.grid().cell(0, 0).unwrap();
        assert_eq!(lead.width, 2);
        assert_eq!(lead.cluster(), "👨\u{200D}👩\u{200D}👧");
    }

    #[test]
    fn decstbm_sets_region_and_homes() {
        let mut t = test_terminal(4, 5);
        feed_str(&mut t, "\x1b[2;4r");
        assert_eq!(t.grid().scroll_region(), (1, 3));
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (0, 0));
        // Empty params reset to full.
        feed_str(&mut t, "\x1b[r");
        assert_eq!(t.grid().scroll_region(), (0, 4));
        // Invalid region is ignored.
        feed_str(&mut t, "\x1b[4;2r");
        assert_eq!(t.grid().scroll_region(), (0, 4));
    }

    #[test]
    fn decom_makes_cup_relative() {
        let mut t = test_terminal(4, 5);
        feed_str(&mut t, "\x1b[2;4r\x1b[?6h");
        assert!(t.grid().origin_mode());
        // CUP 1;1 homes to the region top, not screen top.
        feed_str(&mut t, "\x1b[1;1H");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (0, 1));
        feed_str(&mut t, "\x1b[2;1H");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (0, 2));
        // VPA is relative too.
        feed_str(&mut t, "\x1b[1d");
        assert_eq!(t.grid().cursor().y, 1);
        feed_str(&mut t, "\x1b[?6l");
        assert!(!t.grid().origin_mode());
        feed_str(&mut t, "\x1b[1;1H");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (0, 0));
    }

    #[test]
    fn irm_and_decawm_toggle() {
        let mut t = test_terminal(5, 2);
        assert!(!t.grid().insert_mode());
        assert!(t.grid().auto_wrap());
        feed_str(&mut t, "\x1b[?4h");
        assert!(t.grid().insert_mode());
        feed_str(&mut t, "\x1b[?4l");
        assert!(!t.grid().insert_mode());
        // Non-private SM/RM also drive IRM.
        feed_str(&mut t, "\x1b[4h");
        assert!(t.grid().insert_mode());
        feed_str(&mut t, "\x1b[4l");
        assert!(!t.grid().insert_mode());
        feed_str(&mut t, "\x1b[?7l");
        assert!(!t.grid().auto_wrap());
        feed_str(&mut t, "abcdeF");
        // No wrap: last column overwritten, second row untouched.
        assert_eq!(t.grid().cell(4, 0).unwrap().ch, 'F');
        assert_eq!(t.grid().cell(0, 1).unwrap().ch, ' ');
        feed_str(&mut t, "\x1b[?7h");
        assert!(t.grid().auto_wrap());
    }

    #[test]
    fn reverse_index_inside_region_scrolls_down() {
        let mut t = test_terminal(3, 4);
        feed_str(&mut t, "a\r\nb\r\nc\r\nd");
        feed_str(&mut t, "\x1b[2;3r");
        // Move to region top (screen row 2, 1-based) then reverse-index.
        feed_str(&mut t, "\x1b[2;1H\x1bM");
        assert_eq!(t.grid().cell(0, 1).unwrap().ch, ' ');
        assert_eq!(t.grid().cell(0, 2).unwrap().ch, 'b');
    }

    #[test]
    fn full_reset_clears_margins_and_modes() {
        let mut t = test_terminal(4, 4);
        feed_str(&mut t, "\x1b[2;3r\x1b[?6h\x1b[?4h\x1b[?7l");
        feed_str(&mut t, "\x1bc");
        assert_eq!(t.grid().scroll_region(), (0, 3));
        assert!(!t.grid().origin_mode());
        assert!(!t.grid().insert_mode());
        assert!(t.grid().auto_wrap());
    }

    #[test]
    fn mouse_modes_enable_and_highest_wins() {
        use crate::grid::MouseMode;
        let mut t = test_terminal(5, 3);
        assert_eq!(t.mouse_mode(), MouseMode::Off);
        feed_str(&mut t, "\x1b[?1000h");
        assert_eq!(t.mouse_mode(), MouseMode::Press);
        feed_str(&mut t, "\x1b[?1002h");
        assert_eq!(t.mouse_mode(), MouseMode::Drag);
        feed_str(&mut t, "\x1b[?1003h");
        assert_eq!(t.mouse_mode(), MouseMode::Any);
        // Disabling the top level falls back to the next enabled.
        feed_str(&mut t, "\x1b[?1003l");
        assert_eq!(t.mouse_mode(), MouseMode::Drag);
        feed_str(&mut t, "\x1b[?1002l");
        assert_eq!(t.mouse_mode(), MouseMode::Press);
        feed_str(&mut t, "\x1b[?1000l");
        assert_eq!(t.mouse_mode(), MouseMode::Off);
    }

    #[test]
    fn mouse_sgr_focus_and_sync_modes() {
        let mut t = test_terminal(5, 3);
        assert!(!t.mouse_sgr());
        assert!(!t.focus_report());
        assert!(!t.in_sync());
        feed_str(&mut t, "\x1b[?1006h\x1b[?1004h\x1b[?2026h");
        assert!(t.mouse_sgr());
        assert!(t.focus_report());
        assert!(t.in_sync());
        // Sync nests; two ends are needed to leave.
        feed_str(&mut t, "\x1b[?2026h");
        feed_str(&mut t, "\x1b[?2026l");
        assert!(t.in_sync());
        feed_str(&mut t, "\x1b[?2026l");
        assert!(!t.in_sync());
        feed_str(&mut t, "\x1b[?1006l\x1b[?1004l");
        assert!(!t.mouse_sgr());
        assert!(!t.focus_report());
    }

    #[test]
    fn full_reset_clears_mouse_focus_and_sync() {
        use crate::grid::MouseMode;
        let mut t = test_terminal(5, 3);
        feed_str(&mut t, "\x1b[?1000h\x1b[?1006h\x1b[?1004h\x1b[?2026h");
        assert!(t.in_sync());
        feed_str(&mut t, "\x1bc");
        assert_eq!(t.mouse_mode(), MouseMode::Off);
        assert!(!t.mouse_sgr());
        assert!(!t.focus_report());
        assert!(!t.in_sync());
    }

    #[test]
    fn ich_inserts_blanks_and_shifts_right() {
        let mut t = test_terminal(5, 2);
        feed_str(&mut t, "abcde");
        feed_str(&mut t, "\x1b[1;2H\x1b[2@");
        let row: String = (0..5).map(|x| t.grid().cell(x, 0).unwrap().ch).collect();
        assert_eq!(row, "a  bc");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (1, 0));
    }

    #[test]
    fn dch_deletes_and_shifts_left() {
        let mut t = test_terminal(5, 2);
        feed_str(&mut t, "abcde");
        feed_str(&mut t, "\x1b[1;2H\x1b[2P");
        let row: String = (0..5).map(|x| t.grid().cell(x, 0).unwrap().ch).collect();
        assert_eq!(row, "ade  ");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (1, 0));
    }

    #[test]
    fn ech_erases_without_shifting() {
        let mut t = test_terminal(5, 2);
        feed_str(&mut t, "abcde");
        feed_str(&mut t, "\x1b[1;2H\x1b[2X");
        let row: String = (0..5).map(|x| t.grid().cell(x, 0).unwrap().ch).collect();
        assert_eq!(row, "a  de");
        assert_eq!((t.grid().cursor().x, t.grid().cursor().y), (1, 0));
    }

    #[test]
    fn char_ops_use_pen_bg() {
        let mut t = test_terminal(4, 1);
        let theme = t.theme();
        feed_str(&mut t, "\x1b[41m");
        let red = theme.ansi(1);
        feed_str(&mut t, "abcd");
        feed_str(&mut t, "\x1b[1;1H\x1b[1@");
        assert_eq!(t.grid().cell(0, 0).unwrap().bg, red);
        feed_str(&mut t, "\x1b[1;1H\x1b[1P");
        assert_eq!(t.grid().cell(3, 0).unwrap().bg, red);
        feed_str(&mut t, "\x1b[1;1H\x1b[1X");
        assert_eq!(t.grid().cell(0, 0).unwrap().bg, red);
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, ' ');
    }

    #[test]
    fn rep_repeats_last_graphic() {
        let mut t = test_terminal(6, 1);
        feed_str(&mut t, "a\x1b[3b");
        let row: String = (0..6).map(|x| t.grid().cell(x, 0).unwrap().ch).collect();
        assert_eq!(&row[..4], "aaaa");
        // No predecessor: no-op, no panic.
        let mut t = test_terminal(3, 1);
        feed_str(&mut t, "\x1b[3b");
        assert_eq!(t.grid().cell(0, 0).unwrap().ch, ' ');
        assert!(t.take_response().is_empty());
    }

    #[test]
    fn da_dsr_and_cpr_reply() {
        let mut t = test_terminal(5, 3);
        feed_str(&mut t, "\x1b[c");
        assert_eq!(t.take_response(), b"\x1b[?1;2c");
        feed_str(&mut t, "\x1b[>c");
        assert_eq!(t.take_response(), b"\x1b[>0;95;0c");
        feed_str(&mut t, "\x1b[5n");
        assert_eq!(t.take_response(), b"\x1b[0n");
        feed_str(&mut t, "\x1b[2;3H\x1b[6n");
        assert_eq!(t.take_response(), b"\x1b[2;3R");
        // Extended CPR form.
        feed_str(&mut t, "\x1b[?6n");
        assert_eq!(t.take_response(), b"\x1b[?2;3R");
    }

    #[test]
    fn cpr_is_origin_relative() {
        let mut t = test_terminal(4, 5);
        feed_str(&mut t, "\x1b[2;4r\x1b[?6h\x1b[1;1H\x1b[6n");
        assert_eq!(t.take_response(), b"\x1b[1;1R");
    }

    #[test]
    fn decrqm_reports_set_reset_and_unknown() {
        let mut t = test_terminal(5, 3);
        feed_str(&mut t, "\x1b[?25l\x1b[?25$p");
        assert_eq!(t.take_response(), b"\x1b[?25;2$y");
        feed_str(&mut t, "\x1b[?25h\x1b[?25$p");
        assert_eq!(t.take_response(), b"\x1b[?25;1$y");
        feed_str(&mut t, "\x1b[?9999$p");
        assert_eq!(t.take_response(), b"\x1b[?9999;0$y");
        // ANSI (non-private) IRM.
        feed_str(&mut t, "\x1b[4$p");
        assert_eq!(t.take_response(), b"\x1b[4;2$y");
        feed_str(&mut t, "\x1b[4h\x1b[4$p");
        assert_eq!(t.take_response(), b"\x1b[4;1$y");
    }

    /// Full observable state after a feed: cells plus the side channels
    /// (`take_*` drain, so each snapshot owns its copy).
    #[derive(Debug, PartialEq)]
    struct FeedSnapshot {
        cells: Vec<Vec<crate::grid::Cell>>,
        title: String,
        cwd: Option<String>,
        clipboard: Vec<ClipboardOp>,
        response: Vec<u8>,
    }

    fn snapshot_after(t: &mut Terminal) -> FeedSnapshot {
        let cells = (0..t.rows())
            .map(|y| {
                (0..t.cols())
                    .map(|x| t.grid().cell(x, y).unwrap())
                    .collect()
            })
            .collect();
        FeedSnapshot {
            cells,
            title: t.title().to_string(),
            cwd: t.cwd().map(|s| s.to_string()),
            clipboard: t.take_clipboard(),
            response: t.take_response(),
        }
    }

    #[test]
    fn split_feeds_match_whole_feeds_at_every_offset() {
        // The 8KB PTY read boundary can cut an escape sequence anywhere:
        // every split point must render exactly like the one-shot feed.
        // Covers CSI, SGR truecolor, OSC title/cwd/hyperlink/clipboard,
        // ESC singles, alt-screen, and multibyte UTF-8 cut mid-char.
        let cases: &[&[u8]] = &[
            b"\x1b[2;3H",
            b"\x1b[38;2;10;20;30mX",
            b"\x1b]0;nvim | ~/dev\x07",
            b"\x1b]7;file://localhost/Users/x/dev\x07",
            b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\",
            b"\x1b]52;c;aGk=\x07",
            b"\x1bM\x1b7ab\x1b8",
            b"\x1b[?1049hX\x1b[?1049l",
            b"\x1b[1;31mhi\x1b]0;ti;tle\x07\x1b[0m",
            "é中\x1b[31m✓ok".as_bytes(),
        ];
        for seq in cases {
            let mut reference = test_terminal(10, 5);
            reference.feed(seq);
            let reference = snapshot_after(&mut reference);
            for split in 1..seq.len() {
                let mut t = test_terminal(10, 5);
                t.feed(&seq[..split]);
                t.feed(&seq[split..]);
                assert_eq!(
                    snapshot_after(&mut t),
                    reference,
                    "seq={seq:?} split={split}"
                );
            }
        }
    }

    #[test]
    fn byte_at_a_time_feed_matches() {
        // Degenerate chunking: one byte per `feed` must still converge.
        let seq = b"\x1b[1;31mhi\x1b]0;title\x07\x1b[0m\x1b[2;3H";
        let mut reference = test_terminal(10, 5);
        reference.feed(seq);
        let reference = snapshot_after(&mut reference);
        let mut t = test_terminal(10, 5);
        for b in seq.iter() {
            t.feed(std::slice::from_ref(b));
        }
        assert_eq!(snapshot_after(&mut t), reference);
    }
}
