//! Cell-perfect geometry for Unicode block elements (U+2580..U+259F).
//!
//! Fonts draw these glyphs to fill their *line box* (ascent + descent), which
//! is usually shorter than the terminal cell (line height ≥ font size), so
//! stacked blocks leave hairline gaps between rows. TUIs use them as "pixels"
//! (`btop`, `btm`, `cava`, `chafa`), so the renderer draws them itself as
//! rects covering the full cell.
//!
//! All rects are cell-relative (`0..=1` on both axes) and rectilinear; the
//! shades (U+2591..U+2593) stay with the font — they are dither patterns,
//! not solid fills, and low contrast enough that the line-box gap is not
//! noticeable.

/// A rectangle in cell-relative units: `(x0, y0, x1, y1)`.
pub(crate) type Rect = (f32, f32, f32, f32);

/// Rects to fill (in the cell's effective foreground) for a block element.
/// `None` when the character should keep its font glyph.
pub(crate) fn block_rects(ch: char) -> Option<&'static [Rect]> {
    match ch {
        // Halves and eighths (U+2580..U+258F, U+2590, U+2594, U+2595).
        '\u{2580}' => Some(&[(0.0, 0.0, 1.0, 0.5)]), // ▀ upper half
        '\u{2581}' => Some(&[(0.0, 0.875, 1.0, 1.0)]), // ▁ lower one eighth
        '\u{2582}' => Some(&[(0.0, 0.75, 1.0, 1.0)]), // ▂ lower one quarter
        '\u{2583}' => Some(&[(0.0, 0.625, 1.0, 1.0)]), // ▃ lower three eighths
        '\u{2584}' => Some(&[(0.0, 0.5, 1.0, 1.0)]), // ▄ lower half
        '\u{2585}' => Some(&[(0.0, 0.375, 1.0, 1.0)]), // ▅ lower five eighths
        '\u{2586}' => Some(&[(0.0, 0.25, 1.0, 1.0)]), // ▆ lower three quarters
        '\u{2587}' => Some(&[(0.0, 0.125, 1.0, 1.0)]), // ▇ lower seven eighths
        '\u{2588}' => Some(&[(0.0, 0.0, 1.0, 1.0)]), // █ full block
        '\u{2589}' => Some(&[(0.0, 0.0, 0.875, 1.0)]), // ▉ left seven eighths
        '\u{258A}' => Some(&[(0.0, 0.0, 0.75, 1.0)]), // ▊ left three quarters
        '\u{258B}' => Some(&[(0.0, 0.0, 0.625, 1.0)]), // ▋ left five eighths
        '\u{258C}' => Some(&[(0.0, 0.0, 0.5, 1.0)]), // ▌ left half
        '\u{258D}' => Some(&[(0.0, 0.0, 0.375, 1.0)]), // ▍ left three eighths
        '\u{258E}' => Some(&[(0.0, 0.0, 0.25, 1.0)]), // ▎ left one quarter
        '\u{258F}' => Some(&[(0.0, 0.0, 0.125, 1.0)]), // ▏ left one eighth
        '\u{2590}' => Some(&[(0.5, 0.0, 1.0, 1.0)]), // ▐ right half
        '\u{2594}' => Some(&[(0.0, 0.0, 1.0, 0.125)]), // ▔ upper one eighth
        '\u{2595}' => Some(&[(0.875, 0.0, 1.0, 1.0)]), // ▕ right one eighth
        // Quadrants (U+2596..U+259F).
        '\u{2596}' => Some(&[(0.0, 0.5, 0.5, 1.0)]), // ▖ lower left
        '\u{2597}' => Some(&[(0.5, 0.5, 1.0, 1.0)]), // ▗ lower right
        '\u{2598}' => Some(&[(0.0, 0.0, 0.5, 0.5)]), // ▘ upper left
        '\u{2599}' => Some(&[(0.0, 0.0, 0.5, 1.0), (0.5, 0.5, 1.0, 1.0)]), // ▙
        '\u{259A}' => Some(&[(0.0, 0.0, 0.5, 0.5), (0.5, 0.5, 1.0, 1.0)]), // ▚
        '\u{259B}' => Some(&[(0.0, 0.0, 0.5, 1.0), (0.5, 0.0, 1.0, 0.5)]), // ▛
        '\u{259C}' => Some(&[(0.5, 0.0, 1.0, 1.0), (0.0, 0.0, 0.5, 0.5)]), // ▜
        '\u{259D}' => Some(&[(0.5, 0.0, 1.0, 0.5)]), // ▝ upper right
        '\u{259E}' => Some(&[(0.5, 0.0, 1.0, 0.5), (0.0, 0.5, 0.5, 1.0)]), // ▞
        '\u{259F}' => Some(&[(0.5, 0.0, 1.0, 1.0), (0.0, 0.5, 0.5, 1.0)]), // ▟
        _ => None,
    }
}

/// True when the renderer paints the cell itself instead of shaping the
/// glyph.
pub(crate) fn is_drawable(ch: char) -> bool {
    block_rects(ch).is_some()
}

