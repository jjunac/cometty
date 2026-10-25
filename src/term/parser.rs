//! A bounded OSC integration for vte 0.15's growable `std` parser.
//!
//! vte's const-generic OSC size only applies without `std`. Track just the
//! transitions into/out of OSC, and stop forwarding *before* its Vec can grow
//! past our limit. Everything else, including UTF-8, stays with vte. This is a
//! projection of vte's states, not a second CSI/DCS/UTF-8 parser: ESC enters
//! Escape from every state, and only Escape + `]` enters OscString.

use vte::Perform;

/// Wire bytes after `ESC ]`, including the number/separators/ignored C0s but
/// excluding the terminator. Fits a 1 MiB OSC 52 transfer, base64 and wrapping.
pub(super) const MAX_RAW_OSC_BYTES: usize = 2 << 20;
/// Small strings reuse vte's allocation; exceptional strings don't pin it.
const OSC_REUSE_BYTES: usize = 64 << 10;

#[derive(Clone, Copy)]
enum State {
    /// Any vte state other than Escape or OscString.
    Other,
    Escape,
    Osc(usize),
    Discard,
}

pub(super) struct BoundedParser {
    parser: vte::Parser,
    state: State,
    overflow_warned: bool,
}

impl BoundedParser {
    pub(super) fn new() -> Self {
        Self {
            parser: vte::Parser::new(),
            state: State::Other,
            overflow_warned: false,
        }
    }

    pub(super) fn advance<P: Perform>(&mut self, performer: &mut P, bytes: &[u8]) {
        self.advance_bounded::<MAX_RAW_OSC_BYTES, OSC_REUSE_BYTES, P>(performer, bytes);
    }

    // Const limits let tests exhaust every split around a small boundary using
    // the very same implementation as the production two-megabyte boundary.
    fn advance_bounded<const MAX: usize, const REUSE: usize, P: Perform>(
        &mut self,
        performer: &mut P,
        mut bytes: &[u8],
    ) {
        while !bytes.is_empty() {
            match self.state {
                State::Other => {
                    // Keep vte's bulk/SIMD ground-state path, not bytewise feeds.
                    let end = memchr::memchr(0x1b, bytes).map_or(bytes.len(), |i| i + 1);
                    self.parser.advance(performer, &bytes[..end]);
                    if bytes[end - 1] == 0x1b {
                        self.state = State::Escape;
                    }
                    bytes = &bytes[end..];
                }
                State::Escape => {
                    let byte = bytes[0];
                    self.parser.advance(performer, &bytes[..1]);
                    self.state = match byte {
                        b']' => State::Osc(0),
                        // Intermediates leave Escape too: ESC SP ] is *not* OSC.
                        0x18 | 0x1a | 0x20..=0x7e => State::Other,
                        // C0s execute but leave Escape active; DEL/high bytes
                        // are ignored there, and repeated ESC stays Escape.
                        _ => State::Escape,
                    };
                    bytes = &bytes[1..];
                }
                State::Osc(len) => {
                    let available = MAX - len;
                    let scan = &bytes[..bytes.len().min(available + 1)];
                    let end = scan.iter().position(|&b| osc_end(b));
                    let body = end.unwrap_or(scan.len());
                    let take = body.min(available);
                    self.parser.advance(performer, &bytes[..take]);
                    bytes = &bytes[take..];
                    if body > available {
                        // Do not synthesize a terminator: vte would dispatch
                        // the partial title/link/clipboard. Drop it outright.
                        self.parser = vte::Parser::new();
                        self.state = State::Discard;
                        if !self.overflow_warned {
                            self.overflow_warned = true;
                            log::warn!(
                                "OSC ignored: raw sequence exceeds {MAX} bytes (further overflows suppressed)"
                            );
                        }
                    } else if end.is_some() {
                        let byte = bytes[0];
                        self.parser.advance(performer, &bytes[..1]);
                        self.state = after_osc(byte);
                        if len + take > REUSE {
                            self.parser = vte::Parser::new();
                            if byte == 0x1b {
                                // ESC ends OSC *and* starts the next escape.
                                // Replay only that transition, not a callback.
                                self.parser.advance(performer, b"\x1b");
                            }
                        }
                        bytes = &bytes[1..];
                    } else {
                        self.state = State::Osc(len + take);
                    }
                }
                State::Discard => {
                    let Some(end) = bytes.iter().position(|&b| osc_end(b)) else {
                        return;
                    };
                    bytes = &bytes[end..];
                    let byte = bytes[0];
                    // Match vte's cancellation/escape transitions, but BEL
                    // must not become an execute callback outside the OSC.
                    if byte != 0x07 {
                        self.parser.advance(performer, &bytes[..1]);
                    }
                    self.state = after_osc(byte);
                    bytes = &bytes[1..];
                }
            }
        }
    }
}

fn osc_end(byte: u8) -> bool {
    // vte 0.15 ends OSC on *any* ESC, not just ESC \\. Raw 0x9c is data,
    // including when it is a UTF-8 continuation byte; never split on it.
    matches!(byte, 0x07 | 0x18 | 0x1a | 0x1b)
}

