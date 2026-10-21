//! Async TCP client for APRS-IS (Internet Service).
//!
//! Provides a [`AprsIsClient`] that connects to an APRS-IS server over
//! TCP, authenticates, and exchanges APRS packets as line-delimited text.
//! This is the complement to the pure-data helpers at the crate root
//! (e.g. [`crate::parse_is_line`], [`crate::format_is_packet`],
//! [`crate::build_login_string`]), which stay transport-agnostic.
//!
//! # Usage
//!
//! ```no_run
//! use aprs_is::{AprsIsClient, AprsIsConfig, AprsIsEvent};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let config = AprsIsConfig::new("N0CALL")?
//!     .with_raw_filter("r/35.25/-97.75/100")?;
//! let mut client = AprsIsClient::connect(config).await?;
//!
//! loop {
//!     match client.next_event().await? {
//!         AprsIsEvent::Packet(pkt) => println!("Got: {}", pkt.line()),
//!         AprsIsEvent::Comment(line) => println!("Server: {line}"),
//!         AprsIsEvent::LoggedIn { server } => {
//!             println!("Authenticated (server {server:?})");
//!         }
//!         AprsIsEvent::LoginRejected { reason } => {
//!             println!("Login rejected: {reason}");
//!             break;
//!         }
//!         AprsIsEvent::Disconnected => break,
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Keepalive
//!
//! The client offers a 120-second idle-write policy. Call
//! [`AprsIsClient::send_keepalive`] on a timer, or
//! use [`AprsIsClient::maybe_send_keepalive`] which only sends if the
//! keepalive interval has elapsed since the last write.
//!
//! # Reconnection
//!
//! On [`AprsIsEvent::Disconnected`], call [`AprsIsClient::reconnect`] to
//! re-establish the TCP connection and re-login. Callers typically wrap
//! this in an exponential backoff loop.

use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use crate::error::AprsIsError;
use crate::events::{AprsIsEvent, AprsIsPacket, LoginState};
use crate::line::{format_is_packet, parse_is_line};
use crate::login::{AprsIsConfig, Passcode, build_login_string};
use crate::uplink::AprsIsUplinkLine;

/// Recognizes a complete login-response prefix for this connection's identity.
fn parse_logresp<'line>(line: &'line str, callsign: &str) -> Option<(bool, Option<&'line str>)> {
    let mut words = line.split_ascii_whitespace();
    if words.next()? != "#"
        || words.next()? != "logresp"
        || !words.next()?.eq_ignore_ascii_case(callsign)
    {
        return None;
    }
    let verified = match words.next()? {
        "verified," => true,
        "unverified," => false,
        _ => return None,
    };
    let server = if words.next() == Some("server") {
        words
            .next()
            .map(|name| name.trim_end_matches(','))
            .filter(|name| !name.is_empty())
    } else {
        None
    };
    Some((verified, server))
}

/// Default APRS-IS keepalive interval.
///
/// This is the client's optional idle-write policy. Server idle limits vary.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(120);

/// Total deadline for TCP establishment and writing the login line.
/// Server login verification arrives later through [`AprsIsClient::next_event`].
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Deadline for one complete outgoing line or explicit socket shutdown.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum length, in bytes, of a single inbound line read from the
/// server before the reader gives up and resynchronises.
///
/// The client connects to arbitrary internet servers, so a malicious or
/// buggy peer can stream bytes without ever sending a `\n`; an unbounded
/// `read_until(b'\n', ..)` would grow the line buffer until the process
/// runs out of memory (a denial-of-service). This cap bounds the buffer.
///
/// The value is double [`crate::MAX_IS_LINE_BYTES`] (the outbound 512-byte
/// protocol limit including `CRLF`), allowing additional room for comments.
/// It is an implementation limit on all incoming lines. A line that
/// exceeds this length (including its newline when present) yields
/// [`AprsIsError::ReadLineTooLong`]; an exact-cap final line at EOF remains
/// valid.
pub const MAX_IS_READ_LINE_BYTES: usize = 1024;

/// Keepalive comment text (sent as `# aprs-is keepalive\r\n`).
const KEEPALIVE_COMMENT: &str = "# aprs-is keepalive";

/// Read one newline-terminated line from `reader` into `buf`, bounding
/// the line length at `max` bytes.
///
/// Returns the complete retained byte count, including prefixes from cancelled
/// calls and the terminating newline when present. Zero signals clean EOF.
/// The caller clears the buffer and overflow flag after a completed line.
///
/// Unlike [`AsyncBufReadExt::read_until`], this caps the amount of data
/// buffered for a single line: once more than `max` bytes arrive, the function
/// drains and discards the rest of the oversized line up to (and including)
/// the next newline so the stream stays framed, then returns
/// [`AprsIsError::ReadLineTooLong`]. If EOF arrives first, the error is still
/// returned rather than exposing the retained prefix as a packet. This
/// prevents a server that never sends a newline from growing `buf` without
/// bound.
async fn read_is_line<R>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    overflowed: &mut bool,
    max: usize,
) -> Result<usize, AprsIsError>
where
    R: AsyncBufReadExt + Unpin,
{
    let mut copied = buf.len();
    loop {
        let chunk = reader.fill_buf().await.map_err(AprsIsError::Read)?;
        if chunk.is_empty() {
            // EOF. Surface an in-cap unterminated final line, but never turn
            // the retained prefix of an oversized line into a valid packet.
            return if *overflowed {
                Err(AprsIsError::ReadLineTooLong { max })
            } else {
                Ok(copied)
            };
        }

        // Newline found in this chunk: the line ends at `pos`, and this
        // arm always returns (Ok on accept, Err on over-cap reject).
        if let Some(pos) = chunk.iter().position(|&b| b == b'\n') {
            // `pos` is in-bounds, so `get(..=pos)` is `Some`.
            let upto = chunk
                .get(..=pos)
                .unwrap_or_else(|| unreachable!("newline position is within the fill_buf slice"));
            // Always consume through the newline so the stream stays
            // framed regardless of whether we accept or reject.
            let consume_n = pos + 1;
            let fits = !*overflowed && copied.saturating_add(upto.len()) <= max;
            if fits {
                buf.extend_from_slice(upto);
                copied += upto.len();
            }
            reader.consume(consume_n);
            if fits {
                return Ok(copied);
            }
            // Over the cap: the oversized line has now been drained up to
            // and including its newline, so the next read starts cleanly
            // on the following line.
            return Err(AprsIsError::ReadLineTooLong { max });
        }

        // No newline yet: copy what still fits under the cap, discard the
        // rest, and keep looping until the newline arrives so a
        // newline-less flood resynchronises instead of wedging.
        let remaining = max.saturating_sub(copied);
        let take = remaining.min(chunk.len());
        if take > 0 {
            let head = chunk.get(..take).unwrap_or_else(|| {
                unreachable!("take is bounded by chunk.len() via the min above")
            });
            buf.extend_from_slice(head);
            copied += take;
        }
        if take < chunk.len() {
            *overflowed = true;
        }
        let consume_n = chunk.len();
        reader.consume(consume_n);
        // `copied` never exceeds `max`; once it reaches `max` we keep
        // draining subsequent chunks (take == 0) until the newline, then
        // return ReadLineTooLong above.
    }
}

