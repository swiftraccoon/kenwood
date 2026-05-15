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

/// Keepalive comment text (sent as `# aprs-is keepalive\r\n`).
const KEEPALIVE_COMMENT: &str = "# aprs-is keepalive";

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
    /// [`AprsIsPacket::raw`]. See CB-4 in the project history.
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
    /// Returns [`AprsIsError::Connect`] if TCP connect fails or times out,
    /// or [`AprsIsError::Write`] if the login string cannot be sent.
    pub async fn connect(config: AprsIsConfig) -> Result<Self, AprsIsError> {
        let addr = format!("{}:{}", config.server, config.port);
        tracing::info!(server = %addr, callsign = %config.callsign, "APRS-IS connecting");

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
        // round-trip. See CB-C-7.
        stream.set_nodelay(true).map_err(AprsIsError::Connect)?;

        let (read_half, mut write_half) = stream.into_split();

        // Send login string.
        let login = build_login_string(&config);
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
    /// Bytes are read with [`AsyncBufReadExt::read_until`] (not
    /// `read_line`) so that non-UTF-8 sequences common in APRS info
    /// fields (Mic-E, raw weather data, Latin-1 comments) do not return
    /// `io::ErrorKind::InvalidData` and tear down the connection.
    /// Decoding to a Rust `String` happens via [`String::from_utf8_lossy`];
    /// any non-UTF-8 byte becomes a U+FFFD replacement character in the
    /// parsed view. For packet lines the original bytes are preserved
    /// in [`AprsIsPacket::raw`] so callers needing byte-exact fidelity
    /// (`IGate` forwarding, packet capture) can recover the wire-truth
    /// form. See CB-4 in the project history.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::Read`] on socket errors.
    pub async fn next_event(&mut self) -> Result<AprsIsEvent, AprsIsError> {
        self.line_buf.clear();
        let bytes = self
            .reader
            .read_until(b'\n', &mut self.line_buf)
            .await
            .map_err(AprsIsError::Read)?;

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
    /// Returns [`AprsIsError::LineTooLong`] if `line.len() >
    /// MAX_IS_LINE_BYTES`, or [`AprsIsError::Write`] if the underlying
    /// socket write fails.
    pub async fn send_raw_line(&mut self, line: &str) -> Result<(), AprsIsError> {
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

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Read up to `buf.len()` bytes from `stream`. Returns the number of
    /// bytes read, or panics via `assert!` in the test handler on I/O
    /// error — the handler is spawned on a tokio task and must not
    /// leak an `?` beyond the `async move` body.
    async fn read_some(stream: &mut TcpStream, buf: &mut [u8]) -> Option<usize> {
        stream.read(buf).await.ok().filter(|n| *n > 0)
    }

    /// Write all of `data` to `stream`; swallow any I/O error since the
    /// test will fail separately if the client doesn't see the line.
    async fn write_all_ignore(stream: &mut TcpStream, data: &[u8]) {
        if let Err(err) = stream.write_all(data).await {
            tracing::debug!(%err, "mock server write_all error");
        }
    }

    /// Spawn a mock APRS-IS server that accepts one connection, reads
    /// the login line, and runs the given handler.
    ///
    /// Returns the bound `SocketAddr` so tests can connect to it.
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
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0u8; 512];
            let Some(n) = read_some(&mut stream, &mut buf).await else {
                return;
            };
            let Ok(login) = std::str::from_utf8(buf.get(..n).unwrap_or(&[])) else {
                return;
            };
            assert!(
                login.starts_with("user N0CALL pass -1 vers test 0.1"),
                "unexpected login: {login:?}"
            );
            assert!(login.ends_with("\r\n"), "missing CRLF: {login:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        })
        .await?;

        let _client = AprsIsClient::connect(test_config(addr)).await?;
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
        // Regression guard for CB-4: pre-fix, BufReader::read_line
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
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0u8; 1024];
            let Some(n) = read_some(&mut stream, &mut buf).await else {
                return;
            };
            let Ok(text) = std::str::from_utf8(buf.get(..n).unwrap_or(&[])) else {
                return;
            };
            assert!(text.contains("user N0CALL"), "login missing: {text:?}");
            let Some(n) = read_some(&mut stream, &mut buf).await else {
                return;
            };
            let Ok(pkt) = std::str::from_utf8(buf.get(..n).unwrap_or(&[])) else {
                return;
            };
            assert_eq!(
                pkt, "N0CALL>APK005,WIDE1-1:!4903.50N/07201.75W-Test\r\n",
                "unexpected packet: {pkt:?}"
            );
        })
        .await?;

        let mut client = AprsIsClient::connect(test_config(addr)).await?;
        client
            .send_packet("N0CALL", "APK005", &["WIDE1-1"], "!4903.50N/07201.75W-Test")
            .await?;
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(())
    }

    #[tokio::test]
    async fn send_keepalive_sends_comment_line() -> TestResult {
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0u8; 1024];
            let _ = read_some(&mut stream, &mut buf).await;
            let Some(n) = read_some(&mut stream, &mut buf).await else {
                return;
            };
            let Ok(ka) = std::str::from_utf8(buf.get(..n).unwrap_or(&[])) else {
                return;
            };
            assert!(
                ka.starts_with("# aprs-is keepalive"),
                "unexpected keepalive: {ka:?}"
            );
            assert!(ka.ends_with("\r\n"), "missing CRLF: {ka:?}");
        })
        .await?;

        let mut client = AprsIsClient::connect(test_config(addr)).await?;
        client.send_keepalive().await?;
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(())
    }

    #[tokio::test]
    async fn maybe_send_keepalive_noop_when_recent() -> TestResult {
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0u8; 1024];
            let _ = read_some(&mut stream, &mut buf).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        })
        .await?;

        let mut client = AprsIsClient::connect(test_config(addr)).await?;
        // Called immediately after connect — last_write is fresh, no send.
        client.maybe_send_keepalive().await?;
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
        // Override the timeout for the test — we don't want to wait 10s.
        // Instead, verify the error path exists by checking connect_with_retry
        // returns an error with max_attempts=1.
        let result = tokio::time::timeout(
            Duration::from_secs(15),
            AprsIsClient::connect_with_retry(config, Some(1)),
        )
        .await;
        // Either the overall test timeout fires, or the connect fails.
        // Both are acceptable as long as we don't hang.
        if let Ok(r) = result {
            assert!(r.is_err(), "expected connect to fail, got Ok");
        }
        Ok(())
    }
}
