//! Reusable fake-reflector harness for dstar-gateway integration tests.
//!
//! Binds a loopback UDP socket and spawns a tokio task that speaks
//! enough of each protocol (`DExtra`, `DPlus`, DCS) to drive connect,
//! keepalive, voice, and disconnect flows. Tests can query received
//! packets via the accessor methods and inject packet drops via
//! `drop_next_n()` to test retransmission.

#![allow(dead_code)] // not all tests use every field
#![allow(unreachable_pub)] // `mod fake_reflector;` include triggers this

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

/// Protocol flavor for the fake reflector task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeProtocol {
    /// `DExtra` protocol flavor.
    DExtra,
    /// `DPlus` protocol flavor.
    DPlus,
    /// DCS protocol flavor.
    Dcs,
}

/// Behavior of a fake `DPlus` reflector on the step-2 login reply.
///
/// Real REF reflectors return either `OKRW` (accept) or `BUSY`
/// (reject) on the 8-byte login reply. See
/// `ref/xlxd/src/cdplusprotocol.cpp:535-544` for the exact bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DPlusMode {
    /// Accept the login — reply with `[0x08, 0xC0, 0x04, 0x00, 'O', 'K', 'R', 'W']`.
    Accept,
    /// Reject the login with `BUSY` —
    /// reply with `[0x08, 0xC0, 0x04, 0x00, 'B', 'U', 'S', 'Y']`.
    RejectWithBusy,
}

/// Behavior of a fake DCS reflector on the 519-byte connect request.
///
/// Real DCS reflectors reply with a 14-byte ACK (`...ACK\0`) on accept
/// or a 14-byte NAK (`...NAK\0`) on reject. See
/// `ref/xlxd/src/cdcsprotocol.cpp` `EncodeConnectAckPacket` /
/// `EncodeConnectNakPacket` for the exact bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DcsMode {
    /// Accept the connect request — reply with the 14-byte ACK frame.
    Accept,
    /// Reject the connect request — reply with the 14-byte NAK frame.
    RejectWithNak,
}

/// Shared state between the harness task and test code.
#[derive(Debug, Default)]
struct State {
    connects: Vec<Vec<u8>>,
    keepalives: Vec<Vec<u8>>,
    voice_frames: Vec<Vec<u8>>,
    disconnects: Vec<Vec<u8>>,
}

/// Fake reflector server for integration tests.
#[derive(Debug)]
pub struct FakeReflector {
    local_addr: SocketAddr,
    state: Arc<Mutex<State>>,
    drop_next: Arc<AtomicUsize>,
    _task: tokio::task::JoinHandle<()>,
}

impl FakeReflector {
    /// Spawn a fake `DExtra` reflector bound to loopback.
    pub async fn spawn_dextra(refl_callsign: &str, refl_module: char) -> Self {
        Self::spawn(
            FakeProtocol::DExtra,
            refl_callsign,
            refl_module,
            DPlusMode::Accept,
            DcsMode::Accept,
        )
        .await
    }

    /// Spawn a fake `DPlus` reflector bound to loopback (accepts all logins).
    pub async fn spawn_dplus(refl_callsign: &str, refl_module: char) -> Self {
        Self::spawn(
            FakeProtocol::DPlus,
            refl_callsign,
            refl_module,
            DPlusMode::Accept,
            DcsMode::Accept,
        )
        .await
    }

    /// Spawn a fake `DPlus` reflector that rejects the step-2 login with
    /// `BUSY` instead of `OKRW`.
    pub async fn spawn_dplus_rejecting(refl_callsign: &str, refl_module: char) -> Self {
        Self::spawn(
            FakeProtocol::DPlus,
            refl_callsign,
            refl_module,
            DPlusMode::RejectWithBusy,
            DcsMode::Accept,
        )
        .await
    }

    /// Spawn a fake DCS reflector bound to loopback.
    pub async fn spawn_dcs(refl_callsign: &str, refl_module: char) -> Self {
        Self::spawn(
            FakeProtocol::Dcs,
            refl_callsign,
            refl_module,
            DPlusMode::Accept,
            DcsMode::Accept,
        )
        .await
    }

    /// Spawn a fake DCS reflector that rejects the 519-byte connect
    /// request with a 14-byte NAK frame instead of the ACK frame.
    pub async fn spawn_dcs_rejecting(refl_callsign: &str, refl_module: char) -> Self {
        Self::spawn(
            FakeProtocol::Dcs,
            refl_callsign,
            refl_module,
            DPlusMode::Accept,
            DcsMode::RejectWithNak,
        )
        .await
    }

    async fn spawn(
        protocol: FakeProtocol,
        refl_callsign: &str,
        refl_module: char,
        dplus_mode: DPlusMode,
        dcs_mode: DcsMode,
    ) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").await.expect("bind loopback");
        let local_addr = socket.local_addr().expect("local_addr");
        let state = Arc::new(Mutex::new(State::default()));
        let drop_next = Arc::new(AtomicUsize::new(0));

        let state_clone = Arc::clone(&state);
        let drop_clone = Arc::clone(&drop_next);
        let refl_cs = refl_callsign.to_owned();
        let task = tokio::spawn(async move {
            fake_reflector_loop(
                protocol,
                refl_cs,
                refl_module,
                dplus_mode,
                dcs_mode,
                socket,
                state_clone,
                drop_clone,
            )
            .await;
        });

