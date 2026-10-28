//! Unicode width + grapheme-cluster helpers.
//!
//! Display width follows `unicode-width` (`UAX #11`-ish): 0 = combining /
//! format, 1 = narrow, 2 = wide (CJK + most emoji). Regional Indicators are
//! forced to 2 so flag pairs (`RI RI`) occupy one wide cell. Grapheme
//! boundaries follow `UAX #29` via `unicode-segmentation`, which keeps ZWJ
//! sequences, skin-tone modifiers, VS15/16, keycaps and flags in one cell.

#[cfg(test)]
use unicode_segmentation::UnicodeSegmentation;
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};
use unicode_width::UnicodeWidthChar;

/// Zero Width Joiner: joins emoji into a single grapheme (e.g. family).
pub const ZWJ: char = '\u{200D}';

/// Display width of a single char: 0, 1 or 2.
/// Controls map to 1 (they never reach `print` via vte anyway).
pub fn char_width(c: char) -> usize {
    if c.is_ascii() {
        return 1;
    }
    // Regional Indicators (flags) always take a full wide cell.
    if ('\u{1F1E6}'..='\u{1F1FF}').contains(&c) {
        return 2;
    }
    // Format / variation selectors that must never advance the cursor.
    if matches!(c, '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01FF}' | ZWJ) {
        return 0;
    }
    UnicodeWidthChar::width(c).unwrap_or(1)
}

/// True when `c` is a format char that always joins the previous cluster
/// (combining width-0, VS, ZWJ, skin-tone modifiers, keycap mark).
pub fn is_joining_modifier(c: char) -> bool {
    if c.is_ascii() {
        return false;
    }
    if char_width(c) == 0 {
        return true;
    }
    // Emoji skin-tone modifiers: attach to the previous emoji base even
    // though `unicode-width` reports them as wide on their own.
    if ('\u{1F3FB}'..='\u{1F3FF}').contains(&c) {
        return true;
    }
    // Combining enclosing keycap (e.g. `#` + VS16 + U+20E3).
    if c == '\u{20E3}' {
        return true;
    }
    false
}

/// True when `prev` + `next` form a single extended grapheme cluster.
/// Used to decide ZWJ / flag / modifier continuation for chars that are
/// wide on their own (e.g. the second emoji after a ZWJ).
#[allow(dead_code)]
pub fn continues_cluster(prev: &str, next: char) -> bool {
    if prev.is_empty() {
        return false;
    }
    continues_parts(prev, "", next)
}

/// Segment borrowed chunks as one string. The lead and incoming scalar live in
/// stack UTF-8 buffers; the tail stays borrowed. Looking for the *first* boundary
/// also preserves the old behavior for cells containing multiple graphemes
/// (explicit width-zero modifiers need not be UAX #29 continuations).
fn continues_parts(first: &str, tail: &str, next: char) -> bool {
    let mut utf8 = [0; 4];
    let next = next.encode_utf8(&mut utf8);
    let length = first.len() + tail.len() + next.len();
    let parts = [
        (first, 0),
        (tail, first.len()),
        (next as &str, first.len() + tail.len()),
    ];
    let mut cursor = GraphemeCursor::new(0, length, true);
    let mut index = 0;
    loop {
        let (chunk, start) = parts[index];
        match cursor.next_boundary(chunk, start) {
            Ok(Some(end)) => return end == length,
            Ok(None) => return true,
            Err(GraphemeIncomplete::NextChunk) => {
                index += 1;
                if parts[index].0.is_empty() {
                    index += 1;
                }
            }
            Err(GraphemeIncomplete::PreContext(end)) => {
                let &(chunk, start) = parts
                    .iter()
                    .rev()
                    .find(|&&(s, start)| !s.is_empty() && start < end)
                    .expect("grapheme pre-context");
                cursor.provide_context(&chunk[..end - start], start);
            }
            Err(other) => unreachable!("forward grapheme boundary: {other:?}"),
        }
    }
}

/// Grid-specific continuation rules without materializing the old cluster
/// for explicit modifiers, blanks, or the common printable ASCII pair.
pub(crate) fn continues_cell(lead: &super::Cell, next: char) -> bool {
    if is_joining_modifier(next) {
        return true;
    }
    if lead.extra.is_none()
        && (lead.ch == ' '
            || (lead.ch.is_ascii()
                && !lead.ch.is_ascii_control()
                && next.is_ascii()
                && !next.is_ascii_control()))
    {
        return false;
    }
    let mut utf8 = [0; 4];
    continues_parts(
        lead.ch.encode_utf8(&mut utf8),
        lead.extra.as_deref().unwrap_or_default(),
        next,
    )
}

