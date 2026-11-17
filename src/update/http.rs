//! HTTPS fetching through the pinned client and transport error classification.
//!
//! The update jobs talk to the network only through [`Fetch`], so tests drive
//! the pipelines with a scripted fake and never open a socket. [`UreqFetcher`]
//! is the real implementation: it translates the [`limits`] table into one
//! `ureq::Agent` per request kind, built on first use and dropped with the
//! fetcher when the job ends, so no pooled connection outlives a job. See
//! `docs/update.md#http-limits`.
use std::cell::OnceCell;
use std::io::{self, Read};

use super::RUNNING_VERSION;
use super::limits::{RequestKind, limits};
use super::state::Failure;

/// Redirects followed per request before the request fails.
const MAX_REDIRECTS: u32 = 5;

/// A blocking HTTPS GET, abstracted so update pipelines can run on fixtures.
pub(crate) trait Fetch {
    /// Sends one GET for `url` with the headers and limits of `kind`.
    ///
    /// Any received status is `Ok`, including 404 and 5xx; callers classify
    /// it. Transport failures (TLS, DNS, timeouts, redirects) are `Err`.
    fn get(&self, url: &str, kind: RequestKind) -> Result<FetchResponse, Failure>;
}

/// A received response: its status and an unbounded body stream.
///
/// The body has no size cap of its own; read it through
/// [`copy_capped`](super::limits::copy_capped). Read errors can be classified
/// with [`classify_io_error`].
pub(crate) struct FetchResponse {
    pub status: u16,
    pub body: Box<dyn Read + Send>,
}

/// The `User-Agent` value sent with every request.
pub(crate) fn user_agent() -> String {
    format!("cometty/{RUNNING_VERSION}")
}

/// The `Accept` value for `kind`.
pub(crate) const fn accept(kind: RequestKind) -> &'static str {
    match kind {
        RequestKind::Feed => "application/vnd.github+json",
        RequestKind::Checksum | RequestKind::Archive => "application/octet-stream",
    }
}

/// Builds the agent for `kind` from the limits table.
///
/// With `default-features = false` ureq adds no `Accept-Encoding` header, so
/// the only identifying headers are `User-Agent` and `Accept`.
/// `https_only` is enforced on every redirect hop.
fn build_agent(kind: RequestKind) -> ureq::Agent {
    let limits = limits(kind);
    ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(MAX_REDIRECTS)
        .max_redirects_will_error(true)
        .http_status_as_error(false)
        .user_agent(user_agent())
        .accept(accept(kind))
        // `timeout_connect` excludes name resolution; bound both by the
        // connection budget.
        .timeout_resolve(Some(limits.connect))
        .timeout_connect(Some(limits.connect))
        .timeout_global(Some(limits.global))
        .timeout_per_read(limits.per_read)
        .build()
        .into()
}

/// The real [`Fetch`]. Create one per job and drop it when the job ends.
///
/// Not `Sync`: a fetcher belongs to the single worker thread running its job.
#[derive(Default)]
pub(crate) struct UreqFetcher {
    /// Lazily built agents indexed by [`kind_index`].
    agents: [OnceCell<ureq::Agent>; 3],
}

const fn kind_index(kind: RequestKind) -> usize {
    match kind {
        RequestKind::Feed => 0,
        RequestKind::Checksum => 1,
        RequestKind::Archive => 2,
    }
}

impl UreqFetcher {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn agent(&self, kind: RequestKind) -> &ureq::Agent {
        self.agents[kind_index(kind)].get_or_init(|| build_agent(kind))
    }
}

impl Fetch for UreqFetcher {
    fn get(&self, url: &str, kind: RequestKind) -> Result<FetchResponse, Failure> {
        let response = self
            .agent(kind)
            .get(url)
            .call()
            .map_err(|e| classify_error(&e))?;
        let status = response.status().as_u16();
        let body = response.into_body().into_reader();
        Ok(FetchResponse {
            status,
            body: Box::new(body),
        })
    }
}

/// Maps a ureq error to the transport [`Failure`] it represents.
///
/// `RequireHttpsOnly` (an `http` URL or redirect) is `InsecureRedirect`; the
/// redirect limit is `Redirects`; any timeout is `Timeout`; DNS, TLS, proxy,
/// connection, and socket I/O errors are `Network`; anything else is `Http`.
pub(crate) fn classify_error(error: &ureq::Error) -> Failure {
    use ureq::Error as E;
    match error {
        E::RequireHttpsOnly(_) => Failure::InsecureRedirect,
        E::TooManyRedirects => Failure::Redirects,
        E::Timeout(_) => Failure::Timeout,
        E::Io(e) if e.kind() == io::ErrorKind::TimedOut => Failure::Timeout,
        E::Io(_)
        | E::HostNotFound
        | E::ConnectionFailed
        | E::ConnectProxyFailed(_)
        | E::Tls(_)
        | E::Pem(_)
        | E::Rustls(_)
        | E::TlsRequired => Failure::Network(error.to_string()),
        _ => Failure::Http(error.to_string()),
    }
}

