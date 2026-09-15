//! Global logger: fans records out to stderr (unchanged `env_logger`
//! behavior) and to the in-app ring buffer ([`crate::logbuf`]).
//!
//! Two independent filters. Stderr keeps `RUST_LOG` (default: `error`
//! only) so terminal runs stay exactly as quiet as before. The buffer
//! records by **directives**, not by a bare level (`[log] filter`, else
//! `cometty=<level>,warn`), so dependency noise (`naga::proc::overloads`,
//! `cosmic_text::font::system`, …) stays out while app diagnostics and
//! dependency *warnings* still reach the panel. `log::max_level` is the
//! more verbose of the two.
//!
//! Replaces `env_logger::init()` in `main`; the winit event-loop proxy is
//! stored so records can wake the loop while the panel is open.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use env_filter::Filter;
use log::{LevelFilter, Log, Metadata, Record};
use winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;
use crate::logbuf::LogBuffer;

/// Active buffer filter plus the directive string that produced it (the
/// panel shows it so "why don't I see X" is answerable). `None` until
/// [`install`] runs.
static RECORD_FILTER: RwLock<Option<(Filter, String)>> = RwLock::new(None);
/// Why the last [`set_record_filter`] call was rejected (the panel shows
/// this instead of spamming a warning per keystroke while typing).
static FILTER_ERROR: RwLock<Option<String>> = RwLock::new(None);
/// Stderr level, mirrored so [`set_record_filter`] can recompute
/// `log::max_level` without holding the logger.
static STDERR_LEVEL: AtomicUsize = AtomicUsize::new(LevelFilter::Off as usize);
/// Wake the event loop for new records only while the panel is open:
/// a `RUST_LOG=trace` session must not pay a cross-thread wake per line.
static WAKE_ENABLED: AtomicBool = AtomicBool::new(false);

/// Install the global logger, with `directives` as the initial buffer
/// filter (see [`crate::config::LogConfig::filter_string`]). Failure is
/// non-fatal for the app: cometty keeps running, and stderr logging still
/// works even when the initial directives are rejected.
pub fn install(
    buffer: Arc<Mutex<LogBuffer>>,
    proxy: Option<EventLoopProxy<UserEvent>>,
    directives: &str,
) -> anyhow::Result<()> {
    let stderr = env_logger::Builder::from_env(env_logger::Env::default()).build();
    STDERR_LEVEL.store(stderr.filter() as usize, Ordering::Relaxed);
    let rejected = match parse_filter(directives) {
        Ok(filter) => {
            *RECORD_FILTER
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                Some((filter, directives.to_string()));
            None
        }
        // Install anyway: a typo must not take stderr logging (or the
        // app) down with it. The caller reports the error.
        Err(e) => Some(format!(
            "invalid log filter {directives:?} ({e}); panel recording disabled"
        )),
    };
    *FILTER_ERROR
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = rejected.clone();
    log::set_max_level(effective_max());
    log::set_boxed_logger(Box::new(AppLogger {
        stderr,
        buffer,
        proxy,
    }))?;
    match rejected {
        Some(message) => Err(anyhow::anyhow!(message)),
        None => Ok(()),
    }
}

/// Replace the buffer filter from `RUST_LOG`-style directives. Invalid
/// input keeps the previous filter and is reported through
/// [`record_filter_error`] (and to the caller, which may log it once); the
/// panel shows it next to the active filter, so typing an incomplete
/// directive doesn't produce a warning per keystroke.
pub fn set_record_filter(directives: &str) -> Result<(), String> {
    let mut builder = env_filter::Builder::new();
    let filter = match builder.try_parse(directives) {
        Ok(builder) => builder.build(),
        Err(e) => {
            let message = format!("invalid log filter {directives:?} ({e}); keeping the previous");
            *FILTER_ERROR
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(message.clone());
            return Err(message);
        }
    };
    *RECORD_FILTER
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((filter, directives.to_string()));
    *FILTER_ERROR
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    log::set_max_level(effective_max());
    Ok(())
}

/// Directive string currently in effect, for the panel header.
pub fn record_filter_string() -> Option<String> {
    RECORD_FILTER
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .map(|(_, directives)| directives.clone())
}

/// Why the last filter update was rejected, if it was.
pub fn record_filter_error() -> Option<String> {
    FILTER_ERROR
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Let new records wake the event loop (panel open) or not (panel closed).
pub fn set_wake_enabled(enabled: bool) {
    WAKE_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Test seam for the panel's wake gating.
#[cfg(test)]
pub fn wake_enabled() -> bool {
    WAKE_ENABLED.load(Ordering::Relaxed)
}

fn parse_filter(directives: &str) -> anyhow::Result<Filter> {
    let mut builder = env_filter::Builder::new();
    let filter = builder
        .try_parse(directives)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .build();
    Ok(filter)
}

fn record_enabled(metadata: &Metadata<'_>) -> bool {
    RECORD_FILTER
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .is_some_and(|(filter, _)| filter.enabled(metadata))
}

struct AppLogger {
    stderr: env_logger::Logger,
    buffer: Arc<Mutex<LogBuffer>>,
    proxy: Option<EventLoopProxy<UserEvent>>,
}

impl Log for AppLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        record_enabled(metadata) || self.stderr.enabled(metadata)
    }

    fn log(&self, record: &Record<'_>) {
        if record_enabled(record.metadata()) {
            {
                // A panicking thread must not silence logging (poisoning).
                let mut buffer = self
                    .buffer
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                buffer.push(record.level(), record.target(), record.args().to_string());
            }
            if WAKE_ENABLED.load(Ordering::Relaxed)
                && let Some(proxy) = &self.proxy
            {
                let _ = proxy.send_event(UserEvent::LogAvailable);
            }
        }
        if self.stderr.enabled(record.metadata()) {
            self.stderr.log(record);
        }
    }

    fn flush(&self) {
        self.stderr.flush();
    }
}

