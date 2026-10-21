//! Bounded retrieval of the unauthenticated HTTP XLX directory.

use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use dstar_gateway_core::hosts::{HostEntry, HostFileError, parse_xlx_directory};
use dstar_gateway_core::types::ProtocolKind;

const DIRECTORY_URL: &str = "http://xlxapi.rlx.lu/api.php?do=GetReflectorHostname";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_LIMIT: NonZeroUsize = NonZeroUsize::new(2 * 1024 * 1024).unwrap();

/// HTTP, resource, text or host-row failure during directory retrieval.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InsecurePlaintextXlxDirectoryFetchError {
    /// Request construction, socket transfer or HTTP status failed.
    #[error("insecure plaintext HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// The server returned a non-success HTTP status, including a redirect.
    #[error("directory server returned HTTP {0}")]
    Status(reqwest::StatusCode),
    /// The total request/body deadline expired.
    #[error("directory retrieval exceeded {0:?}")]
    Timeout(Duration),
    /// The configured timeout exceeds the platform Instant range.
    #[error("directory timeout {0:?} cannot be represented")]
    DeadlineOverflow(Duration),
    /// The body exceeded its configured retained-byte limit.
    #[error("directory body exceeds {0} bytes")]
    TooLarge(NonZeroUsize),
    /// The response body was not UTF-8.
    #[error("directory body is not UTF-8: {0}")]
    Text(#[from] std::string::FromUtf8Error),
    /// A recognized protocol row contained invalid host data.
    #[error(transparent)]
    Host(#[from] HostFileError),
}

/// Opt-in client for untrusted directory data over plaintext HTTP.
///
/// The selected URL provides no transport confidentiality or authenticated
/// content. A response is bounded and structurally validated before returning
/// entries; the application decides whether an address is trusted.
///
/// ```no_run
/// use std::num::NonZeroUsize;
/// use std::time::Duration;
/// use dstar_gateway::insecure_plaintext_xlx_directory::InsecurePlaintextXlxDirectoryFetcher;
/// # async fn fetch() -> Result<(), Box<dyn std::error::Error>> {
/// let fetcher = InsecurePlaintextXlxDirectoryFetcher::new()?
///     .with_limits(Duration::from_secs(5), NonZeroUsize::new(65_536).ok_or("zero limit")?);
/// let entries = fetcher.fetch_over_plaintext_http().await?;
/// for (protocol, entry) in entries {
///     println!("{protocol:?}: {}", entry.name());
/// }
/// # Ok(()) }
/// ```
#[derive(Debug, Clone)]
pub struct InsecurePlaintextXlxDirectoryFetcher {
    client: reqwest::Client,
    timeout: Duration,
    limit: NonZeroUsize,
}

impl InsecurePlaintextXlxDirectoryFetcher {
    /// Build a client with ten-second and two-MiB local limits.
    ///
    /// Redirects are disabled; an unexpected redirect is an HTTP status error.
    ///
    /// # Errors
    ///
    /// Returns the underlying HTTP-client initialization error.
    pub fn new() -> Result<Self, InsecurePlaintextXlxDirectoryFetchError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self::with_client(client))
    }

    /// Use a caller-configured HTTP client with the default local limits.
    ///
    /// The caller controls proxy/redirect/TLS behavior. The total deadline and
    /// retained body limit still apply independently of that client.
    #[must_use]
    pub const fn with_client(client: reqwest::Client) -> Self {
        Self {
            client,
            timeout: DEFAULT_TIMEOUT,
            limit: DEFAULT_LIMIT,
        }
    }

    /// Set the total request/body deadline and maximum retained bytes.
    ///
    /// Zero time expires immediately. Unrepresentable spans return
    /// [`InsecurePlaintextXlxDirectoryFetchError::DeadlineOverflow`] before I/O.
    #[must_use]
    pub const fn with_limits(mut self, timeout: Duration, limit: NonZeroUsize) -> Self {
        self.timeout = timeout;
        self.limit = limit;
        self
    }

    /// Retrieve and parse recognized REF/XRF/DCS rows over plaintext HTTP.
    ///
    /// Each returned pair describes one row's protocol and checked host entry.
    ///
    /// # Errors
    ///
    /// Returns typed deadline, byte-limit, HTTP/status, UTF-8 or host-row errors.
    /// A malformed recognized row rejects the complete response.
    ///
    /// # Cancellation
    ///
    /// Dropping the future abandons its response. Previously sent request
    /// bytes may have reached the server; the reusable client remains usable.
    pub async fn fetch_over_plaintext_http(
        &self,
    ) -> Result<Vec<(ProtocolKind, HostEntry)>, InsecurePlaintextXlxDirectoryFetchError> {
        self.fetch_url(DIRECTORY_URL).await
    }

    async fn fetch_url(
        &self,
        url: &str,
    ) -> Result<Vec<(ProtocolKind, HostEntry)>, InsecurePlaintextXlxDirectoryFetchError> {
        if self.timeout.is_zero() {
            return Err(InsecurePlaintextXlxDirectoryFetchError::Timeout(
                self.timeout,
            ));
        }
        let cutoff = Instant::now().checked_add(self.timeout).ok_or(
            InsecurePlaintextXlxDirectoryFetchError::DeadlineOverflow(self.timeout),
        )?;
        tokio::time::timeout_at(tokio::time::Instant::from_std(cutoff), async {
            let mut response = self.client.get(url).send().await?;
            if !response.status().is_success() {
                return Err(InsecurePlaintextXlxDirectoryFetchError::Status(
                    response.status(),
                ));
            }
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                if body.len().saturating_add(chunk.len()) > self.limit.get() {
                    return Err(InsecurePlaintextXlxDirectoryFetchError::TooLarge(
                        self.limit,
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            let text = String::from_utf8(body)?;
            parse_xlx_directory(&text).map_err(Into::into)
        })
        .await
        .map_err(|_| InsecurePlaintextXlxDirectoryFetchError::Timeout(self.timeout))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    async fn serve(
        body: Vec<u8>,
    ) -> Result<(String, tokio::task::JoinHandle<std::io::Result<()>>), std::io::Error> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/", listener.local_addr()?);
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            let mut request = [0_u8; 4096];
            let _bytes = socket.read(&mut request).await?;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(header.as_bytes()).await?;
            socket.write_all(&body).await?;
            Ok(())
        });
        Ok((url, task))
    }

    #[tokio::test]
    async fn limits_and_invalid_rows_reject_the_whole_response() -> TestResult {
        let fetcher = InsecurePlaintextXlxDirectoryFetcher::with_client(
            reqwest::Client::builder().no_proxy().build()?,
        )
        .with_limits(
            Duration::from_secs(2),
            NonZeroUsize::new(64).ok_or("zero limit")?,
        );
        let (url, task) = serve(vec![b'x'; 65]).await?;
        let result = fetcher.fetch_url(&url).await;
        assert!(
            matches!(
                result,
                Err(InsecurePlaintextXlxDirectoryFetchError::TooLarge(_))
            ),
            "{result:?}"
        );
        task.await??;
        let (url, task) = serve(b"REF030 localhost 20001\nXRF030 localhost 0\n".to_vec()).await?;
        let result = fetcher.fetch_url(&url).await;
        assert!(
            matches!(
                result,
                Err(InsecurePlaintextXlxDirectoryFetchError::Host(_))
            ),
            "{result:?}"
        );
        task.await??;
        let (url, task) = serve(b"REF030 localhost 20001\n".to_vec()).await?;
        let rows = fetcher.fetch_url(&url).await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows.first().map(|(protocol, _)| *protocol),
            Some(ProtocolKind::DPlus)
        );
        task.await??;
        Ok(())
    }
}
