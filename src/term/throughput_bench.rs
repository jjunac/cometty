//! Exact comparison-corpus replay, without PTY/window/rendering. Deliberately
//! separate from the older 200x60 microbenchmarks; see docs/performance.md.

use std::hint::black_box;
use std::time::{Duration, Instant};

use super::{Terminal, parser::BoundedParser};
use crate::{config::Config, perf_bench::allocation_counts, theme::Theme};

#[derive(Debug, Default, PartialEq, Eq)]
struct Work {
    prints: u64,
    executes: u64,
    csi: u64,
    checksum: u64,
}

impl vte::Perform for Work {
    fn print(&mut self, c: char) {
        self.prints += 1;
        self.checksum = self.checksum.wrapping_add(c as u64);
    }

    fn execute(&mut self, byte: u8) {
        self.executes += 1;
        self.checksum = self.checksum.wrapping_add(byte as u64);
    }

    fn csi_dispatch(&mut self, _: &vte::Params, _: &[u8], _: bool, action: char) {
        self.csi += 1;
        self.checksum = self.checksum.wrapping_add(action as u64);
    }
}

fn selected(variable: &str, value: &str) -> bool {
    std::env::var(variable).map_or(true, |filter| filter.split(',').any(|s| s == value))
}

fn number(variable: &str, default: usize) -> usize {
    std::env::var(variable).map_or(default, |s| s.parse().expect(variable))
}

fn measure(name: &str, lane: &str, bytes: usize, warmups: usize, mut operation: impl FnMut()) {
    for _ in 0..warmups {
        operation(); // Warm parser/history/pools; not part of the timed samples.
    }
    let samples: Vec<_> = (0..number("COMETTY_BENCH_SAMPLES", 5))
        .map(|_| {
            let start = Instant::now();
            operation();
            start.elapsed().as_secs_f64()
        })
        .collect();
    assert!(!samples.is_empty());
    // Allocation counting is never active during timing or native sampling.
    let (allocs, reallocs, requested) = allocation_counts(&mut operation);
    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    let seconds = sorted[sorted.len() / 2];
    let mib = bytes as f64 / (1 << 20) as f64;
    println!(
        "THROUGHPUT case={name} lane={lane} bytes={bytes} warmups={warmups} mib_s={:.3} ns_byte={:.3} allocations_mib={:.1} reallocations_mib={:.1} requested_bytes_mib={:.1} samples_s={samples:?}",
        mib / seconds,
        seconds * 1e9 / bytes as f64,
        allocs as f64 / mib,
        reallocs as f64 / mib,
        requested as f64 / mib,
    );
    // For `sample PID ...` / other native CPU profilers: repeat only a selected
    // case/lane. Each operation still uses the real streaming feed boundaries.
    let duration = Duration::from_secs(number("COMETTY_BENCH_PROFILE_SECONDS", 0) as u64);
    if !duration.is_zero() {
        println!("PROFILE pid={} case={name} lane={lane}", std::process::id());
        let start = Instant::now();
        while start.elapsed() < duration {
            operation();
        }
    }
}

