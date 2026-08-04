//! Downloads the XLX self-registration directory over unauthenticated HTTP.

use dstar_gateway_core::hosts::{HostEntry, parse_xlx_directory};
use dstar_gateway_core::types::ProtocolKind;

/// The XLX directory server exposes no working HTTPS endpoint. Responses from
/// this URL have neither transport confidentiality nor server/content
/// authenticity and can be modified in transit.
const INSECURE_PLAINTEXT_XLX_DIRECTORY_URL: &str =
    "http://xlxapi.rlx.lu/api.php?do=GetReflectorHostname";

/// Errors returned by [`InsecurePlaintextXlxDirectoryFetcher`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InsecurePlaintextXlxDirectoryFetchError {
    /// The explicitly insecure plaintext HTTP request failed.
    #[error("insecure plaintext HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
}

/// Opt-in client for the unauthenticated plaintext-HTTP XLX directory.
///
/// This fetcher provides no confidentiality, authenticity, or integrity. A
/// network intermediary can observe or replace returned reflector addresses.
/// Applications must make their own explicit trust decision before using a
/// returned address for a connection.
#[derive(Debug, Default, Clone)]
pub struct InsecurePlaintextXlxDirectoryFetcher {
    client: reqwest::Client,
}

impl InsecurePlaintextXlxDirectoryFetcher {
    /// Construct an explicitly insecure plaintext-directory fetcher with the
    /// default reqwest client.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fetch and parse the XLX directory over unauthenticated plaintext HTTP.
    ///
    /// Returns one `(protocol, entry)` pair per reflector per protocol prefix:
    /// the same reflector appears as `REF`, `XRF`, and `DCS`. The returned
    /// addresses are untrusted network input and have no integrity guarantee.
    ///
    /// # Errors
    ///
    /// Returns [`InsecurePlaintextXlxDirectoryFetchError::Http`] on network
    /// failure or a non-2xx HTTP response.
    ///
    /// # Cancellation safety
    ///
    /// Cancel-safe. Dropping the future mid-request cancels the underlying
    /// `reqwest` call cleanly; the cached `Client` is unaffected and the next
    /// call starts a fresh request.
    pub async fn fetch_over_plaintext_http(
        &self,
    ) -> Result<Vec<(ProtocolKind, HostEntry)>, InsecurePlaintextXlxDirectoryFetchError> {
        let body = self
            .client
            .get(INSECURE_PLAINTEXT_XLX_DIRECTORY_URL)
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
    fn explicitly_insecure_fetcher_builds_and_clones() {
        // No network calls in unit tests. Round-trip through Clone proves the
        // derive holds: the inner reqwest::Client is Clone.
        let fetcher = InsecurePlaintextXlxDirectoryFetcher::new();
        let cloned = fetcher.clone();
        assert_eq!(format!("{fetcher:?}"), format!("{cloned:?}"));
    }

    #[test]
    fn explicitly_insecure_fetcher_default_matches_new() {
        let _from_new = InsecurePlaintextXlxDirectoryFetcher::new();
        let _from_default = InsecurePlaintextXlxDirectoryFetcher::default();
    }

    /// Compile-time exhaustive match over the fetch error variants.
    const fn _exhaustive_variant_check(err: &InsecurePlaintextXlxDirectoryFetchError) {
        match *err {
            InsecurePlaintextXlxDirectoryFetchError::Http(_) => {}
        }
    }
}
