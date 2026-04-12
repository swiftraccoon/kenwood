//! `AuthClient` — `DPlus` TCP auth.
//!
//! Performs the mandatory TCP authentication step against
//! `auth.dstargateway.org:20001` and returns the [`HostList`] cached
//! by the auth server. The resulting host list is then handed to
//! [`dstar_gateway_core::session::client::Session`] via its
//! `authenticate` method to promote the sans-io core's typestate to
//! [`dstar_gateway_core::session::client::Authenticated`].
//!
//! The on-wire packet format matches
//! `ircDDBGateway/Common/DPlusAuthenticator.cpp:111-143`.

use std::net::SocketAddr;
use std::time::Duration;

use dstar_gateway_core::codec::dplus::{HostList, parse_auth_response};
use dstar_gateway_core::error::IoOperation;
use dstar_gateway_core::types::Callsign;
use dstar_gateway_core::validator::NullSink;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Default auth server hostname+port, matching `ircDDBGateway`.
pub const DEFAULT_AUTH_ENDPOINT: &str = "auth.dstargateway.org:20001";

/// Default connect timeout for the TCP auth connection.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Default per-read timeout while draining the TCP auth response.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Length of the 56-byte `DPlus` auth request packet.
const AUTH_PACKET_LEN: usize = 56;

/// `DPlus` TCP authentication client.
///
/// Performs the mandatory TCP auth step against `auth.dstargateway.org`
/// and returns the [`HostList`] cached by the auth server. Caller
/// then hands the host list to the sans-io session to transition the
/// typestate to [`dstar_gateway_core::session::client::Authenticated`].
#[derive(Debug, Default, Clone)]
pub struct AuthClient {
    /// Optional override of the auth endpoint. `None` falls back to
    /// [`DEFAULT_AUTH_ENDPOINT`], which is resolved by tokio at call
    /// time.
    endpoint: Option<SocketAddr>,
    /// Timeout for the initial TCP connect.
    connect_timeout: Duration,
    /// Per-read timeout for draining the auth response. The auth
    /// server closes the socket when done, so this only fires when
    /// the server has hung mid-stream.
    read_timeout: Duration,
}

