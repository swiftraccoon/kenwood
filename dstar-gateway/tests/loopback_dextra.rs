//! End-to-end loopback test of the `DExtra` connect flow through the tokio shell.
//!
//! Drives the sans-io `Session<DExtra, _>` typestate through the
//! handshake manually, hands off the promoted `Session<DExtra, Connected>`
//! to [`AsyncSession::spawn`], then exercises `send_header`,
//! `send_voice`, `send_eot`, and `disconnect` over a real
//! `UdpSocket`. The paired [`FakeReflector`] replies to the 11-byte
//! LINK and records every datagram the shell emits.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::fake_reflector::FakeReflector;
use dstar_gateway::tokio_shell::AsyncSession;
use dstar_gateway_core::header::DStarHeader;
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{ClientStateKind, Configured, DExtra, Event, Session};
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
async fn dextra_connect_via_loopback_and_send_voice() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Spawn a DExtra fake reflector on loopback.
    let fake = FakeReflector::spawn_dextra().await?;
    let reflector_addr = fake.local_addr()?;

    // 2. Bind a client UDP socket.
    let client_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);

    // 3. Build a `Session<DExtra, Configured>`.
    let session: Session<DExtra, Configured> = Session::<DExtra, Configured>::builder()
        .callsign(Callsign::from_wire_bytes(*b"W1AW    "))
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer(reflector_addr)
        .build();

    // 4. Drive the connect handshake manually via the `Driver` trait.
    //    `AsyncSession::spawn` requires a `Session<P, Connected>` — the
    //    typestate won't let us hand it a Configured session. The
    //    handshake runs on the test thread so the loop never sees a
    //    "not yet connected" state.
    let now = Instant::now();
    let mut connecting = session.connect(now)?;

    // 4a. Pop the LINK packet and send it over the client socket.
    {
        let tx = connecting.poll_transmit(now).ok_or("LINK not ready")?;
        let _ = client_sock.send_to(tx.payload, tx.dst).await?;
    }

    // 4b. Wait for the 14-byte ACK on the client socket.
    let mut buf = [0u8; 64];
    let (n, peer) = timeout(Duration::from_secs(2), client_sock.recv_from(&mut buf)).await??;
    connecting.handle_input(Instant::now(), peer, buf.get(..n).ok_or("n out of bounds")?)?;

    // 4c. Promote `Connecting → Connected`.
    assert_eq!(connecting.state_kind(), ClientStateKind::Connected);
    let connected = connecting.promote()?;

    // 5. Hand off to the tokio shell. From here on, `SessionLoop`
    //    owns the socket and drives the session via `select!`.
    let mut async_session = AsyncSession::spawn(connected, Arc::clone(&client_sock));

    // 6. Send a voice header + 5 voice data frames + EOT.
    let header = DStarHeader {
        flag1: 0,
        flag2: 0,
        flag3: 0,
        rpt2: Callsign::from_wire_bytes(*b"XRF030 G"),
        rpt1: Callsign::from_wire_bytes(*b"XRF030 C"),
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

    // 7. Give the reflector task a moment to receive everything.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 8. Verify shape of the received packets:
    //    - at least 1 LINK (11 bytes) — the one we sent manually
    //    - exactly 1 voice header (56 bytes)
    //    - exactly 6 × 27-byte voice packets (5 data + 1 EOT)
    let received = fake.received_packets().await;
    let link_count = received.iter().filter(|p| p.len() == 11).count();
    let header_count = received.iter().filter(|p| p.len() == 56).count();
    let voice_count = received.iter().filter(|p| p.len() == 27).count();

    assert!(
        link_count >= 1,
        "expected at least 1 LINK packet, got {link_count} (received: {received:?})"
    );
    assert_eq!(
        header_count, 1,
        "expected exactly 1 voice header (56 bytes), got {header_count}"
    );
    assert_eq!(
        voice_count, 6,
        "expected 5 voice data + 1 EOT = 6 × 27-byte packets, got {voice_count}"
    );

    // 9. Graceful disconnect. The UNLINK sails through the shell and
    //    arrives at the fake; the DExtra state machine doesn't wait
    //    for a reply.
    async_session.disconnect().await?;

    // 10. Drain any final events before the session task exits. We
    //     don't assert on a specific disconnect reason — the DExtra
    //     reflector harness doesn't echo the UNLINK, so the reason
    //     surfaces as `DisconnectTimeout` once the 2 s deadline
    //     fires. Dropping the handle terminates the loop well
    //     before that, which is the intended shell behavior.
    drop(async_session);

    Ok(())
}

/// Smoke test: verify the tokio shell surfaces a `Connected` event
/// from the spawned session loop immediately after spawn. The handshake
/// already fired the `Event::Connected` on the sans-io side, so the
/// loop just needs to drain it into the consumer channel.
#[tokio::test]
async fn dextra_async_session_observes_connected_event() -> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeReflector::spawn_dextra().await?;
    let client_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);

    let session: Session<DExtra, Configured> = Session::<DExtra, Configured>::builder()
        .callsign(Callsign::from_wire_bytes(*b"W1AW    "))
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer(fake.local_addr()?)
        .build();

    let now = Instant::now();
    let mut connecting = session.connect(now)?;
    {
        let tx = connecting.poll_transmit(now).ok_or("LINK not ready")?;
        let _ = client_sock.send_to(tx.payload, tx.dst).await?;
    }
    let mut buf = [0u8; 64];
    let (n, peer) = timeout(Duration::from_secs(2), client_sock.recv_from(&mut buf)).await??;
    connecting.handle_input(Instant::now(), peer, buf.get(..n).ok_or("n out of bounds")?)?;
    let connected = connecting.promote()?;

    let mut async_session = AsyncSession::spawn(connected, Arc::clone(&client_sock));

    // The `Connected` event was pushed by `finalize_connected` during
    // handshake — the loop's first iteration drains it to the consumer.
    let evt = timeout(Duration::from_secs(1), async_session.next_event())
        .await?
        .ok_or("event channel closed")?;
    assert!(
        matches!(evt, Event::<DExtra>::Connected { .. }),
        "expected Connected event, got {evt:?}"
    );

    Ok(())
}
