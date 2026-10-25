//! Text buffer shaping (cosmic-text lines from grid cells).

use std::collections::HashMap;

use cosmic_text::{
    Attrs, AttrsList, Buffer, BufferLine, Family, FeatureTag, FontFeatures, FontSystem, LineEnding,
    Metrics, Shaping, Style, Weight, Wrap,
};

use crate::grid::Cell;

use super::Renderer;

pub(crate) fn font_family(name: &str) -> Family<'static> {
    match name.trim().to_ascii_lowercase().as_str() {
        "sans" | "sans-serif" => Family::SansSerif,
        "serif" => Family::Serif,
        "cursive" => Family::Cursive,
        "fantasy" => Family::Fantasy,
        _ => Family::Monospace,
    }
}

/// Terminal shaping features: no ligatures/kerning across grid columns.
/// Advanced shaping stays on (fallback fonts, emoji, complex clusters), but
/// `liga`/`clig`/`calt`/`dlig`/`kern` would merge or shift neighbouring
/// cells (e.g. `...` ligating, `->` arrows), breaking column alignment.
pub(crate) fn terminal_font_features() -> FontFeatures {
    let mut f = FontFeatures::new();
    f.disable(FeatureTag::KERNING);
    f.disable(FeatureTag::STANDARD_LIGATURES);
    f.disable(FeatureTag::CONTEXTUAL_LIGATURES);
    f.disable(FeatureTag::CONTEXTUAL_ALTERNATES);
    f.disable(FeatureTag::DISCRETIONARY_LIGATURES);
    f
}

/// Natural-advance cache for single-char clusters: `(natural_px, glyphs)`.
/// Keyed by `(char, bold, italic)`; cleared when family/size changes.
/// Multi-char clusters (ZWJ, combining) are measured on the fly (rare).
pub(crate) type AdvanceCache = HashMap<(char, bool, bool), (f32, usize)>;

/// Fixed-cell shaping inputs. Grouped so shaping helpers stay under the
/// clippy arg limit (see `ScrollCtx`/`HighlightCtx` in `renderer::mod`).
pub(crate) struct NormCtx<'a> {
    pub cell_width: f32,
    pub font_size: f32,
    pub line_height: f32,
    pub family: Family<'static>,
    pub features: FontFeatures,
    pub font_system: &'a mut FontSystem,
    pub cache: &'a mut AdvanceCache,
}

fn measure_cluster(cluster: &str, bold: bool, italic: bool, ctx: &mut NormCtx<'_>) -> (f32, usize) {
    let weight = if bold { Weight::BOLD } else { Weight::NORMAL };
    let style = if italic { Style::Italic } else { Style::Normal };
    let attrs = Attrs::new()
        .family(ctx.family)
        .weight(weight)
        .style(style)
        .font_features(ctx.features.clone());
    let (font_size, line_height) = (ctx.font_size, ctx.line_height);
    let mut buf = Buffer::new(ctx.font_system, Metrics::new(font_size, line_height));
    buf.set_wrap(Wrap::None);
    buf.set_size(Some(800.0), Some(600.0));
    let line = BufferLine::new(
        cluster.to_string(),
        LineEnding::None,
        AttrsList::new(&attrs),
        Shaping::Advanced,
    );
    buf.lines.clear();
    buf.lines.push(line);
    buf.line_layout(ctx.font_system, 0);
    buf.line_layout(ctx.font_system, 0)
        .and_then(|ls| ls.first().map(|l| (l.w, l.glyphs.len())))
        .unwrap_or((font_size * 0.6, 1))
}

fn cached_advance(cluster: &str, bold: bool, italic: bool, ctx: &mut NormCtx<'_>) -> (f32, usize) {
    // Fast path: single-char clusters hit a Copy-keyed cache (covers ASCII,
    // dots, squares, Braille, CJK, single emoji). Multi-char clusters are
    // rare (ZWJ/combining) and measured directly.
    if let Some(ch) = cluster.chars().next()
        && cluster.chars().count() == 1
    {
        let key = (ch, bold, italic);
        if let Some(&hit) = ctx.cache.get(&key) {
            return hit;
        }
        let m = measure_cluster(cluster, bold, italic, ctx);
        ctx.cache.insert(key, m);
        return m;
    }
    measure_cluster(cluster, bold, italic, ctx)
}

pub(crate) fn build_buffer_lines(
    rows: &[Vec<Cell>],
    theme: &crate::theme::Theme,
    ctx: &mut NormCtx<'_>,
) -> Vec<BufferLine> {
    rows.iter()
        .map(|row| build_buffer_line(row, theme, ctx))
        .collect()
}

