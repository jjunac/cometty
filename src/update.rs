//! Release checking and, for macOS `.app` launches, background download,
//! verification, staging, and a confirmed in-place install.
//!
//! Pure decision helpers live in the submodules below; the I/O submodules
//! (`http`, `worker`, `fsops`, `procs`) run only on detached update worker
//! threads, apart from the relaunch `exiting()` starts. The trust model,
//! state machine, staging layout, and HTTP limits are documented in
//! [`docs/update.md`](../docs/update.md).

pub(crate) mod bundle;
pub(crate) mod checksum;
pub(crate) mod fsops;
pub(crate) mod http;
pub(crate) mod limits;
pub(crate) mod mode;
pub(crate) mod procs;
pub(crate) mod release;
pub(crate) mod schedule;
pub(crate) mod staging;
pub(crate) mod state;
pub(crate) mod version;
pub(crate) mod worker;

/// Log target for every updater record; `cometty::update=trace` isolates them.
pub(crate) const TARGET: &str = "cometty::update";

/// The Cometty version compiled into this binary, in `X.Y.Z` form.
pub(crate) const RUNNING_VERSION: &str = env!("CARGO_PKG_VERSION");
