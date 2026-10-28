use super::*;
use crate::theme::Theme;

fn context<'a>(fonts: &'a mut FontSystem, cache: &'a mut AdvanceCache) -> NormCtx<'a> {
    NormCtx {
        cell_width: 9.632,
        font_size: 16.0,
        line_height: 20.0,
        family: Family::Monospace,
        features: terminal_font_features(),
        font_system: fonts,
        cache,
    }
}

// Independent reference for the original per-cell-string construction. Its
// measurements use a fresh Buffer per cluster, like the pre-tranche path.
fn reference_line(row: &[Cell], ctx: &mut NormCtx<'_>) -> BufferLine {
    let mut clusters: Vec<_> = row
        .iter()
        .enumerate()
        .filter(|(_, c)| c.width != 0)
        .map(|(col, c)| {
            let text =
                if c.width == 1 && c.extra.is_none() && super::super::blocks::is_drawable(c.ch) {
                    " ".into()
                } else {
                    c.cluster()
                };
            (col, text)
        })
        .collect();
    while clusters.last().is_some_and(|(_, text)| text == " ") {
        clusters.pop();
    }
    let mut text: String = clusters.iter().map(|(_, text)| text.as_str()).collect();
    if text.trim().is_empty() {
        text = " ".into();
        clusters.clear();
    }
    let attrs = Attrs::new()
        .family(ctx.family)
        .font_features(ctx.features.clone());
    let mut list = AttrsList::new(&attrs);
    let mut start = 0;
    for (col, text) in clusters {
        let cell = &row[col];
        ctx.cache.measurement = None;
        let (natural, glyphs) = measure_cluster(&text, cell.bold, cell.italic, ctx);
        let mut spacing = (cell.width.max(1) as f32 * ctx.cell_width - natural)
            / (glyphs.max(1) as f32 * ctx.font_size);
        if !spacing.is_finite() || spacing.abs() < 1e-9 {
            spacing = 0.0;
        }
        let attrs = attrs
            .clone()
            .color(cell.effective_fg().as_glyphon_color())
            .weight(if cell.bold {
                Weight::BOLD
            } else {
                Weight::NORMAL
            })
            .style(if cell.italic {
                Style::Italic
            } else {
                Style::Normal
            })
            .letter_spacing(spacing);
        list.add_span(start..start + text.len(), &attrs);
        start += text.len();
    }
    BufferLine::new(text, LineEnding::None, list, Shaping::Advanced)
}

#[test]
fn compact_line_construction_matches_original_text_attributes_and_layout() {
    let theme = Theme::default();
    let mut fonts = FontSystem::new();
    let mut cache = AdvanceCache::new();
    for corpus in [
        "ASCII ... -> ffi   ",
        "e\u{301}a\u{308}e\u{301}",
        "あい😀x",
        "👨\u{200d}👩\u{200d}👧🇫🇷x",
        "العربية हिन्दी x",
        "██░▀x █ ",
        "\u{2003}\u{3000}  ",
        "   ",
        "",
    ] {
        let mut terminal = crate::term::Terminal::new(80, 2, theme);
        terminal.feed(corpus.as_bytes());
        let mut row = terminal.grid().visible_rows()[0].clone();
        // Mixed attrs, inverse/dim and empty tails exercise byte/column spans.
        for (i, cell) in row.iter_mut().enumerate() {
            cell.bold = i % 3 == 0;
            cell.italic = i % 4 == 0;
            cell.inverse = i % 7 == 0;
            cell.dim = i % 5 == 0;
        }
        let mut ctx = context(&mut fonts, &mut cache);
        let mut expected = reference_line(&row, &mut ctx);
        ctx.cache.clear();
        let mut cold = build_buffer_line(&row, &theme, &mut ctx);
        let mut warm = build_buffer_line(&row, &theme, &mut ctx);
        assert_eq!(cold.text(), expected.text(), "{corpus:?}");
        assert_eq!(cold.attrs_list(), expected.attrs_list(), "{corpus:?}");
        assert_eq!(warm.attrs_list(), expected.attrs_list(), "{corpus:?}");
        let fingerprint = |line: &mut BufferLine, ctx: &mut NormCtx<'_>| {
            let mut buffer = Buffer::new(
                ctx.font_system,
                Metrics::new(ctx.font_size, ctx.line_height),
            );
            buffer.set_wrap(Wrap::None);
            buffer.set_size(Some(2000.0), Some(600.0));
            buffer.lines.clear();
            buffer.lines.push(line.clone());
            format!("{:?}", buffer.line_layout(ctx.font_system, 0))
        };
        assert_eq!(
            fingerprint(&mut cold, &mut ctx),
            fingerprint(&mut expected, &mut ctx),
            "{corpus:?}"
        );
        assert_eq!(
            fingerprint(&mut warm, &mut ctx),
            fingerprint(&mut expected, &mut ctx),
            "{corpus:?}"
        );
    }
}

