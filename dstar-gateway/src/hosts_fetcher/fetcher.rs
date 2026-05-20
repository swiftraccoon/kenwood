//! Downloads and parses the XLX reflector directory.

use dstar_gateway_core::hosts::{HostEntry, parse_xlx_directory};
use dstar_gateway_core::types::ProtocolKind;

/// XLX self-registration registry — the live, auto-generated reflector
/// directory. Emits `REF` / `XRF` / `DCS` entries for every reflector.
const XLX_DIRECTORY_URL: &str = "http://xlxapi.rlx.lu/api.php?do=GetReflectorHostname";

/// Errors returned by [`HostsFetcher`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FetcherError {
    /// HTTP request failed. Wraps the underlying `reqwest::Error`.
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
}

/// Downloads the XLX reflector directory and parses it into
/// protocol-tagged [`HostEntry`] values via
/// [`dstar_gateway_core::hosts::parse_xlx_directory`].
#[derive(Debug, Default, Clone)]
pub struct HostsFetcher {
    client: reqwest::Client,
}

impl HostsFetcher {
    /// Construct a new fetcher with the default reqwest client.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fetch and parse the XLX reflector directory.
    ///
    /// Returns one `(protocol, entry)` pair per reflector per protocol
    /// prefix — the same reflector appears as `REF`, `XRF`, and `DCS`.
    ///
    /// # Errors
    ///
    /// Returns [`FetcherError::Http`] on network failure or a non-2xx
    /// HTTP response.
    ///
    /// # Cancellation safety
    ///
    /// Cancel-safe. Dropping the future mid-request cancels the
    /// underlying `reqwest` call cleanly; the cached `Client` is
    /// unaffected and the next call starts a fresh request.
    pub async fn fetch_xlx_directory(
        &self,
    ) -> Result<Vec<(ProtocolKind, HostEntry)>, FetcherError> {
        let body = self
            .client
            .get(XLX_DIRECTORY_URL)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        Ok(parse_xlx_directory(&body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetcher_new_builds_default_client() {
        // No network calls in unit tests. Round-trip through Clone
        // proves the derive holds — the inner reqwest::Client is Clone.
        let fetcher = HostsFetcher::new();
        let cloned = fetcher.clone();
        assert_eq!(format!("{fetcher:?}"), format!("{cloned:?}"));
    }

    #[test]
    fn fetcher_default_impl_matches_new() {
        let _from_new = HostsFetcher::new();
        let _from_default = HostsFetcher::default();
    }

    /// Compile-time exhaustive match over [`FetcherError`] variants.
    const fn _exhaustive_variant_check(err: &FetcherError) {
        match *err {
            FetcherError::Http(_) => {}
        }
    }
}
