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
//! let mut config = AprsIsConfig::new("N0CALL");
//! config.filter = "r/35.25/-97.75/100".to_owned();
//! let mut client = AprsIsClient::connect(config).await?;
//!
//! loop {
//!     match client.next_event().await? {
//!         AprsIsEvent::Packet(pkt) => println!("Got: {}", pkt.line),
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
//! APRS-IS expects a comment line every ~2 minutes if the client is
//! otherwise idle. Call [`AprsIsClient::send_keepalive`] on a timer, or
//! use [`AprsIsClient::maybe_send_keepalive`] which only sends if the
//! keepalive interval has elapsed since the last write.
//!
//! # Reconnection
//!
//! On [`AprsIsEvent::Disconnected`], call [`AprsIsClient::reconnect`] to
//! re-establish the TCP connection and re-login. Callers typically wrap
//! this in an exponential backoff loop.

use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use crate::error::AprsIsError;
use crate::events::{AprsIsEvent, AprsIsPacket};
use crate::line::{format_is_packet, parse_is_line};
use crate::login::{AprsIsConfig, build_login_string};

/// Extract the server hostname from a `# logresp ... verified, server X`
/// comment line. Returns `None` if the `server` clause is absent.
fn parse_logresp_server(line: &str) -> Option<String> {
    let idx = line.find("server ")?;
    let rest = line.get(idx + "server ".len()..)?;
    // Skip any extra whitespace after "server" and take the next
    // whitespace-delimited token.
    let name = rest
        .split_whitespace()
        .next()
        .map(|s| s.trim_matches(',').to_owned())?;
    if name.is_empty() { None } else { Some(name) }
}

/// Default APRS-IS keepalive interval.
///
/// APRS-IS servers expect the client to send something (a packet or a
/// comment line) at least every 2 minutes or they may disconnect.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(120);

/// Default connect timeout for the initial TCP handshake + login.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum APRS-IS line length on the wire, in bytes — **including**
/// the trailing `\r\n`.
///
/// Per <http://www.aprs-is.net/Connecting.aspx>: "No line may exceed
/// 512 bytes including the CR/LF sequence." Servers silently truncate
/// lines that exceed this limit; rejecting the send up-front turns the
/// truncation into a visible error.
pub const MAX_IS_LINE_BYTES: usize = 512;

/// Maximum length, in bytes, of a single inbound line read from the
/// server before the reader gives up and resynchronises.
///
/// The client connects to arbitrary internet servers, so a malicious or
/// buggy peer can stream bytes without ever sending a `\n`; an unbounded
/// `read_until(b'\n', ..)` would grow the line buffer until the process
/// runs out of memory (a denial-of-service). This cap bounds the buffer.
///
/// The value is double [`MAX_IS_LINE_BYTES`] (the *outbound* 512-byte
/// spec limit including `CRLF`). APRS-IS payloads sit well under 512
/// bytes, but inbound lines can legitimately carry a verbose server
/// comment or a long path, so the doubled headroom avoids rejecting any
/// valid line while still capping memory at a fixed, small bound. A line
/// that reaches this length without a terminating newline yields
/// [`AprsIsError::ReadLineTooLong`].
pub const MAX_IS_READ_LINE_BYTES: usize = 1024;

/// Keepalive comment text (sent as `# aprs-is keepalive\r\n`).
const KEEPALIVE_COMMENT: &str = "# aprs-is keepalive";

