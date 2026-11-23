//! `OSC 7501` Program Status Protocol (revision 0.4).
//!
//! A program reports what it is doing (`idle`, `working`, `blocked`,
//! `done`, `error`) as `key=value` pairs separated by `:`. Each terminal
//! keeps one record per hierarchical `id`; the root record has no id.
//! Presentation is left to the terminal: cometty counts records per tab
//! (see [`StatusSummary`]) and raises the attention glow on transitions.
//!
//! Pure and toolkit-free: [`parse`] validates one report against every
//! limit before [`ProgramStatus::apply`] touches a record, so a rejected
//! report never changes state. `msg`/`title` are validated (base64, size,
//! no control characters) but not retained: no surface shows them yet, and
//! keeping them would add up to ~0.5 MiB of untrusted text per tab.

use base64::Engine as _;

/// Largest whole sequence the protocol allows.
pub const MAX_SEQUENCE_BYTES: usize = 4096;
/// `ESC ] 7501 ;` plus an `ESC \` terminator around the payload.
const FRAMING_BYTES: usize = 9;
/// Largest payload (after `7501;`) accepted; longer reports are discarded
/// before their bytes are copied.
pub const MAX_PAYLOAD_BYTES: usize = MAX_SEQUENCE_BYTES - FRAMING_BYTES;
const MAX_KEY_BYTES: usize = 16;
const MAX_MSG_ENCODED: usize = 2732;
const MAX_MSG_DECODED: usize = 2048;
const MAX_TITLE_ENCODED: usize = 256;
const MAX_TITLE_DECODED: usize = 192;
const MAX_ID_BYTES: usize = 128;
const MAX_SEGMENT_BYTES: usize = 32;
const MAX_SEGMENTS: usize = 8;
/// Records kept per terminal; past the cap the least recently updated
/// record makes way.
pub const MAX_RECORDS: usize = 256;

/// Answer to `OSC 7501 ; ?`. Without `states`/`kinds` lists it means
/// exactly the five states and three kinds of the specification. It never
/// carries anything taken from a report.
pub const QUERY_REPLY: &[u8] = b"\x1b]7501;?\x1b\\";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Working,
    Blocked,
    Done,
    Error,
}

impl State {
    fn parse(value: &[u8]) -> Option<Self> {
        Some(match value {
            b"idle" => Self::Idle,
            b"working" => Self::Working,
            b"blocked" => Self::Blocked,
            b"done" => Self::Done,
            b"error" => Self::Error,
            _ => return None,
        })
    }

    /// Entering this state is worth the user's attention.
    fn wants_attention(self) -> bool {
        matches!(self, Self::Blocked | Self::Done | Self::Error)
    }
}

/// One parsed report.
#[derive(Debug, PartialEq, Eq)]
pub enum Report {
    /// Replace record `id` (empty = root) with `state`.
    Set { id: String, state: State },
    /// Remove record `id` and everything beneath it; `None` removes all.
    Clear { id: Option<String> },
}

/// A valid `OSC 7501` payload.
#[derive(Debug, PartialEq, Eq)]
pub enum Message {
    /// `OSC 7501 ; ?`: the program asks whether the protocol is supported.
    Query,
    Report(Report),
}

/// Why a report was ignored or discarded, for the debug log. Never carries
/// report text: only which rule and which key failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    Oversized,
    NoState,
    UnknownState,
    BadId,
    /// `title` or `msg`, and the check it failed.
    BadText(&'static str, TextFault),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextFault {
    EncodedTooLong,
    NotBase64,
    DecodedTooLong,
    NotUtf8,
    Control,
}

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Oversized => write!(f, "over {MAX_PAYLOAD_BYTES} bytes"),
            Self::NoState => write!(f, "no valid `state` pair"),
            Self::UnknownState => write!(f, "unknown state"),
            Self::BadId => write!(f, "invalid `id`"),
            Self::BadText(key, fault) => {
                let why = match fault {
                    TextFault::EncodedTooLong => "encoded value too long",
                    TextFault::NotBase64 => "not base64",
                    TextFault::DecodedTooLong => "decoded text too long",
                    TextFault::NotUtf8 => "not UTF-8",
                    TextFault::Control => "contains a control character",
                };
                write!(f, "`{key}` {why}")
            }
        }
    }
}