impl AuthClient {
    /// Create a new auth client with defaults.
    ///
    /// The endpoint is `None` (resolve `auth.dstargateway.org:20001`
    /// via DNS at call time), the connect timeout is 10 s, and the
    /// per-read timeout is 5 s.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            endpoint: None,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: DEFAULT_READ_TIMEOUT,
        }
    }

    /// Override the TCP auth endpoint.
    ///
    /// Used by integration tests to point the client at a local fake
    /// auth server on an ephemeral port.
    #[must_use]
    pub const fn with_endpoint(mut self, endpoint: SocketAddr) -> Self {
        self.endpoint = Some(endpoint);
        self
    }

    /// Override the TCP connect timeout.
    #[must_use]
    pub const fn with_connect_timeout(mut self, dur: Duration) -> Self {
        self.connect_timeout = dur;
        self
    }

    /// Override the per-read timeout while draining the response.
    #[must_use]
    pub const fn with_read_timeout(mut self, dur: Duration) -> Self {
        self.read_timeout = dur;
        self
    }

    /// Current endpoint override, if any.
    #[must_use]
    pub const fn endpoint(&self) -> Option<SocketAddr> {
        self.endpoint
    }

    /// Perform the TCP auth against the configured endpoint.
    ///
    /// Builds the 56-byte auth packet per
    /// `ircDDBGateway/Common/DPlusAuthenticator.cpp:111-143`, sends it
    /// over a fresh TCP connection, and accumulates the framed
    /// response until the server closes the socket. The accumulated
    /// bytes are then parsed via
    /// [`dstar_gateway_core::codec::dplus::parse_auth_response`] with
    /// a [`NullSink`] for diagnostics.
    ///
    /// # Errors
    ///
    /// - [`AuthError::Timeout`] if any phase (connect, write, read)
    ///   exceeds the configured timeout
    /// - [`AuthError::Io`] if the underlying socket call fails
    /// - [`AuthError::Parse`] if the response body is malformed
    ///
    /// # Cancellation safety
    ///
    /// This method is **not** cancel-safe. Cancelling the future may
    /// leave a half-written request on the wire or an auth TCP session
    /// dangling from the upstream host list server's perspective. The
    /// method owns a transient [`tokio::net::TcpStream`] internally
    /// and relies on drop-on-cancel to close it, but the upstream
    /// server may briefly see the partial packet. Callers should
    /// either await the future to completion or apply an outer
    /// [`tokio::time::timeout`] that matches the configured
    /// `connect_timeout`.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use dstar_gateway::auth::AuthClient;
    /// use dstar_gateway_core::types::Callsign;
    ///
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = AuthClient::new();
    /// let hosts = client.authenticate(Callsign::try_from_str("W1AW")?).await?;
    /// println!("{} known REF hosts", hosts.len());
    /// # Ok(()) }
    /// ```
    ///
    /// # See also
    ///
    /// `ircDDBGateway/Common/DPlusAuthenticator.cpp:111-143` — the
    /// reference 56-byte packet layout this client mirrors.
    pub async fn authenticate(&self, callsign: Callsign) -> Result<HostList, AuthError> {
        let endpoint_display = self.endpoint.map_or_else(
            || DEFAULT_AUTH_ENDPOINT.to_string(),
            |addr| addr.to_string(),
        );
        tracing::info!(
            target: "dstar_gateway::auth",
            %callsign,
            endpoint = %endpoint_display,
            connect_timeout_ms = u64::try_from(self.connect_timeout.as_millis()).unwrap_or(u64::MAX),
            "DPlus TCP auth starting"
        );

        let mut stream = match self.connect().await {
            Ok(s) => {
                tracing::debug!(
                    target: "dstar_gateway::auth",
                    "DPlus TCP auth connected"
                );
                s
            }
            Err(e) => {
                tracing::warn!(
                    target: "dstar_gateway::auth",
                    error = %e,
                    %callsign,
                    endpoint = %endpoint_display,
                    "DPlus TCP auth connect failed"
                );
                return Err(e);
            }
        };
        let packet = build_auth_packet(callsign);

        if let Err(e) = timeout(self.connect_timeout, stream.write_all(&packet))
            .await
            .map_err(|_| AuthError::Timeout {
                elapsed: self.connect_timeout,
                phase: AuthPhase::Write,
            })
            .and_then(|res| {
                res.map_err(|source| AuthError::Io {
                    source,
                    operation: IoOperation::TcpAuthWrite,
                })
            })
        {
            tracing::warn!(
                target: "dstar_gateway::auth",
                error = %e,
                %callsign,
                "DPlus TCP auth write failed"
            );
            return Err(e);
        }

        let response = match self.read_response(&mut stream).await {
            Ok(r) => {
                tracing::debug!(
                    target: "dstar_gateway::auth",
                    response_len = r.len(),
                    "DPlus TCP auth response received"
                );
                r
            }
            Err(e) => {
                tracing::warn!(
                    target: "dstar_gateway::auth",
                    error = %e,
                    %callsign,
                    "DPlus TCP auth read failed"
                );
                return Err(e);
            }
        };

        let mut sink = NullSink;
        match parse_auth_response(&response, &mut sink) {
            Ok(hosts) => {
                tracing::info!(
                    target: "dstar_gateway::auth",
                    %callsign,
                    host_count = hosts.len(),
                    "DPlus TCP auth succeeded"
                );
                Ok(hosts)
            }
            Err(e) => {
                tracing::warn!(
                    target: "dstar_gateway::auth",
                    error = %e,
                    %callsign,
                    response_len = response.len(),
                    "DPlus TCP auth response parse failed"
                );
                Err(e.into())
            }
        }
    }

    /// Connect to the auth server, respecting [`Self::connect_timeout`].
    ///
    /// `auth.dstargateway.org` round-robins across multiple A records
    /// and at least one is typically filtered (connect times out) or
    /// RST-refused (no listener). Passing the hostname to a plain
    /// `TcpStream::connect` and wrapping it in an overall timeout
    /// means if the OS resolver hands back a dead address first,
    /// the macOS TCP stack's per-address retry budget (~75 s of SYN
    /// retransmit before giving up and moving to the next address)
    /// blows through our 10 s budget before any live address is
    /// reached — the auth flow then times out deterministically on
    /// every run that happens to draw a dead address first.
    ///
    /// The fix is to resolve the hostname ourselves with
    /// [`tokio::net::lookup_host`], then race each resolved address
    /// sequentially with a short per-address timeout
    /// ([`Self::per_address_timeout`]). The first address that
    /// completes the TCP handshake wins; dead addresses are abandoned
    /// quickly so the next one gets a real chance. This is a
    /// minimal happy-eyeballs-style fallback without the
    /// IPv4/IPv6 staggering of the full RFC 8305 algorithm.
    async fn connect(&self) -> Result<TcpStream, AuthError> {
        let hostport = self.endpoint.map_or_else(
            || DEFAULT_AUTH_ENDPOINT.to_string(),
            |addr| addr.to_string(),
        );

        // Resolve all A/AAAA records, then try each in order.
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host(&hostport)
            .await
            .map_err(|source| AuthError::Io {
                source,
                operation: IoOperation::TcpAuthConnect,
            })?
            .collect();

        if addrs.is_empty() {
            return Err(AuthError::Io {
                source: std::io::Error::new(
                    std::io::ErrorKind::AddrNotAvailable,
                    format!("no addresses resolved for {hostport}"),
                ),
                operation: IoOperation::TcpAuthConnect,
            });
        }

        let per_addr_timeout = Duration::from_secs(3);
        let mut last_err: Option<std::io::Error> = None;

        for (idx, addr) in addrs.iter().enumerate() {
            tracing::debug!(
                target: "dstar_gateway::auth",
                index = idx,
                addr = %addr,
                timeout_ms = u64::try_from(per_addr_timeout.as_millis()).unwrap_or(u64::MAX),
                "DPlus TCP auth trying address"
            );
            match timeout(per_addr_timeout, TcpStream::connect(addr)).await {
                Ok(Ok(stream)) => {
                    tracing::debug!(
                        target: "dstar_gateway::auth",
                        addr = %addr,
                        "DPlus TCP auth connected to address"
                    );
                    return Ok(stream);
                }
                Ok(Err(e)) => {
                    tracing::debug!(
                        target: "dstar_gateway::auth",
                        addr = %addr,
                        error = %e,
                        "DPlus TCP auth address refused"
                    );
                    last_err = Some(e);
                }
                Err(_) => {
                    tracing::debug!(
                        target: "dstar_gateway::auth",
                        addr = %addr,
                        timeout_ms = u64::try_from(per_addr_timeout.as_millis()).unwrap_or(u64::MAX),
                        "DPlus TCP auth address timed out, trying next"
                    );
                    last_err = Some(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("{addr} did not respond within {per_addr_timeout:?}"),
                    ));
                }
            }
        }

        // Every resolved address failed — fall through to the outer
        // Timeout variant if appropriate, otherwise an Io error with
        // the last underlying cause.
        Err(last_err.map_or(
            AuthError::Timeout {
                elapsed: self.connect_timeout,
                phase: AuthPhase::Connect,
            },
            |source| AuthError::Io {
                source,
                operation: IoOperation::TcpAuthConnect,
            },
        ))
    }

    /// Drain the TCP response body until EOF.
    ///
    /// Each `read` call is wrapped in [`Self::read_timeout`]. A
    /// timeout is treated as a fatal read error (no data arrived
    /// within the configured window), matching the behavior of the
    /// legacy `DPlusClient::authenticate` loop.
    async fn read_response(&self, stream: &mut TcpStream) -> Result<Vec<u8>, AuthError> {
        let mut response = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let read_fut = stream.read(&mut buf);
            let n_result =
                timeout(self.read_timeout, read_fut)
                    .await
                    .map_err(|_| AuthError::Timeout {
                        elapsed: self.read_timeout,
                        phase: AuthPhase::Read,
                    })?;
            let n = n_result.map_err(|source| AuthError::Io {
                source,
                operation: IoOperation::TcpAuthRead,
            })?;
            if n == 0 {
                break;
            }
            let slice = buf.get(..n).unwrap_or(&[]);
            response.extend_from_slice(slice);
        }
        Ok(response)
    }
}