#[test]
#[ignore = "manual release throughput diagnosis; needs generated comparison corpora"]
#[allow(clippy::assertions_on_constants)]
fn replay() {
    assert!(!cfg!(debug_assertions), "run this test with --release");
    let directory = std::env::var("COMETTY_BENCH_CORPUS").expect("run scripts/bench-throughput.py");
    let cols = number("COMETTY_BENCH_COLS", 118);
    let rows = number("COMETTY_BENCH_ROWS", 37);
    let chunk_size = number("COMETTY_BENCH_CHUNK", 8192);
    assert!(chunk_size > 0);
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "vt"))
        .collect();
    paths.sort();
    println!(
        "THROUGHPUT metadata arch={} os={} grid={cols}x{rows} chunk={chunk_size} cell_bytes={}",
        std::env::consts::ARCH,
        std::env::consts::OS,
        size_of::<crate::grid::Cell>(),
    );
    let mut cases = 0;
    for path in paths {
        let name = path.file_stem().unwrap().to_str().unwrap();
        if !selected("COMETTY_BENCH_CASE", name) {
            continue;
        }
        cases += 1;
        let corpus = std::fs::read(&path).unwrap();
        assert!(!corpus.is_empty());
        let mut work = Work::default();
        vte::Parser::new().advance(&mut work, &corpus);
        println!(
            "THROUGHPUT work case={name} bytes={} prints={} executes={} csi={}",
            corpus.len(),
            work.prints,
            work.executes,
            work.csi,
        );
        for lane in ["vte", "guard", "history", "no_history", "alt"] {
            if !selected("COMETTY_BENCH_LANE", lane) {
                continue;
            }
            let mut raw = vte::Parser::new();
            let mut guarded = BoundedParser::new();
            let mut sink = Work::default();
            let mut config = Config::default();
            if lane == "no_history" {
                config.terminal.scrollback_lines = 0;
            }
            let mut terminal = Terminal::new_with_config(cols, rows, Theme::default(), &config);
            terminal.feed(b"\x1b[?25l\x1b[2J\x1b[H");
            if lane == "alt" {
                terminal.feed(b"\x1b[?1049h");
            }
            let lines = corpus.iter().filter(|&&b| b == b'\n').count();
            let warmups = if lane == "history" && lines > 0 {
                (config.terminal.scrollback_lines + rows).div_ceil(lines)
            } else {
                1
            };
            measure(name, lane, corpus.len(), warmups, || {
                for chunk in corpus.chunks(chunk_size) {
                    match lane {
                        "vte" => raw.advance(&mut sink, black_box(chunk)),
                        "guard" => guarded.advance(&mut sink, black_box(chunk)),
                        _ => terminal.feed(black_box(chunk)),
                    }
                }
                black_box((&sink, terminal.grid().version));
            });
            println!(
                "THROUGHPUT retained case={name} lane={lane} history_rows={} history_bytes={}",
                terminal.grid().scrollback_len(),
                terminal.grid().history_bytes(),
            );
        }
    }
    assert!(cases > 0, "no matching corpus");
}

#[test]
fn parser_lanes_count_identical_work_across_feed_boundaries() {
    let corpus = "ASCII\r\n漢e\u{301}👨\u{200d}👩\x1b[38;5;42mx\x1b[0m\x1b[c";
    for size in 1..=corpus.len() {
        let mut raw = vte::Parser::new();
        let mut guard = BoundedParser::new();
        let mut expected = Work::default();
        let mut actual = Work::default();
        for chunk in corpus.as_bytes().chunks(size) {
            raw.advance(&mut expected, chunk);
            guard.advance(&mut actual, chunk);
        }
        assert_eq!(actual, expected, "chunk={size}");
        assert!(actual.prints > 0 && actual.executes > 0 && actual.csi > 0);
    }
}

