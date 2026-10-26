//! Opt-in, headless release measurements of production code. Run only the
//! ignored `headless_release` test, with one test thread; see docs/performance.md.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::config::Config;
use crate::grid::Grid;
use crate::renderer::text::{self, AdvanceCache, NormCtx};
use crate::search::{self, SearchView};
use crate::term::Terminal;
use crate::theme::Theme;

#[derive(Clone, Copy, Default)]
pub(crate) struct Allocations {
    calls: u64,
    reallocations: u64,
    bytes: u64,
    /// Relative to the start of a counting window, not process-wide live RAM.
    /// Frees of allocations made before the window can make this negative.
    pub(crate) live_delta: i64,
    pub(crate) peak_delta: i64,
    pub(crate) largest_request: usize,
}

thread_local! {
    // Thread-local counting excludes the test harness/other test threads.
    static ALLOCATIONS: Cell<Option<Allocations>> = const { Cell::new(None) };
}

struct CountingAllocator;

fn count(bytes: usize, reallocation: bool, delta: i64) {
    let _ = ALLOCATIONS.try_with(|slot| {
        if let Some(mut counts) = slot.get() {
            counts.calls += u64::from(!reallocation);
            counts.reallocations += u64::from(reallocation);
            counts.bytes += bytes as u64;
            counts.live_delta += delta;
            counts.peak_delta = counts.peak_delta.max(counts.live_delta);
            counts.largest_request = counts.largest_request.max(bytes);
            slot.set(Some(counts));
        }
    });
}

fn count_free(bytes: usize) {
    let _ = ALLOCATIONS.try_with(|slot| {
        if let Some(mut counts) = slot.get() {
            counts.live_delta -= bytes as i64;
            slot.set(Some(counts));
        }
    });
}

// SAFETY: every operation delegates unchanged to the system allocator. The
// accounting uses allocation-free TLS and is enabled only around one probe.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            count(layout.size(), false, layout.size() as i64);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            count(layout.size(), false, layout.size() as i64);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count_free(layout.size());
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !ptr.is_null() {
            count(new_size, true, new_size as i64 - layout.size() as i64);
        }
        ptr
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

pub(crate) fn allocation_counts(operation: impl FnOnce()) -> (u64, u64, u64) {
    let counts = allocation_stats(operation);
    (counts.calls, counts.reallocations, counts.bytes)
}

/// Tracks successful requested Rust bytes, not malloc slack/native/GPU/RSS or
/// an allocator's internal realloc copying peak. Use stable pre-window owners
/// (or create/drop them within the window) when asserting retention deltas.
pub(crate) fn allocation_stats(operation: impl FnOnce()) -> Allocations {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            ALLOCATIONS.with(|slot| slot.set(None));
        }
    }
    let _reset = Reset;
    ALLOCATIONS.with(|slot| slot.set(Some(Allocations::default())));
    operation();
    ALLOCATIONS.with(|slot| slot.take().unwrap())
}

#[test]
fn ascii_without_row_growth_allocates_nothing() {
    let mut terminal = Terminal::new(200, 2, Theme::default());
    terminal.feed(b"a");
    let counts = allocation_counts(|| terminal.feed(black_box(&[b'a'; 100])));
    assert_eq!(counts, (0, 0, 0));
    let mut margin = Terminal::new(2, 2, Theme::default());
    margin.feed(b"ab");
    assert_eq!(allocation_counts(|| margin.feed(b"c")), (0, 0, 0));
}

#[test]
fn bounded_row_operations_allocate_nothing_even_for_extreme_counts() {
    let mut terminal = Terminal::new(200, 60, Theme::default());
    assert_eq!(
        allocation_counts(
            || terminal.feed(b"\x1b[65535L\x1b[65535M\x1b[65535T\x1b[2;4r\x1b[65535S")
        ),
        (0, 0, 0)
    );
}

#[test]
fn word_regex_cache_reuses_success_and_failure_and_tracks_edits() {
    let mut config = crate::config::SelectionConfig::default();
    for (pattern, valid, expected) in [
        ("[a-z]+", true, "word"),
        ("([a-z", false, "word_1"),
        ("", true, "word_1"),
    ] {
        config.word_regex = pattern.into();
        config.with_word_regex(|re, result| {
            assert_eq!(result, valid);
            assert_eq!(re.find("word_1").unwrap().as_str(), expected);
        });
        assert_eq!(
            allocation_counts(|| {
                assert_eq!(config.word_regex_valid(), valid);
                config.with_word_regex(|re, _| {
                    black_box(re);
                });
            }),
            (0, 0, 0)
        );
    }
}

fn measure(name: &str, iterations: usize, mut operation: impl FnMut()) {
    operation();
    let samples: Vec<f64> = (0..7)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..iterations {
                operation();
            }
            start.elapsed().as_secs_f64() * 1e6 / iterations as f64
        })
        .collect();
    ALLOCATIONS.with(|slot| slot.set(Some(Allocations::default())));
    operation();
    let counts = ALLOCATIONS.with(|slot| slot.take().unwrap());
    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    println!(
        "PERF case={name} median_us={:.3} samples_us={samples:?} allocations={} reallocations={} requested_bytes={}",
        sorted[3], counts.calls, counts.reallocations, counts.bytes
    );
}