/// Build the 56-byte `DPlus` auth request packet for the given callsign.
///
/// Layout per `ircDDBGateway/Common/DPlusAuthenticator.cpp:111-143`:
/// - `[0..4]` = magic `0x38 0xC0 0x01 0x00`
/// - `[4..12]` = callsign (8 bytes, space-padded)
/// - `[12..20]` = `"DV019999"`
/// - `[28..33]` = `"W7IB2"`
/// - `[40..47]` = `"DHS0257"`
/// - all other bytes stay `0x20` (space)
fn build_auth_packet(callsign: Callsign) -> [u8; AUTH_PACKET_LEN] {
    let mut pkt = [b' '; AUTH_PACKET_LEN];

    // Magic bytes.
    if let Some(slot) = pkt.get_mut(0) {
        *slot = 0x38;
    }
    if let Some(slot) = pkt.get_mut(1) {
        *slot = 0xC0;
    }
    if let Some(slot) = pkt.get_mut(2) {
        *slot = 0x01;
    }
    if let Some(slot) = pkt.get_mut(3) {
        *slot = 0x00;
    }

    // Callsign (8 bytes, space-padded by `Callsign` invariant).
    if let Some(dst) = pkt.get_mut(4..12) {
        dst.copy_from_slice(callsign.as_bytes());
    }

    // `"DV019999"` client version tag.
    if let Some(dst) = pkt.get_mut(12..20) {
        dst.copy_from_slice(b"DV019999");
    }

    // `"W7IB2"` reference author tag.
    if let Some(dst) = pkt.get_mut(28..33) {
        dst.copy_from_slice(b"W7IB2");
    }

    // `"DHS0257"` reference client id.
    if let Some(dst) = pkt.get_mut(40..47) {
        dst.copy_from_slice(b"DHS0257");
    }

    pkt
}

