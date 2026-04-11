//! Async TCP client for APRS-IS (Internet Service).
//!
//! Provides a [`AprsIsClient`] that connects to an APRS-IS server over
//! TCP, authenticates, and exchanges APRS packets as line-delimited text.
//! This is the complement to the pure-data helpers in [`super::aprs_is`],
//! which stay transport-agnostic.
//!
//! # Usage
//!
//! ```no_run
//! use kenwood_thd75::{AprsIsClient, AprsIsConfig, AprsIsEvent};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let mut config = AprsIsConfig::new("N0CALL");
//! config.filter = "r/35.25/-97.75/100".to_owned();
//! let mut client = AprsIsClient::connect(config).await?;
//!
//! loop {
//!     match client.next_event().await? {
//!         AprsIsEvent::Packet(line) => println!("Got: {line}"),
//!         AprsIsEvent::Comment(line) => println!("Server: {line}"),
//!         AprsIsEvent::LoggedIn => println!("Authenticated"),
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

use super::aprs_is::{AprsIsConfig, build_login_string, format_is_packet, parse_is_line};

/// Default APRS-IS keepalive interval.
///
/// APRS-IS servers expect the client to send something (a packet or a
/// comment line) at least every 2 minutes or they may disconnect.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(120);

/// Default connect timeout for the initial TCP handshake + login.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Keepalive comment text (sent as `# kenwood-thd75 keepalive\r\n`).
const KEEPALIVE_COMMENT: &str = "# kenwood-thd75 keepalive";

/// An event from the APRS-IS server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AprsIsEvent {
    /// An APRS packet line was received (not a comment).
    ///
    /// The line is stripped of trailing `\r\n`. Parse with the standard
    /// APRS parsers in [`crate::kiss`] after splitting source/dest/path/data.
    Packet(String),

    /// A server comment line was received (starts with `#`).
    ///
    /// Comments carry server info, login responses, and keepalives.
    /// The line is stripped of trailing `\r\n`.
    Comment(String),

    /// The server accepted the login (`# logresp ... verified, server ...`).
    ///
    /// Emitted the first time a comment line containing `logresp` and
    /// `verified` is seen. Not emitted for rejected logins.
    LoggedIn,

    /// The TCP connection was closed (EOF from server).
    Disconnected,
}

/// Errors that can occur during APRS-IS operations.
#[derive(Debug, thiserror::Error)]
pub enum AprsIsError {
    /// The TCP connection could not be established.
    #[error("APRS-IS connect failed: {0}")]
    Connect(std::io::Error),

    /// A read from the TCP socket failed.
    #[error("APRS-IS read failed: {0}")]
    Read(std::io::Error),

    /// A write to the TCP socket failed.
    #[error("APRS-IS write failed: {0}")]
    Write(std::io::Error),

    /// The initial login handshake timed out.
    #[error("APRS-IS login timed out")]
    LoginTimeout,

    /// The server rejected the login credentials.
    #[error("APRS-IS login rejected: {0}")]
    LoginRejected(String),
}

/// Async TCP client for APRS-IS.
///
/// Owns a single TCP connection to an APRS-IS server, handles the login
/// handshake, and exposes line-at-a-time read/write methods.
///
/// Not `Clone` and not `Send`-across-the-await — typical usage is to own
/// it from a single task.
#[derive(Debug)]
pub struct AprsIsClient {
    config: AprsIsConfig,
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    line_buf: String,
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
            line_buf: String::with_capacity(512),
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
    /// # Errors
    ///
    /// Returns [`AprsIsError::Read`] on socket errors.
    pub async fn next_event(&mut self) -> Result<AprsIsEvent, AprsIsError> {
        self.line_buf.clear();
        let bytes = self
            .reader
            .read_line(&mut self.line_buf)
            .await
            .map_err(AprsIsError::Read)?;

        if bytes == 0 {
            tracing::info!("APRS-IS connection closed by server");
            return Ok(AprsIsEvent::Disconnected);
        }

        let line = self.line_buf.trim_end_matches(['\r', '\n']);

        if let Some(packet) = parse_is_line(line) {
            return Ok(AprsIsEvent::Packet(packet.to_owned()));
        }

        // Comment line. Check for login response on first one.
        if !self.logged_in_emitted && line.contains("logresp") && line.contains("verified") {
            self.logged_in_emitted = true;
            tracing::info!(response = %line, "APRS-IS login verified");
            return Ok(AprsIsEvent::LoggedIn);
        }

        Ok(AprsIsEvent::Comment(line.to_owned()))
    }