fn steady_terminal() -> Terminal {
    let mut terminal = Terminal::new(200, 60, Theme::default());
    terminal.feed(&b"\r\n".repeat(10_100));
    terminal
}

fn feed_case(name: &str, corpus: &[u8]) {
    let mut terminal = steady_terminal();
    measure(name, 1, || {
        for chunk in corpus.chunks(8192) {
            terminal.feed(black_box(chunk));
        }
        black_box(terminal.grid().version);
    });
}

fn history(dense: bool) -> Grid {
    let mut grid = Grid::new(200, 60, Theme::default());
    let row = if dense {
        "a".repeat(199)
    } else {
        "abcdefghij".repeat(20)
    };
    for _ in 0..10_059 {
        for c in row.chars() {
            grid.put_char(c);
        }
        grid.newline();
    }
    grid
}

fn history_memory_case(cols: usize, dense: bool) {
    let row = format!("{}\r\n", "a".repeat(if dense { cols - 1 } else { 1 }));
    let mut retained = None;
    let counts = allocation_stats(|| {
        let mut terminal = Terminal::new(cols, 60, Theme::default());
        for _ in 0..20_000 {
            terminal.feed(row.as_bytes());
        }
        retained = Some(terminal);
    });
    let terminal = retained.as_mut().unwrap();
    let growth = allocation_stats(|| {
        for _ in 0..20_000 {
            terminal.feed(row.as_bytes());
        }
    });
    println!(
        "PERF history_memory cols={cols} dense={dense} rows={} accounted_bytes={} live_requested_bytes={} peak_requested_bytes={} later_live_delta={} later_peak_delta={}",
        terminal.grid().scrollback_len(),
        terminal.grid().history_bytes(),
        counts.live_delta,
        counts.peak_delta,
        growth.live_delta,
        growth.peak_delta,
    );
    assert!(
        growth.live_delta.abs() < 512 * 1024,
        "history still plateaus across fixed-size output"
    );
    let freed = allocation_stats(|| drop(retained));
    assert_eq!(counts.live_delta + growth.live_delta + freed.live_delta, 0);
}