/// Async TCP client for APRS-IS.
///
/// Owns a single TCP connection to an APRS-IS server, handles the login
/// handshake, and exposes line-at-a-time read/write methods.
///
/// The client is `Send` and `Sync`; one task exclusively borrows it for each
/// operation. Connection ownership is unique.
///
/// # TLS support
///
/// This client uses plaintext TCP. A caller that owns a TLS stream can use
/// [`crate::build_login_string`], [`crate::AprsIsLine`], and
/// [`crate::AprsIsUplinkLine`] for its connection's login and packet framing.
#[derive(Debug)]
pub struct AprsIsClient {
    config: AprsIsConfig,
    reader: BufReader<OwnedReadHalf>,
    writer: Option<OwnedWriteHalf>,
    /// Persistent byte buffer for the bounded line reader. The client reads raw
    /// bytes (not `String` via `read_line`) so non-UTF-8 sequences in
    /// APRS info fields (Mic-E, raw weather, Latin-1 comments) do not
    /// surface as `io::ErrorKind::InvalidData` and tear down the
    /// long-lived TCP session. The bytes are decoded via
    /// `String::from_utf8_lossy` at the event boundary; callers that
    /// need byte-exact fidelity get the raw bytes in
    /// [`AprsIsPacket::raw`].
    line_buf: Vec<u8>,
    line_overflowed: bool,
    line_complete: bool,
    last_write: Instant,
    login_state: LoginState,
}

impl AprsIsClient {
    /// Connect to the APRS-IS server and perform the login handshake.
    ///
    /// Establishes TCP, writes and flushes the login line, then returns.
    /// Login verification (the `# logresp`
    /// line) is reported asynchronously via [`AprsIsEvent::LoggedIn`]
    /// from [`next_event`](Self::next_event).
    /// Packet uploads remain disabled until [`Self::login_state`] becomes
    /// [`LoginState::Verified`].
    ///
    /// TCP establishment and login writing share [`CONNECT_TIMEOUT`]. Cancelling
    /// this future drops its newly opened socket; no client is returned.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::InvalidLoginField`] if a login field
    /// (callsign / software name / version / filter) is invalid,
    /// [`AprsIsError::InvalidUplinkLine`] if the completed login line
    /// exceeds the APRS-IS 512-byte limit,
    /// [`AprsIsError::Connect`] if TCP connect fails or times out, or
    /// [`AprsIsError::Write`] if the login string cannot be sent.
    pub async fn connect(config: AprsIsConfig) -> Result<Self, AprsIsError> {
        let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
        let addr = format!("{}:{}", config.server, config.port);
        tracing::info!(server = %addr, callsign = %config.callsign, "APRS-IS connecting");

        // Login validation precedes every socket side effect.
        let login = build_login_string(&config)?;
        let login = AprsIsUplinkLine::from_wire_bytes(login.as_bytes())?;

        let stream = tokio::time::timeout_at(
            deadline,
            TcpStream::connect((config.server.as_str(), config.port)),
        )
        .await
        .map_err(|_| {
            AprsIsError::Connect(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "TCP connect timed out",
            ))
        })?
        .map_err(AprsIsError::Connect)?;

        // Connecting.aspx recommends TCP_NODELAY for bidirectional clients.
        stream.set_nodelay(true).map_err(AprsIsError::Connect)?;

        let (read_half, mut write_half) = stream.into_split();

        tokio::time::timeout_at(deadline, async {
            write_half.write_all(login.as_bytes()).await?;
            write_half.flush().await
        })
        .await
        .map_err(|_| AprsIsError::LoginTimeout)?
        .map_err(AprsIsError::Write)?;

        tracing::debug!("APRS-IS login sent");