        Self {
            local_addr,
            state,
            drop_next,
            _task: task,
        }
    }

    /// Local address the harness is bound to.
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Drop the next `n` incoming packets without responding (for retransmit tests).
    pub fn drop_next_n(&self, n: usize) {
        self.drop_next.store(n, Ordering::SeqCst);
    }

    /// Number of connect packets received so far.
    pub async fn connects_received(&self) -> usize {
        self.state.lock().await.connects.len()
    }

    /// Number of keepalive packets received so far.
    pub async fn keepalives_received(&self) -> usize {
        self.state.lock().await.keepalives.len()
    }

    /// Snapshot of all voice frames received so far.
    pub async fn voice_frames(&self) -> Vec<Vec<u8>> {
        self.state.lock().await.voice_frames.clone()
    }

    /// Number of disconnect packets received so far.
    pub async fn disconnects_received(&self) -> usize {
        self.state.lock().await.disconnects.len()
    }
}

#[allow(clippy::too_many_arguments)] // test harness: protocol + identity + modes + i/o + state
async fn fake_reflector_loop(
    protocol: FakeProtocol,
    refl_callsign: String,
    refl_module: char,
    dplus_mode: DPlusMode,
    dcs_mode: DcsMode,
    socket: UdpSocket,
    state: Arc<Mutex<State>>,
    drop_next: Arc<AtomicUsize>,
) {
    let mut buf = [0u8; 2048];
    loop {
        let Ok((n, src)) = socket.recv_from(&mut buf).await else {
            break;
        };
        let pkt = buf[..n].to_vec();

        if drop_next.load(Ordering::SeqCst) > 0 {
            let _ = drop_next.fetch_sub(1, Ordering::SeqCst);
            continue;
        }

        classify_and_respond(
            protocol,
            &refl_callsign,
            refl_module,
            dplus_mode,
            dcs_mode,
            &socket,
            src,
            &pkt,
            &state,
        )
        .await;
    }
}

#[allow(clippy::too_many_arguments)] // test harness: protocol + identity + modes + i/o + state
#[allow(clippy::cognitive_complexity)] // straight-line match; splitting hurts readability
async fn classify_and_respond(
    protocol: FakeProtocol,
    _refl_callsign: &str,
    _refl_module: char,
    dplus_mode: DPlusMode,
    dcs_mode: DcsMode,
    socket: &UdpSocket,
    src: SocketAddr,
    pkt: &[u8],
    state: &Arc<Mutex<State>>,
) {
    match protocol {
        FakeProtocol::DExtra => match pkt.len() {
            11 => {
                state.lock().await.connects.push(pkt.to_vec());
                let _ = socket.send_to(pkt, src).await;
            }
            9 => {
                state.lock().await.keepalives.push(pkt.to_vec());
                let _ = socket.send_to(pkt, src).await;
            }
            56 | 27 => {
                state.lock().await.voice_frames.push(pkt.to_vec());
            }
            _ => {}
        },
        FakeProtocol::DPlus => match pkt.len() {
            5 => {
                // LINK1 and UNLINK are both 5 bytes and share the same
                // `[0x05, 0x00, 0x18, 0x00, _]` header. Byte 4 is the flag:
                // 0x01 = LINK1 (connect), 0x00 = UNLINK (disconnect).
                // Per `ircDDBGateway` `ConnectData::getDPlusData`.
                if pkt[4] == 0x01 {
                    state.lock().await.connects.push(pkt.to_vec());
                    let _ = socket.send_to(pkt, src).await;
                } else {
                    // UNLINK: record but do not echo — let the client's
                    // retransmission loop run to completion.
                    state.lock().await.disconnects.push(pkt.to_vec());
                }
            }
            28 => {
                // Step-2 login reply. Per xlxd cdplusprotocol.cpp:535-544,
                // the accept frame is `{0x08,0xC0,0x04,0x00,'O','K','R','W'}`
                // and the reject frame is
                // `{0x08,0xC0,0x04,0x00,'B','U','S','Y'}`. Only the trailing
                // 4 ASCII bytes differ; the parser keys off `[4..8]`.
                let reply: [u8; 8] = match dplus_mode {
                    DPlusMode::Accept => [0x08, 0xC0, 0x04, 0x00, b'O', b'K', b'R', b'W'],
                    DPlusMode::RejectWithBusy => [0x08, 0xC0, 0x04, 0x00, b'B', b'U', b'S', b'Y'],
                };
                let _ = socket.send_to(&reply, src).await;
            }
            3 => {
                state.lock().await.keepalives.push(pkt.to_vec());
                let _ = socket.send_to(pkt, src).await;
            }
            58 | 29 | 32 => {
                state.lock().await.voice_frames.push(pkt.to_vec());
            }
            _ => {}
        },
        FakeProtocol::Dcs => match pkt.len() {
            519 => {
                state.lock().await.connects.push(pkt.to_vec());
                // Per xlxd EncodeConnectAckPacket / EncodeConnectNakPacket:
                //   callsign[7] + 0x20 + module + refl_module + "ACK\0"
                //   callsign[7] + 0x20 + module + refl_module + "NAK\0"
                // The client's DCS parser keys off data[10] (`A` vs `N`).
                let mut reply = Vec::with_capacity(14);
                reply.extend_from_slice(&pkt[0..10]);
                match dcs_mode {
                    DcsMode::Accept => reply.extend_from_slice(b"ACK\0"),
                    DcsMode::RejectWithNak => reply.extend_from_slice(b"NAK\0"),
                }
                let _ = socket.send_to(&reply, src).await;
            }
            17 => {
                state.lock().await.keepalives.push(pkt.to_vec());
                let _ = socket.send_to(pkt, src).await;
            }
            100 => {
                state.lock().await.voice_frames.push(pkt.to_vec());
            }
            19 | 11 => {
                state.lock().await.disconnects.push(pkt.to_vec());
            }
            _ => {}
        },
    }
}
