//! Opt-in, aggregate CPU phase diagnostics (`COMETTY_PERF=1`). No hot-cell
//! instrumentation, new wake timers, or log-ring feedback. These timings
//! are CPU-side elapsed phases (including waits/preemption), not process
//! CPU time, GPU execution or input-to-visible latency.

use std::collections::BTreeMap;
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
    AdvanceMiss,
    PrepareReuse,
    QueueWait,
    QueuedBytes,
    CoalescedWake,
    DrainYield,
    UploadFlush,
    HistorySeal,
    TitleRefresh,
    RedrawTotal,
}

const NAMES: [&str; 29] = [
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
    "advance_miss",
    "prepare_reuse",
    "queue_wait",
    "queued_bytes",
    "coalesced_wake",
    "drain_yield",
    "upload_flush",
    "history_seal",
    "title_refresh",
    "redraw_total",
];

#[derive(Clone, Copy, Default)]
struct Sample {
    count: u64,
    total: Duration,
    max: Duration,
    value_max: usize,
    // Log2 microsecond buckets; fixed memory, approximate percentile upper bounds.
    buckets: [u64; 32],
}

impl Sample {
    fn record(&mut self, count: u64, duration: Duration) {
        self.count = self.count.saturating_add(count);
        self.total = self.total.saturating_add(duration);
        self.max = self.max.max(duration);
        let us = duration.as_nanos().div_ceil(1000).min(u32::MAX as u128) as u32;
        let bucket = (u32::BITS - us.saturating_sub(1).leading_zeros()).min(31) as usize;
        self.buckets[bucket] = self.buckets[bucket].saturating_add(count);
    }

    fn percentile_us(&self, percent: u64) -> u64 {
        let timed = self
            .buckets
            .iter()
            .fold(0_u64, |sum, n| sum.saturating_add(*n));
        if timed == 0 {
            return 0;
        }
        let target = timed.saturating_mul(percent).div_ceil(100);
        let mut seen: u64 = 0;
        for (i, count) in self.buckets.iter().enumerate() {
            seen = seen.saturating_add(*count);
            if seen >= target {
                return (1_u64 << i).max(if i == 31 {
                    self.max.as_nanos().div_ceil(1000).min(u64::MAX as u128) as u64
                } else {
                    0
                });
            }
        }
        0
    }
}

#[derive(Clone, Copy)]
pub(crate) enum TabMetric {
    ServiceGap,
    InputOutput,
    OutputPresent,
}

const TAB_NAMES: [&str; 3] = ["service_gap", "input_first_output", "output_present"];
const MAX_PROFILE_TABS: usize = 64;

struct Metrics {
    since: Instant,
    samples: [Sample; NAMES.len()],
    tabs: BTreeMap<u64, [Sample; 3]>,
}

static METRICS: OnceLock<Option<Mutex<Metrics>>> = OnceLock::new();

fn state() -> Option<&'static Mutex<Metrics>> {
    METRICS
        .get_or_init(|| {
            (std::env::var("COMETTY_PERF").as_deref() == Ok("1")).then(|| {
                Mutex::new(Metrics {
                    since: Instant::now(),
                    samples: [Sample::default(); NAMES.len()],
                    tabs: BTreeMap::new(),
                })
            })
        })
        .as_ref()
}

pub(crate) fn enabled() -> bool {
    state().is_some()
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
        sample.record(count, duration);
    }
}

pub(crate) fn add(metric: Metric, count: usize) {
    if let Some(state) = state() {
        let mut metrics = state.lock().unwrap_or_else(|e| e.into_inner());
        let sample = &mut metrics.samples[metric as usize];
        sample.count = sample.count.saturating_add(count as u64);
    }
}

pub(crate) fn duration(metric: Metric, duration: Duration) {
    record(metric, 1, duration);
}

pub(crate) fn peak(metric: Metric, value: usize) {
    if let Some(state) = state() {
        let mut metrics = state.lock().unwrap_or_else(|e| e.into_inner());
        let sample = &mut metrics.samples[metric as usize];
        sample.value_max = sample.value_max.max(value);
    }
}

pub(crate) fn tab_duration(tab: u64, metric: TabMetric, duration: Duration) {
    if let Some(state) = state() {
        let mut metrics = state.lock().unwrap_or_else(|e| e.into_inner());
        if metrics.tabs.len() < MAX_PROFILE_TABS || metrics.tabs.contains_key(&tab) {
            metrics.tabs.entry(tab).or_default()[metric as usize].record(1, duration);
        }
    }
}

/// Reports only when the event loop is already awake. Stderr deliberately
/// bypasses both application log filters, so observing cannot wake itself.
pub(crate) fn report_due(now: Instant) {
    let Some(state) = state() else {
        return;
    };
    let (interval, samples, tabs) = {
        let mut metrics = state.lock().unwrap_or_else(|e| e.into_inner());
        let interval = now.saturating_duration_since(metrics.since);
        if interval < Duration::from_secs(10) {
            return;
        }
        metrics.since = now;
        (
            interval,
            std::mem::replace(&mut metrics.samples, [Sample::default(); NAMES.len()]),
            std::mem::take(&mut metrics.tabs),
        )
    };
    let mut report = format!(
        "cometty-perf (CPU-side elapsed, interval counters) interval_us={}:",
        interval.as_micros()
    );
    use std::fmt::Write;
    for (name, sample) in NAMES.iter().zip(samples) {
        if sample.count > 0 || sample.value_max > 0 {
            let _ = write!(
                report,
                " {name}={{n:{},us:{},max_us:{},p99_us_le:{},max_value:{}}}",
                sample.count,
                sample.total.as_micros(),
                sample.max.as_micros(),
                sample.percentile_us(99),
                sample.value_max,
            );
        }
    }
    eprintln!("{report}");
    for (tab, samples) in tabs {
        let mut report = format!("cometty-perf tab={tab}:");
        for (name, sample) in TAB_NAMES.iter().zip(samples) {
            if sample.count > 0 {
                let _ = write!(
                    report,
                    " {name}={{n:{},p99_us_le:{},max_us:{}}}",
                    sample.count,
                    sample.percentile_us(99),
                    sample.max.as_micros()
                );
            }
        }
        eprintln!("{report}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_has_bounded_storage_and_reports_percentile_upper_bounds() {
        let mut sample = Sample::default();
        assert_eq!(sample.percentile_us(99), 0);
        for _ in 0..99 {
            sample.record(1, Duration::from_micros(3));
        }
        sample.record(1, Duration::from_millis(5));
        assert_eq!(sample.percentile_us(99), 4);
        assert_eq!(sample.percentile_us(100), 8192);
        let mut sub_microsecond = Sample::default();
        sub_microsecond.record(1, Duration::from_nanos(1900));
        assert_eq!(sub_microsecond.percentile_us(99), 2);
        sample.record(1, Duration::from_secs(5000));
        assert!(sample.percentile_us(100) >= 5_000_000_000);
    }
}
