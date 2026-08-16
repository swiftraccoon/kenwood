//! The protocol-erased `AnyAsyncSession` drives a full `DExtra` TX flow
//! (header, voice, EOT, unlink) through the same wire path as the typed
//! handle, against the fake reflector.

// Deps visible to every dstar-gateway test target but unused here.
#[cfg(feature = "insecure-plaintext-xlx-directory")]
use reqwest as _;

use pcap_parser as _;
use thiserror as _;
use tracing as _;
use tracing_subscriber as _;
use trybuild as _;

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::fake_reflector::FakeReflector;
use dstar_gateway::tokio_shell::{AnyAsyncSession, AsyncSession, drive_connecting};
use dstar_gateway_core::header::DstarHeader;
use dstar_gateway_core::session::client::{Configured, DExtra, Session};
use dstar_gateway_core::types::{Callsign, Module, ProtocolKind, StreamId, Suffix};
use dstar_gateway_core::voice::VoiceFrame;
use tokio::net::UdpSocket;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn erased_session_drives_a_full_dextra_tx_flow() -> TestResult {
    let fake = FakeReflector::spawn_dextra().await?;
    let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);
    let connecting = Session::<DExtra, Configured>::builder()
        .callsign(Callsign::from_wire_bytes(*b"W1AW    "))
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer(fake.local_addr()?)
        .build()
        .connect(Instant::now())
        .map_err(|failed| failed.error)?;
    let connected = drive_connecting(connecting, &socket, Duration::from_secs(2)).await?;

    let mut session = AnyAsyncSession::from(AsyncSession::spawn(connected, Arc::clone(&socket)));
    assert_eq!(session.protocol_kind(), ProtocolKind::DExtra);

    let header = DstarHeader::for_relay(
        Callsign::from_wire_bytes(*b"W1AW    "),
        Module::B,
        Callsign::from_wire_bytes(*b"XRF001  "),
        Module::C,
        Callsign::from_wire_bytes(*b"W1AW    "),
        Suffix::from_wire_bytes(*b"D75 "),
    );
    let sid = StreamId::new(0x1234).ok_or("zero stream id")?;
    session.send_header(header, sid).await?;
    for seq in 0_u8..3 {
        session.send_voice(sid, seq, VoiceFrame::silence()).await?;
    }
    session.send_eot(sid, 3).await?;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // One 56-byte voice header; 4 × 27-byte voice packets of which
    // exactly one (the EOT) carries the 0x40 end bit at byte [14].
    let received = fake.received_packets().await;
    let header_count = received.iter().filter(|p| p.len() == 56).count();
    let voice: Vec<&Vec<u8>> = received.iter().filter(|p| p.len() == 27).collect();
    let eot_count = voice
        .iter()
        .filter(|p| p.get(14).is_some_and(|b| b & 0x40 != 0))
        .count();
    assert_eq!(header_count, 1, "one 56-byte voice header on the wire");
    assert_eq!(voice.len(), 4, "3 data frames + 1 EOT, 27 bytes each");
    assert_eq!(eot_count, 1, "exactly one frame carries the EOT bit");

    let links_before = received.iter().filter(|p| p.len() == 11).count();
    session.disconnect().await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let links_after = fake
        .received_packets()
        .await
        .iter()
        .filter(|p| p.len() == 11)
        .count();
    assert!(
        links_after > links_before,
        "disconnect must put an 11-byte unlink on the wire \
         ({links_before} link-sized packets before, {links_after} after)"
    );
    Ok(())
}
