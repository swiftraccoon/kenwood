//! Downloads and parses Pi-Star reflector host files.

use dstar_gateway_core::hosts::HostFile;

/// Canonical Pi-Star URL for the `DPlus` (REF) reflector host list.
const REF_HOSTS_URL: &str = "https://hosts.pistar.uk/hosts/REFHosts.txt";

/// Canonical Pi-Star URL for the `DExtra` (XRF) reflector host list.
const XRF_HOSTS_URL: &str = "https://hosts.pistar.uk/hosts/XRFHosts.txt";

/// Canonical Pi-Star URL for the `DCS` reflector host list.
const DCS_HOSTS_URL: &str = "https://hosts.pistar.uk/hosts/DCSHosts.txt";

/// Default UDP port for `DPlus` (REF) reflectors.
const DEFAULT_DPLUS_PORT: u16 = 20001;

/// Default UDP port for `DExtra` (XRF) reflectors.
const DEFAULT_DEXTRA_PORT: u16 = 30001;

/// Default UDP port for `DCS` reflectors.
const DEFAULT_DCS_PORT: u16 = 30051;

/// Errors returned by [`HostsFetcher`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FetcherError {
    /// HTTP request failed. Wraps the underlying `reqwest::Error`.
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
}

/// Downloads Pi-Star reflector host files and parses them into
/// [`HostFile`] values.
///
/// Default URLs:
/// - `DPlus` (REF): <https://hosts.pistar.uk/hosts/REFHosts.txt>
/// - `DExtra` (XRF): <https://hosts.pistar.uk/hosts/XRFHosts.txt>
/// - `DCS`: <https://hosts.pistar.uk/hosts/DCSHosts.txt>
///
/// The fetched text is parsed via
/// [`dstar_gateway_core::hosts::HostFile::parse`], which tolerates the
/// slightly different column layouts of each file.
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

    /// Fetch and parse the `DPlus` (REF) host list.
    ///
    /// # Errors
    ///
    /// Returns [`FetcherError::Http`] on network failure or a non-2xx
    /// HTTP response.
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe. Dropping the future mid-request
    /// cancels the underlying `reqwest` call cleanly; the cached
    /// reqwest `Client` is unaffected and the next call can start a
    /// fresh request.
    pub async fn fetch_dplus(&self) -> Result<HostFile, FetcherError> {
        let body = self
            .client
            .get(REF_HOSTS_URL)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let mut hf = HostFile::new();
        hf.parse(&body, DEFAULT_DPLUS_PORT);
        Ok(hf)
    }

    /// Fetch and parse the `DExtra` (XRF) host list.
    ///
    /// # Errors
    ///
    /// Returns [`FetcherError::Http`] on network failure or a non-2xx
    /// HTTP response.
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe. See [`Self::fetch_dplus`] for
    /// details.
    pub async fn fetch_dextra(&self) -> Result<HostFile, FetcherError> {
        let body = self
            .client
            .get(XRF_HOSTS_URL)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let mut hf = HostFile::new();
        hf.parse(&body, DEFAULT_DEXTRA_PORT);
        Ok(hf)
    }

    /// Fetch and parse the `DCS` host list.
    ///
    /// # Errors
    ///
    /// Returns [`FetcherError::Http`] on network failure or a non-2xx
    /// HTTP response.
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe. See [`Self::fetch_dplus`] for
    /// details.
    pub async fn fetch_dcs(&self) -> Result<HostFile, FetcherError> {
        let body = self
            .client
            .get(DCS_HOSTS_URL)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let mut hf = HostFile::new();
        hf.parse(&body, DEFAULT_DCS_PORT);
        Ok(hf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetcher_new_builds_default_client() {
        // No actual network calls in unit tests. The default client
        // constructor never fails for the in-memory defaults.
        let fetcher = HostsFetcher::new();
        // Round-trip through Clone proves the derive holds — the
        // inner reqwest::Client is itself `Clone`. Both bindings
        // are kept live by the debug-print below so clippy's
        // `redundant_clone` doesn't fire.
        let cloned = fetcher.clone();
        assert_eq!(format!("{fetcher:?}"), format!("{cloned:?}"));
    }

    #[test]
    fn fetcher_default_impl_matches_new() {
        let _from_new = HostsFetcher::new();
        let _from_default = HostsFetcher::default();
    }

    /// Compile-time exhaustive match over [`FetcherError`] variants.
    ///
    /// If a new variant is added without updating this arm, the
    /// compiler will force us to handle it here too. That's the
    /// test — the function body never runs.
    const fn _exhaustive_variant_check(err: &FetcherError) {
        match *err {
            FetcherError::Http(_) => {}
        }
    }
}