#[test]
#[ignore = "manual release benchmark; not a wall-clock CI gate"]
// Runtime guard, not a const assertion: debug test binaries must still build.
#[allow(clippy::assertions_on_constants)]
fn headless_release() {
    assert!(!cfg!(debug_assertions), "run this test with --release");
    let mode = std::env::var("COMETTY_BENCH_LOG").unwrap_or_else(|_| "default".into());
    let directives = match mode.as_str() {
        "default" => "cometty=debug,warn",
        "quiet" => "off",
        _ => panic!("COMETTY_BENCH_LOG must be default or quiet"),
    };
    crate::logging::install(
        Arc::new(Mutex::new(crate::logbuf::LogBuffer::new(1000))),
        None,
        directives,
    )
    .expect("benchmark runs in its own test process");
    println!(
        "PERF metadata logging={mode} arch={} os={} font=monospace size=16 scale=1 grid=200x60 chunk=8192 batches=7",
        std::env::consts::ARCH,
        std::env::consts::OS
    );
    feed_case("ascii", &vec![b'a'; 262_144]);
    feed_case(
        "crlf",
        &format!("{}\r\n", "a".repeat(78)).repeat(3277).into_bytes(),
    );
    feed_case("sgr", &b"\x1b[38;2;100;150;200mx\x1b[0m".repeat(10_923));
    feed_case(
        "osc8",
        &b"\x1b]8;;https://example.com/path\x1b\\x\x1b]8;;\x1b\\".repeat(6500),
    );
    let ignored = format!("\x1b]999;{}\x07", "x".repeat(1 << 20));
    let mut terminal = Terminal::new(200, 60, Theme::default());
    measure("ignored_osc", 1, || {
        terminal.feed(black_box(ignored.as_bytes()))
    });
    for (name, sequence) in [("il_65535", "\x1b[65535L"), ("dl_65535", "\x1b[65535M")] {
        measure(name, 1, || terminal.feed(black_box(sequence.as_bytes())));
    }

    for dense in [false, true] {
        let grid = history(dense);
        println!(
            "PERF history dense={dense} rows={} accounted_bytes={}",
            grid.scrollback_len(),
            grid.history_bytes()
        );
        for query in ["missing", "a"] {
            let name = format!(
                "search_{}_{}",
                if dense { "dense" } else { "sparse" },
                query
            );
            measure(&name, 2, || {
                black_box(search::find_matches(&grid, black_box(query)));
            });
        }
        let matches = search::find_matches(&grid, "a");
        println!(
            "PERF memory dense={dense} matches={} capacity={} inline_bytes={}",
            matches.len(),
            matches.capacity(),
            matches.capacity() * size_of::<search::Match>()
        );
        measure(
            if dense {
                "projection_dense"
            } else {
                "projection_sparse"
            },
            20,
            || {
                black_box(SearchView::build(
                    &matches,
                    Some(0),
                    grid.scrollback_len(),
                    0,
                    60,
                ));
            },
        );
        let mut panel = crate::app::search::SearchPanel::default();
        panel.focus_or_open();
        panel.query = "a".into();
        panel.refresh(&grid);
        measure(
            if dense {
                "projection_cached_dense"
            } else {
                "projection_cached_sparse"
            },
            1000,
            || {
                black_box(panel.view(black_box(&grid)));
            },
        );
    }

    for cols in [80, 200, 400, 1024] {
        for dense in [false, true] {
            history_memory_case(cols, dense);
        }
    }

    let config = Config::default();
    let theme = Theme::default();
    let mut fonts = cosmic_text::FontSystem::new();
    let mut cache = AdvanceCache::new();
    for (name, cluster) in [
        ("shape_ascii", "e"),
        ("shape_decomposed", "e\u{301}"),
        ("shape_zwj", "👨\u{200d}👩\u{200d}👧"),
    ] {
        let mut grid = Grid::new(200, 60, theme);
        for _ in 0..200 {
            for c in cluster.chars() {
                grid.put_char(c);
            }
        }
        let row = &grid.visible_rows()[0];
        let mut buffer = cosmic_text::Buffer::new(
            &mut fonts,
            cosmic_text::Metrics::new(
                config.font.size,
                config.font.size * config.font.line_height_factor,
            ),
        );
        buffer.set_wrap(cosmic_text::Wrap::None);
        buffer.set_size(Some(3200.0), Some(2000.0));
        let mut ctx = NormCtx {
            cell_width: config.font.size * config.font.cell_width_factor,
            font_size: config.font.size,
            line_height: config.font.size * config.font.line_height_factor,
            family: text::font_family(&config.font.family),
            features: text::terminal_font_features(),
            font_system: &mut fonts,
            cache: &mut cache,
        };
        measure(name, 5, || {
            black_box(text::build_buffer_line(black_box(row), &theme, &mut ctx));
        });
        measure(&name.replacen("shape_", "layout_", 1), 5, || {
            let line = text::build_buffer_line(black_box(row), &theme, &mut ctx);
            buffer.lines.clear();
            buffer.lines.push(line);
            black_box(buffer.line_layout(ctx.font_system, 0));
        });
        measure(&format!("{name}_cold_advances"), 5, || {
            ctx.cache.clear();
            black_box(text::build_buffer_line(black_box(row), &theme, &mut ctx));
        });
    }
    let grid = Grid::new(200, 60, theme);
    let mut hashes: Vec<_> = grid
        .visible_rows()
        .iter()
        .map(|r| text::row_text_hash(r))
        .collect();
    measure("hash_unchanged", 100, || {
        black_box(text::diff_rows(&mut hashes, grid.visible_rows()));
    });
    let row: Vec<_> = "an_example_word and another"
        .chars()
        .map(|c| crate::selection::SelCell::narrow(&c.to_string()))
        .collect();
    measure("expand_word", 100, || {
        black_box(crate::selection::expand_word(&row, 5, &config.selection));
    });

    // Third tranche: lazy history width projection, recycled scroll storage,
    // one-pass paste construction and incremental log filtering.
    let mut resize_grid = Grid::new_with_config(200, 60, theme, &config);
    for _ in 0..10_100 {
        resize_grid.newline();
    }
    measure("resize_lazy_200_201", 5, || {
        resize_grid.resize(201, 60);
        resize_grid.resize(200, 60);
    });
    let mut scroll_grid = Grid::new_with_config(200, 60, theme, &config);
    for _ in 0..10_100 {
        scroll_grid.newline();
    }
    measure("scroll_recycle_1000", 1, || {
        for _ in 0..1000 {
            scroll_grid.newline();
        }
        black_box(scroll_grid.version);
    });
    let paste = "a".repeat(1 << 20);
    measure("paste_build_1m", 5, || {
        black_box(crate::app::clipboard::paste_bytes(black_box(&paste), true));
    });
    let ring = {
        let mut ring = crate::logbuf::LogBuffer::new(100_000);
        for i in 0..100_000 {
            ring.push(log::Level::Debug, "cometty::bench", format!("entry {i}"));
        }
        Mutex::new(ring)
    };
    let mut index = crate::logbuf::LogIndex::default();
    index.refresh(&ring, log::LevelFilter::Debug, "");
    while index.scanning {
        index.refresh(&ring, log::LevelFilter::Debug, "");
    }
    measure("log_index_unchanged", 100, || {
        index.refresh(&ring, log::LevelFilter::Debug, "");
        black_box(index.ids.len());
    });
    measure("log_index_slice", 5, || {
        let mut index = crate::logbuf::LogIndex::default();
        index.refresh(&ring, log::LevelFilter::Debug, "");
        black_box(index.ids.len());
    });
    measure("log_index_full_scan", 5, || {
        let mut index = crate::logbuf::LogIndex::default();
        index.refresh(&ring, log::LevelFilter::Debug, "");
        while index.scanning {
            index.refresh(&ring, log::LevelFilter::Debug, "");
        }
        black_box(index.ids.len());
    });
}
