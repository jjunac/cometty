//! Opt-in, aggregate CPU phase diagnostics (`COMETTY_PERF=1`). No hot-cell
//! instrumentation, event-loop wake timers, or log-ring feedback. These
//! timings are CPU-side elapsed phases (including waits/preemption), not
//! process CPU time, GPU execution or input-to-visible latency. Each report
//! also carries the process's own CPU time and wakeup deltas for its
//! interval, plus event-loop wake causes and redraw-request reasons (idle
//! diagnosis). A reporter thread prints every ten seconds, even while the
//! event loop sleeps, and once more at exit.

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
    // Idle/wake diagnostics: why the loop woke and why frames were requested.
    LoopTurn,
    WakeTimer,
    WakeEvent,
    WindowEvent,
    PointerEvent,
    FocusIn,
    FocusOut,
    Occluded,
    RequestBlink,
    RequestEguiInput,
    RequestEguiDeadline,
    RequestEguiImmediate,
    RequestOutput,
    RequestOutputDeadline,
    RequestChrome,
    RequestPointer,
    EguiPass,
    BgVertices,
    SupervisorPoll,
}

const NAMES: [&str; 48] = [
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
    "loop_turn",
    "wake_timer",
    "wake_event",
    "window_event",
    "pointer_event",
    "focus_in",
    "focus_out",
    "occluded",
    "request_blink",
    "request_egui_input",
    "request_egui_deadline",
    "request_egui_immediate",
    "request_output",
    "request_output_deadline",
    "request_chrome",
    "request_pointer",
    "egui_pass",
    "bg_vertices",
    "supervisor_poll",
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

/// Whole-process counters, diffed per report interval. They include every
/// thread (PTY workers, the supervisor, Metal/AppKit queues), unlike the
/// main-thread phase timings above.
#[derive(Clone, Copy, Default)]
struct ProcessUsage {
    cpu: Duration,
    voluntary_switches: u64,
    involuntary_switches: u64,
    /// macOS only (`proc_pid_rusage`): wakeups from package idle, the
    /// counter Activity Monitor shows as "Idle Wake Ups", and all
    /// interrupt-driven wakeups.
    idle_wakeups: u64,
    interrupt_wakeups: u64,
    /// macOS only: retired instructions. Unlike CPU time and energy, this
    /// does not depend on whether work ran on performance or efficiency cores.
    instructions: u64,
}

#[cfg(unix)]
fn process_usage() -> Option<ProcessUsage> {
    // SAFETY: an all-zero `rusage` is a valid out-parameter for getrusage.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `ru` is a live, writable rusage for the duration of the call.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } != 0 {
        return None;
    }
    let time = |t: libc::timeval| {
        Duration::from_secs(t.tv_sec.max(0) as u64) + Duration::from_micros(t.tv_usec.max(0) as u64)
    };
    #[allow(unused_mut)]
    let mut usage = ProcessUsage {
        cpu: time(ru.ru_utime) + time(ru.ru_stime),
        voluntary_switches: ru.ru_nvcsw.max(0) as u64,
        involuntary_switches: ru.ru_nivcsw.max(0) as u64,
        ..ProcessUsage::default()
    };
    #[cfg(target_os = "macos")]
    {
        // SAFETY: an all-zero v4 record is a valid out-parameter.
        let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
        // SAFETY: the flavor matches the record type, which stays live and
        // writable for the call; `rusage_info_t *` is how the C API takes it.
        let ok = unsafe {
            libc::proc_pid_rusage(
                libc::getpid(),
                libc::RUSAGE_INFO_V4,
                (&mut info as *mut libc::rusage_info_v4).cast(),
            )
        } == 0;
        if ok {
            usage.idle_wakeups = info.ri_pkg_idle_wkups;
            usage.interrupt_wakeups = info.ri_interrupt_wkups;
            usage.instructions = info.ri_instructions;
        }
    }
    Some(usage)
}

#[cfg(not(unix))]
fn process_usage() -> Option<ProcessUsage> {
    None
}

struct Metrics {
    started: Instant,
    since: Instant,
    usage: Option<ProcessUsage>,
    samples: [Sample; NAMES.len()],
    tabs: BTreeMap<u64, [Sample; 3]>,
}

static METRICS: OnceLock<Option<Mutex<Metrics>>> = OnceLock::new();