        Ok(Self {
            config,
            reader: BufReader::new(read_half),
            writer: Some(write_half),
            line_buf: Vec::with_capacity(512),
            line_overflowed: false,
            line_complete: false,
            last_write: Instant::now(),
            login_state: LoginState::Pending,
        })
    }

    /// Connect with exponential backoff.
    ///
    /// Retries the TCP connection up to `max_attempts` times, doubling
    /// the delay from 1 second up to a cap of 60 seconds between attempts.
    /// Pass `None` for `max_attempts` to retry forever.
    ///
    /// # Errors
    ///
    /// Returns the last [`AprsIsError`] after exhausting all attempts.
    pub async fn connect_with_retry(
        config: AprsIsConfig,
        max_attempts: Option<NonZeroU32>,
    ) -> Result<Self, AprsIsError> {
        let mut delay = Duration::from_secs(1);
        let mut attempt: u32 = 0;
        loop {
            attempt = attempt.saturating_add(1);
            match Self::connect(config.clone()).await {
                Ok(client) => return Ok(client),
                Err(e) => {
                    if !matches!(
                        e,
                        AprsIsError::Connect(_) | AprsIsError::Write(_) | AprsIsError::LoginTimeout
                    ) || max_attempts.is_some_and(|max| attempt >= max.get())
                    {
                        return Err(e);
                    }
                    tracing::warn!(
                        attempt,
                        error = %e,
                        retry_in_secs = delay.as_secs(),
                        "APRS-IS connect failed, retrying"
                    );
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(60));
                }
            }
        }
    }

    /// Reconnect to the APRS-IS server after a disconnect.
    ///
    /// Keeps the old connection until replacement TCP establishment and login
    /// writing succeed. A cancelled or failed replacement retains the old state.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`connect`](Self::connect).
    pub async fn reconnect(&mut self) -> Result<(), AprsIsError> {
        tracing::info!("APRS-IS reconnecting");
        let new = Self::connect(self.config.clone()).await?;
        *self = new;
        Ok(())
    }

    /// Read the next event from the server.
    ///
    /// Returns when a complete line arrives or the connection closes.
    /// No intrinsic read deadline applies. Cancellation retains partial bytes
    /// and oversized-line draining state; the next call resumes the same line.
    ///
    /// # Encoding policy
    ///
    /// Bytes are read with a bounded byte-oriented reader (not
    /// `read_line`) so that non-UTF-8 sequences common in APRS info
    /// fields (Mic-E, raw weather data, Latin-1 comments) do not return
    /// `io::ErrorKind::InvalidData` and tear down the connection.
    /// Decoding to a Rust `String` happens via [`String::from_utf8_lossy`];
    /// any non-UTF-8 byte becomes a U+FFFD replacement character in the
    /// parsed view. For packet lines the original bytes are preserved
    /// in [`AprsIsPacket::raw`] so callers needing byte-exact fidelity
    /// (`IGate` forwarding, packet capture) can recover the wire-truth
    /// form.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::Read`] on socket errors, or
    /// [`AprsIsError::ReadLineTooLong`] if the server sends more than
    /// [`MAX_IS_READ_LINE_BYTES`] without a terminating newline (the
    /// oversized line is discarded and the stream resynchronises, so the
    /// caller may continue calling `next_event`).
    pub async fn next_event(&mut self) -> Result<AprsIsEvent, AprsIsError> {
        if self.line_complete {
            self.line_buf.clear();
            self.line_overflowed = false;
            self.line_complete = false;
        }
        // Bounded read: a malicious/buggy server could otherwise stream
        // bytes with no `\n` and grow `line_buf` until OOM. See
        // `read_is_line` and `MAX_IS_READ_LINE_BYTES`. On overflow the
        // helper drains the oversized line up to the next newline and
        // returns `ReadLineTooLong`, leaving the stream resynchronised.
        let result = read_is_line(
            &mut self.reader,
            &mut self.line_buf,
            &mut self.line_overflowed,
            MAX_IS_READ_LINE_BYTES,
        )
        .await;
        self.line_complete =
            result.is_ok() || matches!(&result, Err(AprsIsError::ReadLineTooLong { .. }));
        let bytes = result?;

        if bytes == 0 {
            tracing::info!("APRS-IS connection closed by server");
            return Ok(AprsIsEvent::Disconnected);
        }

        // Strip terminal line delimiters while retaining information bytes.
        let raw_len = self
            .line_buf
            .iter()
            .rposition(|&b| b != b'\r' && b != b'\n')
            .map_or(0, |i| i + 1);
        let raw_bytes = self.line_buf.get(..raw_len).unwrap_or_else(|| {
            unreachable!("raw_len is bounded by line_buf.len() by construction")
        });

        // Lossy decode: every non-UTF-8 byte becomes U+FFFD. Used for
        // line-level parsing (TNC2 monitor format) and the human-
        // readable view; the raw bytes are kept separately for the
        // packet event so wire-truth is recoverable downstream.
        let line_str = String::from_utf8_lossy(raw_bytes);
        let line: &str = line_str.as_ref();

        if parse_is_line(line).is_some() {
            return Ok(AprsIsEvent::Packet(AprsIsPacket::from_received(
                raw_bytes.to_vec(),
            )));
        }

        if self.login_state == LoginState::Pending
            && let Some((verified, server)) = parse_logresp(line, &self.config.callsign)
        {
            if verified || self.config.passcode == Passcode::ReceiveOnly {
                self.login_state = if verified {
                    LoginState::Verified
                } else {
                    LoginState::ReceiveOnly
                };
                return Ok(AprsIsEvent::LoggedIn {
                    server: server.map(str::to_owned),
                });
            }
            self.login_state = LoginState::Rejected;
            return Ok(AprsIsEvent::LoginRejected {
                reason: crate::LoginRejection::Unverified {
                    response: line.to_owned(),
                },
            });
        }

        Ok(AprsIsEvent::Comment(line.to_owned()))
    }

    /// Returns the server acknowledgement state for this connection.
    #[must_use]
    pub const fn login_state(&self) -> LoginState {
        self.login_state
    }

    /// Send a formatted APRS packet to the server.
    ///
    /// The packet is formatted as `source>destination,path:data\r\n` via
    /// [`crate::format_is_packet`] and written to the TCP socket.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::InvalidUplinkLine`] if any input inserts a
    /// framing byte or makes the completed line exceed 512 bytes, or
    /// [`AprsIsError::Write`] if the socket write or flush fails.
    /// Login and stream availability errors follow [`Self::send_uplink_line`].
    pub async fn send_packet(
        &mut self,
        source: &str,
        destination: &str,
        path: &[&str],
        data: &str,
    ) -> Result<(), AprsIsError> {
        let line = format_is_packet(source, destination, path.iter().copied(), data)?;
        self.send_uplink_line(&line).await
    }

    /// Send one already validated APRS-IS uplink line.
    ///
    /// This is the byte-preserving send path for RF packets, including
    /// Mic-E and other information fields that are not valid UTF-8.
    ///
    /// The [`AprsIsUplinkLine`] type proves that the bytes have exactly
    /// one trailing `CRLF`, no embedded framing bytes, and fit within the
    /// APRS-IS 512-byte line limit.
    /// Packet lines require [`LoginState::Verified`]; comments and empty lines
    /// are permitted in every login state. A rejected upload writes no bytes.
    ///
    /// Writing and flushing share [`SEND_TIMEOUT`]. Cancellation, a timeout,
    /// or an I/O failure retires the outgoing stream because a partial line
    /// may have reached the peer. Subsequent sends return
    /// [`AprsIsError::ConnectionRetired`] until reconnection succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::Write`] if writing fails or times out, or
    /// [`AprsIsError::ConnectionRetired`] if an earlier send retired the stream.
    /// Returns [`AprsIsError::UnverifiedConnection`] for a packet upload before
    /// the server's verified acknowledgement.
    pub async fn send_uplink_line(&mut self, line: &AprsIsUplinkLine) -> Result<(), AprsIsError> {
        if self.writer.is_none() {
            return Err(AprsIsError::ConnectionRetired);
        }
        let first = line.as_bytes().first().copied();
        if !matches!(first, Some(b'#' | b'\r')) && self.login_state != LoginState::Verified {
            return Err(AprsIsError::UnverifiedConnection {
                state: self.login_state,
            });
        }
        let mut writer = self.writer.take().ok_or(AprsIsError::ConnectionRetired)?;
        tokio::time::timeout(SEND_TIMEOUT, async {
            writer.write_all(line.as_bytes()).await?;
            writer.flush().await
        })
        .await
        .map_err(|_| {
            AprsIsError::Write(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "APRS-IS line write timed out",
            ))
        })?
        .map_err(AprsIsError::Write)?;
        self.writer = Some(writer);
        self.last_write = Instant::now();
        Ok(())
    }

    /// Validate and send exact APRS-IS wire bytes.
    ///
    /// The bytes must end with exactly one `CRLF`; the body must contain
    /// no carriage return or line feed; and the complete line must be no
    /// longer than [`crate::MAX_IS_LINE_BYTES`]. Validation never decodes the
    /// bytes as UTF-8, so accepted bytes reach the socket unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::InvalidUplinkLine`] when framing, injection,
    /// or length validation fails, or [`AprsIsError::Write`] when the
    /// socket write or flush fails.
    /// Login and stream availability errors follow [`Self::send_uplink_line`].
    pub async fn send_raw_bytes(&mut self, wire: &[u8]) -> Result<(), AprsIsError> {
        let line = AprsIsUplinkLine::from_wire_bytes(wire)?;
        self.send_uplink_line(&line).await
    }

    /// Validate and send a UTF-8 APRS-IS wire line.
    ///
    /// This helper accepts text-only callers.
    /// RF uplinks should use [`send_uplink_line`](Self::send_uplink_line)
    /// or [`send_raw_bytes`](Self::send_raw_bytes) so no UTF-8 conversion
    /// can alter the information bytes.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`send_raw_bytes`](Self::send_raw_bytes).
    pub async fn send_raw_line(&mut self, line: &str) -> Result<(), AprsIsError> {
        self.send_raw_bytes(line.as_bytes()).await
    }

    /// Send a keepalive comment line unconditionally.
    ///
    /// Sends `# aprs-is keepalive\r\n` to the server. Call this
    /// on a timer or use [`maybe_send_keepalive`](Self::maybe_send_keepalive)
    /// to only send if the interval has elapsed.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::Write`] if the write fails.
    pub async fn send_keepalive(&mut self) -> Result<(), AprsIsError> {
        self.send_raw_line(&format!("{KEEPALIVE_COMMENT}\r\n"))
            .await
    }

    /// Send a keepalive if the keepalive interval has elapsed.
    ///
    /// No-op if less than [`KEEPALIVE_INTERVAL`] has passed since the
    /// last write of any kind (keepalive or packet).
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::Write`] if the write fails.
    pub async fn maybe_send_keepalive(&mut self) -> Result<(), AprsIsError> {
        if self.last_write.elapsed() >= KEEPALIVE_INTERVAL {
            self.send_keepalive().await?;
        }
        Ok(())
    }

    /// Get the configuration this client was created with.
    #[must_use]
    pub const fn config(&self) -> &AprsIsConfig {
        &self.config
    }

    /// Gracefully shut down the TCP connection.
    ///
    /// Consumes the client and bounds write-half shutdown by [`SEND_TIMEOUT`].
    /// Cancellation drops both halves. An already retired write half requires
    /// no further shutdown operation.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::Write`] if the shutdown flush fails.
    pub async fn shutdown(mut self) -> Result<(), AprsIsError> {
        tracing::debug!("APRS-IS shutting down");
        if let Some(mut writer) = self.writer.take() {
            tokio::time::timeout(SEND_TIMEOUT, writer.shutdown())
                .await
                .map_err(|_| {
                    AprsIsError::Write(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "APRS-IS shutdown timed out",
                    ))
                })?
                .map_err(AprsIsError::Write)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::login::Passcode;
    use std::future::Future;
    use tokio::io::AsyncReadExt as _;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Read up to `buf.len()` bytes from `stream`. Returns `Some(n)` for
    /// a non-empty read, or `None` on EOF or I/O error. Handlers treat
    /// `None` as "stop capturing" and return early; dropping their
    /// `oneshot` sender then fails the test body's [`recv_captured`]
    /// await instead of being lost in the detached server task.
    async fn read_some(stream: &mut TcpStream, buf: &mut [u8]) -> Option<usize> {
        stream.read(buf).await.ok().filter(|n| *n > 0)
    }

    /// Capture bytes from `stream` until `lines` newline bytes have been
    /// seen, EOF is reached, or a read fails. Returns everything read.
    ///
    /// Framing on `\n` counts (rather than on `read` call boundaries)
    /// makes the capture immune to TCP coalescing and fragmentation:
    /// the client's login and packet writes may arrive merged into one
    /// segment or split across several. Pass `usize::MAX` to capture
    /// until the peer closes the connection.
    async fn read_lines_capture(stream: &mut TcpStream, lines: usize) -> Vec<u8> {
        let mut captured = Vec::new();
        let mut seen = 0_usize;
        let mut buf = [0_u8; 512];
        while seen < lines {
            let Some(n) = read_some(stream, &mut buf).await else {
                break;
            };
            let chunk = buf.get(..n).unwrap_or(&[]);
            for &byte in chunk {
                if byte == b'\n' {
                    seen += 1;
                }
            }
            captured.extend_from_slice(chunk);
        }
        captured
    }

    /// Await the bytes a mock-server handler captured, bounding the wait
    /// so a wedged handler fails the test instead of hanging it. A
    /// handler that exited without sending (dropped sender) fails the
    /// test immediately. Decodes the bytes as UTF-8; everything these
    /// tests put on the wire is ASCII.
    async fn recv_captured(
        rx: oneshot::Receiver<Vec<u8>>,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let bytes = recv_captured_bytes(rx).await?;
        Ok(String::from_utf8(bytes)?)
    }

    /// Await exact bytes captured by a mock server, including non-UTF-8
    /// information fields.
    async fn recv_captured_bytes(
        rx: oneshot::Receiver<Vec<u8>>,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let bytes = tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .map_err(|_| "mock server handler did not report captured bytes within 5s")??;
        Ok(bytes)
    }

    /// Write all of `data` to `stream`; swallow any I/O error since the
    /// test will fail separately if the client doesn't see the line.
    async fn write_all_ignore(stream: &mut TcpStream, data: &[u8]) {
        if let Err(err) = stream.write_all(data).await {
            tracing::debug!(%err, "mock server write_all error");
        }
    }

    /// Spawn a mock APRS-IS server that accepts one connection and runs
    /// the given handler on it.
    ///
    /// Returns the bound `SocketAddr` so tests can connect to it.
    ///
    /// The handler runs on a detached tokio task, so a panic inside it
    /// (e.g. a failed `assert!`) is swallowed by the runtime and can
    /// never fail the test. Handlers must therefore not assert. Instead
    /// they capture the bytes they read (see [`read_lines_capture`]) and
    /// ship them to the test body over a `oneshot` channel, and the test
    /// body asserts on the wire content after [`recv_captured`].
    async fn spawn_mock_server<F, Fut>(handler: F) -> Result<std::net::SocketAddr, std::io::Error>
    where
        F: FnOnce(TcpStream) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        drop(tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                handler(stream).await;
            }
        }));
        Ok(addr)
    }

    fn test_config(addr: std::net::SocketAddr) -> AprsIsConfig {
        AprsIsConfig {
            callsign: "N0CALL".to_owned(),
            passcode: Passcode::ReceiveOnly,
            server: addr.ip().to_string(),
            port: addr.port(),
            filter: String::new(),
            software_name: "test".to_owned(),
            software_version: "0.1".to_owned(),
        }
    }

    async fn spawn_verified_server<F, Fut>(
        handler: F,
    ) -> Result<std::net::SocketAddr, std::io::Error>
    where
        F: FnOnce(TcpStream) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        spawn_mock_server(|mut stream| async move {
            write_all_ignore(&mut stream, b"# logresp N0CALL verified, server T2TEST\r\n").await;
            handler(stream).await;
        })
        .await
    }

    async fn connect_verified(
        addr: std::net::SocketAddr,
    ) -> Result<AprsIsClient, Box<dyn std::error::Error>> {
        let config = test_config(addr).with_passcode(Passcode::for_callsign("N0CALL"))?;
        let mut client = AprsIsClient::connect(config).await?;
        assert_eq!(client.login_state(), LoginState::Pending);
        assert!(matches!(
            client.next_event().await?,
            AprsIsEvent::LoggedIn { .. }
        ));
        assert_eq!(client.login_state(), LoginState::Verified);
        Ok(client)
    }

    #[tokio::test]
    async fn packet_upload_waits_for_verified_login_without_retiring_writer() -> TestResult {
        for (passcode, response, state) in [
            (
                Passcode::ReceiveOnly,
                b"unverified".as_slice(),
                LoginState::ReceiveOnly,
            ),
            (
                Passcode::for_callsign("N0CALL"),
                b"unverified".as_slice(),
                LoginState::Rejected,
            ),
            (
                Passcode::for_callsign("N0CALL"),
                b"verified".as_slice(),
                LoginState::Verified,
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let config = test_config(listener.local_addr()?).with_passcode(passcode)?;
            let (client, accepted) = tokio::join!(AprsIsClient::connect(config), listener.accept());
            let mut client = client?;
            let (mut peer, _) = accepted?;
            let login = read_lines_capture(&mut peer, 1).await;
            assert!(login.starts_with(b"user N0CALL "));
            let packet = AprsIsUplinkLine::from_body_bytes(b"N0CALL>APRS:test".as_slice())?;
            let result = client.send_uplink_line(&packet).await;
            assert!(
                matches!(
                    result,
                    Err(AprsIsError::UnverifiedConnection {
                        state: LoginState::Pending
                    })
                ),
                "{result:?}"
            );
            assert!(client.writer.is_some());
            client.send_keepalive().await?;
            assert_eq!(
                read_lines_capture(&mut peer, 1).await,
                b"# aprs-is keepalive\r\n"
            );
            let mut acknowledgement = b"# logresp N0CALL ".to_vec();
            acknowledgement.extend_from_slice(response);
            acknowledgement.extend_from_slice(b", server T2TEST\r\n");
            peer.write_all(&acknowledgement).await?;
            let event = client.next_event().await?;
            assert_eq!(client.login_state(), state);
            assert_eq!(
                matches!(event, AprsIsEvent::LoginRejected { .. }),
                state == LoginState::Rejected
            );
            let result = client.send_uplink_line(&packet).await;
            if state == LoginState::Verified {
                result?;
                assert_eq!(read_lines_capture(&mut peer, 1).await, packet.as_bytes());
            } else {
                assert!(
                    matches!(result, Err(AprsIsError::UnverifiedConnection { state: actual }) if actual == state),
                    "{result:?}"
                );
                client.send_keepalive().await?;
                assert_eq!(
                    read_lines_capture(&mut peer, 1).await,
                    b"# aprs-is keepalive\r\n"
                );
            }
        }
        Ok(())
    }

    async fn cancel_pending<F: Future>(future: F) -> TestResult {
        let mut future = std::pin::pin!(future);
        std::future::poll_fn(|cx| match future.as_mut().poll(cx) {
            std::task::Poll::Pending => std::task::Poll::Ready(Ok(())),
            std::task::Poll::Ready(_) => std::task::Poll::Ready(Err(
                "operation completed before its cancellation boundary".into(),
            )),
        })
        .await
    }

    #[tokio::test]
    async fn cancelled_reads_retain_prefix_and_oversized_line_state() -> TestResult {
        for oversized in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let config = test_config(listener.local_addr()?);
            let (client, accepted) = tokio::join!(AprsIsClient::connect(config), listener.accept());
            let mut client = client?;
            let (mut peer, _) = accepted?;
            let prefix = if oversized {
                vec![b'X'; MAX_IS_READ_LINE_BYTES + 17]
            } else {
                b"N0CALL>APRS:retained".to_vec()
            };
            peer.write_all(&prefix).await?;
            let _ = client.reader.fill_buf().await?;
            cancel_pending(client.next_event()).await?;
            assert_eq!(
                client.line_buf.len(),
                prefix.len().min(MAX_IS_READ_LINE_BYTES)
            );
            assert_eq!(client.line_overflowed, oversized);
            peer.write_all(b" suffix\r\nN1CALL>APRS:next\r\n").await?;
            let resumed = client.next_event().await;
            if oversized {
                assert!(
                    matches!(resumed, Err(AprsIsError::ReadLineTooLong { .. })),
                    "{resumed:?}"
                );
            } else {
                let AprsIsEvent::Packet(packet) = resumed? else {
                    return Err("resumed ordinary line was not a packet".into());
                };
                assert_eq!(packet.raw(), b"N0CALL>APRS:retained suffix");
            }
            let AprsIsEvent::Packet(packet) = client.next_event().await? else {
                return Err("reader did not resume at the following packet".into());
            };
            assert_eq!(packet.raw(), b"N1CALL>APRS:next");
        }
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_oversized_eof_stays_rejected() -> TestResult {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let config = test_config(listener.local_addr()?);
        let (client, accepted) = tokio::join!(AprsIsClient::connect(config), listener.accept());
        let mut client = client?;
        let (mut peer, _) = accepted?;
        peer.write_all(&vec![b'X'; MAX_IS_READ_LINE_BYTES + 17])
            .await?;
        let _ = client.reader.fill_buf().await?;
        cancel_pending(client.next_event()).await?;
        assert!(client.line_overflowed);
        peer.shutdown().await?;
        let result = client.next_event().await;
        assert!(
            matches!(result, Err(AprsIsError::ReadLineTooLong { .. })),
            "{result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancelling_a_blocked_send_retires_the_outgoing_stream() -> TestResult {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let config = test_config(listener.local_addr()?);
        let (client, accepted) = tokio::join!(AprsIsClient::connect(config), listener.accept());
        let mut client = client?;
        let (_peer, _) = accepted?;
        let writer = client.writer.as_ref().ok_or("missing initial write half")?;
        let mut blocked = false;
        let fill = vec![b'X'; 65_536];
        for _ in 0..512 {
            match writer.try_write(&fill) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    blocked = true;
                    break;
                }
                Err(error) => return Err(error.into()),
            }
        }
        assert!(
            blocked,
            "loopback peer did not apply send-buffer backpressure"
        );
        cancel_pending(client.send_keepalive()).await?;
        assert!(
            client.writer.is_none(),
            "cancelled write half remained reusable"
        );
        let result = client.send_keepalive().await;
        assert!(
            matches!(result, Err(AprsIsError::ConnectionRetired)),
            "{result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn connect_sends_login_string() -> TestResult {
        let (tx, rx) = oneshot::channel::<Vec<u8>>();
        let addr = spawn_mock_server(move |mut stream| async move {
            let captured = read_lines_capture(&mut stream, 1).await;
            drop(tx.send(captured));
        })
        .await?;

        let _client = AprsIsClient::connect(test_config(addr)).await?;

        let login = recv_captured(rx).await?;
        assert!(
            login.starts_with("user N0CALL pass -1 vers test 0.1"),
            "unexpected login: {login:?}"
        );
        assert!(login.ends_with("\r\n"), "missing CRLF: {login:?}");
        Ok(())
    }

    #[tokio::test]
    async fn next_event_receives_packet_line() -> TestResult {
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0u8; 512];
            let _ = read_some(&mut stream, &mut buf).await;
            write_all_ignore(&mut stream, b"N0CALL>APK005:!4903.50N/07201.75W-Test\r\n").await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        })
        .await?;

        let mut client = AprsIsClient::connect(test_config(addr)).await?;
        let event = client.next_event().await?;
        let AprsIsEvent::Packet(ref pkt) = event else {
            return Err(format!("expected Packet, got {event:?}").into());
        };
        assert_eq!(pkt.line(), "N0CALL>APK005:!4903.50N/07201.75W-Test");
        assert_eq!(pkt.raw(), b"N0CALL>APK005:!4903.50N/07201.75W-Test");
        Ok(())
    }

    #[tokio::test]
    async fn next_event_survives_non_utf8_in_packet() -> TestResult {
        // Mic-E and raw weather packets routinely carry bytes ≥ 0x80.
        // The lossy display view replaces invalid UTF-8 with U+FFFD,
        // while `raw` preserves the original bytes exactly.
        let payload: &[u8] = &[
            b'N', b'0', b'C', b'A', b'L', b'L', b'>', b'A', b'P', b'K', b'0', b'0', b'5', b':',
            b'`', // Mic-E type byte
            0xC1, 0x82, 0x91, // bytes that are illegal as a UTF-8 sequence
            b'\r', b'\n',
        ];
        let payload_owned: Vec<u8> = payload.to_vec();
        let addr = spawn_mock_server(move |mut stream| async move {
            let mut buf = [0u8; 512];
            let _ = read_some(&mut stream, &mut buf).await;
            write_all_ignore(&mut stream, &payload_owned).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        })
        .await?;

        let mut client = AprsIsClient::connect(test_config(addr)).await?;
        let event = client.next_event().await?;
        let AprsIsEvent::Packet(ref pkt) = event else {
            return Err(format!("expected Packet, got {event:?}").into());
        };
        // Raw bytes are byte-identical (minus the CRLF that the reader
        // strips on every line).
        let expected_raw = payload
            .get(..payload.len().saturating_sub(2))
            .ok_or("payload too short")?;
        assert_eq!(pkt.raw(), expected_raw);
        // The lossy view contains U+FFFD replacement characters for the
        // invalid UTF-8 bytes but is otherwise the original ASCII.
        assert!(
            pkt.line().contains('\u{FFFD}'),
            "expected U+FFFD in lossy view, got {:?}",
            pkt.line()
        );
        assert!(
            pkt.line().starts_with("N0CALL>APK005:`"),
            "ASCII prefix should survive: {:?}",
            pkt.line()
        );
        Ok(())
    }

    #[tokio::test]
    async fn next_event_receives_comment_line() -> TestResult {
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0u8; 512];
            let _ = read_some(&mut stream, &mut buf).await;
            write_all_ignore(&mut stream, b"# javAPRSSrvr 4.2.0b05\r\n").await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        })
        .await?;

        let mut client = AprsIsClient::connect(test_config(addr)).await?;
        let event = client.next_event().await?;
        assert!(
            matches!(event, AprsIsEvent::Comment(ref line) if line == "# javAPRSSrvr 4.2.0b05"),
            "expected Comment, got {event:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn next_event_detects_login_verified() -> TestResult {
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0u8; 512];
            let _ = read_some(&mut stream, &mut buf).await;
            write_all_ignore(&mut stream, b"# logresp N0CALL verified, server T2TEST\r\n").await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        })
        .await?;

        let mut client = AprsIsClient::connect(test_config(addr)).await?;
        let event = client.next_event().await?;
        assert!(
            matches!(event, AprsIsEvent::LoggedIn { ref server } if server.as_deref() == Some("T2TEST")),
            "expected LoggedIn, got {event:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn next_event_detects_login_rejected() -> TestResult {
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0u8; 512];
            let _ = read_some(&mut stream, &mut buf).await;
            write_all_ignore(
                &mut stream,
                b"# logresp N0CALL unverified, server T2TEST\r\n",
            )
            .await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        })
        .await?;

        // A login that SENT a real passcode and got "unverified" back
        // was genuinely rejected (wrong passcode).
        let mut config = test_config(addr);
        config.passcode = Passcode::for_callsign("N0CALL");
        let mut client = AprsIsClient::connect(config).await?;
        let event = client.next_event().await?;
        assert!(
            matches!(event, AprsIsEvent::LoginRejected { ref reason } if reason.response().contains("unverified")),
            "expected LoginRejected, got {event:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn next_event_accepts_unverified_for_receive_only_login() -> TestResult {
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0u8; 512];
            let _ = read_some(&mut stream, &mut buf).await;
            write_all_ignore(
                &mut stream,
                b"# logresp N0CALL unverified, server T2TEST\r\n",
            )
            .await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        })
        .await?;

        // "unverified" is the EXPECTED success response to a
        // receive-only (passcode -1) login: the server keeps serving
        // the filtered feed, it just will not gate our packets to RF.
        let mut client = AprsIsClient::connect(test_config(addr)).await?;
        let event = client.next_event().await?;
        assert!(
            matches!(event, AprsIsEvent::LoggedIn { ref server } if server.as_deref() == Some("T2TEST")),
            "expected LoggedIn for receive-only unverified, got {event:?}"
        );
        Ok(())
    }

    #[test]
    fn login_responses_require_an_exact_prefix_status_and_matching_identity() {
        assert_eq!(
            parse_logresp("# logresp n0call verified, server T2TEST", "N0CALL"),
            Some((true, Some("T2TEST")))
        );
        assert_eq!(
            parse_logresp("# logresp N0CALL unverified, server  T2A ", "N0CALL"),
            Some((false, Some("T2A")))
        );
        for invalid in [
            "# javAPRSSrvr logresp N0CALL verified, server T2TEST",
            "# logresp N1CALL verified, server T2TEST",
            "# logresp N0CALL-14 verified, server T2TEST",
            "# logresp N0CALL notverified, server T2TEST",
            "# logresp N0CALL invalid, verified, server T2TEST",
            "logresp N0CALL verified, server T2TEST",
        ] {
            assert_eq!(parse_logresp(invalid, "N0CALL"), None, "{invalid:?}");
        }
    }

    #[tokio::test]
    async fn unrelated_login_comments_do_not_consume_the_login_response() -> TestResult {
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0_u8; 512];
            let _ = read_some(&mut stream, &mut buf).await;
            write_all_ignore(&mut stream,
                b"# logresp N1CALL verified, server T2OTHER\r\n# banner logresp N0CALL verified, server T2OTHER\r\n# logresp N0CALL verified, server T2TEST\r\n").await;
        }).await?;
        let mut client = AprsIsClient::connect(test_config(addr)).await?;
        assert!(matches!(
            client.next_event().await?,
            AprsIsEvent::Comment(_)
        ));
        assert!(matches!(
            client.next_event().await?,
            AprsIsEvent::Comment(_)
        ));
        let event = client.next_event().await?;
        assert!(
            matches!(event, AprsIsEvent::LoggedIn { server } if server.as_deref() == Some("T2TEST"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn next_event_detects_disconnect() -> TestResult {
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0u8; 512];
            let _ = read_some(&mut stream, &mut buf).await;
            drop(stream);
        })
        .await?;

        let mut client = AprsIsClient::connect(test_config(addr)).await?;
        let event = client.next_event().await?;
        assert!(
            matches!(event, AprsIsEvent::Disconnected),
            "expected Disconnected, got {event:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn send_packet_formats_line() -> TestResult {
        let (tx, rx) = oneshot::channel::<Vec<u8>>();
        let addr = spawn_verified_server(move |mut stream| async move {
            // Login line + packet line.
            let captured = read_lines_capture(&mut stream, 2).await;
            drop(tx.send(captured));
        })
        .await?;

        let mut client = connect_verified(addr).await?;
        client
            .send_packet("N0CALL", "APK005", &["WIDE1-1"], "!4903.50N/07201.75W-Test")
            .await?;

        let wire = recv_captured(rx).await?;
        let mut lines = wire.split_inclusive('\n');
        let login = lines.next().ok_or("no login line captured")?;
        assert!(login.contains("user N0CALL"), "login missing: {login:?}");
        let pkt = lines.next().ok_or("no packet line captured")?;
        assert_eq!(
            pkt, "N0CALL>APK005,WIDE1-1:!4903.50N/07201.75W-Test\r\n",
            "unexpected packet: {pkt:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn send_keepalive_sends_comment_line() -> TestResult {
        let (tx, rx) = oneshot::channel::<Vec<u8>>();
        let addr = spawn_mock_server(move |mut stream| async move {
            // Login line + keepalive line.
            let captured = read_lines_capture(&mut stream, 2).await;
            drop(tx.send(captured));
        })
        .await?;

        let mut client = AprsIsClient::connect(test_config(addr)).await?;
        client.send_keepalive().await?;

        let wire = recv_captured(rx).await?;
        let mut lines = wire.split_inclusive('\n');
        let login = lines.next().ok_or("no login line captured")?;
        assert!(
            login.starts_with("user N0CALL"),
            "unexpected login: {login:?}"
        );
        let ka = lines.next().ok_or("no keepalive line captured")?;
        assert!(
            ka.starts_with("# aprs-is keepalive"),
            "unexpected keepalive: {ka:?}"
        );
        assert!(ka.ends_with("\r\n"), "missing CRLF: {ka:?}");
        Ok(())
    }

    #[tokio::test]
    async fn maybe_send_keepalive_noop_when_recent() -> TestResult {
        let (tx, rx) = oneshot::channel::<Vec<u8>>();
        let addr = spawn_mock_server(move |mut stream| async move {
            // Capture everything until the client closes the connection.
            let captured = read_lines_capture(&mut stream, usize::MAX).await;
            drop(tx.send(captured));
        })
        .await?;

        let mut client = AprsIsClient::connect(test_config(addr)).await?;
        // Called immediately after connect: last_write is fresh, no send.
        client.maybe_send_keepalive().await?;
        // Close the socket so the capture ends at EOF, then prove the
        // login line is the ONLY thing that went on the wire.
        drop(client);

        let wire = recv_captured(rx).await?;
        assert!(
            wire.starts_with("user N0CALL"),
            "expected the login line on the wire, got {wire:?}"
        );
        assert_eq!(
            wire.matches('\n').count(),
            1,
            "expected nothing after the login line, got {wire:?}"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn refused_loopback_connection_obeys_attempt_limit() -> TestResult {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let config = test_config(listener.local_addr()?);
        drop(listener);
        let started = tokio::time::Instant::now();
        let result = AprsIsClient::connect_with_retry(config, NonZeroU32::new(3)).await;
        assert!(matches!(result, Err(AprsIsError::Connect(_))), "{result:?}");
        assert!(tokio::time::Instant::now() - started >= Duration::from_secs(3));
        Ok(())
    }

    #[tokio::test]
    async fn read_is_line_reads_a_normal_line() -> TestResult {
        let data: &[u8] = b"N0CALL>APK005:hello\r\nnext line\n";
        let mut reader = BufReader::new(data);
        let mut buf = Vec::new();
        let n = read_is_line(&mut reader, &mut buf, &mut false, MAX_IS_READ_LINE_BYTES).await?;
        assert_eq!(n, buf.len());
        assert_eq!(buf.as_slice(), b"N0CALL>APK005:hello\r\n");
        // The next line is still readable (consume positioned correctly).
        buf.clear();
        let _ = read_is_line(&mut reader, &mut buf, &mut false, MAX_IS_READ_LINE_BYTES).await?;
        assert_eq!(buf.as_slice(), b"next line\n");
        Ok(())
    }

    #[tokio::test]
    async fn read_is_line_accepts_long_but_valid_line() -> TestResult {
        // 600 bytes of payload + CRLF: under the 1024 cap, must parse in
        // full without truncation.
        let mut data = vec![b'X'; 600];
        data.extend_from_slice(b"\r\n");
        let mut reader = BufReader::new(data.as_slice());
        let mut buf = Vec::new();
        let n = read_is_line(&mut reader, &mut buf, &mut false, MAX_IS_READ_LINE_BYTES).await?;
        assert_eq!(n, 602, "long valid line should be returned whole");
        assert_eq!(buf.len(), 602);
        Ok(())
    }

    #[tokio::test]
    async fn read_is_line_rejects_huge_no_newline_chunk_at_eof() -> TestResult {
        // A server streams far more than the cap with NO newline, then
        // EOF. The buffer must stay bounded by the cap rather than
        // growing to the input size (the OOM vector).
        let data = vec![b'A'; MAX_IS_READ_LINE_BYTES * 64];
        let mut reader = BufReader::new(data.as_slice());
        let mut buf = Vec::new();
        let result = read_is_line(&mut reader, &mut buf, &mut false, MAX_IS_READ_LINE_BYTES).await;
        assert!(
            buf.len() <= MAX_IS_READ_LINE_BYTES,
            "buffer grew past the cap: {} > {}",
            buf.len(),
            MAX_IS_READ_LINE_BYTES
        );
        assert!(
            matches!(
                result,
                Err(AprsIsError::ReadLineTooLong { max }) if max == MAX_IS_READ_LINE_BYTES
            ),
            "expected oversized EOF error, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn read_is_line_accepts_exact_cap_at_eof() -> TestResult {
        let data = vec![b'A'; MAX_IS_READ_LINE_BYTES];
        let mut reader = BufReader::new(data.as_slice());
        let mut buf = Vec::new();
        let n = read_is_line(&mut reader, &mut buf, &mut false, MAX_IS_READ_LINE_BYTES).await?;
        assert_eq!(n, MAX_IS_READ_LINE_BYTES);
        assert_eq!(buf, data);
        Ok(())
    }

    #[tokio::test]
    async fn read_is_line_errors_then_resyncs_on_overflow() -> TestResult {
        // A line that exceeds the cap *and* is eventually terminated must
        // return ReadLineTooLong (not a silent truncate-then-misparse),
        // leave the buffer bounded, and resync so the *next* line parses.
        let mut data = vec![b'B'; MAX_IS_READ_LINE_BYTES + 50];
        data.extend_from_slice(b"\r\n");
        data.extend_from_slice(b"N0CALL>APK005:ok\r\n");
        let mut reader = BufReader::new(data.as_slice());

        let mut buf = Vec::new();
        let first = read_is_line(&mut reader, &mut buf, &mut false, MAX_IS_READ_LINE_BYTES).await;
        assert!(
            matches!(first, Err(AprsIsError::ReadLineTooLong { max }) if max == MAX_IS_READ_LINE_BYTES),
            "expected ReadLineTooLong, got {first:?}"
        );
        assert!(
            buf.len() <= MAX_IS_READ_LINE_BYTES,
            "overflow buffer not bounded: {}",
            buf.len()
        );

        // Resync: the following well-formed line is delivered intact.
        buf.clear();
        let _ = read_is_line(&mut reader, &mut buf, &mut false, MAX_IS_READ_LINE_BYTES).await?;
        assert_eq!(buf.as_slice(), b"N0CALL>APK005:ok\r\n");
        Ok(())
    }

    #[tokio::test]
    async fn send_packet_rejects_crlf_injection_in_data() -> TestResult {
        // The mock server records every byte it receives so we can prove
        // no forged second line is written.
        let (tx, rx) = oneshot::channel::<Vec<u8>>();
        let addr = spawn_mock_server(move |mut stream| async move {
            let captured = read_lines_capture(&mut stream, usize::MAX).await;
            drop(tx.send(captured));
        })
        .await?;

        let mut client = AprsIsClient::connect(test_config(addr)).await?;
        // A data field carrying a forged second packet must be rejected
        // at the send choke point.
        let result = client
            .send_packet("N0CALL", "APK005", &[], "a\r\nN0CALL>X:forged")
            .await;
        assert!(
            matches!(
                result,
                Err(AprsIsError::InvalidUplinkLine(
                    crate::AprsIsUplinkLineError::EmbeddedNewline { .. }
                ))
            ),
            "expected an embedded-newline validation error, got {result:?}"
        );
        // Close the socket so the capture ends at EOF, then prove the
        // login line is the ONLY thing that went on the wire; the
        // rejected send must not have written anything.
        drop(client);

        let wire = recv_captured(rx).await?;
        assert!(
            wire.starts_with("user N0CALL"),
            "expected the login line on the wire, got {wire:?}"
        );
        assert_eq!(
            wire.matches('\n').count(),
            1,
            "rejected send leaked bytes onto the wire: {wire:?}"
        );
        assert!(
            !wire.contains("forged"),
            "forged line reached the wire: {wire:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn send_raw_line_rejects_embedded_newline_but_allows_crlf() -> TestResult {
        let addr = spawn_verified_server(|mut stream| async move {
            let mut buf = [0u8; 1024];
            let _ = read_some(&mut stream, &mut buf).await;
            tokio::time::sleep(Duration::from_millis(80)).await;
        })
        .await?;

        let mut client = connect_verified(addr).await?;
        // Embedded newline in the body is rejected.
        let injected = client.send_raw_line("good\r\nEVIL>X:forged\r\n").await;
        assert!(
            matches!(
                injected,
                Err(AprsIsError::InvalidUplinkLine(
                    crate::AprsIsUplinkLineError::EmbeddedNewline { .. }
                ))
            ),
            "expected an embedded-newline validation error, got {injected:?}"
        );
        // A normal CRLF-terminated line is accepted.
        client.send_raw_line("N0CALL>APK005:ok\r\n").await?;
        Ok(())
    }

    #[tokio::test]
    async fn send_raw_bytes_preserves_non_utf8_wire_bytes() -> TestResult {
        let (tx, rx) = oneshot::channel::<Vec<u8>>();
        let addr = spawn_verified_server(move |mut stream| async move {
            let captured = read_lines_capture(&mut stream, 2).await;
            drop(tx.send(captured));
        })
        .await?;

        let mut client = connect_verified(addr).await?;
        let packet: &[u8] = b"N0CALL>APK005:`\xC1\x82\x91\r\n";
        client.send_raw_bytes(packet).await?;

        let wire = recv_captured_bytes(rx).await?;
        let packet_start = wire
            .iter()
            .position(|&byte| byte == b'\n')
            .map(|offset| offset + 1)
            .ok_or("captured login had no newline")?;
        assert_eq!(wire.get(packet_start..), Some(packet));
        Ok(())
    }

    #[tokio::test]
    async fn send_raw_bytes_requires_crlf_and_enforces_wire_limit() -> TestResult {
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0u8; 1024];
            let _ = read_some(&mut stream, &mut buf).await;
            tokio::time::sleep(Duration::from_millis(80)).await;
        })
        .await?;

        let mut client = AprsIsClient::connect(test_config(addr)).await?;
        for invalid in [
            b"N0CALL>APK005:test".as_slice(),
            b"N0CALL>APK005:test\n".as_slice(),
        ] {
            assert!(matches!(
                client.send_raw_bytes(invalid).await,
                Err(AprsIsError::InvalidUplinkLine(
                    crate::AprsIsUplinkLineError::MissingCrlf
                ))
            ));
        }

        let mut oversized = vec![b'A'; crate::MAX_IS_LINE_BYTES - 1];
        oversized.extend_from_slice(b"\r\n");
        assert!(matches!(
            client.send_raw_bytes(&oversized).await,
            Err(AprsIsError::InvalidUplinkLine(
                crate::AprsIsUplinkLineError::TooLong { .. }
            ))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn send_packet_normal_still_sends() -> TestResult {
        let (tx, rx) = oneshot::channel::<Vec<u8>>();
        let addr = spawn_verified_server(move |mut stream| async move {
            // Login line + packet line.
            let captured = read_lines_capture(&mut stream, 2).await;
            drop(tx.send(captured));
        })
        .await?;

        let mut client = connect_verified(addr).await?;
        client
            .send_packet("N0CALL", "APK005", &[], "!4903.50N/07201.75W-Test")
            .await?;

        let wire = recv_captured(rx).await?;
        let mut lines = wire.split_inclusive('\n');
        let login = lines.next().ok_or("no login line captured")?;
        assert!(
            login.starts_with("user N0CALL"),
            "unexpected login: {login:?}"
        );
        let pkt = lines.next().ok_or("no packet line captured")?;
        assert_eq!(
            pkt, "N0CALL>APK005:!4903.50N/07201.75W-Test\r\n",
            "unexpected packet: {pkt:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn connect_rejects_invalid_login_field() -> TestResult {
        // A software_name with a space must fail before any socket work.
        let mut config = AprsIsConfig::new("N0CALL")?;
        config.software_name = "my app".to_owned();
        let result = AprsIsClient::connect(config).await;
        assert!(
            matches!(
                result,
                Err(AprsIsError::InvalidLoginField {
                    field: crate::LoginField::SoftwareName,
                    ..
                })
            ),
            "expected InvalidLoginField(software_name), got {:?}",
            result.map(|_| "Ok(client)")
        );
        Ok(())
    }

    #[tokio::test]
    async fn connect_rejects_oversized_login_before_opening_socket() -> TestResult {
        let mut config = AprsIsConfig::new("N0CALL")?;
        config.filter = "A".repeat(crate::MAX_IS_LINE_BYTES);

        let result = AprsIsClient::connect(config).await;
        assert!(matches!(
            result,
            Err(AprsIsError::InvalidUplinkLine(
                crate::AprsIsUplinkLineError::TooLong { .. }
            ))
        ));
        Ok(())
    }
}