/// Parse one `OSC 7501` payload (the bytes after `7501;`).
///
/// Rejects anything the specification says to ignore or discard whole: a
/// missing or unknown `state`, a size limit broken, invalid base64, text
/// decoding to a control character, or an `id` with a disallowed byte.
/// Malformed pairs are skipped, unknown keys ignored, and the last of a
/// repeated key wins.
pub fn parse(payload: &[u8]) -> Result<Message, Reject> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(Reject::Oversized);
    }
    if payload.trim_ascii() == b"?" {
        return Ok(Message::Query);
    }
    let mut state = None;
    let mut id = None;
    let mut title = None;
    let mut msg = None;
    for pair in payload.split(|&b| b == b':') {
        let Some(eq) = pair.iter().position(|&b| b == b'=') else {
            continue;
        };
        let key = pair[..eq].trim_ascii();
        let value = pair[eq + 1..].trim_ascii();
        if key.is_empty()
            || key.len() > MAX_KEY_BYTES
            || !key.iter().all(u8::is_ascii_lowercase)
            || !value.iter().copied().all(is_value_byte)
        {
            continue;
        }
        match key {
            b"state" => state = Some(value),
            b"id" => id = Some(value),
            b"title" => title = Some(value),
            b"msg" => msg = Some(value),
            // `kind`, `progress` and `app` only refine presentation that
            // cometty does not show; their invalid values count as none.
            _ => {}
        }
    }
    let state = state.ok_or(Reject::NoState)?;
    let id = match id {
        Some(id) => Some(valid_id(id).ok_or(Reject::BadId)?),
        None => None,
    };
    if let Some(title) = title {
        valid_text(title, MAX_TITLE_ENCODED, MAX_TITLE_DECODED)
            .map_err(|fault| Reject::BadText("title", fault))?;
    }
    if let Some(msg) = msg {
        valid_text(msg, MAX_MSG_ENCODED, MAX_MSG_DECODED)
            .map_err(|fault| Reject::BadText("msg", fault))?;
    }
    let report = if state == b"clear" {
        Report::Clear {
            id: id.filter(|id| !id.is_empty()),
        }
    } else {
        Report::Set {
            id: id.unwrap_or_default(),
            state: State::parse(state).ok_or(Reject::UnknownState)?,
        }
    };
    Ok(Message::Report(report))
}

/// Value alphabet: letters, digits and `_ . , + / = -`, which leaves out
/// `:` and `;` so nothing ever needs escaping.
fn is_value_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b',' | b'+' | b'/' | b'=' | b'-')
}

fn is_segment_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-')
}

/// A path of up to 8 `/`-separated segments of 1–32 bytes, 128 in total.
/// An empty value addresses the root.
fn valid_id(id: &[u8]) -> Option<String> {
    if id.is_empty() {
        return Some(String::new());
    }
    if id.len() > MAX_ID_BYTES {
        return None;
    }
    let mut segments = 0;
    for segment in id.split(|&b| b == b'/') {
        segments += 1;
        if segments > MAX_SEGMENTS
            || segment.is_empty()
            || segment.len() > MAX_SEGMENT_BYTES
            || !segment.iter().copied().all(is_segment_byte)
        {
            return None;
        }
    }
    // Every byte is ASCII after the segment check.
    String::from_utf8(id.to_vec()).ok()
}

/// Free text is base64 of UTF-8 (padding optional) that must not decode
/// to any control character, not even a newline or tab.
fn valid_text(encoded: &[u8], max_encoded: usize, max_decoded: usize) -> Result<(), TextFault> {
    if encoded.len() > max_encoded {
        return Err(TextFault::EncodedTooLong);
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(encoded))
        .map_err(|_| TextFault::NotBase64)?;
    if decoded.len() > max_decoded {
        return Err(TextFault::DecodedTooLong);
    }
    let text = std::str::from_utf8(&decoded).map_err(|_| TextFault::NotUtf8)?;
    if text.chars().any(char::is_control) {
        return Err(TextFault::Control);
    }
    Ok(())
}

/// Record counts by state, for the tab badge. When a program reports child
/// records the root is its summary of them, so only children are counted;
/// a program reporting just its own state is counted through the root.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatusSummary {
    pub blocked: usize,
    pub working: usize,
    pub idle: usize,
    pub done: usize,
    pub error: usize,
}

