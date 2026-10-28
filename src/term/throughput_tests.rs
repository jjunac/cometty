//! Correctness and allocation bounds for the throughput paths, not timer gates.
use super::*;
use crate::perf_bench::allocation_counts;
use vte::Perform;

struct Scalar<'a>(&'a mut Terminal);
impl Perform for Scalar<'_> {
    fn print(&mut self, c: char) {
        self.0.print_scalar(c);
    }
    fn execute(&mut self, b: u8) {
        self.0.execute(b);
    }
    fn csi_dispatch(&mut self, p: &Params, i: &[u8], ignore: bool, c: char) {
        self.0.csi_dispatch(p, i, ignore, c);
    }
    fn esc_dispatch(&mut self, i: &[u8], ignore: bool, b: u8) {
        self.0.esc_dispatch(i, ignore, b);
    }
    fn osc_dispatch(&mut self, p: &[&[u8]], bell: bool) {
        self.0.osc_dispatch(p, bell);
    }
}

#[test]
fn ascii_fast_writes_match_scalar_path_with_modes_unicode_and_every_split() {
    let corpus = concat!(
        "abcdefghijklmnopq\r\n12345678901234567890",
        "漢e\u{301}👨\u{200d}👩🇫🇷x\u{600}abc",
        "\x1b[38;2;1;2;3m\x1b[48;5;120m\x1b[1;3;4:2mabcdef",
        "\x1b]8;;https://example.com\x07link\x1b[0mX\x1b]8;;\x07",
        "\x1b[2;3H\x1b[4habc\x1b[4l\x1b[?7l01234567890123456789\x1b[?7h",
        "\x1b[2;3r\x1b[?6h\x1b[Hmargin\r\nscroll\r\nagain\x1b[?6l",
        "\x1b[?1049hALT\r\n漢abc\x1b[?1049l\x1b[3Jend"
    )
    .as_bytes();
    for cols in [1, 2, 7, 20] {
        for split in 0..=corpus.len() {
            let mut actual = Terminal::new(cols, 4, Theme::default());
            let mut expected = Terminal::new(cols, 4, Theme::default());
            let mut reference_parser = BoundedParser::new();
            for chunk in [&corpus[..split], &corpus[split..]] {
                actual.feed(chunk);
                reference_parser.advance(&mut Scalar(&mut expected), chunk);
                for (a, b) in actual
                    .grid
                    .view_rows()
                    .iter()
                    .zip(expected.grid.view_rows().iter())
                {
                    assert_eq!(a.to_vec(), b.to_vec(), "cols={cols} split={split}");
                }
                assert_eq!(actual.grid.cursor(), expected.grid.cursor());
                assert_eq!(actual.pending_wrap, expected.pending_wrap);
                assert_eq!(actual.grid.scrollback_len(), expected.grid.scrollback_len());
                assert_eq!(actual.last_graphic, expected.last_graphic);
            }
        }
    }
}

#[test]
fn independent_cjk_boundaries_and_borrowed_sgr_allocate_nothing() {
    let mut terminal = Terminal::new(120, 4, Theme::default());
    let corpus = "\r漢字中文日本語\r\x1b[38;5;42;48;2;1;2;3;4:3;58:2::1:2:3m\x1b[0m";
    terminal.feed(corpus.as_bytes());
    assert_eq!(
        allocation_counts(|| {
            for _ in 0..100 {
                terminal.feed(corpus.as_bytes());
            }
        }),
        (0, 0, 0)
    );
    // vte's complete parameter capacity, including subparameters, stays on the
    // stack. Mix truncated/malformed color tails with resets and attributes.
    for sgr in [
        "\x1b[1;2;3;4;7;9;21;22;23;24;27;29;30;31;32;33;34;35;36;37;39;40;41;42;43;44;45;46;47;49;53;55m",
        "\x1b[38:2;1;2;3;48:5;42;58;2:1:2:3;4:5m",
        "\x1b[38;2;1m\x1b[38:2;1;4:3m\x1b[48;5m\x1b[m",
    ] {
        assert_eq!(
            allocation_counts(|| terminal.feed(sgr.as_bytes())),
            (0, 0, 0)
        );
    }
}

#[test]
fn long_appends_grow_amortized_without_reconstructing_the_cluster() {
    for prefix in ["e", "1234567", "12345漢"] {
        let mut terminal = Terminal::new(7, 2, Theme::default());
        terminal.feed(prefix.as_bytes());
        let cursor = terminal.grid.cursor();
        let pending = terminal.pending_wrap;
        let counts = allocation_counts(|| {
            for _ in 0..4096 {
                terminal.feed("\u{301}".as_bytes());
            }
        });
        assert!(counts.0 <= 1 && counts.1 <= 13, "{prefix:?}: {counts:?}");
        assert!(
            counts.2 < 32 * 1024,
            "linear allocation traffic: {counts:?}"
        );
        assert_eq!(terminal.grid.cursor(), cursor);
        assert_eq!(terminal.pending_wrap, pending);
        let x = if pending { cursor.x } else { cursor.x - 1 };
        let cell = terminal.grid.cell(x, cursor.y).unwrap();
        assert_eq!(cell.extra.as_deref().unwrap().len(), 8192);
        assert!(
            cell.extra
                .as_deref()
                .unwrap()
                .chars()
                .all(|c| c == '\u{301}')
        );
    }
}