/// Display width of a full cluster: 2 when it contains anything wide,
/// otherwise 1. Never 0 so a lone combining mark still occupies a cell.
/// Used by tests and future width-promotion work; the grid itself keeps
/// lead widths stable on append (see `Grid::append_to_prev`).
#[allow(dead_code)]
pub fn cluster_width(s: &str) -> usize {
    if s.is_empty() {
        return 1;
    }
    for c in s.chars() {
        if char_width(c) == 2 {
            return 2;
        }
        // Skin-tone / keycap clusters built on a narrow base (e.g. `#️⃣`)
        // still render as emoji presentation: promote to wide.
        if ('\u{1F3FB}'..='\u{1F3FF}').contains(&c) || c == '\u{20E3}' {
            return 2;
        }
    }
    // Emoji ZWJ sequences whose bases already report wide stay wide; a
    // narrow-base ZWJ cluster (rare) also presents as wide.
    if s.contains(ZWJ) {
        return 2;
    }
    // Variation Selector 16 forces emoji presentation.
    if s.contains('\u{FE0F}') {
        return 2;
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_narrow() {
        assert_eq!(char_width('a'), 1);
        assert_eq!(char_width(' '), 1);
    }

    #[test]
    fn cjk_is_wide() {
        assert_eq!(char_width('中'), 2);
        assert_eq!(char_width('あ'), 2);
    }

    #[test]
    fn combining_is_zero_width() {
        assert_eq!(char_width('\u{0301}'), 0);
        assert!(is_joining_modifier('\u{0301}'));
    }

    #[test]
    fn zwj_and_vs_are_joining() {
        assert_eq!(char_width(ZWJ), 0);
        assert_eq!(char_width('\u{FE0F}'), 0);
        assert!(is_joining_modifier(ZWJ));
        assert!(is_joining_modifier('\u{FE0F}'));
        assert!(is_joining_modifier('\u{1F3FB}'));
        assert!(is_joining_modifier('\u{20E3}'));
    }

    #[test]
    fn regional_indicators_are_wide() {
        assert_eq!(char_width('\u{1F1E6}'), 2);
    }

    #[test]
    fn emoji_is_wide() {
        // Single emoji base renders double-width in the terminal grid.
        assert_eq!(char_width('😀'), 2);
    }

    #[test]
    fn zwj_sequence_is_single_grapheme() {
        let family = "👨\u{200D}👩\u{200D}👧";
        assert_eq!(family.graphemes(true).count(), 1);
        assert!(continues_cluster("👨\u{200D}", '👩'));
        assert_eq!(cluster_width(family), 2);
    }

    #[test]
    fn flag_pair_is_single_grapheme() {
        let flag = "🇺🇸";
        assert_eq!(flag.graphemes(true).count(), 1);
        assert_eq!(cluster_width(flag), 2);
    }

    #[test]
    fn distinct_chars_break() {
        assert!(!continues_cluster("a", 'b'));
        assert!(!continues_cluster("👨", '👩'));
    }

    #[test]
    fn ascii_fast_path_matches_grapheme_segmentation() {
        for previous in ' '..='~' {
            let lead = super::super::Cell {
                ch: previous,
                ..Default::default()
            };
            for next in ' '..='~' {
                assert_eq!(
                    continues_cell(&lead, next),
                    continues_cluster(&previous.to_string(), next)
                );
            }
        }
        // An incoming ASCII character can join a non-ASCII prepend char.
        let lead = super::super::Cell {
            ch: '\u{600}',
            ..Default::default()
        };
        assert!(continues_cell(&lead, 'a'));
    }

    #[test]
    fn borrowed_boundaries_match_contiguous_segmentation_without_allocating() {
        let alphabet = [
            'a',
            ' ',
            '漢',
            'é',
            '\u{600}',
            '\u{301}',
            ZWJ,
            '👨',
            '👩',
            '\u{1f1fa}',
            '\u{1f1f8}',
            '\u{1f3fb}',
            '\u{fe0f}',
            '\u{200e}',
            'क',
            '\u{94d}',
            '\u{915}',
            '\u{1100}',
            '\u{1161}',
            '\u{11a8}',
        ];
        let mut seed = 7_u32;
        for _ in 0..2000 {
            let mut previous = String::new();
            for _ in 0..1 + seed as usize % 12 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                previous.push(alphabet[seed as usize % alphabet.len()]);
            }
            for next in alphabet {
                let combined = format!("{previous}{next}");
                let expected = combined.graphemes(true).count() == 1;
                let ch = previous.chars().next().unwrap();
                let tail = &previous[ch.len_utf8()..];
                let lead = super::super::Cell {
                    ch,
                    extra: (!tail.is_empty()).then(|| tail.into()),
                    ..Default::default()
                };
                assert_eq!(
                    continues_cluster(&previous, next),
                    expected,
                    "{previous:?} + {next:?}"
                );
                let expected =
                    is_joining_modifier(next) || (ch != ' ' || !tail.is_empty()) && expected;
                assert_eq!(
                    continues_cell(&lead, next),
                    expected,
                    "{previous:?} + {next:?}"
                );
                assert_eq!(
                    crate::perf_bench::allocation_counts(|| {
                        std::hint::black_box(continues_cell(&lead, next));
                    }),
                    (0, 0, 0)
                );
            }
        }
    }
}