/// More verbose of the two sinks: the `log` crate drops records above this
/// before they ever reach [`AppLogger`].
fn effective_max() -> LevelFilter {
    let stderr = level_from_usize(STDERR_LEVEL.load(Ordering::Relaxed));
    let record = RECORD_FILTER
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .map_or(LevelFilter::Off, |(filter, _)| filter.filter());
    if (stderr as usize) >= (record as usize) {
        stderr
    } else {
        record
    }
}

fn level_from_usize(value: usize) -> LevelFilter {
    LevelFilter::iter()
        .find(|filter| *filter as usize == value)
        .unwrap_or(LevelFilter::Off)
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Level;

    fn metadata(level: Level, target: &'static str) -> Metadata<'static> {
        Metadata::builder().level(level).target(target).build()
    }

    #[test]
    fn directives_scope_recording_by_target() {
        let filter = parse_filter("cometty=debug,warn").expect("default directives parse");
        // Our crate at debug…
        assert!(filter.enabled(&metadata(Level::Debug, "cometty::app::pty_io")));
        assert!(!filter.enabled(&metadata(Level::Trace, "cometty::renderer")));
        // …dependency warnings and errors still get through…
        assert!(filter.enabled(&metadata(Level::Warn, "cosmic_text::font::system")));
        assert!(filter.enabled(&metadata(Level::Error, "wgpu_core::device::resource")));
        // …but their debug/trace spam does not.
        assert!(!filter.enabled(&metadata(Level::Debug, "naga::proc::overloads::list")));
        assert!(!filter.enabled(&metadata(Level::Trace, "cosmic_text::font::system")));

        // Explicit dependency directives widen only what was asked for.
        let filter = parse_filter("cometty=info,wgpu=debug,warn").expect("parses");
        assert!(!filter.enabled(&metadata(Level::Debug, "cometty::app")));
        assert!(filter.enabled(&metadata(Level::Debug, "wgpu_core::device")));
        assert!(!filter.enabled(&metadata(Level::Debug, "naga::proc")));
        assert!(filter.enabled(&metadata(Level::Warn, "naga::proc")));
    }

    #[test]
    fn invalid_directives_are_rejected() {
        let mut builder = env_filter::Builder::new();
        assert!(builder.try_parse("cometty=notalevel").is_err());
        assert!(builder.try_parse("").is_ok(), "empty means: record nothing");
    }

    #[test]
    fn record_filter_gates_the_buffer() {
        // Only test in this binary that swaps the filter: keeps the
        // assertions deterministic under parallel test threads.
        let saved = record_filter_string();
        assert!(
            set_record_filter("cometty=debug,warn").is_ok(),
            "default directives are valid"
        );
        assert_eq!(
            record_filter_string().as_deref(),
            Some("cometty=debug,warn"),
            "active directives are readable for the panel"
        );
        assert!(record_filter_error().is_none());

        let stderr = env_logger::Builder::new()
            .filter_level(LevelFilter::Off)
            .build();
        let buffer = Arc::new(Mutex::new(LogBuffer::new(8)));
        let logger = AppLogger {
            stderr,
            buffer: buffer.clone(),
            proxy: None,
        };
        let emit = |level, target: &'static str, message: &str| {
            logger.log(
                &Record::builder()
                    .level(level)
                    .target(target)
                    .args(format_args!("{message}"))
                    .build(),
            );
        };

        emit(Level::Debug, "cometty::app", "kept");
        emit(Level::Debug, "naga::proc::overloads::list", "dropped");
        emit(Level::Trace, "cometty::renderer", "dropped");
        emit(Level::Warn, "cosmic_text::font::system", "kept");
        let kept = buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .filtered(LevelFilter::Trace, "");
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].message, "kept");
        assert_eq!(kept[1].target, "cosmic_text::font::system");

        // Invalid directives are rejected, keep the previous filter in
        // place, and are reported for the panel (not logged per keystroke).
        let rejected = set_record_filter("cometty=notalevel");
        assert!(rejected.is_err());
        assert_eq!(
            record_filter_string().as_deref(),
            Some("cometty=debug,warn")
        );
        assert!(record_filter_error().is_some_and(|e| e.contains("cometty=notalevel")));

        // A later valid value clears the error again.
        assert!(set_record_filter("cometty=info,warn").is_ok());
        assert!(record_filter_error().is_none());

        match saved {
            Some(saved) => {
                let _ = set_record_filter(&saved);
            }
            None => {
                *RECORD_FILTER
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
            }
        }
    }
}