struct Entry {
    /// Empty for the root record.
    id: String,
    state: State,
    /// Logical clock of the last report, for least-recently-updated eviction.
    updated: u64,
}

/// Per-terminal record set. Records belong to the terminal, not the screen:
/// switching to the alternate screen keeps them; RIS removes them all.
#[derive(Default)]
pub struct ProgramStatus {
    entries: Vec<Entry>,
    clock: u64,
    /// Bumped on every change, so callers can detect one without diffing.
    version: u64,
}

impl ProgramStatus {
    /// Apply one report. Returns true when a record *entered* `blocked`,
    /// `done` or `error` (an attention transition); repeating the same
    /// state is not a transition.
    pub fn apply(&mut self, report: Report) -> bool {
        match report {
            Report::Set { id, state } => {
                self.clock += 1;
                self.version += 1;
                if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
                    let previous = entry.state;
                    entry.state = state;
                    entry.updated = self.clock;
                    return state.wants_attention() && previous != state;
                }
                if self.entries.len() >= MAX_RECORDS
                    && let Some(oldest) = self
                        .entries
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, e)| e.updated)
                        .map(|(i, _)| i)
                {
                    self.entries.swap_remove(oldest);
                }
                self.entries.push(Entry {
                    id,
                    state,
                    updated: self.clock,
                });
                state.wants_attention()
            }
            Report::Clear { id: None } => {
                self.clear();
                false
            }
            Report::Clear { id: Some(id) } => {
                let addressed = |e: &Entry| {
                    e.id == id
                        || (e.id.len() > id.len()
                            && e.id.starts_with(id.as_str())
                            && e.id.as_bytes()[id.len()] == b'/')
                };
                // Inferred result: programs such as opencode clear a task's
                // record when it stops instead of reporting `done`. A child
                // that was `working` becomes `done` (unread until the tab is
                // seen); anything else addressed goes away, e.g. a `blocked`
                // record the user just answered.
                self.clock += 1;
                let clock = self.clock;
                let mut finished = false;
                for entry in self.entries.iter_mut().filter(|e| addressed(e)) {
                    if entry.state == State::Working {
                        entry.state = State::Done;
                        entry.updated = clock;
                        finished = true;
                    }
                }
                if finished {
                    self.version += 1;
                }
                self.retain(|e| !addressed(e) || (e.state == State::Done && e.updated == clock));
                finished
            }
        }
    }

    /// The foreground job ended: `working` and `blocked` records must go;
    /// `idle` ones are dropped too so a finished program leaves no badge.
    /// `done` and `error` stay for the user to find.
    pub fn end_job(&mut self) -> bool {
        self.retain(|e| matches!(e.state, State::Done | State::Error))
    }

    /// The user has seen the tab: its `done` and `error` results are read.
    pub fn acknowledge(&mut self) -> bool {
        self.retain(|e| !matches!(e.state, State::Done | State::Error))
    }

    /// Remove every record (RIS, a clear without id, replayed dumps).
    pub fn clear(&mut self) -> bool {
        self.retain(|_| false)
    }

    fn retain(&mut self, keep: impl FnMut(&Entry) -> bool) -> bool {
        let before = self.entries.len();
        self.entries.retain(keep);
        let changed = self.entries.len() != before;
        if changed {
            self.version += 1;
        }
        changed
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn summary(&self) -> StatusSummary {
        let children = self.entries.iter().any(|e| !e.id.is_empty());
        let mut summary = StatusSummary::default();
        // A lone idle root means "nothing pending" (opencode with no busy
        // session), not an idle task worth a permanent badge.
        for entry in self.entries.iter().filter(|e| {
            if e.id.is_empty() {
                !children && e.state != State::Idle
            } else {
                true
            }
        }) {
            match entry.state {
                State::Idle => summary.idle += 1,
                State::Working => summary.working += 1,
                State::Blocked => summary.blocked += 1,
                State::Done => summary.done += 1,
                State::Error => summary.error += 1,
            }
        }
        summary
    }

    #[allow(dead_code)] // Inspection helper for tests.
    pub fn state(&self, id: &str) -> Option<State> {
        self.entries.iter().find(|e| e.id == id).map(|e| e.state)
    }

    #[allow(dead_code)] // Inspection helper for tests.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(text: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(text)
    }

    fn set(id: &str, state: State) -> Result<Message, Reject> {
        Ok(Message::Report(Report::Set {
            id: id.into(),
            state,
        }))
    }

    fn text_fault(key: &'static str, fault: TextFault) -> Result<Message, Reject> {
        Err(Reject::BadText(key, fault))
    }

    #[test]
    fn parses_states_ids_and_the_query() {
        assert_eq!(parse(b"?"), Ok(Message::Query));
        assert_eq!(parse(b" ? "), Ok(Message::Query));
        assert_eq!(parse(b"state=idle:app=opencode"), set("", State::Idle));
        assert_eq!(
            parse(b"state=blocked:kind=permission:id=ses_1:progress=40"),
            set("ses_1", State::Blocked)
        );
        assert_eq!(
            parse(b" state = working : id = a/b "),
            set("a/b", State::Working)
        );
        assert_eq!(
            parse(b"state=clear:id=build"),
            Ok(Message::Report(Report::Clear {
                id: Some("build".into())
            }))
        );
        assert_eq!(
            parse(b"state=clear"),
            Ok(Message::Report(Report::Clear { id: None }))
        );
    }

    /// Byte-exact reports from OpenTUI's own encoder tests
    /// (`renderer.program-status.test.ts`), which opencode uses.
    #[test]
    fn accepts_opentui_encoded_reports() {
        assert_eq!(
            parse(
                b"state=blocked:id=eu-west:kind=permission:progress=10:app=deploy:\
                  title=5LiW55WM:msg=QXBwcm92ZSBkZXBsb3kgdG8gZXUtd2VzdCAocHJvZHVjdGlvbik/"
            ),
            set("eu-west", State::Blocked)
        );
        assert_eq!(
            parse(b"state=working:id=build"),
            set("build", State::Working)
        );
        // opencode's per-session shapes: unpadded base64 title, generic msg.
        let blocked = format!(
            "state=blocked:id=ses_01JXYZabcdefghij0123456789:kind=question:app=opencode:title={}:msg={}",
            b64("Fix the parser…").trim_end_matches('='),
            b64("Input needs response").trim_end_matches('='),
        );
        assert_eq!(
            parse(blocked.as_bytes()),
            set("ses_01JXYZabcdefghij0123456789", State::Blocked)
        );
    }

    #[test]
    fn ignores_reports_without_a_known_state() {
        assert_eq!(parse(b"app=cargo"), Err(Reject::NoState));
        assert_eq!(parse(b"state=sleeping"), Err(Reject::UnknownState));
        assert_eq!(parse(b"state="), Err(Reject::UnknownState));
        assert_eq!(parse(b""), Err(Reject::NoState));
    }

    #[test]
    fn skips_malformed_pairs_and_unknown_keys_and_keeps_the_last_duplicate() {
        assert_eq!(
            parse(b"junk:state=working:Bad=1:x=a;b:future=yes"),
            set("", State::Working)
        );
        assert_eq!(parse(b"state=idle:state=done"), set("", State::Done));
        // A key longer than 16 bytes is malformed, not fatal.
        assert_eq!(
            parse(b"abcdefghijklmnopq=1:state=idle"),
            set("", State::Idle)
        );
    }

    #[test]
    fn invalid_ids_discard_the_whole_report() {
        let bad = Err(Reject::BadId);
        let long_segment = format!("state=idle:id={}", "a".repeat(33));
        assert_eq!(parse(long_segment.as_bytes()), bad);
        let deep = format!("state=idle:id={}", ["a"; 9].join("/"));
        assert_eq!(parse(deep.as_bytes()), bad);
        let wide = format!("state=idle:id={}", vec!["a".repeat(32); 4].join("/"));
        assert_eq!(parse(wide.as_bytes()), bad, "over 128 bytes");
        assert_eq!(parse(b"state=idle:id=a//b"), bad);
        assert_eq!(parse(b"state=idle:id=a,b"), bad);
        let ok = format!("state=idle:id={}", ["a"; 8].join("/"));
        assert!(parse(ok.as_bytes()).is_ok());
    }

    #[test]
    fn free_text_is_validated_before_any_record_changes() {
        let ok = format!("state=done:msg={}:title={}", b64("Docs built"), b64("make"));
        assert_eq!(parse(ok.as_bytes()), set("", State::Done));
        let unpadded = format!("state=done:msg={}", b64("hi").trim_end_matches('='));
        assert_eq!(parse(unpadded.as_bytes()), set("", State::Done));
        // Control characters, even a newline, discard the report.
        let newline = format!("state=done:msg={}", b64("a\nb"));
        assert_eq!(
            parse(newline.as_bytes()),
            text_fault("msg", TextFault::Control)
        );
        let c1 = format!("state=done:title={}", b64("a\u{9b}b"));
        assert_eq!(
            parse(c1.as_bytes()),
            text_fault("title", TextFault::Control)
        );
        assert_eq!(
            parse(b"state=done:msg=a"),
            text_fault("msg", TextFault::NotBase64)
        );
        let latin1 = format!(
            "state=done:msg={}",
            base64::engine::general_purpose::STANDARD.encode([0xe9])
        );
        assert_eq!(
            parse(latin1.as_bytes()),
            text_fault("msg", TextFault::NotUtf8)
        );
        // A byte outside the value alphabet makes the pair malformed, which
        // only skips it.
        assert_eq!(parse(b"state=done:msg=!!!"), set("", State::Done));
        let long_title = format!("state=done:title={}", b64(&"t".repeat(193)));
        assert_eq!(
            parse(long_title.as_bytes()),
            text_fault("title", TextFault::EncodedTooLong)
        );
        // 2049 bytes encode to exactly the 2732-byte encoded limit.
        let long_msg = format!("state=done:msg={}", b64(&"m".repeat(2049)));
        assert_eq!(
            parse(long_msg.as_bytes()),
            text_fault("msg", TextFault::DecodedTooLong)
        );
        let longer_msg = format!("state=done:msg={}", b64(&"m".repeat(2052)));
        assert_eq!(
            parse(longer_msg.as_bytes()),
            text_fault("msg", TextFault::EncodedTooLong)
        );
        let max_msg = format!("state=done:msg={}", b64(&"m".repeat(2048)));
        assert!(parse(max_msg.as_bytes()).is_ok());
        assert_eq!(
            Reject::BadText("msg", TextFault::NotBase64).to_string(),
            "`msg` not base64"
        );
    }

    #[test]
    fn oversized_payloads_are_discarded() {
        let mut payload = b"state=idle:x=".to_vec();
        payload.resize(MAX_PAYLOAD_BYTES + 1, b'a');
        assert_eq!(parse(&payload), Err(Reject::Oversized));
        payload.truncate(MAX_PAYLOAD_BYTES);
        assert_eq!(parse(&payload), set("", State::Idle));
    }

    #[test]
    fn apply_reports_attention_transitions_only() {
        let mut status = ProgramStatus::default();
        let report = |id: &str, state| Report::Set {
            id: id.into(),
            state,
        };
        assert!(!status.apply(report("", State::Working)));
        assert!(status.apply(report("", State::Blocked)));
        assert!(!status.apply(report("", State::Blocked)), "same state");
        assert!(status.apply(report("", State::Done)));
        assert!(status.apply(report("child", State::Error)));
        assert_eq!(status.state(""), Some(State::Done));
        assert_eq!(status.state("child"), Some(State::Error));
    }

    #[test]
    fn clear_removes_a_subtree_or_everything() {
        let mut status = ProgramStatus::default();
        for id in ["", "build", "build/test", "buildx", "deploy"] {
            status.apply(Report::Set {
                id: id.into(),
                state: State::Idle,
            });
        }
        assert!(!status.apply(Report::Clear {
            id: Some("build".into()),
        }));
        assert_eq!(status.state("build"), None);
        assert_eq!(status.state("build/test"), None);
        assert_eq!(status.state("buildx"), Some(State::Idle));
        assert_eq!(status.state(""), Some(State::Idle));
        let version = status.version();
        status.apply(Report::Clear { id: None });
        assert_eq!(status.len(), 0);
        assert!(status.version() > version);
        // Clearing nothing is not a change.
        let version = status.version();
        status.apply(Report::Clear { id: None });
        assert_eq!(status.version(), version);
    }

    #[test]
    fn clearing_a_working_child_infers_a_finished_result() {
        let mut status = ProgramStatus::default();
        let set = |status: &mut ProgramStatus, id: &str, state| {
            status.apply(Report::Set {
                id: id.into(),
                state,
            })
        };
        set(&mut status, "", State::Working);
        set(&mut status, "ses_a", State::Working);
        set(&mut status, "ses_b", State::Blocked);
        set(&mut status, "ses_c", State::Done);
        // opencode clears a session that stopped working: it finished.
        assert!(status.apply(Report::Clear {
            id: Some("ses_a".into())
        }));
        assert_eq!(status.state("ses_a"), Some(State::Done));
        // An answered prompt or an explicitly cleared result just goes.
        assert!(!status.apply(Report::Clear {
            id: Some("ses_b".into())
        }));
        assert_eq!(status.state("ses_b"), None);
        assert!(!status.apply(Report::Clear {
            id: Some("ses_c".into())
        }));
        assert_eq!(status.state("ses_c"), None);
        // Clearing the inferred result again removes it.
        assert!(!status.apply(Report::Clear {
            id: Some("ses_a".into())
        }));
        assert_eq!(status.state("ses_a"), None);
        // A new report replaces an inferred result; seeing the tab reads it.
        set(&mut status, "ses_d", State::Working);
        status.apply(Report::Clear {
            id: Some("ses_d".into()),
        });
        set(&mut status, "ses_d", State::Working);
        assert_eq!(status.state("ses_d"), Some(State::Working));
        status.apply(Report::Clear {
            id: Some("ses_d".into()),
        });
        assert!(status.acknowledge());
        assert_eq!(status.state("ses_d"), None);
        // Quitting (a clear without id) leaves nothing behind.
        set(&mut status, "ses_e", State::Working);
        status.apply(Report::Clear {
            id: Some("ses_e".into()),
        });
        status.apply(Report::Clear { id: None });
        assert_eq!(status.len(), 0);
    }

    #[test]
    fn evicts_the_least_recently_updated_record_past_the_cap() {
        let mut status = ProgramStatus::default();
        for i in 0..MAX_RECORDS {
            status.apply(Report::Set {
                id: format!("r{i}"),
                state: State::Idle,
            });
        }
        // Refresh r0 so r1 becomes the oldest.
        status.apply(Report::Set {
            id: "r0".into(),
            state: State::Working,
        });
        status.apply(Report::Set {
            id: "new".into(),
            state: State::Idle,
        });
        assert_eq!(status.len(), MAX_RECORDS);
        assert_eq!(status.state("r1"), None);
        assert_eq!(status.state("r0"), Some(State::Working));
        assert_eq!(status.state("new"), Some(State::Idle));
    }

    #[test]
    fn job_end_and_acknowledge_follow_the_record_lifetime() {
        let mut status = ProgramStatus::default();
        for (id, state) in [
            ("a", State::Idle),
            ("b", State::Working),
            ("c", State::Blocked),
            ("d", State::Done),
            ("e", State::Error),
        ] {
            status.apply(Report::Set {
                id: id.into(),
                state,
            });
        }
        assert!(status.end_job());
        assert_eq!(status.len(), 2);
        assert!(!status.end_job(), "nothing left to drop");
        assert!(status.acknowledge());
        assert_eq!(status.len(), 0);
    }

    #[test]
    fn summary_counts_children_when_present_else_the_root() {
        let mut status = ProgramStatus::default();
        status.apply(Report::Set {
            id: String::new(),
            state: State::Idle,
        });
        // A lone idle root is "nothing pending", not a badge.
        assert_eq!(status.summary(), StatusSummary::default());
        status.apply(Report::Set {
            id: String::new(),
            state: State::Working,
        });
        assert_eq!(
            status.summary(),
            StatusSummary {
                working: 1,
                ..Default::default()
            }
        );
        // opencode: a root summary plus one record per pending session.
        status.apply(Report::Set {
            id: String::new(),
            state: State::Blocked,
        });
        for (id, state) in [
            ("ses_a", State::Blocked),
            ("ses_b", State::Working),
            ("ses_c", State::Working),
        ] {
            status.apply(Report::Set {
                id: id.into(),
                state,
            });
        }
        assert_eq!(
            status.summary(),
            StatusSummary {
                blocked: 1,
                working: 2,
                ..Default::default()
            }
        );
        assert_eq!(ProgramStatus::default().summary(), StatusSummary::default());
    }
}