#[test]
fn warm_row_allocations_do_not_scale_one_per_cell() {
    let theme = Theme::default();
    let mut fonts = FontSystem::new();
    let mut cache = AdvanceCache::new();
    let mut ctx = context(&mut fonts, &mut cache);
    for extra in [None, Some("\u{301}".into())] {
        let row = vec![
            Cell {
                ch: 'e',
                extra,
                ..Cell::default()
            };
            200
        ];
        build_buffer_line(&row, &theme, &mut ctx);
        let large = crate::perf_bench::allocation_counts(|| {
            build_buffer_line(&row, &theme, &mut ctx);
        });
        let small = crate::perf_bench::allocation_counts(|| {
            build_buffer_line(&row[..20], &theme, &mut ctx);
        });
        assert!(large.0 <= small.0 + 1, "large={large:?}, small={small:?}");
        assert!(large.0 < 20, "{large:?}");
        assert_eq!(large.1, 0, "warm descriptors must not grow");
    }
}

#[test]
fn advance_cache_separates_styles_and_drops_font_generations() {
    let mut fonts = FontSystem::new();
    let mut cache = AdvanceCache::new();
    let mut ctx = context(&mut fonts, &mut cache);
    for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
        let measured = cached_advance("e\u{301}", bold, italic, &mut ctx);
        let counts = crate::perf_bench::allocation_counts(|| {
            assert_eq!(cached_advance("e\u{301}", bold, italic, &mut ctx), measured);
        });
        assert_eq!(counts, (0, 0, 0));
    }
    assert_eq!(ctx.cache.cluster_entries, 4);
    assert!(ctx.cache.clusters.iter().all(|style| style.len() == 1));
    ctx.cache.clear();
    assert_eq!(ctx.cache.cluster_entries, 0);
    assert_eq!(ctx.cache.key_bytes, 0);
    assert!(ctx.cache.measurement.is_none());
    ctx.font_size *= 2.0;
    let new = cached_advance("e\u{301}", false, false, &mut ctx);
    assert!(new.0 > 0.0);
    assert_eq!(ctx.cache.cluster_entries, 1);
}

#[test]
fn advance_keys_have_entry_byte_and_individual_limits() {
    let mut cache = AdvanceCache::new();
    for n in 0..MAX_CLUSTER_ADVANCES * 3 {
        let text = format!("{n}\u{301}");
        cache.insert_cluster(n % 4, &text, (1.0, 1));
        assert!(cache.cluster_entries <= MAX_CLUSTER_ADVANCES);
        assert!(cache.key_bytes <= MAX_CLUSTER_KEY_BYTES);
    }
    cache.clear();
    for n in 0..MAX_CLUSTER_ADVANCES {
        let text = format!("{n:08}{}", "x".repeat(MAX_CLUSTER_BYTES - 8));
        cache.insert_cluster(n % 4, &text, (1.0, 1));
        assert!(cache.key_bytes <= MAX_CLUSTER_KEY_BYTES);
    }
    let before = (cache.cluster_entries, cache.key_bytes);
    cache.insert_cluster(0, &"x".repeat(MAX_CLUSTER_BYTES + 1), (1.0, 1));
    assert_eq!((cache.cluster_entries, cache.key_bytes), before);
}

#[test]
fn font_names_are_not_silently_replaced_with_generic_monospace() {
    assert_eq!(
        font_family("  My Custom Mono  "),
        Family::Name("My Custom Mono")
    );
    assert_eq!(font_family("MONOSPACE"), Family::Monospace);
    assert_eq!(font_family("Sans-Serif"), Family::SansSerif);
    let fonts = FontSystem::new();
    assert_eq!(
        resolved_font_name(&fonts, "Missing Custom Font"),
        "Missing Custom Font"
    );
    let name = resolved_font_name(&fonts, "monospace");
    if fonts.db().faces().any(|f| f.monospaced) {
        assert!(matches!(font_family(&name), Family::Name(_)));
        assert!(
            fonts
                .db()
                .faces()
                .any(|f| f.monospaced && f.families.iter().any(|(n, _)| *n == name))
        );
    }
}

