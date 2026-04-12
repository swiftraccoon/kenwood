//! End-to-end loopback test of the DCS connect flow through the tokio shell.
//!
//! DCS uses a single-round handshake like `DExtra` (519-byte LINK,
//! 14-byte ACK) but has completely different voice framing: every
//! voice packet is 100 bytes and embeds the full D-STAR header, so
//! there is no separate "voice header" packet length on the wire.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::fake_reflector::FakeReflector;
use dstar_gateway::tokio_shell::AsyncSession;
use dstar_gateway_core::header::DStarHeader;
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{ClientStateKind, Configured, Dcs, Session};
use dstar_gateway_core::types::{Callsign, Module, StreamId, Suffix};
use dstar_gateway_core::voice::VoiceFrame;
use tokio::net::UdpSocket;
use tokio::time::timeout;

use pcap_parser as _;
use thiserror as _;
use tracing as _;
use tracing_subscriber as _;
use trybuild as _;

#[tokio::test]
async fn dcs_connect_via_loopback_and_send_voice() -> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeReflector::spawn_dcs().await?;
    let reflector_addr = fake.local_addr()?;

    let client_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);

    let session: Session<Dcs, Configured> = Session::<Dcs, Configured>::builder()
        .callsign(Callsign::from_wire_bytes(*b"W1AW    "))
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer(reflector_addr)
        .build();

    // Drive the single-round handshake manually:
    //   519-byte LINK out → 14-byte ACK in → Connected.
    let now = Instant::now();
    let mut connecting = session.connect(now)?;
    {
        let tx = connecting.poll_transmit(now).ok_or("LINK not ready")?;
        let _ = client_sock.send_to(tx.payload, tx.dst).await?;
    }

    let mut buf = [0u8; 2048];
    let (n, peer) = timeout(Duration::from_secs(2), client_sock.recv_from(&mut buf)).await??;
    connecting.handle_input(Instant::now(), peer, buf.get(..n).ok_or("n out of bounds")?)?;
    assert_eq!(
        connecting.state_kind(),
        ClientStateKind::Connected,
        "DCS should be Connected after ACK"
    );
    let connected = connecting.promote()?;

    // Hand off to the tokio shell.
    let mut async_session = AsyncSession::spawn(connected, Arc::clone(&client_sock));

    // Send header + 5 voice frames + EOT. DCS packs everything into
    // 100-byte frames (header is embedded), so every send produces
    // exactly one 100-byte datagram.
    let header = DStarHeader {
        flag1: 0,
        flag2: 0,
        flag3: 0,
        rpt2: Callsign::from_wire_bytes(*b"DCS030 G"),
        rpt1: Callsign::from_wire_bytes(*b"DCS030 C"),
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

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Inspect the received packets. DCS shapes:
    //   519 — LINK
    //    14 — ACK (echoed from the reflector, not sent by client)
    //   100 — every voice frame (header + 5 data + EOT = 7 total).
    //    19 — UNLINK
    let received = fake.received_packets().await;
    let link_count = received.iter().filter(|p| p.len() == 519).count();
    let voice_count = received.iter().filter(|p| p.len() == 100).count();

    assert!(
        link_count >= 1,
        "expected ≥1 LINK (519 bytes), got {link_count}"
    );
    // send_header + 5 × send_voice + send_eot = 7 voice packets.
    assert_eq!(
        voice_count, 7,
        "expected 7 × 100-byte voice frames (1 header-sync + 5 data + 1 EOT), got {voice_count}"
    );

    async_session.disconnect().await?;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify the UNLINK was recorded.
    let final_received = fake.received_packets().await;
    let unlink_count = final_received.iter().filter(|p| p.len() == 19).count();
    assert!(
        unlink_count >= 1,
        "expected ≥1 UNLINK (19 bytes) after disconnect, got {unlink_count}"
    );

    drop(async_session);

    Ok(())
}
