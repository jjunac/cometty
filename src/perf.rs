//! Opt-in, aggregate CPU phase diagnostics (`COMETTY_PERF=1`). No hot-cell
//! instrumentation, new wake timers, or log-ring feedback. These timings
//! are CPU-side elapsed phases (including waits/preemption), not process
//! CPU time, GPU execution or input-to-visible latency.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum Metric {
    Drain,
    Parse,
    Search,
    Projection,
    Snapshot,
    Rebuild,
    Background,
    Overlay,
    Acquire,
    Prepare,
    EncodeSubmit,
    Present,
    Wake,
    Redraw,
    Presented,
    Skipped,
    ParsedBytes,
    ShapedRows,
    SnapshotCells,
}

const NAMES: [&str; 19] = [
    "drain",
    "parse",
    "search",
    "projection",
    "snapshot",
    "rebuild",
    "background",
    "overlay",
    "acquire",
    "prepare",
    "encode_submit",
    "present",
    "pty_wake",
    "redraw",
    "presented",
    "skipped",
    "parsed_bytes",
    "shaped_rows",
    "snapshot_cells",
];

#[derive(Clone, Copy, Default)]
struct Sample {
    count: u64,
    total: Duration,
    max: Duration,
}

struct Metrics {
    since: Instant,
    samples: [Sample; NAMES.len()],
}

static METRICS: OnceLock<Option<Mutex<Metrics>>> = OnceLock::new();

fn state() -> Option<&'static Mutex<Metrics>> {
    METRICS
        .get_or_init(|| {
            (std::env::var("COMETTY_PERF").as_deref() == Ok("1")).then(|| {
                Mutex::new(Metrics {
                    since: Instant::now(),
                    samples: [Sample::default(); NAMES.len()],
                })
            })
        })
        .as_ref()
}

pub(crate) struct Timer {
    metric: Metric,
    start: Option<Instant>,
}

pub(crate) fn measure(metric: Metric) -> Timer {
    Timer {
        metric,
        start: state().map(|_| Instant::now()),
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Some(start) = self.start {
            record(self.metric, 1, start.elapsed());
        }
    }
}

fn record(metric: Metric, count: u64, duration: Duration) {
    if let Some(state) = state() {
        let mut metrics = state.lock().unwrap_or_else(|e| e.into_inner());
        let sample = &mut metrics.samples[metric as usize];
        sample.count = sample.count.saturating_add(count);
        sample.total = sample.total.saturating_add(duration);
        sample.max = sample.max.max(duration);
    }
}

pub(crate) fn add(metric: Metric, count: usize) {
    record(metric, count as u64, Duration::ZERO);
}

/// Reports only when the event loop is already awake. Stderr deliberately
/// bypasses both application log filters, so observing cannot wake itself.
pub(crate) fn report_due(now: Instant) {
    let Some(state) = state() else {
        return;
    };
    let samples = {
        let mut metrics = state.lock().unwrap_or_else(|e| e.into_inner());
        if now.saturating_duration_since(metrics.since) < Duration::from_secs(10) {
            return;
        }
        metrics.since = now;
        std::mem::replace(&mut metrics.samples, [Sample::default(); NAMES.len()])
    };
    let mut report = String::from("cometty-perf (CPU-side elapsed, interval counters):");
    use std::fmt::Write;
    for (name, sample) in NAMES.iter().zip(samples) {
        if sample.count > 0 {
            let _ = write!(
                report,
                " {name}={{n:{},us:{},max_us:{}}}",
                sample.count,
                sample.total.as_micros(),
                sample.max.as_micros()
            );
        }
    }
    eprintln!("{report}");
}