fn state() -> Option<&'static Mutex<Metrics>> {
    METRICS
        .get_or_init(|| {
            (std::env::var("COMETTY_PERF").as_deref() == Ok("1")).then(|| {
                let now = Instant::now();
                Mutex::new(Metrics {
                    started: now,
                    since: now,
                    usage: process_usage(),
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

const REPORT_INTERVAL: Duration = Duration::from_secs(10);

/// With counters enabled, report every ten seconds from a dedicated thread,
/// so a sleeping event loop is observable too. This costs one process wakeup
/// per report and never wakes the event loop. Call once, at startup.
pub(crate) fn start() {
    if !enabled() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("perf-report".into())
        .spawn(|| {
            loop {
                std::thread::sleep(REPORT_INTERVAL);
                report(Instant::now());
            }
        });
    if let Err(e) = spawned {
        eprintln!("cometty-perf: reporter thread unavailable ({e}); exit report only");
    }
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

/// Flush the partial interval at exit (after the last periodic report), so
/// short runs still report. Stderr deliberately bypasses both application
/// log filters, so observing cannot feed the log panel or wake the loop.
pub(crate) fn report_final() {
    report(Instant::now());
}

fn report(now: Instant) {
    let Some(state) = state() else {
        return;
    };
    let (interval, at, samples, tabs, usage) = {
        let mut metrics = state.lock().unwrap_or_else(|e| e.into_inner());
        let interval = now.saturating_duration_since(metrics.since);
        metrics.since = now;
        let current = process_usage();
        let previous = std::mem::replace(&mut metrics.usage, current);
        (
            interval,
            now.saturating_duration_since(metrics.started),
            std::mem::replace(&mut metrics.samples, [Sample::default(); NAMES.len()]),
            std::mem::take(&mut metrics.tabs),
            previous.zip(current),
        )
    };
    let mut report = format!(
        "cometty-perf (CPU-side elapsed, interval counters) interval_us={} at_ms={}:",
        interval.as_micros(),
        at.as_millis()
    );
    use std::fmt::Write;
    if let Some((before, after)) = usage {
        let cpu = after.cpu.saturating_sub(before.cpu);
        let _ = write!(
            report,
            " process={{cpu_us:{},cpu_pct:{:.3}",
            cpu.as_micros(),
            100.0 * cpu.as_secs_f64() / interval.as_secs_f64().max(1e-9),
        );
        // getrusage's context-switch counters are not maintained on macOS;
        // its kernel wakeup and instruction counters are the useful signal.
        let _ = if cfg!(target_os = "macos") {
            write!(
                report,
                ",idle_wakeups:{},interrupt_wakeups:{},instructions:{}}}",
                after.idle_wakeups.saturating_sub(before.idle_wakeups),
                after
                    .interrupt_wakeups
                    .saturating_sub(before.interrupt_wakeups),
                after.instructions.saturating_sub(before.instructions),
            )
        } else {
            write!(
                report,
                ",vcsw:{},ivcsw:{}}}",
                after
                    .voluntary_switches
                    .saturating_sub(before.voluntary_switches),
                after
                    .involuntary_switches
                    .saturating_sub(before.involuntary_switches),
            )
        };
    }
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

    #[test]
    fn metric_names_follow_the_enum_order() {
        // Reports index NAMES by discriminant; a missed insertion would
        // silently mislabel every later counter.
        for (metric, name) in [
            (Metric::Drain, "drain"),
            (Metric::RedrawTotal, "redraw_total"),
            (Metric::LoopTurn, "loop_turn"),
            (Metric::FocusOut, "focus_out"),
            (Metric::RequestBlink, "request_blink"),
            (Metric::RequestChrome, "request_chrome"),
            (Metric::SupervisorPoll, "supervisor_poll"),
        ] {
            assert_eq!(NAMES[metric as usize], name);
        }
        assert_eq!(Metric::SupervisorPoll as usize, NAMES.len() - 1);
    }

    #[cfg(unix)]
    #[test]
    fn process_usage_is_monotonic() {
        let before = process_usage().expect("getrusage works on unix");
        let mut spin = 0_u64;
        for i in 0..2_000_000_u64 {
            spin = spin.wrapping_add(i).rotate_left(1);
        }
        std::hint::black_box(spin);
        let after = process_usage().expect("getrusage works on unix");
        assert!(after.cpu >= before.cpu);
        assert!(after.interrupt_wakeups >= before.interrupt_wakeups);
        assert!(after.voluntary_switches >= before.voluntary_switches);
        #[cfg(target_os = "macos")]
        assert!(after.instructions > before.instructions);
    }
}