/// Rebuild the exact final viewport's text using production helpers and retina
/// metrics, but no GPU. Font-selection lanes keep shaping mode/features,
/// cell metrics and text identical; they do not install a production fix.
#[test]
#[ignore = "manual release layout/font-fallback diagnosis; needs generated corpora"]
#[allow(clippy::assertions_on_constants)]
fn layout() {
    use crate::renderer::text::{AdvanceCache, NormCtx, build_buffer_line, terminal_font_features};
    use cosmic_text::{Buffer, Family, FontSystem, Metrics, Wrap};
    use std::collections::BTreeSet;

    assert!(!cfg!(debug_assertions), "run this test with --release");
    let directory = std::env::var("COMETTY_BENCH_CORPUS").expect("run scripts/bench-throughput.py");
    let cols = number("COMETTY_BENCH_COLS", 118);
    let rows = number("COMETTY_BENCH_ROWS", 37);
    let font_size = number("COMETTY_BENCH_FONT_SIZE", 28) as f32;
    let line_height = font_size * 1.25;
    let cell_width = font_size * 0.602;
    static FAMILY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let resolved = FAMILY
        .get_or_init(|| std::env::var("COMETTY_BENCH_FONT").unwrap_or_else(|_| "Menlo".into()))
        .as_str();
    for name in ["ascii", "sgr", "unicode", "scroll_region", "redraw"] {
        if !selected("COMETTY_BENCH_CASE", name) {
            continue;
        }
        let corpus =
            std::fs::read(std::path::Path::new(&directory).join(format!("{name}.vt"))).unwrap();
        let mut terminal = Terminal::new(cols, rows, Theme::default());
        // Redraw restores the primary screen at the end; inspect its TUI first.
        let body = corpus.strip_suffix(b"\x1b[?1049l").unwrap_or(&corpus);
        for chunk in body.chunks(8192) {
            terminal.feed(chunk);
        }
        let view: Vec<_> = terminal
            .grid()
            .view_rows()
            .iter()
            .map(|row| row.to_vec())
            .collect();
        for lane in ["default", "resolved_mono", "named", "production"] {
            if !selected("COMETTY_BENCH_LANE", lane) {
                continue;
            }
            let mut fonts = FontSystem::new();
            let production_name = crate::renderer::text::resolved_font_name(&fonts, "monospace");
            if lane == "production" {
                crate::renderer::text::configure_shaping_cache(&mut fonts);
            }
            if matches!(lane, "resolved_mono" | "named") {
                if !fonts
                    .db()
                    .faces()
                    .any(|face| face.families.iter().any(|(name, _)| name == resolved))
                {
                    println!("LAYOUT skipped case={name} lane={lane} missing_family={resolved:?}");
                    continue;
                }
                if lane == "resolved_mono" {
                    fonts.db_mut().set_monospace_family(resolved);
                }
            }
            let generic = fonts.db().family_name(&Family::Monospace).to_owned();
            let generic_found = fonts
                .db()
                .faces()
                .any(|face| face.families.iter().any(|(name, _)| name == &generic));
            println!(
                "LAYOUT fonts case={name} lane={lane} faces={} generic={generic:?} generic_found={generic_found}",
                fonts.db().len()
            );
            let mut cache = AdvanceCache::new();
            let mut buffer = Buffer::new(&mut fonts, Metrics::new(font_size, line_height));
            buffer.set_wrap(Wrap::None);
            buffer.set_size(
                Some(cols as f32 * cell_width),
                Some(rows as f32 * line_height),
            );
            let (samples, counts) = {
                let mut operation = || {
                    let mut build = Duration::ZERO;
                    let mut layout = Duration::ZERO;
                    for (i, row) in view.iter().enumerate() {
                        let start = Instant::now();
                        let line = build_buffer_line(
                            row,
                            &Theme::default(),
                            &mut NormCtx {
                                cell_width,
                                font_size,
                                line_height,
                                family: if lane == "named" {
                                    Family::Name(resolved)
                                } else if lane == "production" {
                                    crate::renderer::text::font_family(&production_name)
                                } else {
                                    Family::Monospace
                                },
                                features: terminal_font_features(),
                                font_system: &mut fonts,
                                cache: &mut cache,
                            },
                        );
                        if i < buffer.lines.len() {
                            buffer.lines[i] = line;
                        } else {
                            buffer.lines.push(line);
                        }
                        build += start.elapsed();
                        let start = Instant::now();
                        black_box(buffer.line_layout(&mut fonts, i));
                        layout += start.elapsed();
                    }
                    (build.as_secs_f64() * 1e3, layout.as_secs_f64() * 1e3)
                };
                operation();
                let samples: Vec<_> = (0..number("COMETTY_BENCH_SAMPLES", 5))
                    .map(|_| operation())
                    .collect();
                let counts = allocation_counts(|| {
                    black_box(operation());
                });
                (samples, counts)
            };
            assert!(!samples.is_empty());
            let median = |mut values: Vec<f64>| {
                values.sort_by(f64::total_cmp);
                values[values.len() / 2]
            };
            let families: BTreeSet<_> = buffer
                .layout_runs()
                .flat_map(|run| run.glyphs.iter())
                .filter_map(|glyph| {
                    fonts
                        .db()
                        .face(glyph.font_id)
                        .map(|face| face.post_script_name.clone())
                })
                .collect();
            println!(
                "LAYOUT cache case={name} lane={lane} entries={} retained_bytes={}",
                fonts.shape_run_cache.len(),
                fonts.shape_run_cache.retained_bytes(),
            );
            println!(
                "LAYOUT case={name} lane={lane} grid={cols}x{rows} font_size={font_size} frame_ms={:.3} build_ms={:.3} layout_ms={:.3} allocations_frame={} reallocations_frame={} requested_bytes_frame={} fonts={families:?} samples_ms={samples:?}",
                median(
                    samples
                        .iter()
                        .map(|(build, layout)| build + layout)
                        .collect()
                ),
                median(samples.iter().map(|(build, _)| *build).collect()),
                median(samples.iter().map(|(_, layout)| *layout).collect()),
                counts.0,
                counts.1,
                counts.2,
            );
        }
    }
}
