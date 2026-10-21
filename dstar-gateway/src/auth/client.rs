//! Bounded `DPlus` TCP authentication and host-list retrieval.
//!
//! The 56-byte request follows
//! `ircDDBGateway/Common/DPlusAuthenticator.cpp:111-143`.

use std::future::Future;
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use dstar_gateway_core::codec::dplus::{HostList, parse_auth_response};
use dstar_gateway_core::error::IoOperation;
use dstar_gateway_core::types::Callsign;
use dstar_gateway_core::validator::NullSink;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;

/// Default authentication host and port used by ircDDBGateway.
pub const DEFAULT_AUTH_ENDPOINT: &str = "auth.dstargateway.org:20001";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const ADDRESS_TIMEOUT: Duration = Duration::from_secs(3);
const RESPONSE_LIMIT: NonZeroUsize = NonZeroUsize::new(2 * 1024 * 1024).unwrap();
const AUTH_PACKET_LEN: usize = 56;

/// `DPlus` authentication with bounded DNS/connect, write and response phases.
///
/// Defaults are ten seconds for DNS plus all TCP attempts, ten seconds for
/// writing, five seconds total response time, five seconds between reads,
/// and a two-MiB response limit. These are local resource policies.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuthClient {
    endpoint: Option<SocketAddr>,
    connect_timeout: Duration,
    read_timeout: Duration,
    idle_timeout: Duration,
    response_limit: NonZeroUsize,
}

impl Default for AuthClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthClient {
    /// Construct the default bounded client described by [`AuthClient`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            endpoint: None,
            connect_timeout: CONNECT_TIMEOUT,
            read_timeout: READ_TIMEOUT,
            idle_timeout: READ_TIMEOUT,
            response_limit: RESPONSE_LIMIT,
        }
    }

    /// Set an IP endpoint, bypassing hostname resolution.
    #[must_use]
    pub const fn with_endpoint(mut self, endpoint: SocketAddr) -> Self {
        self.endpoint = Some(endpoint);
        self
    }

    /// Set the total DNS/connect budget and the independent write budget.
    ///
    /// Each resolved address receives at most three seconds within the shared
    /// connect deadline. Zero expires immediately; an unrepresentable deadline
    /// returns [`AuthError::DeadlineOverflow`] before starting authentication.
    #[must_use]
    pub const fn with_connect_timeout(mut self, duration: Duration) -> Self {
        self.connect_timeout = duration;
        self
    }

    /// Set the total response budget, measured from the end of request writing.
    ///
    /// Partial responses never renew this deadline. Zero expires immediately.
    #[must_use]
    pub const fn with_read_timeout(mut self, duration: Duration) -> Self {
        self.read_timeout = duration;
        self
    }

    /// Set the maximum idle interval between response reads.
    ///
    /// The total response deadline still applies while data keeps arriving.
    #[must_use]
    pub const fn with_idle_timeout(mut self, duration: Duration) -> Self {
        self.idle_timeout = duration;
        self
    }

    /// Set the maximum retained response bytes before parsing.
    #[must_use]
    pub const fn with_response_limit(mut self, limit: NonZeroUsize) -> Self {
        self.response_limit = limit;
        self
    }

    /// Current IP endpoint override, or None for the default DNS endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> Option<SocketAddr> {
        self.endpoint
    }

    /// Authenticate over one transient TCP connection and return its host list.
    ///
    /// Responses accumulate until EOF, within both total/idle deadlines and
    /// the configured byte limit. Parsing uses the core's diagnostic-skipping
    /// host-list policy.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::DeadlineOverflow`] for unrepresentable configured
    /// spans before I/O, [`AuthError::Timeout`] for a phase deadline,
    /// [`AuthError::ResponseTooLarge`] before retaining excess response bytes,
    /// or a typed I/O/parser error.
    ///
    /// # Cancellation
    ///
    /// Dropping this future closes its TCP stream. A partly written request
    /// may already have reached the server. An OS DNS lookup already started
    /// by Tokio may finish after the future is dropped.
    ///
    /// ```no_run
    /// use dstar_gateway::auth::AuthClient;
    /// use dstar_gateway_core::Callsign;
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let hosts = AuthClient::new()
    ///     .authenticate(Callsign::try_from_str("W1AW")?).await?;
    /// println!("{} known hosts", hosts.len());
    /// # Ok(()) }
    /// ```
    pub async fn authenticate(&self, callsign: Callsign) -> Result<HostList, AuthError> {
        for (duration, phase) in [
            (self.connect_timeout, AuthPhase::Connect),
            (self.read_timeout, AuthPhase::Read),
            (self.idle_timeout, AuthPhase::Read),
        ] {
            let _deadline = deadline(duration, phase)?;
        }
        let mut stream = within(self.connect_timeout, AuthPhase::Connect, self.connect()).await??;
        within(
            self.connect_timeout,
            AuthPhase::Write,
            stream.write_all(&build_auth_packet(callsign)),
        )
        .await?
        .map_err(|source| AuthError::Io {
            source,
            operation: IoOperation::TcpAuthWrite,
        })?;
        let response = self.read_response(&mut stream).await?;
        parse_auth_response(&response, &mut NullSink).map_err(Into::into)
    }

    /// Resolve and sequentially attempt addresses inside the caller's deadline.
    async fn connect(&self) -> Result<TcpStream, AuthError> {
        let host = self.endpoint.map_or_else(
            || DEFAULT_AUTH_ENDPOINT.to_owned(),
            |address| address.to_string(),
        );
        let addresses = tokio::net::lookup_host(host)
            .await
            .map_err(|source| AuthError::Io {
                source,
                operation: IoOperation::TcpAuthConnect,
            })?;
        let mut last_error = std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            "authentication endpoint resolved to no addresses",
        );
        for address in addresses {
            match tokio::time::timeout(ADDRESS_TIMEOUT, TcpStream::connect(address)).await {
                Ok(Ok(stream)) => return Ok(stream),
                Ok(Err(error)) => last_error = error,
                Err(_) => {
                    last_error = std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("TCP connection to {address} exceeded {ADDRESS_TIMEOUT:?}"),
                    );
                }
            }
        }
        Err(AuthError::Io {
            source: last_error,
            operation: IoOperation::TcpAuthConnect,
        })
    }

    /// Retain at most the configured byte limit until EOF or a fixed deadline.
    async fn read_response(&self, stream: &mut TcpStream) -> Result<Vec<u8>, AuthError> {
        if self.read_timeout.is_zero() {
            return Err(AuthError::Timeout {
                elapsed: self.read_timeout,
                phase: AuthPhase::Read,
            });
        }
        let cutoff = deadline(self.read_timeout, AuthPhase::Read)?;
        let mut response = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = tokio::time::timeout_at(
                cutoff,
                within(self.idle_timeout, AuthPhase::Read, stream.read(&mut buffer)),
            )
            .await
            .map_err(|_| AuthError::Timeout {
                elapsed: self.read_timeout,
                phase: AuthPhase::Read,
            })??
            .map_err(|source| AuthError::Io {
                source,
                operation: IoOperation::TcpAuthRead,
            })?;
            if count == 0 {
                return Ok(response);
            }
            let length = response.len().saturating_add(count);
            if length > self.response_limit.get() {
                return Err(AuthError::ResponseTooLarge {
                    limit: self.response_limit,
                    observed_at_least: length,
                });
            }
            response.extend_from_slice(buffer.get(..count).unwrap_or(&[]));
        }
    }
}