/// Map a cell-relative rect to whole device pixels inside the (already
/// pixel-snapped) `cell` rect, at least 1x1 px.
pub(crate) fn snap_rect(cell: Rect, unit: Rect) -> Rect {
    let (cx0, cy0, cx1, cy1) = cell;
    let w = (cx1 - cx0).max(1.0);
    let h = (cy1 - cy0).max(1.0);
    let x_hi = (cx1 - 1.0).max(cx0);
    let y_hi = (cy1 - 1.0).max(cy0);
    let x0 = (cx0 + unit.0 * w).round().clamp(cx0, x_hi);
    let x1 = (cx0 + unit.2 * w).round().clamp(x0 + 1.0, cx1);
    let y0 = (cy0 + unit.1 * h).round().clamp(cy0, y_hi);
    let y1 = (cy0 + unit.3 * h).round().clamp(y0 + 1.0, cy1);
    (x0, y0, x1, y1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_rects(ch: char, expected: &[Rect]) {
        assert_eq!(block_rects(ch), Some(expected), "char {ch:?}");
    }

    #[test]
    fn halves_and_eighths_cover_the_expected_side() {
        assert_rects('\u{2580}', &[(0.0, 0.0, 1.0, 0.5)]);
        assert_rects('\u{2584}', &[(0.0, 0.5, 1.0, 1.0)]);
        assert_rects('\u{258C}', &[(0.0, 0.0, 0.5, 1.0)]);
        assert_rects('\u{2590}', &[(0.5, 0.0, 1.0, 1.0)]);
        assert_rects('\u{2581}', &[(0.0, 0.875, 1.0, 1.0)]);
        assert_rects('\u{2594}', &[(0.0, 0.0, 1.0, 0.125)]);
        assert_rects('\u{258F}', &[(0.0, 0.0, 0.125, 1.0)]);
        assert_rects('\u{2595}', &[(0.875, 0.0, 1.0, 1.0)]);
    }

    #[test]
    fn full_block_covers_the_whole_cell() {
        assert_rects('\u{2588}', &[(0.0, 0.0, 1.0, 1.0)]);
    }

    #[test]
    fn lower_eighths_step_from_the_bottom() {
        let y = |ch: char| block_rects(ch).unwrap()[0].1;
        assert_eq!(y('\u{2581}'), 0.875);
        assert_eq!(y('\u{2582}'), 0.75);
        assert_eq!(y('\u{2583}'), 0.625);
        assert_eq!(y('\u{2584}'), 0.5);
        assert_eq!(y('\u{2585}'), 0.375);
        assert_eq!(y('\u{2586}'), 0.25);
        assert_eq!(y('\u{2587}'), 0.125);
        for ch in '\u{2581}'..='\u{2587}' {
            assert_eq!(block_rects(ch).unwrap()[0].3, 1.0);
        }
    }

    #[test]
    fn quadrants_compose() {
        assert_rects('\u{2596}', &[(0.0, 0.5, 0.5, 1.0)]);
        assert_rects('\u{2598}', &[(0.0, 0.0, 0.5, 0.5)]);
        assert_rects('\u{259D}', &[(0.5, 0.0, 1.0, 0.5)]);
        assert_rects('\u{2597}', &[(0.5, 0.5, 1.0, 1.0)]);
        assert_rects('\u{2599}', &[(0.0, 0.0, 0.5, 1.0), (0.5, 0.5, 1.0, 1.0)]);
        assert_rects('\u{259A}', &[(0.0, 0.0, 0.5, 0.5), (0.5, 0.5, 1.0, 1.0)]);
        assert_rects('\u{259B}', &[(0.0, 0.0, 0.5, 1.0), (0.5, 0.0, 1.0, 0.5)]);
        assert_rects('\u{259C}', &[(0.5, 0.0, 1.0, 1.0), (0.0, 0.0, 0.5, 0.5)]);
        assert_rects('\u{259E}', &[(0.5, 0.0, 1.0, 0.5), (0.0, 0.5, 0.5, 1.0)]);
        assert_rects('\u{259F}', &[(0.5, 0.0, 1.0, 1.0), (0.0, 0.5, 0.5, 1.0)]);
    }

    #[test]
    fn shades_and_text_keep_their_glyphs() {
        assert!(!is_drawable('\u{2591}'));
        assert!(!is_drawable('\u{2592}'));
        assert!(!is_drawable('\u{2593}'));
        assert!(!is_drawable('a'));
        assert!(!is_drawable(' '));
        assert!(!is_drawable('\u{2502}'));
        for ch in '\u{2580}'..='\u{258F}' {
            assert!(is_drawable(ch), "{ch:?}");
        }
        assert!(is_drawable('\u{2590}'));
        for ch in '\u{2594}'..='\u{259F}' {
            assert!(is_drawable(ch), "{ch:?}");
        }
    }

    #[test]
    fn snap_rect_rounds_inside_the_cell() {
        let cell = (0.0, 0.0, 10.0, 20.0);
        assert_eq!(
            snap_rect(cell, (0.0, 0.0, 1.0, 1.0)),
            (0.0, 0.0, 10.0, 20.0)
        );
        assert_eq!(
            snap_rect(cell, (0.0, 0.0, 1.0, 0.5)),
            (0.0, 0.0, 10.0, 10.0)
        );
        assert_eq!(
            snap_rect(cell, (0.0, 0.875, 1.0, 1.0)),
            (0.0, 18.0, 10.0, 20.0)
        );
        // Never collapses to zero pixels, even in a 1px cell.
        let tiny = (4.0, 5.0, 5.0, 6.0);
        assert_eq!(
            snap_rect(tiny, (0.0, 0.0, 0.25, 0.25)),
            (4.0, 5.0, 5.0, 6.0)
        );
    }
}