/// Read one newline-terminated line from `reader` into `buf`, bounding
/// the line length at `max` bytes.
///
/// On success returns `Ok(n)` where `n` is the number of bytes appended
/// to `buf` (including the terminating `\n`, if any). `Ok(0)` signals
/// clean EOF. `buf` is **not** cleared — the caller clears it.
///
/// Unlike [`AsyncBufReadExt::read_until`], this caps the amount of data
/// buffered for a single line: if `max` bytes accumulate without a `\n`,
/// the function drains and discards the rest of the oversized line up to
/// (and including) the next newline so the stream stays framed, then
/// returns [`AprsIsError::ReadLineTooLong`]. This prevents a server that
/// never sends a newline from growing `buf` without bound.
async fn read_is_line<R>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
) -> Result<usize, AprsIsError>
where
    R: AsyncBufReadExt + Unpin,
{
    let mut copied = 0usize;
    loop {
        let chunk = reader.fill_buf().await.map_err(AprsIsError::Read)?;
        if chunk.is_empty() {
            // EOF. Anything already copied is an unterminated final line;
            // surface what we have so the caller can parse it (or detect
            // a clean `Ok(0)` close).
            return Ok(copied);
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
            let fits = copied.saturating_add(upto.len()) <= max;
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
        let consume_n = chunk.len();
        reader.consume(consume_n);
        // `copied` never exceeds `max`; once it reaches `max` we keep
        // draining subsequent chunks (take == 0) until the newline, then
        // return ReadLineTooLong above.
    }
}

/// `true` if `line` has a `\r` or `\n` anywhere except as a single
/// trailing line terminator.
///
/// APRS-IS frames each line with a trailing `CRLF`; a `\r`/`\n` embedded
/// in the body would split one write into two lines, the second forged.
/// Callers pass lines that already carry their own terminator (`\n` or
/// `\r\n`), so this strips at most one trailing terminator and then
/// checks the remaining body. Examples:
/// - `"data\r\n"` → body `"data"` → `false` (allowed).
/// - `"data\n"` → body `"data"` → `false` (allowed).
/// - `"data"` → body `"data"` → `false` (allowed, unterminated).
/// - `"a\r\nN0CALL>X:forged\r\n"` → body `"a\r\nN0CALL>X:forged"`
///   contains an embedded `\r\n` → `true` (rejected).
fn line_body_has_embedded_newline(line: &str) -> bool {
    // Strip a single trailing terminator: an optional `\n`, then an
    // optional `\r` that immediately preceded it.
    let body = line.strip_suffix('\n').unwrap_or(line);
    let body = body.strip_suffix('\r').unwrap_or(body);
    body.contains('\r') || body.contains('\n')
}

/// Async TCP client for APRS-IS.
///
/// Owns a single TCP connection to an APRS-IS server, handles the login
/// handshake, and exposes line-at-a-time read/write methods.
///
/// Not `Clone` and not `Send`-across-the-await — typical usage is to own
/// it from a single task.
///
/// # TLS support
///
/// This client speaks plaintext TCP only. APRS-IS T2 servers also
/// support TLS on port 24580 — to use it, build the connection
/// yourself with your preferred TLS library (e.g. `tokio-rustls` or
/// `tokio-native-tls`) and use the line-level helpers at the crate
/// root ([`crate::build_login_string`], [`crate::format_is_packet`],
/// [`crate::AprsIsLine`]):
///
/// ```no_run
/// use aprs_is::{AprsIsConfig, AprsIsLine, build_login_string, format_is_packet};
/// // 1. TLS handshake against `core.aprs2.net:24580` using your TLS library.
/// // 2. Send the result of `build_login_string(&config)` over the stream.
/// // 3. Read lines from the stream and parse them with `AprsIsLine::parse`.
/// // 4. Send packets formatted via `format_is_packet`.
/// # let _ = (AprsIsConfig::new("N0CALL"), build_login_string, format_is_packet,
/// #     |line: &str| AprsIsLine::parse(line));
/// ```
///
/// The library deliberately does not bundle a TLS implementation so
/// callers can choose their preferred backend.
#[derive(Debug)]
pub struct AprsIsClient {
    config: AprsIsConfig,
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    /// Persistent byte buffer for `read_until`. We deliberately read
    /// bytes (not `String` via `read_line`) so non-UTF-8 sequences in
    /// APRS info fields (Mic-E, raw weather, Latin-1 comments) do not
    /// surface as `io::ErrorKind::InvalidData` and tear down the
    /// long-lived TCP session. The bytes are decoded via
    /// `String::from_utf8_lossy` at the event boundary; callers that
    /// need byte-exact fidelity get the raw bytes in
    /// [`AprsIsPacket::raw`].
    line_buf: Vec<u8>,
    last_write: Instant,
    logged_in_emitted: bool,
}

impl AprsIsClient {
    /// Connect to the APRS-IS server and perform the login handshake.
    ///
    /// Performs TCP connect, sends the login string, and returns as soon
    /// as the socket is writable. Login verification (the `# logresp`
    /// line) is reported asynchronously via [`AprsIsEvent::LoggedIn`]
    /// from [`next_event`](Self::next_event).
    ///
    /// Times out after [`CONNECT_TIMEOUT`] (10 seconds) during TCP connect.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::InvalidLoginField`] if a login field
    /// (callsign / software name / version / filter) is invalid,
    /// [`AprsIsError::Connect`] if TCP connect fails or times out, or
    /// [`AprsIsError::Write`] if the login string cannot be sent.
    pub async fn connect(config: AprsIsConfig) -> Result<Self, AprsIsError> {
        let addr = format!("{}:{}", config.server, config.port);
        tracing::info!(server = %addr, callsign = %config.callsign, "APRS-IS connecting");

        // Build + validate the login line before opening the socket so an
        // invalid field (whitespace/CRLF that could corrupt or inject the
        // handshake) fails fast without a wasted TCP connection.
        let login = build_login_string(&config)?;

        let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr))
            .await
            .map_err(|_| {
                AprsIsError::Connect(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "TCP connect timed out",
                ))
            })?
            .map_err(AprsIsError::Connect)?;

        // Disable Nagle's algorithm per the explicit recommendation at
        // <http://www.aprs-is.net/Connecting.aspx>: "If your client
        // software is bidirectional (sends and receives), turn off the
        // Nagle algorithm when connecting to APRS-IS as it can
        // introduce significant delays (TCP_NODELAY)." APRS lines are
        // short and latency-sensitive (ack/rej + IGate forwarding);
        // leaving Nagle on would batch a single-frame send with the
        // following keepalive write, adding hundreds of milliseconds
        // round-trip.
        stream.set_nodelay(true).map_err(AprsIsError::Connect)?;

        let (read_half, mut write_half) = stream.into_split();

        // Send the login string built+validated above.
        write_half
            .write_all(login.as_bytes())
            .await
            .map_err(AprsIsError::Write)?;
        write_half.flush().await.map_err(AprsIsError::Write)?;

        tracing::debug!("APRS-IS login sent");

        Ok(Self {
            config,
            reader: BufReader::new(read_half),
            writer: write_half,
            line_buf: Vec::with_capacity(512),
            last_write: Instant::now(),
            logged_in_emitted: false,
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
        max_attempts: Option<u32>,
    ) -> Result<Self, AprsIsError> {
        let mut delay = Duration::from_secs(1);
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            match Self::connect(config.clone()).await {
                Ok(client) => return Ok(client),
                Err(e) => {
                    if max_attempts.is_some_and(|max| attempt >= max) {
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
    /// Drops the current connection (if any) and performs a fresh
    /// connect + login. Preserves the configuration.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::Connect`] if the TCP connect fails or
    /// [`AprsIsError::Write`] if the login string cannot be sent.
    pub async fn reconnect(&mut self) -> Result<(), AprsIsError> {
        tracing::info!("APRS-IS reconnecting");
        let new = Self::connect(self.config.clone()).await?;
        self.reader = new.reader;
        self.writer = new.writer;
        self.line_buf.clear();
        self.last_write = new.last_write;
        self.logged_in_emitted = false;
        Ok(())
    }

    /// Read the next event from the server.
    ///
    /// Returns when a complete line arrives or the connection closes.
    /// This is a blocking read — wrap in a `tokio::select!` with a
    /// keepalive timer if you need concurrency.
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
        self.line_buf.clear();
        // Bounded read: a malicious/buggy server could otherwise stream
        // bytes with no `\n` and grow `line_buf` until OOM. See
        // `read_is_line` and `MAX_IS_READ_LINE_BYTES`. On overflow the
        // helper drains the oversized line up to the next newline and
        // returns `ReadLineTooLong`, leaving the stream resynchronised.
        let bytes =
            read_is_line(&mut self.reader, &mut self.line_buf, MAX_IS_READ_LINE_BYTES).await?;

        if bytes == 0 {
            tracing::info!("APRS-IS connection closed by server");
            return Ok(AprsIsEvent::Disconnected);
        }

        // Strip CRLF / LF / lone CR from the tail. `raw_len` is bounded
        // by the buffer length by construction (rposition can only
        // return an in-bounds index), so the `get(..raw_len)` lookup is
        // guaranteed to return `Some`. We still go through `get` to
        // satisfy `-D clippy::indexing-slicing` and document the
        // invariant explicitly.
        let raw_len = self
            .line_buf
            .iter()
            .rposition(|&b| b != b'\r' && b != b'\n')
            .map_or(0, |i| i + 1);
        let raw_bytes = self.line_buf.get(..raw_len).unwrap_or_else(|| {
            unreachable!("raw_len is bounded by line_buf.len() by construction")
        });

        // Lossy decode — every non-UTF-8 byte becomes U+FFFD. Used for
        // line-level parsing (TNC2 monitor format) and the human-
        // readable view; the raw bytes are kept separately for the
        // packet event so wire-truth is recoverable downstream.
        let line_str = String::from_utf8_lossy(raw_bytes);
        let line: &str = line_str.as_ref();

        if let Some(packet) = parse_is_line(line) {
            return Ok(AprsIsEvent::Packet(AprsIsPacket {
                raw: raw_bytes.to_vec(),
                line: packet.to_owned(),
            }));
        }

        // Comment line. Check for login response on first one.
        if !self.logged_in_emitted && line.contains("logresp") {
            // The verified response has the form
            //   "# logresp CALL verified, server T2FOO"
            // and the rejected response has
            //   "# logresp CALL unverified, ..."
            // We have to check `unverified` before `verified` because the
            // latter is a substring of the former.
            if line.contains("unverified") {
                self.logged_in_emitted = true;
                tracing::warn!(response = %line, "APRS-IS login rejected");
                return Ok(AprsIsEvent::LoginRejected {
                    reason: line.to_owned(),
                });
            }
            if line.contains("verified") {
                self.logged_in_emitted = true;
                let server = parse_logresp_server(line);
                tracing::info!(response = %line, ?server, "APRS-IS login verified");
                return Ok(AprsIsEvent::LoggedIn { server });
            }
        }

        Ok(AprsIsEvent::Comment(line.to_owned()))
    }

    /// Send a formatted APRS packet to the server.
    ///
    /// The packet is formatted as `source>destination,path:data\r\n` via
    /// [`crate::format_is_packet`] and written to the TCP socket.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::Write`] if the write fails.
    pub async fn send_packet(
        &mut self,
        source: &str,
        destination: &str,
        path: &[&str],
        data: &str,
    ) -> Result<(), AprsIsError> {
        let line = format_is_packet(source, destination, path, data);
        self.send_raw_line(&line).await
    }

    /// Send a raw line to the server (must already be CRLF-terminated).
    ///
    /// Use this for custom formatting or to forward packets from RF.
    /// This is the single send choke point: every outbound line — packet
    /// formatting, keepalives, and direct raw forwarding — funnels
    /// through here, so the CR/LF-injection guard below covers all of
    /// them regardless of who built the string.
    ///
    /// # CRLF-injection guard
    ///
    /// APRS-IS lines are framed by a trailing `CRLF`. A `\r` or `\n`
    /// embedded in the **body** of the line (anything before a single
    /// trailing terminator) would split the write into a second, forged
    /// packet on the uplink stream. Because the client may forward
    /// caller- or RF-supplied content (e.g. an AX.25 info field decoded
    /// via `from_utf8_lossy`, which can contain a raw `0x0A`), this
    /// method rejects any line whose body contains an embedded `\r`/`\n`
    /// with [`AprsIsError::EmbeddedNewline`] rather than writing it. A
    /// single trailing terminator (`\n` or `\r\n`) is permitted, since
    /// that is the line's own framing.
    ///
    /// # Length limit
    ///
    /// Per APRS-IS spec at <http://www.aprs-is.net/Connecting.aspx>,
    /// "No line may exceed 512 bytes including the CR/LF sequence."
    /// This method enforces the limit and returns
    /// [`AprsIsError::LineTooLong`] for over-length input rather than
    /// letting the server silently truncate the wire bytes. See
    /// [`MAX_IS_LINE_BYTES`].
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::EmbeddedNewline`] if the line body contains
    /// an embedded `\r`/`\n`; [`AprsIsError::LineTooLong`] if the line
    /// exceeds [`MAX_IS_LINE_BYTES`]; or [`AprsIsError::Write`] if the
    /// underlying socket write fails.
    pub async fn send_raw_line(&mut self, line: &str) -> Result<(), AprsIsError> {
        if line_body_has_embedded_newline(line) {
            return Err(AprsIsError::EmbeddedNewline);
        }
        if line.len() > MAX_IS_LINE_BYTES {
            return Err(AprsIsError::LineTooLong {
                actual: line.len(),
                max: MAX_IS_LINE_BYTES,
            });
        }
        self.writer
            .write_all(line.as_bytes())
            .await
            .map_err(AprsIsError::Write)?;
        self.writer.flush().await.map_err(AprsIsError::Write)?;
        self.last_write = Instant::now();
        Ok(())
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
    /// # Errors
    ///
    /// Returns [`AprsIsError::Write`] if the shutdown flush fails.
    pub async fn shutdown(mut self) -> Result<(), AprsIsError> {
        tracing::debug!("APRS-IS shutting down");
        self.writer.shutdown().await.map_err(AprsIsError::Write)?;
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
    /// test immediately. Decodes the bytes as UTF-8 — everything these
    /// tests put on the wire is ASCII.
    async fn recv_captured(
        rx: oneshot::Receiver<Vec<u8>>,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let bytes = tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .map_err(|_| "mock server handler did not report captured bytes within 5s")??;
        Ok(String::from_utf8(bytes)?)
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
        assert_eq!(pkt.line, "N0CALL>APK005:!4903.50N/07201.75W-Test");
        assert_eq!(pkt.raw, b"N0CALL>APK005:!4903.50N/07201.75W-Test");
        Ok(())
    }

    #[tokio::test]
    async fn next_event_survives_non_utf8_in_packet() -> TestResult {
        // Regression guard: pre-fix, BufReader::read_line
        // returned io::ErrorKind::InvalidData on the first non-UTF-8
        // byte and disconnected the session. Mic-E and raw weather
        // packets routinely carry bytes ≥ 0x80, so a strict UTF-8 read
        // would kill the connection on the first such packet.
        //
        // Post-fix the lossy decoder replaces invalid bytes with
        // U+FFFD in the `line` view, while the original bytes are
        // preserved in `raw`.
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
        assert_eq!(pkt.raw.as_slice(), expected_raw);
        // The lossy view contains U+FFFD replacement characters for the
        // invalid UTF-8 bytes but is otherwise the original ASCII.
        assert!(
            pkt.line.contains('\u{FFFD}'),
            "expected U+FFFD in lossy view, got {:?}",
            pkt.line
        );
        assert!(
            pkt.line.starts_with("N0CALL>APK005:`"),
            "ASCII prefix should survive: {:?}",
            pkt.line
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

        let mut client = AprsIsClient::connect(test_config(addr)).await?;
        let event = client.next_event().await?;
        assert!(
            matches!(event, AprsIsEvent::LoginRejected { ref reason } if reason.contains("unverified")),
            "expected LoginRejected, got {event:?}"
        );
        Ok(())
    }

    #[test]
    fn parse_logresp_server_extracts_name() {
        assert_eq!(
            parse_logresp_server("# logresp N0CALL verified, server T2TEST"),
            Some("T2TEST".to_owned())
        );
        assert_eq!(
            parse_logresp_server("# logresp N0CALL verified, server  T2A "),
            Some("T2A".to_owned())
        );
        assert_eq!(parse_logresp_server("# javAPRSSrvr 4.2.0b05"), None);
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
        let addr = spawn_mock_server(move |mut stream| async move {
            // Login line + packet line.
            let captured = read_lines_capture(&mut stream, 2).await;
            drop(tx.send(captured));
        })
        .await?;

        let mut client = AprsIsClient::connect(test_config(addr)).await?;
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
        // Called immediately after connect — last_write is fresh, no send.
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

    #[tokio::test]
    async fn connect_timeout() -> TestResult {
        // Connect to a non-routable IP to trigger timeout.
        // Using 198.51.100.1 (TEST-NET-2) which should not respond.
        let config = AprsIsConfig {
            callsign: "N0CALL".to_owned(),
            passcode: Passcode::ReceiveOnly,
            server: "198.51.100.1".to_owned(),
            port: 14580,
            filter: String::new(),
            software_name: "test".to_owned(),
            software_version: "0.1".to_owned(),
        };
        // `connect` bounds the TCP handshake at CONNECT_TIMEOUT (10s), so
        // a single attempt must resolve within that plus margin. If the
        // outer timer fires, the client hung past its own deadline —
        // exactly the regression this test guards — and the `?` fails
        // the test.
        let result = tokio::time::timeout(
            CONNECT_TIMEOUT + Duration::from_secs(5),
            AprsIsClient::connect_with_retry(config, Some(1)),
        )
        .await
        .map_err(|_| "connect_with_retry hung past CONNECT_TIMEOUT + margin")?;
        // Within the deadline the attempt must have failed: fast-reject
        // networks yield Connect(unreachable) immediately, SYN-dropping
        // networks yield Connect(TimedOut) at CONNECT_TIMEOUT. Either
        // way, reaching a TEST-NET-2 address is a failure.
        assert!(
            result.is_err(),
            "expected connect to TEST-NET-2 to fail, got Ok"
        );
        Ok(())
    }

    // --- Bounded inbound read (BUG 1: unbounded line-read DoS) ---

    #[test]
    fn line_body_embedded_newline_detection() {
        // Trailing terminators are allowed; embedded CR/LF is not.
        assert!(!line_body_has_embedded_newline("data\r\n"));
        assert!(!line_body_has_embedded_newline("data\n"));
        assert!(!line_body_has_embedded_newline("data"));
        assert!(line_body_has_embedded_newline("a\r\nN0CALL>X:forged\r\n"));
        assert!(line_body_has_embedded_newline("a\nb\n"));
        assert!(line_body_has_embedded_newline("mid\rline\r\n"));
    }

    #[tokio::test]
    async fn read_is_line_reads_a_normal_line() -> TestResult {
        let data: &[u8] = b"N0CALL>APK005:hello\r\nnext line\n";
        let mut reader = BufReader::new(data);
        let mut buf = Vec::new();
        let n = read_is_line(&mut reader, &mut buf, MAX_IS_READ_LINE_BYTES).await?;
        assert_eq!(n, buf.len());
        assert_eq!(buf.as_slice(), b"N0CALL>APK005:hello\r\n");
        // The next line is still readable (consume positioned correctly).
        buf.clear();
        let _ = read_is_line(&mut reader, &mut buf, MAX_IS_READ_LINE_BYTES).await?;
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
        let n = read_is_line(&mut reader, &mut buf, MAX_IS_READ_LINE_BYTES).await?;
        assert_eq!(n, 602, "long valid line should be returned whole");
        assert_eq!(buf.len(), 602);
        Ok(())
    }

    #[tokio::test]
    async fn read_is_line_bounds_huge_no_newline_chunk() -> TestResult {
        // A server streams far more than the cap with NO newline, then
        // EOF. The buffer must stay bounded by the cap rather than
        // growing to the input size (the OOM vector).
        let data = vec![b'A'; MAX_IS_READ_LINE_BYTES * 64];
        let mut reader = BufReader::new(data.as_slice());
        let mut buf = Vec::new();
        let n = read_is_line(&mut reader, &mut buf, MAX_IS_READ_LINE_BYTES).await?;
        assert!(
            buf.len() <= MAX_IS_READ_LINE_BYTES,
            "buffer grew past the cap: {} > {}",
            buf.len(),
            MAX_IS_READ_LINE_BYTES
        );
        assert_eq!(n, buf.len());
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
        let first = read_is_line(&mut reader, &mut buf, MAX_IS_READ_LINE_BYTES).await;
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
        let _ = read_is_line(&mut reader, &mut buf, MAX_IS_READ_LINE_BYTES).await?;
        assert_eq!(buf.as_slice(), b"N0CALL>APK005:ok\r\n");
        Ok(())
    }

    // --- CRLF injection on the uplink (BUG 2) ---

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
            matches!(result, Err(AprsIsError::EmbeddedNewline)),
            "expected EmbeddedNewline, got {result:?}"
        );
        // Close the socket so the capture ends at EOF, then prove the
        // login line is the ONLY thing that went on the wire — the
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
    async fn send_raw_line_rejects_embedded_newline_but_allows_terminator() -> TestResult {
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0u8; 1024];
            let _ = read_some(&mut stream, &mut buf).await;
            tokio::time::sleep(Duration::from_millis(80)).await;
        })
        .await?;

        let mut client = AprsIsClient::connect(test_config(addr)).await?;
        // Embedded newline in the body → rejected.
        let injected = client.send_raw_line("good\r\nEVIL>X:forged\r\n").await;
        assert!(
            matches!(injected, Err(AprsIsError::EmbeddedNewline)),
            "expected EmbeddedNewline, got {injected:?}"
        );
        // A normal CRLF-terminated line (no embedded newline) → accepted.
        client.send_raw_line("N0CALL>APK005:ok\r\n").await?;
        Ok(())
    }

    #[tokio::test]
    async fn send_packet_normal_still_sends() -> TestResult {
        let (tx, rx) = oneshot::channel::<Vec<u8>>();
        let addr = spawn_mock_server(move |mut stream| async move {
            // Login line + packet line.
            let captured = read_lines_capture(&mut stream, 2).await;
            drop(tx.send(captured));
        })
        .await?;

        let mut client = AprsIsClient::connect(test_config(addr)).await?;
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

    // --- Login validation (BUG 3) reaches connect() ---

    #[tokio::test]
    async fn connect_rejects_invalid_login_field() -> TestResult {
        // A software_name with a space must fail before any socket work.
        let mut config = AprsIsConfig::new("N0CALL");
        config.software_name = "my app".to_owned();
        let result = AprsIsClient::connect(config).await;
        assert!(
            matches!(
                result,
                Err(AprsIsError::InvalidLoginField {
                    field: "software_name",
                    ..
                })
            ),
            "expected InvalidLoginField(software_name), got {:?}",
            result.map(|_| "Ok(client)")
        );
        Ok(())
    }
}