/// Convert a configured span into a representable absolute Tokio deadline.
fn deadline(duration: Duration, phase: AuthPhase) -> Result<tokio::time::Instant, AuthError> {
    Instant::now()
        .checked_add(duration)
        .map(tokio::time::Instant::from_std)
        .ok_or(AuthError::DeadlineOverflow { duration, phase })
}

/// Bound every await in a phase by the same absolute deadline.
async fn within<F: Future>(
    duration: Duration,
    phase: AuthPhase,
    operation: F,
) -> Result<F::Output, AuthError> {
    if duration.is_zero() {
        return Err(AuthError::Timeout {
            elapsed: duration,
            phase,
        });
    }
    tokio::time::timeout_at(deadline(duration, phase)?, operation)
        .await
        .map_err(|_| AuthError::Timeout {
            elapsed: duration,
            phase,
        })
}

/// Build the reference 56-byte request; unused bytes remain spaces.
fn build_auth_packet(callsign: Callsign) -> [u8; AUTH_PACKET_LEN] {
    let mut packet = [b' '; AUTH_PACKET_LEN];
    for (start, bytes) in [
        (0, [0x38, 0xC0, 0x01, 0x00].as_slice()),
        (4, callsign.as_bytes().as_slice()),
        (12, b"DV019999".as_slice()),
        (28, b"W7IB2".as_slice()),
        (40, b"DHS0257".as_slice()),
    ] {
        if let Some(destination) = packet.get_mut(start..start + bytes.len()) {
            destination.copy_from_slice(bytes);
        }
    }
    packet
}

/// Authentication phase, resource or parser failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// An underlying socket/resolver operation failed.
    #[error("DPlus auth I/O error during {operation:?}: {source}")]
    Io {
        /// Original operating-system error.
        source: std::io::Error,
        /// Failed resolver/socket operation.
        operation: IoOperation,
    },
    /// A total phase or idle deadline elapsed.
    #[error("DPlus auth timed out after {elapsed:?} during {phase:?}")]
    Timeout {
        /// Configured deadline span.
        elapsed: Duration,
        /// Phase whose deadline expired.
        phase: AuthPhase,
    },
    /// A configured span exceeds the platform's Instant range.
    #[error("DPlus auth deadline {duration:?} cannot be represented during {phase:?}")]
    DeadlineOverflow {
        /// Unrepresentable configured span.
        duration: Duration,
        /// Phase using that span.
        phase: AuthPhase,
    },
    /// Incoming data exceeded the configured retained-byte limit.
    #[error("DPlus auth response exceeds {limit} bytes (at least {observed_at_least} received)")]
    ResponseTooLarge {
        /// Maximum bytes retained.
        limit: NonZeroUsize,
        /// Minimum observed size at rejection.
        observed_at_least: usize,
    },
    /// The bounded response has a fatal host-list framing error.
    #[error(transparent)]
    Parse(#[from] dstar_gateway_core::codec::dplus::DPlusError),
}

/// Deadline phase used by [`AuthError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AuthPhase {
    /// DNS resolution and all TCP connection attempts.
    Connect,
    /// Writing the fixed 56-byte request.
    Write,
    /// Reading the host-list response through EOF.
    Read,
}