fn after_osc(byte: u8) -> State {
    if byte == 0x1b {
        State::Escape
    } else {
        State::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vte::Params;

    #[derive(Default, Debug, PartialEq, Eq)]
    struct Trace(Vec<String>);

    impl Perform for Trace {
        fn print(&mut self, c: char) {
            self.0.push(format!("print {c:?}"));
        }
        fn execute(&mut self, byte: u8) {
            self.0.push(format!("execute {byte}"));
        }
        fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
            self.0.push(format!(
                "hook {params:?} {intermediates:?} {ignore} {action}"
            ));
        }
        fn put(&mut self, byte: u8) {
            self.0.push(format!("put {byte}"));
        }
        fn unhook(&mut self) {
            self.0.push("unhook".into());
        }
        fn osc_dispatch(&mut self, params: &[&[u8]], bell: bool) {
            self.0.push(format!("osc {params:?} {bell}"));
        }
        fn csi_dispatch(
            &mut self,
            params: &Params,
            intermediates: &[u8],
            ignore: bool,
            action: char,
        ) {
            self.0.push(format!(
                "csi {params:?} {intermediates:?} {ignore} {action}"
            ));
        }
        fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
            self.0
                .push(format!("esc {intermediates:?} {ignore} {byte}"));
        }
    }

    fn reference(bytes: &[u8]) -> Trace {
        let mut trace = Trace::default();
        vte::Parser::new().advance(&mut trace, bytes);
        trace
    }

    #[test]
    fn below_limit_matches_vte_callbacks_at_every_split() {
        let cases: &[&[u8]] = &[
            b"\xe4\xb8\xad\xf0\x9f\x92\xa5\xe4\x1b]2;title\x07\xff\x1b[31mX",
            b"\x1b\x00\x7f\xff]2;title\x07\x1b\x1b]7;file:///a\x1b\\Z",
            b"\x1b ]not osc\x07\x1b[?123;4h\x1b[99:2;3m\x1b]2;a;b;c\x07",
            b"\x1bP1;2qDCS ]payload\x07\x1b]2;from dcs\x1b\\after",
            b"\x1bXignored ]\x07\x1b]2;sos\x18\x1b^pm\x1b]2;x\x1a\x1b_apc\x1b]2;y\x07",
            b"\x1b]2;\xc2\x9c\x9c\xe4\xb8\xad\x00\x0a;abc\x1b[31mX\x1b]52;c;aGk=\x07",
            b"\x1b]2;cancel\x18ok\x1b]2;sub\x1aok\x1b]2;esc\x1bc\x1b]2;next\x07",
        ];
        for &bytes in cases {
            assert_all_splits(bytes);
        }
        // Deterministic mixed high bytes/controls, biased toward ESC and OSC
        // entry, to exercise states outside the supported terminal handlers.
        let alphabet = b"\x00\x07\x18\x1a\x1b\x1b\x1b][P_X^;? qabc\\\x7f\x80\x9c\xc2\xe4\xff";
        let mut seed = 7u32;
        for _ in 0..16 {
            let mut bytes = Vec::new();
            for _ in 0..64 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                bytes.push(alphabet[seed as usize % alphabet.len()]);
            }
            bytes.extend_from_slice(b"\x1b]2;recovery\x07OK");
            assert_all_splits(&bytes);
        }
    }

    fn assert_all_splits(bytes: &[u8]) {
        for split in 0..=bytes.len() {
            // Compare the same chunking: vte itself has chunk-dependent
            // callbacks for malformed UTF-8 / split encoded C1 controls.
            // This guard must neither introduce nor try to fix those quirks.
            let mut raw = vte::Parser::new();
            let mut expected = Trace::default();
            raw.advance(&mut expected, &bytes[..split]);
            raw.advance(&mut expected, &bytes[split..]);
            let mut parser = BoundedParser::new();
            let mut trace = Trace::default();
            parser.advance(&mut trace, &bytes[..split]);
            parser.advance(&mut trace, &bytes[split..]);
            assert_eq!(trace, expected, "split={split}, bytes={bytes:?}");
        }
        let mut parser = BoundedParser::new();
        let mut raw = vte::Parser::new();
        let mut trace = Trace::default();
        let mut expected = Trace::default();
        for byte in bytes {
            parser.advance(&mut trace, std::slice::from_ref(byte));
            raw.advance(&mut expected, std::slice::from_ref(byte));
        }
        assert_eq!(trace, expected, "bytewise, bytes={bytes:?}");
    }

    #[test]
    fn overflow_drops_whole_osc_and_recovers_at_every_split() {
        const MAX: usize = 16;
        let suffix = b"\x1b[31mOK\x1b]2;next\x07";
        for body_len in [MAX - 1, MAX, MAX + 1, MAX * 3] {
            for end in [b"\x07".as_slice(), b"\x1b\\", b"\x18", b"\x1a", b"\x1b[0m"] {
                let mut bytes = b"\x1b]2;".to_vec();
                bytes.resize(2 + body_len, b'x');
                bytes.extend_from_slice(end);
                bytes.extend_from_slice(suffix);
                let expected = if body_len <= MAX {
                    reference(&bytes)
                } else {
                    let mut remaining = Vec::new();
                    if end != b"\x07" {
                        remaining.extend_from_slice(end);
                    }
                    remaining.extend_from_slice(suffix);
                    reference(&remaining)
                };
                for split in 0..=bytes.len() {
                    let mut parser = BoundedParser::new();
                    let mut trace = Trace::default();
                    parser.advance_bounded::<MAX, 8, _>(&mut trace, &bytes[..split]);
                    parser.advance_bounded::<MAX, 8, _>(&mut trace, &bytes[split..]);
                    assert_eq!(
                        trace, expected,
                        "len={body_len}, end={end:?}, split={split}"
                    );
                }
                let mut parser = BoundedParser::new();
                let mut trace = Trace::default();
                for byte in &bytes {
                    parser.advance_bounded::<MAX, 8, _>(&mut trace, std::slice::from_ref(byte));
                }
                assert_eq!(trace, expected);
            }
        }
    }

    #[test]
    fn separators_and_ignored_controls_count_toward_wire_limit() {
        for byte in [b';', 0x00, b'\n', 0x9c] {
            let mut parser = BoundedParser::new();
            let mut trace = Trace::default();
            parser.advance_bounded::<16, 8, _>(&mut trace, b"\x1b]2;");
            parser.advance_bounded::<16, 8, _>(&mut trace, &[byte; 15]);
            parser.advance_bounded::<16, 8, _>(&mut trace, b"\x1b\\OK");
            assert_eq!(trace, reference(b"\x1b\\OK"));
        }
    }

    #[derive(Default)]
    struct Sink {
        dispatches: usize,
    }

    impl Perform for Sink {
        fn osc_dispatch(&mut self, _: &[&[u8]], _: bool) {
            self.dispatches += 1;
        }
    }

    fn stream(parser: &mut BoundedParser, sink: &mut Sink, mut len: usize) {
        let chunk = [b'x'; 8192];
        while len > 0 {
            let take = len.min(chunk.len());
            parser.advance(sink, &chunk[..take]);
            len -= take;
        }
    }

    #[test]
    fn unterminated_32m_osc_has_bounded_peak_and_releases_on_overflow() {
        let mut parser = BoundedParser::new();
        let mut sink = Sink::default();
        let stats = crate::perf_bench::allocation_stats(|| {
            parser.advance(&mut sink, b"\x1b]999;");
            stream(&mut parser, &mut sink, 32 << 20);
        });
        assert_eq!(stats.largest_request, MAX_RAW_OSC_BYTES);
        assert_eq!(stats.peak_delta, MAX_RAW_OSC_BYTES as i64);
        assert_eq!(stats.live_delta, 0, "parser is still alive, in discard");
        assert_eq!(sink.dispatches, 0);
        assert_eq!(
            crate::perf_bench::allocation_counts(|| stream(&mut parser, &mut sink, 32 << 20)),
            (0, 0, 0),
            "an indefinitely unterminated tail must allocate nothing"
        );
        parser.advance(&mut sink, b"\x1b\\\x1b]2;next\x07");
        assert_eq!(
            sink.dispatches, 1,
            "only the following valid OSC dispatches"
        );
    }

    #[test]
    fn large_completed_cancelled_and_repeated_osc_buffers_are_released() {
        for body in [OSC_REUSE_BYTES + 1, MAX_RAW_OSC_BYTES] {
            for end in [b"\x07".as_slice(), b"\x18", b"\x1a", b"\x1b"] {
                let mut parser = BoundedParser::new();
                let mut sink = Sink::default();
                let stats = crate::perf_bench::allocation_stats(|| {
                    for _ in 0..3 {
                        parser.advance(&mut sink, b"\x1b]999;");
                        stream(&mut parser, &mut sink, body - 4);
                        parser.advance(&mut sink, end);
                    }
                });
                assert_eq!(stats.live_delta, 0, "body={body}, end={end:?}");
                assert!(stats.peak_delta <= MAX_RAW_OSC_BYTES as i64);
                assert_eq!(sink.dispatches, 3, "the exact limit is accepted");
                if end == b"\x1b" {
                    let mut trace = Trace::default();
                    parser.advance(&mut trace, b"[31mOK");
                    assert_eq!(
                        trace,
                        reference(b"\x1b[31mOK"),
                        "ESC state survived release"
                    );
                }
            }
        }
    }

    #[test]
    fn small_osc_buffers_are_reused_instead_of_shrunk_on_each_sequence() {
        let mut parser = BoundedParser::new();
        let mut sink = Sink::default();
        let stats = crate::perf_bench::allocation_stats(|| {
            parser.advance(&mut sink, b"\x1b]999;");
            stream(&mut parser, &mut sink, OSC_REUSE_BYTES - 4);
            parser.advance(&mut sink, b"\x07");
        });
        assert_eq!(stats.live_delta, OSC_REUSE_BYTES as i64);
        assert_eq!(
            crate::perf_bench::allocation_counts(|| {
                for _ in 0..10 {
                    parser.advance(&mut sink, b"\x1b]999;small\x07");
                }
            }),
            (0, 0, 0)
        );
    }
}
