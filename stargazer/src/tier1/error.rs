//! Fetch error types for Tier 1 data source fetchers.
//!
//! Each fetcher converts its domain-specific errors (HTTP transport, XML/HTML
//! parsing, database writes) into [`FetchError`] so that the orchestrator has a
//! uniform error type to log and discard. Individual fetch failures are never
//! fatal — they are logged as warnings and retried on the next poll interval.

/// Errors that can occur during a Tier 1 fetch-and-store cycle.
///
/// Each variant wraps the underlying library error. The orchestrator logs these
/// at `warn` level and continues polling; no variant is considered fatal.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FetchError {
    /// An HTTP request failed (connection refused, timeout, non-2xx status).
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// XML deserialization of the XLX API response failed.
    #[error("XML parse error: {0}")]
    Xml(#[from] quick_xml::DeError),

    /// HTML scraping of the ircDDB last-heard page failed.
    ///
    /// This is a string-typed error because the `scraper` crate does not expose
    /// a unified error type — parse failures surface as missing elements or
    /// unexpected structure, which we describe as human-readable messages.
    #[error("HTML scrape error: {0}")]
    Html(String),

    /// A database query or insert failed during the store phase.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}