/// `DPlus` TCP auth errors.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// I/O failure during connect, write, or read. The `operation`
    /// field identifies which phase tripped.
    #[error("DPlus auth I/O error during {operation:?}: {source}")]
    Io {
        /// Underlying `std::io::Error`.
        source: std::io::Error,
        /// Which phase of the auth flow failed.
        operation: IoOperation,
    },

    /// Phase timed out — the configured [`AuthClient::with_connect_timeout`]
    /// or [`AuthClient::with_read_timeout`] elapsed before the
    /// operation completed.
    #[error("DPlus auth timed out after {elapsed:?} during {phase:?}")]
    Timeout {
        /// Duration that elapsed before the timeout fired.
        elapsed: Duration,
        /// Which phase of the auth flow timed out.
        phase: AuthPhase,
    },

    /// Response body failed to parse as a valid `DPlus` host list.
    #[error(transparent)]
    Parse(#[from] dstar_gateway_core::codec::dplus::DPlusError),
}

/// Phase discriminator for [`AuthError::Timeout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthPhase {
    /// TCP connect to the auth server.
    Connect,
    /// Writing the 56-byte auth request packet.
    Write,
    /// Reading the framed host list response.
    Read,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_client_default_endpoint_is_none() {
        let client = AuthClient::new();
        assert!(client.endpoint().is_none());
    }

    #[test]
    fn auth_client_with_endpoint_sets_endpoint() {
        use std::net::{IpAddr, Ipv4Addr};

        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 54321);
        let client = AuthClient::new().with_endpoint(addr);
        assert_eq!(client.endpoint(), Some(addr));
    }

    #[test]
    fn auth_phase_variants_distinct() {
        assert_ne!(AuthPhase::Connect, AuthPhase::Write);
        assert_ne!(AuthPhase::Write, AuthPhase::Read);
        assert_ne!(AuthPhase::Connect, AuthPhase::Read);
    }

    #[test]
    fn auth_error_timeout_display_contains_phase_and_elapsed() {
        let err = AuthError::Timeout {
            elapsed: Duration::from_secs(7),
            phase: AuthPhase::Connect,
        };
        let rendered = err.to_string();
        assert!(rendered.contains("Connect"), "display: {rendered}");
        assert!(rendered.contains("7s"), "display: {rendered}");
    }

    #[test]
    fn build_auth_packet_layout_matches_ircddbgateway() {
        let cs = Callsign::from_wire_bytes(*b"W1AW    ");
        let pkt = build_auth_packet(cs);

        // Magic.
        assert_eq!(pkt.first().copied(), Some(0x38));
        assert_eq!(pkt.get(1).copied(), Some(0xC0));
        assert_eq!(pkt.get(2).copied(), Some(0x01));
        assert_eq!(pkt.get(3).copied(), Some(0x00));

        // Callsign field.
        assert_eq!(pkt.get(4..12), Some(cs.as_bytes().as_slice()));

        // Version tag.
        assert_eq!(pkt.get(12..20), Some(b"DV019999".as_slice()));

        // Author tag.
        assert_eq!(pkt.get(28..33), Some(b"W7IB2".as_slice()));

        // Client id.
        assert_eq!(pkt.get(40..47), Some(b"DHS0257".as_slice()));

        // Gap 20..28 is all spaces.
        assert!(
            pkt.get(20..28)
                .is_some_and(|s| s.iter().all(|&b| b == b' '))
        );
        // Gap 33..40 is all spaces.
        assert!(
            pkt.get(33..40)
                .is_some_and(|s| s.iter().all(|&b| b == b' '))
        );
        // Tail 47..56 is all spaces.
        assert!(
            pkt.get(47..56)
                .is_some_and(|s| s.iter().all(|&b| b == b' '))
        );
    }
}