/// Shape a single grid row into a cosmic-text line.
///
/// Cluster leads become glyphs (wide continuations occupy a column but no
/// glyphs; rect-painted block elements shape as spaces); every cluster is
/// advance-normalized to its cell width via `letter_spacing`, and attr
/// spans split on effective fg + bold + italic + spacing. Decorations
/// (underline/strike/overline) paint via bg quads, never text spans.
pub(crate) fn build_buffer_line(
    row_cells: &[Cell],
    theme: &crate::theme::Theme,
    ctx: &mut NormCtx<'_>,
) -> BufferLine {
    let _ = theme;
    // Build the shaped string from cluster leads only; wide
    // continuations occupy a grid column but no glyphs.
    let mut clusters: Vec<(usize, String)> = Vec::new();
    for (col, cell) in row_cells.iter().enumerate() {
        if cell.width == 0 {
            continue;
        }
        // Block elements are painted by the renderer as cell-sized rects
        // (see `renderer::blocks`); shape a space in their place so the
        // font's shorter line box can't leave gaps between rows.
        let drawn = cell.width == 1 && cell.extra.is_none() && super::blocks::is_drawable(cell.ch);
        clusters.push((
            col,
            if drawn {
                " ".to_string()
            } else {
                cell.cluster()
            },
        ));
    }
    // Trim trailing blank cells (single-space narrow cells only; wide
    // clusters and ZWJ sequences are never whitespace-trimmed).
    while clusters.last().is_some_and(|(_, s)| s == " ") {
        clusters.pop();
    }
    let line_string: String = clusters.iter().map(|(_, s)| s.as_str()).collect();
    let (line_text, visible_clusters) = if line_string.trim().is_empty() {
        (" ".to_string(), Vec::new())
    } else {
        (line_string, clusters)
    };
    // Per-cluster advance normalization: pad (or shrink) every cluster
    // to exactly `span_cells * cell_width` via `letter_spacing`, so a
    // narrow `·` and a wide `■` both occupy one column and trailing text
    // never shifts. Measured naturals are cached for single chars.
    let cell_width = ctx.cell_width;
    let font_size = ctx.font_size;
    let mut spacings: Vec<f32> = Vec::with_capacity(visible_clusters.len());
    for (col, s) in &visible_clusters {
        let cell = &row_cells[*col];
        let span_cells = cell.width.max(1) as f32;
        let target = span_cells * cell_width;
        let (natural, n_glyphs) = cached_advance(s, cell.bold, cell.italic, ctx);
        let n = n_glyphs.max(1) as f32;
        let mut spacing = (target - natural) / (n * font_size);
        if !spacing.is_finite() {
            // Bitmap fallbacks (e.g. macOS `GB18030Bitmap`) can shape to
            // inf; leave the advance alone rather than poisoning spans.
            spacing = 0.0;
        }
        if spacing.abs() < 1e-9 {
            spacing = 0.0;
        }
        spacings.push(spacing);
    }
    // Borrow the shared shaping inputs for span building; the exclusive
    // font-system/cache borrows ended with the measuring loop above.
    let (family, features) = (ctx.family, ctx.features.clone());
    let default_attrs = Attrs::new().family(family).font_features(features.clone());
    let mut line = BufferLine::new(
        line_text.clone(),
        LineEnding::None,
        AttrsList::new(&default_attrs),
        Shaping::Advanced,
    );
    // Default attrs use the theme foreground; per-cell spans below
    // override with effective (inverse + dim) colors.
    let mut attrs_list = AttrsList::new(&default_attrs);
    // Attr spans keyed by grid column so bold/color follow cells, not
    // characters: a ZWJ cluster is many chars but one cell.
    // Decorations (underline/strike/overline) paint via bg quads, so
    // text spans split on effective fg + bold + italic + spacing.
    let mut byte_idx = 0usize;
    let mut i = 0usize;
    while i < visible_clusters.len() {
        let (col, _) = visible_clusters[i];
        let cell = &row_cells[col];
        let color = cell.effective_fg().as_glyphon_color();
        let weight = if cell.bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        };
        let style = if cell.italic {
            Style::Italic
        } else {
            Style::Normal
        };
        let spacing = spacings[i];
        let attrs = Attrs::new()
            .family(family)
            .color(color)
            .weight(weight)
            .style(style)
            .font_features(features.clone())
            .letter_spacing(spacing);
        let start_byte = byte_idx;
        let mut j = i;
        while j < visible_clusters.len() {
            let (col2, s2) = &visible_clusters[j];
            let c2 = &row_cells[*col2];
            let same = c2.effective_fg() == cell.effective_fg()
                && c2.bold == cell.bold
                && c2.italic == cell.italic
                && spacings[j].to_bits() == spacing.to_bits();
            if !same {
                break;
            }
            byte_idx += s2.len();
            j += 1;
        }
        if j > i {
            attrs_list.add_span(start_byte..byte_idx, &attrs);
        }
        i = j;
    }
    line.set_attrs_list(attrs_list);
    line
}

