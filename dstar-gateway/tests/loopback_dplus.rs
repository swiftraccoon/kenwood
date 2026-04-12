//! End-to-end loopback test of the `DPlus` 2-step login through the tokio shell.
//!
//! `DPlus` uses a two-phase handshake: LINK1 → `LINK1_ACK` →
//! (shell re-sends) LINK2 → OKRW. The sans-io core triggers the LINK2
//! internally when it sees a `LINK1_ACK`, so the test just needs to
//! drive two recv/enqueue round trips on the client side before it
//! can promote to `Connected`.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::fake_reflector::FakeReflector;
use dstar_gateway::tokio_shell::AsyncSession;
use dstar_gateway_core::codec::dplus::HostList;
use dstar_gateway_core::header::DStarHeader;
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{
    Authenticated, ClientStateKind, Configured, Connecting, DPlus, Session,
};
use dstar_gateway_core::types::{Callsign, Module, StreamId, Suffix};
use dstar_gateway_core::voice::VoiceFrame;
use tokio::net::UdpSocket;
use tokio::time::timeout;

use pcap_parser as _;
use thiserror as _;
use tracing as _;
use tracing_subscriber as _;
use trybuild as _;

/// Drive a `Session<DPlus, Connecting>` through the two-round-trip
/// `LINK1`/`LINK1_ACK`/`LINK2`/`OKRW` dance on the provided client
/// socket. Assumes the session's outbox already contains the LINK1
/// packet.
async fn drive_dplus_handshake(
    connecting: &mut Session<DPlus, Connecting>,
    client_sock: &UdpSocket,
) -> Result<(), Box<dyn std::error::Error>> {
    // Round 1: LINK1 out → LINK1_ACK in. Drain whatever LINK1 packet
    // is already in the outbox (there should be exactly one).
    let now = Instant::now();
    let tx = connecting.poll_transmit(now).ok_or("LINK1 not ready")?;
    let _ = client_sock.send_to(tx.payload, tx.dst).await?;

    let mut buf = [0u8; 64];
    let (n, peer) = timeout(Duration::from_secs(2), client_sock.recv_from(&mut buf)).await??;
    connecting.handle_input(Instant::now(), peer, buf.get(..n).ok_or("n out of bounds")?)?;

    // Handling the LINK1_ACK causes the core to enqueue a LINK2
    // packet. Drain and send it.
    let tx = connecting
        .poll_transmit(Instant::now())
        .ok_or("LINK2 not ready")?;
    let _ = client_sock.send_to(tx.payload, tx.dst).await?;

    // Round 2: wait for LINK2 reply (OKRW or BUSY).
    let (n, peer) = timeout(Duration::from_secs(2), client_sock.recv_from(&mut buf)).await??;
    connecting.handle_input(Instant::now(), peer, buf.get(..n).ok_or("n out of bounds")?)?;

    Ok(())
}

#[tokio::test]
async fn dplus_connect_via_loopback_and_send_voice() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Spawn a DPlus reflector that accepts the 2-step login.
    let fake = FakeReflector::spawn_dplus_accepting().await?;
    let reflector_addr = fake.local_addr()?;

    // 2. Bind a client UDP socket.
    let client_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);

    // 3. Build a `Session<DPlus, Configured>`, authenticate it with
    //    an empty `HostList` (the core attaches the list as the proof
    //    of a completed TCP auth step — we bypass the real auth
    //    entirely in this test), then call `.connect()` to enqueue
    //    the LINK1 packet.
    let session: Session<DPlus, Configured> = Session::<DPlus, Configured>::builder()
        .callsign(Callsign::from_wire_bytes(*b"W1AW    "))
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer(reflector_addr)
        .build();
    let authenticated: Session<DPlus, Authenticated> = session.authenticate(HostList::new())?;
    let mut connecting = authenticated.connect(Instant::now())?;

    // 4. Drive the 2-step handshake.
    drive_dplus_handshake(&mut connecting, &client_sock).await?;
    assert_eq!(
        connecting.state_kind(),
        ClientStateKind::Connected,
        "DPlus should be Connected after OKRW"
    );
    let connected = connecting.promote()?;

    // 5. Hand off to the tokio shell.
    let mut async_session = AsyncSession::spawn(connected, Arc::clone(&client_sock));

    // 6. Send a voice header + 5 voice data frames + EOT.
    let header = DStarHeader {
        flag1: 0,
        flag2: 0,
        flag3: 0,
        rpt2: Callsign::from_wire_bytes(*b"REF030 G"),
        rpt1: Callsign::from_wire_bytes(*b"REF030 C"),
        ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
        my_call: Callsign::from_wire_bytes(*b"W1AW    "),
        my_suffix: Suffix::EMPTY,
    };
    let sid = StreamId::new(0x1234).ok_or("zero stream id")?;
    async_session.send_header(header, sid).await?;

    let frame = VoiceFrame::silence();
    for seq in 0u8..5 {
        async_session.send_voice(sid, seq, frame).await?;
    }
    async_session.send_eot(sid, 5).await?;

    // 7. Give the fake reflector a moment to receive everything.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 8. Inspect the recorded packets:
    //    - 1 LINK1 (5 bytes, byte[4]=0x01)
    //    - 1 LINK2 (28 bytes)
    //    - 1 voice header (58 bytes)
    //    - 5 voice data packets (29 bytes each)
    //    - 1 voice EOT (32 bytes)
    //
    // The DPlus voice packet layout is documented in
    // `dstar_gateway_core/src/codec/dplus/encode.rs` (58-header,
    // 29-data, 32-eot) and differs from DExtra (56/27/27).
    let received = fake.received_packets().await;
    let link1_count = received
        .iter()
        .filter(|p| p.len() == 5 && p.get(4) == Some(&0x01))
        .count();
    let link2_count = received.iter().filter(|p| p.len() == 28).count();
    let header_count = received.iter().filter(|p| p.len() == 58).count();
    let data_count = received.iter().filter(|p| p.len() == 29).count();
    let eot_count = received.iter().filter(|p| p.len() == 32).count();

    assert!(
        link1_count >= 1,
        "expected ≥1 LINK1, got {link1_count} (received: {received:?})"
    );
    assert_eq!(link2_count, 1, "expected 1 LINK2, got {link2_count}");
    assert_eq!(
        header_count, 1,
        "expected 1 voice header (58 bytes), got {header_count}"
    );
    assert_eq!(
        data_count, 5,
        "expected 5 voice data packets (29 bytes), got {data_count}"
    );
    assert_eq!(
        eot_count, 1,
        "expected 1 voice EOT (32 bytes), got {eot_count}"
    );

    // 9. Graceful disconnect and tear down.
    async_session.disconnect().await?;
    drop(async_session);

    Ok(())
}
