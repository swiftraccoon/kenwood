//! Single-reflector XLX UDP JSON monitor client.
//!
//! Each [`XlxMonitor`] manages a single UDP socket connected to one XLX
//! reflector's monitor port (10001). The lifecycle is:
//!
//! 1. **Connect**: [`XlxMonitor::connect`] binds a local UDP socket, sends the
//!    `"hello"` handshake datagram, and returns the monitor handle.
//!
//! 2. **Receive**: [`XlxMonitor::recv`] awaits the next UDP datagram with a
//!    30-second timeout, parses the JSON payload via [`protocol::parse`], and
//!    returns the decoded [`MonitorMessage`]. A timeout returns `None`,
//!    signaling that the reflector may be unresponsive.
//!
//! 3. **Disconnect**: drop the monitor. The [`Drop`] implementation sends
//!    a best-effort `"bye"` datagram so the reflector can clean up its
//!    client entry promptly rather than waiting for a timeout.
//!
//! # Single-client limitation
//!
//! Each XLX reflector monitor port accepts connections from any client, but
//! each `XlxMonitor` instance talks to exactly one reflector. To monitor
//! multiple reflectors, the orchestrator in [`super::run`] manages a pool of
//! monitors.
//!
//! # Timeout behavior
//!
//! The 30-second recv timeout is chosen to be 3x the xlxd update period
//! (~10 seconds). If no data arrives within this window, the reflector is
//! likely down or the network path is broken. The orchestrator can then
//! decide to reconnect or replace the monitor.
//!
//! # UDP socket binding
//!
//! Each monitor binds to `0.0.0.0:0` (ephemeral port) because the XLX
//! monitor protocol is stateless from the server's perspective — the server
//! tracks clients by source address, and each monitor needs its own port to
//! avoid datagram interleaving.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tokio::net::UdpSocket;

use super::protocol::{self, MonitorMessage};

/// XLX monitor port as defined by the xlxd source code.
const XLX_MONITOR_PORT: u16 = 10001;

/// Recv timeout: 3x the xlxd ~10-second update period.
const RECV_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum UDP datagram size for the XLX monitor protocol.
///
/// XLX node dumps can contain up to 250 entries, and each JSON node object
/// is roughly 100-150 bytes. 65535 bytes (maximum UDP payload) is sufficient
/// for any realistic message size.
const MAX_DATAGRAM_SIZE: usize = 65535;

/// A UDP client connected to a single XLX reflector's monitor port.
///
/// Manages the UDP socket lifecycle and provides async methods for receiving
/// parsed monitor messages. Create via [`XlxMonitor::connect`] and receive
/// events via [`XlxMonitor::recv`].
///
/// On drop, a best-effort `"bye"` datagram is sent to the reflector so it can
/// clean up its client tracking state promptly.
#[derive(Debug)]
pub(crate) struct XlxMonitor {
    /// The bound UDP socket, connected to the reflector's monitor port.
    socket: UdpSocket,

    /// The reflector's monitor endpoint (`ip:10001`).
    peer: SocketAddr,

    /// Reflector callsign for logging and database correlation.
    reflector: String,
}

impl XlxMonitor {
    /// Connects to a reflector's XLX monitor port and sends the `"hello"`
    /// handshake.
    ///
    /// Binds a new UDP socket on an ephemeral port, "connects" it to the
    /// reflector's monitor address (so that subsequent `send`/`recv` calls are
    /// scoped to this peer), and sends the initial `"hello"` datagram.
    ///
    /// The reflector will respond with three JSON datagrams (reflector info,
    /// nodes snapshot, stations snapshot) which can be read via [`recv`](Self::recv).
    ///
    /// # Errors
    ///
    /// Returns `io::Error` if socket binding or the initial send fails.
    pub(crate) async fn connect(ip: IpAddr, reflector: String) -> Result<Self, io::Error> {
        // Bind to an ephemeral port. Use the appropriate wildcard address for
        // the peer's address family (IPv4 or IPv6).
        let bind_addr: SocketAddr = if ip.is_ipv4() {
            SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 0)
        } else {
            SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0)
        };

        let socket = UdpSocket::bind(bind_addr).await?;

        let peer = SocketAddr::new(ip, XLX_MONITOR_PORT);
        // Connect the socket to the peer so that send/recv are scoped.
        socket.connect(peer).await?;

        // Send the handshake "hello" datagram.
        let _bytes_sent = socket.send(b"hello").await?;

        tracing::debug!(
            reflector = %reflector,
            peer = %peer,
            "xlx monitor connected"
        );

        Ok(Self {
            socket,
            peer,
            reflector,
        })
    }

    /// Receives the next monitor message from the reflector.
    ///
    /// Blocks (asynchronously) until a UDP datagram arrives or the 30-second
    /// timeout expires. Returns:
    ///
    /// - `Some(message)` — a successfully parsed [`MonitorMessage`].
    /// - `None` — the timeout expired (no data received within 30 seconds),
    ///   or the received datagram was not valid JSON and could not be parsed
    ///   at all (not even as `Unknown`).
    ///
    /// The caller should treat repeated `None` returns as a signal that the
    /// reflector is unresponsive and consider reconnecting.
    pub(crate) async fn recv(&self) -> Option<MonitorMessage> {
        let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];

        // Apply the recv timeout. If no datagram arrives within the window,
        // return None to signal potential reflector unresponsiveness.
        let result = tokio::time::timeout(RECV_TIMEOUT, self.socket.recv(&mut buf)).await;

        match result {
            Ok(Ok(n)) => {
                // Successfully received n bytes. Parse the JSON payload.
                // `n` is bounded by the buffer length (recv writes at most
                // buf.len() bytes), so get(..n) always returns Some.
                buf.get(..n).and_then(protocol::parse)
            }
            Ok(Err(e)) => {
                // UDP recv error (unlikely for connected UDP, but possible
                // with ICMP unreachable or similar).
                tracing::warn!(
                    reflector = %self.reflector,
                    error = %e,
                    "xlx monitor recv error"
                );
                None
            }
            Err(_elapsed) => {
                // Timeout expired — no datagram received within RECV_TIMEOUT.
                tracing::debug!(
                    reflector = %self.reflector,
                    timeout_secs = RECV_TIMEOUT.as_secs(),
                    "xlx monitor recv timeout"
                );
                None
            }
        }
    }

    /// Returns the peer socket address (`ip:10001`).
    pub(crate) const fn peer(&self) -> SocketAddr {
        self.peer
    }
}

impl Drop for XlxMonitor {
    /// Best-effort `"bye"` on drop.
    ///
    /// Uses `try_send` (non-async, non-blocking) because `Drop` cannot be
    /// async. If the send fails (e.g., the socket is already closed or the
    /// runtime is shutting down), the failure is silently ignored — the
    /// reflector will eventually time out the client entry on its own.
    fn drop(&mut self) {
        // try_send is the synchronous equivalent for connected UDP sockets.
        // It will fail if the send buffer is full or the socket is closed,
        // but that's acceptable for a best-effort cleanup.
        let _result = self.socket.try_send(b"bye");
    }
}