/// Classifies an error raised while reading a [`FetchResponse`] body.
///
/// ureq wraps body-time errors (such as the per-read timeout) in an
/// `io::Error`; converting back recovers the original variant. Plain I/O
/// errors classify as `Network`.
pub(crate) fn classify_io_error(error: io::Error) -> Failure {
    classify_error(&ureq::Error::from(error))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ureq::config::AutoHeaderValue;
    use ureq::{Error, Timeout};

    use super::*;

    const KINDS: [RequestKind; 3] = [
        RequestKind::Feed,
        RequestKind::Checksum,
        RequestKind::Archive,
    ];

    fn provided(value: &AutoHeaderValue) -> Option<&str> {
        match value {
            AutoHeaderValue::Provided(v) => Some(v.as_str()),
            _ => None,
        }
    }

    #[test]
    fn https_only_violation_is_insecure_redirect() {
        let e = Error::RequireHttpsOnly("http://example.invalid/".into());
        assert_eq!(classify_error(&e), Failure::InsecureRedirect);
    }

    #[test]
    fn too_many_redirects_is_redirects() {
        assert_eq!(classify_error(&Error::TooManyRedirects), Failure::Redirects);
    }

    #[test]
    fn timeouts_are_timeout() {
        for t in [
            Timeout::Global,
            Timeout::Connect,
            Timeout::Resolve,
            Timeout::PerRead,
            Timeout::RecvBody,
        ] {
            assert_eq!(
                classify_error(&Error::Timeout(t)),
                Failure::Timeout,
                "{t:?}"
            );
        }
        let io = io::Error::new(io::ErrorKind::TimedOut, "slow");
        assert_eq!(classify_error(&Error::Io(io)), Failure::Timeout);
    }

    #[test]
    fn io_dns_tls_and_connect_errors_are_network() {
        for e in [
            Error::Io(io::Error::other("reset")),
            Error::HostNotFound,
            Error::ConnectionFailed,
            Error::Tls("bad certificate"),
            Error::TlsRequired,
            Error::ConnectProxyFailed("refused".into()),
        ] {
            let failure = classify_error(&e);
            assert_eq!(failure, Failure::Network(e.to_string()), "{e:?}");
        }
    }

    #[test]
    fn other_errors_are_http() {
        let e = Error::BadUri("::".into());
        assert_eq!(classify_error(&e), Failure::Http(e.to_string()));
        assert_eq!(
            classify_error(&Error::RedirectFailed),
            Failure::Http(Error::RedirectFailed.to_string())
        );
    }

    #[test]
    fn body_io_errors_recover_the_wrapped_variant() {
        // ureq reports body-time failures as io::Error wrapping its own error.
        let wrapped = Error::Timeout(Timeout::PerRead).into_io();
        assert_eq!(classify_io_error(wrapped), Failure::Timeout);
        let plain = io::Error::other("truncated");
        assert!(matches!(classify_io_error(plain), Failure::Network(_)));
    }

    #[test]
    fn user_agent_is_cometty_slash_running_version() {
        let ua = user_agent();
        let version = ua.strip_prefix("cometty/").expect("cometty/ prefix");
        assert_eq!(version, RUNNING_VERSION);
        let parts: Vec<&str> = version.split('.').collect();
        assert_eq!(parts.len(), 3, "{version}");
        assert!(
            parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
            "{version}"
        );
    }

    #[test]
    fn accept_header_per_kind() {
        assert_eq!(accept(RequestKind::Feed), "application/vnd.github+json");
        assert_eq!(accept(RequestKind::Checksum), "application/octet-stream");
        assert_eq!(accept(RequestKind::Archive), "application/octet-stream");
    }

    #[test]
    fn agent_config_follows_limits_table() {
        // Building an agent opens no connection.
        for kind in KINDS {
            let agent = build_agent(kind);
            let config = agent.config();
            let l = limits(kind);
            assert!(config.https_only(), "{kind:?}");
            assert_eq!(config.max_redirects(), 5, "{kind:?}");
            assert!(config.max_redirects_will_error(), "{kind:?}");
            assert!(!config.http_status_as_error(), "{kind:?}");
            assert_eq!(provided(config.user_agent()), Some(user_agent().as_str()));
            assert_eq!(provided(config.accept()), Some(accept(kind)));
            assert!(
                !matches!(config.accept_encoding(), AutoHeaderValue::Provided(_)),
                "{kind:?}"
            );
            let timeouts = config.timeouts();
            assert_eq!(timeouts.connect, Some(l.connect), "{kind:?}");
            assert_eq!(timeouts.resolve, Some(l.connect), "{kind:?}");
            assert_eq!(timeouts.global, Some(l.global), "{kind:?}");
            assert_eq!(config.timeout_per_read(), l.per_read, "{kind:?}");
        }
        assert_eq!(
            build_agent(RequestKind::Archive)
                .config()
                .timeout_per_read(),
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn fetcher_builds_agents_lazily_and_reuses_them() {
        let fetcher = UreqFetcher::new();
        assert!(fetcher.agents.iter().all(|a| a.get().is_none()));
        let first: *const ureq::Agent = fetcher.agent(RequestKind::Feed);
        let again: *const ureq::Agent = fetcher.agent(RequestKind::Feed);
        assert_eq!(first, again);
        assert!(
            fetcher.agents[kind_index(RequestKind::Archive)]
                .get()
                .is_none()
        );
    }
}
