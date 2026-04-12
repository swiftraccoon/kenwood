//! Loopback `FakeReflector` harness for the tokio-shell integration tests.
//!
//! Lives under `tests/common/`. Binds a loopback UDP socket and
//! spawns a tokio task that speaks just enough of `DExtra`, `DPlus`,
//! or `DCS` to drive a full connect / voice / disconnect cycle
//! against a `dstar_gateway_core::Session<P, _>`.
//!
//! Unlike the legacy harness, the loopback helper records **every**
//! datagram in a single `Vec<Vec<u8>>` so tests can assert on the
//! exact wire shape produced by the new tokio shell's `SessionLoop`.
//!
//! The helper is single-session — it latches onto the first client
//! that speaks to it and doesn't try to multiplex.

#![expect(
    unreachable_pub,
    reason = "test helper module — pub items serve sibling test files"
)]

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::Mutex;

/// Loopback fake reflector used by the tokio-shell integration tests.
#[derive(Debug)]
pub struct FakeReflector {
    socket: Arc<UdpSocket>,
    received: Arc<Mutex<Vec<Vec<u8>>>>,
    peer: Arc<Mutex<Option<SocketAddr>>>,
}

impl FakeReflector {
    /// Spawn a fake `DExtra` reflector.
    ///
    /// Responds to 11-byte LINK packets with a 14-byte ACK (echoing
    /// the sender callsign and module) and echoes 9-byte poll packets.
    /// Voice packets (56-byte header, 27-byte data/EOT) are recorded
    /// but not forwarded.
    pub async fn spawn_dextra() -> Result<Self, std::io::Error> {
        let (socket, received, peer) = Self::new_state().await?;
        let sock_clone = Arc::clone(&socket);
        let received_clone = Arc::clone(&received);
        let peer_clone = Arc::clone(&peer);
        drop(tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            loop {
                let Ok((n, src)) = sock_clone.recv_from(&mut buf).await else {
                    return;
                };
                let slice = buf.get(..n).unwrap_or(&[]);
                received_clone.lock().await.push(slice.to_vec());
                *peer_clone.lock().await = Some(src);

                match n {
                    // LINK (11 bytes) — reply with a 14-byte ACK.
                    // `write_connect_reply` in the core codec writes:
                    //   [0..7]  first 7 chars of callsign
                    //   [7]     memset pad slot (space)
                    //   [8]     client/repeater module (8th char)
                    //   [9]     reflector module
                    //   [10..13] b"ACK"
                    //   [13]    0x00 NUL terminator
                    // The 11-byte LINK request shares the layout of
                    // bytes [0..10] with the ACK, so we can echo the
                    // first 10 bytes of the request and append the
                    // "ACK" tag at [10..13] followed by NUL at [13].
                    11 => {
                        let mut ack = [0u8; 14];
                        if let Some(prefix) = buf.get(..10)
                            && let Some(dst) = ack.get_mut(..10)
                        {
                            dst.copy_from_slice(prefix);
                        }
                        if let Some(dst) = ack.get_mut(10..13) {
                            dst.copy_from_slice(b"ACK");
                        }
                        if let Some(nul) = ack.get_mut(13) {
                            *nul = 0x00;
                        }
                        drop(sock_clone.send_to(&ack, src).await);
                    }
                    // 9-byte poll — echo back.
                    9 => {
                        if let Some(slice) = buf.get(..9) {
                            drop(sock_clone.send_to(slice, src).await);
                        }
                    }
                    _ => {}
                }
            }
        }));

        Ok(Self {
            socket,
            received,
            peer,
        })
    }

    /// Spawn a fake `DPlus` reflector that accepts the 2-step login.
    ///
    /// - 5-byte `[0x05, 0x00, 0x18, 0x00, 0x01]` LINK1 → echoed as
    ///   `LINK1_ACK` (same bytes).
    /// - 28-byte LINK2 → 8-byte `OKRW` reply.
    /// - 3-byte poll `[0x03, 0x60, 0x00]` → echoed.
    /// - 5-byte `[..., 0x00]` UNLINK → echoed as `UNLINK_ACK`.
    pub async fn spawn_dplus_accepting() -> Result<Self, std::io::Error> {
        Self::spawn_dplus(DPlusMode::Accept).await
    }

    /// Spawn a fake `DPlus` reflector that rejects the step-2 login
    /// with `BUSY` instead of `OKRW`.
    pub async fn spawn_dplus_rejecting() -> Result<Self, std::io::Error> {
        Self::spawn_dplus(DPlusMode::RejectWithBusy).await
    }

    async fn spawn_dplus(mode: DPlusMode) -> Result<Self, std::io::Error> {
        let (socket, received, peer) = Self::new_state().await?;
        let sock_clone = Arc::clone(&socket);
        let received_clone = Arc::clone(&received);
        let peer_clone = Arc::clone(&peer);
        drop(tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            loop {
                let Ok((n, src)) = sock_clone.recv_from(&mut buf).await else {
                    return;
                };
                let slice = buf.get(..n).unwrap_or(&[]);
                received_clone.lock().await.push(slice.to_vec());
                *peer_clone.lock().await = Some(src);

                match n {
                    5 => {
                        let ctrl = buf.get(4).copied().unwrap_or(0);
                        if ctrl == 0x01 {
                            // LINK1 → echo LINK1 back.
                            drop(
                                sock_clone
                                    .send_to(&[0x05, 0x00, 0x18, 0x00, 0x01], src)
                                    .await,
                            );
                        } else if ctrl == 0x00 {
                            // UNLINK → echo UNLINK back as UNLINK_ACK.
                            drop(
                                sock_clone
                                    .send_to(&[0x05, 0x00, 0x18, 0x00, 0x00], src)
                                    .await,
                            );
                        }
                    }
                    28 => {
                        // LINK2 login — reply OKRW or BUSY.
                        let reply: [u8; 8] = match mode {
                            DPlusMode::Accept => [0x08, 0xC0, 0x04, 0x00, b'O', b'K', b'R', b'W'],
                            DPlusMode::RejectWithBusy => {
                                [0x08, 0xC0, 0x04, 0x00, b'B', b'U', b'S', b'Y']
                            }
                        };
                        drop(sock_clone.send_to(&reply, src).await);
                    }
                    3 => {
                        // Poll → echo.
                        if let Some(slice) = buf.get(..3) {
                            drop(sock_clone.send_to(slice, src).await);
                        }
                    }
                    _ => {}
                }
            }
        }));

        Ok(Self {
            socket,
            received,
            peer,
        })
    }

    /// Spawn a fake DCS reflector.
    ///
    /// - 519-byte LINK → 14-byte ACK (echoing the sender's callsign
    ///   prefix and module).
    /// - 17-byte poll → echoed.
    /// - 19-byte UNLINK → also answered with a 14-byte ACK-shaped
    ///   reply so the core's disconnect timer short-circuits rather
    ///   than waiting for the 2 s timeout.
    /// - 100-byte voice frames are recorded but not forwarded.
    pub async fn spawn_dcs() -> Result<Self, std::io::Error> {
        let (socket, received, peer) = Self::new_state().await?;
        let sock_clone = Arc::clone(&socket);
        let received_clone = Arc::clone(&received);
        let peer_clone = Arc::clone(&peer);
        drop(tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            loop {
                let Ok((n, src)) = sock_clone.recv_from(&mut buf).await else {
                    return;
                };
                let slice = buf.get(..n).unwrap_or(&[]);
                received_clone.lock().await.push(slice.to_vec());
                *peer_clone.lock().await = Some(src);

                match n {
                    // 519-byte LINK → 14-byte ACK.
                    // Echo the LINK prefix [0..10] (callsign + modules
                    // + reserved 0x00) and append b"ACK\0" at
                    // [10..14]. Matches `write_connect_reply` in the
                    // core codec: tag at [10..13], NUL at [13].
                    519 => {
                        let mut reply = [0u8; 14];
                        if let Some(prefix) = buf.get(..10)
                            && let Some(dst) = reply.get_mut(..10)
                        {
                            dst.copy_from_slice(prefix);
                        }
                        if let Some(dst) = reply.get_mut(10..13) {
                            dst.copy_from_slice(b"ACK");
                        }
                        if let Some(nul) = reply.get_mut(13) {
                            *nul = 0x00;
                        }
                        drop(sock_clone.send_to(&reply, src).await);
                    }
                    // 17-byte poll → echo (symmetric keepalive).
                    17 => {
                        if let Some(slice) = buf.get(..17) {
                            drop(sock_clone.send_to(slice, src).await);
                        }
                    }
                    // 19-byte UNLINK → reply with an ACK-shaped reply
                    // so the session closes immediately.
                    19 => {
                        let mut reply = [0u8; 14];
                        if let Some(prefix) = buf.get(..10)
                            && let Some(dst) = reply.get_mut(..10)
                        {
                            dst.copy_from_slice(prefix);
                        }
                        // For UNLINK the `client_module` is at [8] and
                        // [9] is `b' '`. The core's reply parser needs
                        // a *valid* reflector module at [9], so
                        // overwrite [9] with [8] before shipping.
                        if let Some(src_b) = buf.get(8).copied()
                            && let Some(dst) = reply.get_mut(9)
                        {
                            *dst = src_b;
                        }
                        if let Some(dst) = reply.get_mut(10..13) {
                            dst.copy_from_slice(b"ACK");
                        }
                        if let Some(nul) = reply.get_mut(13) {
                            *nul = 0x00;
                        }
                        drop(sock_clone.send_to(&reply, src).await);
                    }
                    _ => {}
                }
            }
        }));

        Ok(Self {
            socket,
            received,
            peer,
        })
    }

    /// The loopback address the fake reflector is bound to.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O error if the socket address cannot be
    /// retrieved.
    pub fn local_addr(&self) -> Result<SocketAddr, std::io::Error> {
        self.socket.local_addr()
    }

    /// Snapshot of every datagram received so far.
    pub async fn received_packets(&self) -> Vec<Vec<u8>> {
        self.received.lock().await.clone()
    }

    /// Convenience wrapper: total number of received datagrams.
    pub async fn received_count(&self) -> usize {
        self.received.lock().await.len()
    }

    /// Inject bytes from the fake server to the client.
    ///
    /// Requires that the client has already sent at least one packet
    /// so the server knows its ephemeral source address. Returns an
    /// error if no client address has been learned yet.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] with kind [`std::io::ErrorKind::NotConnected`]
    /// if no client address has been observed yet, or any error from
    /// the underlying `send_to` call.
    pub async fn send_to_peer(&self, bytes: &[u8]) -> std::io::Result<()> {
        let peer = {
            let guard = self.peer.lock().await;
            *guard
        };
        match peer {
            Some(addr) => {
                let _ = self.socket.send_to(bytes, addr).await?;
                Ok(())
            }
            None => Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "no client address observed yet",
            )),
        }
    }

    async fn new_state() -> Result<
        (
            Arc<UdpSocket>,
            Arc<Mutex<Vec<Vec<u8>>>>,
            Arc<Mutex<Option<SocketAddr>>>,
        ),
        std::io::Error,
    > {
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);
        let received = Arc::new(Mutex::new(Vec::new()));
        let peer = Arc::new(Mutex::new(None));
        Ok((socket, received, peer))
    }
}

/// Behaviour of the fake `DPlus` reflector on the step-2 LINK2 reply.
#[derive(Debug, Clone, Copy)]
enum DPlusMode {
    Accept,
    RejectWithBusy,
}