/// Hash of everything that shapes a row's text line: cluster text
/// (`ch`/`extra`, minus wide continuations), per-cell width, bold/italic
/// (weight/style + measured advances), and the effective foreground (span
/// colors). Underline/strike/overline/hyperlink paint via bg quads from
/// the live grid, so they are deliberately excluded: a row whose only
/// change is a decoration keeps its layout.
pub(crate) fn row_text_hash(row: &[Cell]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for cell in row {
        // Wide continuations are skipped by shaping entirely (no glyphs,
        // no spans), so their contents must not invalidate the line.
        if cell.width == 0 {
            continue;
        }
        cell.ch.hash(&mut h);
        cell.extra.as_deref().hash(&mut h);
        cell.width.hash(&mut h);
        cell.bold.hash(&mut h);
        cell.italic.hash(&mut h);
        let fg = cell.effective_fg();
        fg.r.hash(&mut h);
        fg.g.hash(&mut h);
        fg.b.hash(&mut h);
    }
    h.finish()
}

/// Diff `rows` against cached per-row hashes: `None` means the row count
/// changed and the caller must fully rebuild; otherwise the indices of
/// rows whose shaped text differs (hashes updated in place, so repeating
/// the call with the same rows yields an empty vec).
pub(crate) fn diff_rows(hashes: &mut [u64], rows: &[Vec<Cell>]) -> Option<Vec<usize>> {
    if hashes.len() != rows.len() {
        return None;
    }
    let mut changed = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let h = row_text_hash(row);
        if h != hashes[i] {
            hashes[i] = h;
            changed.push(i);
        }
    }
    Some(changed)
}

impl Renderer {
    pub(crate) fn rebuild_buffer(&mut self, rows: &[Vec<Cell>]) {
        // Partial path: a matching hash count means metrics, theme, and
        // size are unchanged too (every change to those clears the hashes),
        // so only re-shape rows whose text hash moved. Typing touches one
        // line; unchanged lines keep their layout. A count mismatch (first
        // frame, resize, scrollback growth) takes the full path below.
        if let Some(changed) = diff_rows(&mut self.shaped_hashes, rows) {
            if !changed.is_empty() {
                crate::perf::add(crate::perf::Metric::ShapedRows, changed.len());
                // Shared shaping inputs (family/features resolved once).
                let family = font_family(&self.user_config.font.family);
                let features = terminal_font_features();
                let (cell_width, font_size, line_height) =
                    (self.cell_width, self.font_size, self.line_height);
                for i in changed {
                    let line = {
                        let mut ctx = NormCtx {
                            cell_width,
                            font_size,
                            line_height,
                            family,
                            features: features.clone(),
                            font_system: &mut self.font_system,
                            cache: &mut self.advance_cache,
                        };
                        build_buffer_line(&rows[i], &self.theme, &mut ctx)
                    };
                    // `ctx` (and its font-system/cache borrows) ends here;
                    // the buffer mutation below no longer aliases it.
                    self.buffer.lines[i] = line;
                    self.buffer.line_layout(&mut self.font_system, i);
                }
            }
            return;
        }
        // Full path: reshape every line and re-sync the hash cache.
        crate::perf::add(crate::perf::Metric::ShapedRows, rows.len());
        let metrics = Metrics::new(self.font_size, self.line_height);
        self.buffer.set_metrics(metrics);
        // Fixed-cell advances come from per-span `letter_spacing` (see
        // `build_buffer_lines`); cosmic-text's `monospace_width` rounding
        // only quantizes to a sub-pixel grid and must stay off.
        self.buffer.set_monospace_width(None);
        // Shared shaping inputs (family/features resolved once per rebuild).
        let family = font_family(&self.user_config.font.family);
        let features = terminal_font_features();
        let (cell_width, font_size, line_height) =
            (self.cell_width, self.font_size, self.line_height);
        let mut ctx = NormCtx {
            cell_width,
            font_size,
            line_height,
            family,
            features,
            font_system: &mut self.font_system,
            cache: &mut self.advance_cache,
        };
        let lines = build_buffer_lines(rows, &self.theme, &mut ctx);
        // `ctx` (and its font-system/cache borrows) ends here; the buffer
        // mutation below no longer aliases it.
        self.buffer.lines.clear();
        for line in lines {
            self.buffer.lines.push(line);
        }
        self.buffer
            .set_size(Some(self.width as f32), Some(self.height as f32));
        // `lines` was mutated directly, bypassing `Buffer`'s dirty flags, so
        // `shape_until_scroll` alone would early-return via `resolve_dirty`.
        // Lay out each line explicitly so `TextRenderer::prepare` finds glyphs.
        let n = self.buffer.lines.len();
        for i in 0..n {
            self.buffer.line_layout(&mut self.font_system, i);
        }
        // Keep wrap mode pinned (Buffer::new defaults could change).
        self.buffer.set_wrap(Wrap::None);
        self.shaped_hashes = rows.iter().map(|row| row_text_hash(row)).collect();
    }
}