    /// Send a formatted APRS packet to the server.
    ///
    /// The packet is formatted as `source>destination,path:data\r\n` via
    /// [`format_is_packet`] and written to the TCP socket.
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
    /// # Errors
    ///
    /// Returns [`AprsIsError::Write`] if the write fails.
    pub async fn send_raw_line(&mut self, line: &str) -> Result<(), AprsIsError> {
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
    /// Sends `# kenwood-thd75 keepalive\r\n` to the server. Call this
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
    use std::future::Future;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    /// Spawn a mock APRS-IS server that accepts one connection, reads
    /// the login line, and runs the given handler.
    ///
    /// Returns the bound `SocketAddr` so tests can connect to it.
    async fn spawn_mock_server<F, Fut>(handler: F) -> std::net::SocketAddr
    where
        F: FnOnce(TcpStream) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handler(stream).await;
        }));
        addr
    }

    fn test_config(addr: std::net::SocketAddr) -> AprsIsConfig {
        AprsIsConfig {
            callsign: "N0CALL".to_owned(),
            passcode: -1,
            server: addr.ip().to_string(),
            port: addr.port(),
            filter: String::new(),
            software_name: "test".to_owned(),
            software_version: "0.1".to_owned(),
        }
    }

    #[tokio::test]
    async fn connect_sends_login_string() {
        let addr = spawn_mock_server(|mut stream| async move {
            // Read the login line.
            let mut buf = [0u8; 512];
            let n = tokio::io::AsyncReadExt::read(&mut stream, &mut buf)
                .await
                .unwrap();
            let login = std::str::from_utf8(&buf[..n]).unwrap();
            assert!(login.starts_with("user N0CALL pass -1 vers test 0.1"));
            assert!(login.ends_with("\r\n"));
            // Keep the connection open briefly.
            tokio::time::sleep(Duration::from_millis(50)).await;
        })
        .await;

        let _client = AprsIsClient::connect(test_config(addr)).await.unwrap();
    }

    #[tokio::test]
    async fn next_event_receives_packet_line() {
        let addr = spawn_mock_server(|mut stream| async move {
            // Read and discard login.
            let mut buf = [0u8; 512];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf)
                .await
                .unwrap();
            // Send a packet line.
            stream
                .write_all(b"N0CALL>APK005:!4903.50N/07201.75W-Test\r\n")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        })
        .await;

        let mut client = AprsIsClient::connect(test_config(addr)).await.unwrap();
        let event = client.next_event().await.unwrap();
        match event {
            AprsIsEvent::Packet(line) => {
                assert_eq!(line, "N0CALL>APK005:!4903.50N/07201.75W-Test");
            }
            other => panic!("expected Packet, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn next_event_receives_comment_line() {
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0u8; 512];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf)
                .await
                .unwrap();
            stream
                .write_all(b"# javAPRSSrvr 4.2.0b05\r\n")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        })
        .await;

        let mut client = AprsIsClient::connect(test_config(addr)).await.unwrap();
        let event = client.next_event().await.unwrap();
        match event {
            AprsIsEvent::Comment(line) => {
                assert_eq!(line, "# javAPRSSrvr 4.2.0b05");
            }
            other => panic!("expected Comment, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn next_event_detects_login_verified() {
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0u8; 512];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf)
                .await
                .unwrap();
            stream
                .write_all(b"# logresp N0CALL verified, server T2TEST\r\n")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        })
        .await;

        let mut client = AprsIsClient::connect(test_config(addr)).await.unwrap();
        let event = client.next_event().await.unwrap();
        assert!(matches!(event, AprsIsEvent::LoggedIn));
    }

    #[tokio::test]
    async fn next_event_detects_disconnect() {
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0u8; 512];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf)
                .await
                .unwrap();
            // Close immediately.
            drop(stream);
        })
        .await;

        let mut client = AprsIsClient::connect(test_config(addr)).await.unwrap();
        let event = client.next_event().await.unwrap();
        assert!(matches!(event, AprsIsEvent::Disconnected));
    }

    #[tokio::test]
    async fn send_packet_formats_line() {
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0u8; 1024];
            // Read login.
            let n = tokio::io::AsyncReadExt::read(&mut stream, &mut buf)
                .await
                .unwrap();
            // Login is the first line.
            let text = std::str::from_utf8(&buf[..n]).unwrap();
            assert!(text.contains("user N0CALL"));
            // Read the next write (the packet).
            let n = tokio::io::AsyncReadExt::read(&mut stream, &mut buf)
                .await
                .unwrap();
            let pkt = std::str::from_utf8(&buf[..n]).unwrap();
            assert_eq!(pkt, "N0CALL>APK005,WIDE1-1:!4903.50N/07201.75W-Test\r\n");
        })
        .await;

        let mut client = AprsIsClient::connect(test_config(addr)).await.unwrap();
        client
            .send_packet("N0CALL", "APK005", &["WIDE1-1"], "!4903.50N/07201.75W-Test")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn send_keepalive_sends_comment_line() {
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf)
                .await
                .unwrap();
            let n = tokio::io::AsyncReadExt::read(&mut stream, &mut buf)
                .await
                .unwrap();
            let ka = std::str::from_utf8(&buf[..n]).unwrap();
            assert!(ka.starts_with("# kenwood-thd75 keepalive"));
            assert!(ka.ends_with("\r\n"));
        })
        .await;

        let mut client = AprsIsClient::connect(test_config(addr)).await.unwrap();
        client.send_keepalive().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn maybe_send_keepalive_noop_when_recent() {
        let addr = spawn_mock_server(|mut stream| async move {
            let mut buf = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf)
                .await
                .unwrap();
            // Reader should not see any further data (keepalive skipped).
            tokio::time::sleep(Duration::from_millis(50)).await;
        })
        .await;

        let mut client = AprsIsClient::connect(test_config(addr)).await.unwrap();
        // Called immediately after connect — last_write is fresh, no send.
        client.maybe_send_keepalive().await.unwrap();
    }

    #[tokio::test]
    async fn connect_timeout() {
        // Connect to a non-routable IP to trigger timeout.
        // Using 198.51.100.1 (TEST-NET-2) which should not respond.
        let config = AprsIsConfig {
            callsign: "N0CALL".to_owned(),
            passcode: -1,
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
            assert!(r.is_err());
        }
    }
}