#[test]
fn bounded_final_shaping_matches_uncached_complex_layout_and_reduces_allocations() {
    let mut fonts = FontSystem::new();
    let family_name = resolved_font_name(&fonts, "monospace");
    let mut advances = AdvanceCache::new();
    let mut layout = |fonts: &mut FontSystem, text: &str| {
        let mut terminal = crate::term::Terminal::new(120, 2, Theme::default());
        terminal.feed(text.as_bytes());
        let mut ctx = context(fonts, &mut advances);
        ctx.family = font_family(&family_name);
        let line = build_buffer_line(
            &terminal.grid().visible_rows()[0],
            &Theme::default(),
            &mut ctx,
        );
        let mut buffer = Buffer::new(ctx.font_system, Metrics::new(16.0, 20.0));
        buffer.set_wrap(Wrap::None);
        buffer.set_size(Some(2000.0), Some(100.0));
        buffer.lines.clear();
        buffer.lines.push(line);
        format!("{:?}", buffer.line_layout(ctx.font_system, 0))
    };
    for text in [
        "ASCII ... -> ffi",
        "漢あ한 e\u{301} 👨\u{200d}👩\u{200d}👧 🇫🇷",
        "العربية हिन्दी עברית",
        "abc العربية 123",
        "العربية abc",
        "a\u{600}b",
    ] {
        fonts.shape_run_cache.set_limits(0, 0);
        let expected = layout(&mut fonts, text);
        let uncached = crate::perf_bench::allocation_counts(|| {
            layout(&mut fonts, text);
        });
        configure_shaping_cache(&mut fonts);
        assert_eq!(layout(&mut fonts, text), expected);
        let cached = crate::perf_bench::allocation_counts(|| {
            assert_eq!(layout(&mut fonts, text), expected);
        });
        assert!(
            cached.0 < uncached.0,
            "{text:?}: cached={cached:?} uncached={uncached:?}"
        );
        assert!(fonts.shape_run_cache.retained_bytes() <= 2 * 1024 * 1024);
    }
    assert!(!fonts.shape_run_cache.is_empty());
    fonts.db_mut();
    assert!(
        fonts.shape_run_cache.is_empty(),
        "font generation invalidates shaped results"
    );
}

#[test]
fn final_shaping_cache_charges_capacities_rejects_giant_entries_and_separates_direction() {
    use cosmic_text::{AttrsOwned, ShapeRunCache, ShapeRunKey};
    let mut cache = ShapeRunCache::default();
    let key = |text: String, rtl| ShapeRunKey {
        rtl,
        text,
        default_attrs: AttrsOwned::new(&Attrs::new()),
        attrs_spans: Vec::new(),
    };
    cache.set_limits(8, 16 * 1024);
    cache.insert(key("same".into(), false), Vec::new());
    assert!(cache.get(&key("same".into(), false)).is_some());
    assert!(cache.get(&key("same".into(), true)).is_none());
    let mut huge = String::with_capacity(32 * 1024);
    huge.push('x'); // charge capacity, not this one-byte length
    cache.insert(key(huge, false), Vec::new());
    assert_eq!(cache.len(), 1);
    for n in 0..1024 {
        cache.insert(
            key(format!("{n}{}", "x".repeat(n % 200)), false),
            Vec::new(),
        );
        assert!(cache.len() <= 8);
        assert!(cache.retained_bytes() <= 16 * 1024);
    }
    let mut cloned = cache.clone();
    for n in 0..100 {
        cloned.insert(key(format!("cloned {n}"), false), Vec::new());
        assert!(cloned.len() <= 8);
        assert!(cloned.retained_bytes() <= 16 * 1024);
    }
    cache.trim(0);
    cache.trim(0);
    assert!(cache.is_empty());
    cache.set_limits(0, 0);
    assert_eq!(cache.retained_bytes(), 0);
    cache.insert(key("disabled".into(), false), Vec::new());
    assert!(cache.is_empty());
}

#[test]
fn shaped_run_capacity_budget_bounds_actual_requested_live_storage_and_drop_balances() {
    use cosmic_text::{AttrsOwned, ShapeRunCache, ShapeRunKey};
    let budget = 2 * 1024 * 1024;
    let mut cache = ShapeRunCache::default();
    let stats = crate::perf_bench::allocation_stats(|| {
        cache.set_limits(512, budget);
        for n in 0..1024 {
            let mut text = String::with_capacity(16 * 1024);
            use std::fmt::Write;
            write!(text, "unique {n}").unwrap();
            cache.insert(
                ShapeRunKey {
                    rtl: false,
                    text,
                    default_attrs: AttrsOwned::new(&Attrs::new()),
                    attrs_spans: Vec::new(),
                },
                Vec::new(),
            );
            assert!(cache.len() <= 512);
            assert!(cache.retained_bytes() <= budget);
        }
    });
    assert!(
        stats.live_delta <= budget as i64,
        "live={}",
        stats.live_delta
    );
    assert!(
        stats.peak_delta <= (budget + 64 * 1024) as i64,
        "one incoming key may overlap eviction: peak={}",
        stats.peak_delta
    );
    assert_eq!(
        crate::perf_bench::allocation_stats(|| drop(cache)).live_delta,
        -stats.live_delta
    );
}
