//! Cross-protocol fan-out integration test.
//!
//! Exercises the full pipeline that lets a voice frame received on
//! one protocol's endpoint transparently reach clients on a
//! different protocol:
//!
//! 1. Three `ProtocolEndpoint`s (one per protocol) are constructed
//!    with the same `broadcast::Sender<CrossProtocolEvent>` so they
//!    share a cross-protocol voice bus.
//! 2. A `DExtra` peer LINKs via its endpoint's `handle_inbound`,
//!    then sends a voice header + two data frames + an EOT.
//! 3. A bus subscriber asserts that each voice event was published
//!    on the cross-protocol bus with the originator's protocol,
//!    module, and a cached D-STAR header where appropriate.
//! 4. For each published event, we call `transcode_voice` once per
//!    target protocol (`DPlus`, `DCS`) and verify the result is a
//!    valid wire frame of the expected length. This proves the
//!    cross-protocol fan-out primitive is wired end-to-end.
//!
//! We drive the endpoints directly via `handle_inbound` rather than
//! spinning up `Reflector::run` + UDP sockets so the test stays
//! deterministic and runs in a few milliseconds on any machine.
//! `Reflector::run` is exercised separately by
//! `reflector_dextra_fanout::reflector_new_with_socket_shutdown_smoke_test`.

#![expect(
    clippy::unreachable,
    reason = "Integration test file. Tests live in a separate compilation unit from the \
              library crate, so the library's internal lint posture does not apply — we \
              restate this opt-out here so test code stays expressive while production \
              code remains strict. `clippy::unreachable` fires on `unreachable!()` used \
              inside `match` arms as assertion-style 'this variant cannot occur given \
              the test's setup' guards — if the invariant is violated the test correctly \
              panics with a specific message naming the impossible variant, which is more \
              debuggable than a generic `assert!` failure."
)]

// Workspace dev-deps used by sibling test targets. Acknowledge them
// here so the strict `unused_crate_dependencies` lint stays silent
// for this integration test.
use proptest as _;
use thiserror as _;
use tracing as _;
use tracing_subscriber as _;
use trybuild as _;

use std::sync::Arc;

use tokio::sync::broadcast;

use dstar_gateway_core::codec::dextra::{
    encode_connect_link, encode_voice_data, encode_voice_eot, encode_voice_header,
};
use dstar_gateway_core::header::DStarHeader;
use dstar_gateway_core::session::client::{DExtra, DPlus, Dcs};
use dstar_gateway_core::types::{Callsign, Module, ProtocolKind, StreamId, Suffix};
use dstar_gateway_core::voice::VoiceFrame;

use dstar_gateway_server::{
    AllowAllAuthorizer, CrossProtocolEvent, ProtocolEndpoint, VoiceEvent, transcode_voice,
};

const fn sid() -> StreamId {
    match StreamId::new(0x4242) {
        Some(s) => s,
        None => unreachable!(),
    }
}

const fn test_header(my: [u8; 8]) -> DStarHeader {
    DStarHeader {
        flag1: 0,
        flag2: 0,
        flag3: 0,
        rpt2: Callsign::from_wire_bytes(*b"REF030 G"),
        rpt1: Callsign::from_wire_bytes(*b"REF030 C"),
        ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
        my_call: Callsign::from_wire_bytes(my),
        my_suffix: Suffix::from_wire_bytes(*b"D75 "),
    }
}

const fn client_peer(port: u16) -> std::net::SocketAddr {
    std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port)
}

/// End-to-end cross-protocol voice forwarding test.
///
/// A `DExtra` client transmits a voice burst against its endpoint.
/// The endpoint publishes one `CrossProtocolEvent` per voice
/// lifecycle event onto a shared broadcast channel, and we confirm
/// that `transcode_voice` re-encodes each event into valid `DPlus`
/// and `DCS` wire frames.
#[expect(
    clippy::too_many_lines,
    reason = "test walks a 4-event voice burst with per-event transcode assertions; splitting it would obscure the linear narrative"
)]
#[tokio::test]
async fn three_protocol_cross_protocol_forwarding() -> Result<(), Box<dyn std::error::Error>> {
    // Shared cross-protocol voice bus. 256 slots mirrors the
    // production capacity in `Reflector::new_with_sockets`.
    let (voice_tx, _) = broadcast::channel::<CrossProtocolEvent>(256);

    // One endpoint per protocol. They would normally be behind
    // separate UDP sockets on different ports; for this test we
    // exercise only the sans-io entry point, so no sockets are
    // bound.
    let dextra_ep = Arc::new(ProtocolEndpoint::<DExtra>::new_with_voice_bus(
        ProtocolKind::DExtra,
        Module::C,
        Arc::new(AllowAllAuthorizer),
        Some(voice_tx.clone()),
    ));
    let _dplus_ep = Arc::new(ProtocolEndpoint::<DPlus>::new_with_voice_bus(
        ProtocolKind::DPlus,
        Module::C,
        Arc::new(AllowAllAuthorizer),
        Some(voice_tx.clone()),
    ));
    let _dcs_ep = Arc::new(ProtocolEndpoint::<Dcs>::new_with_voice_bus(
        ProtocolKind::Dcs,
        Module::C,
        Arc::new(AllowAllAuthorizer),
        Some(voice_tx.clone()),
    ));

    // Subscribe BEFORE driving the endpoint so we receive every
    // published event. `broadcast::Receiver::subscribe` returns
    // events sent after the subscription is created.
    let mut rx = voice_tx.subscribe();

    let peer_dextra = client_peer(30001);

    // LINK the DExtra peer first. The endpoint drives the core
    // which transitions to Linked and populates the pool's reverse
    // index. LINK itself emits only a `ClientLinked` event (not a
    // voice event), so the cross-protocol bus stays silent.
    let mut link_buf = [0u8; 16];
    let n = encode_connect_link(
        &mut link_buf,
        &Callsign::from_wire_bytes(*b"W1AW    "),
        Module::C,
        Module::B,
    )?;
    let link_slice = link_buf.get(..n).ok_or("link bytes out of range")?;
    drop(
        dextra_ep
            .handle_inbound(link_slice, peer_dextra, std::time::Instant::now())
            .await?,
    );
    assert!(
        rx.try_recv().is_err(),
        "LINK must not publish to the voice bus"
    );

    // Voice header. The endpoint must publish a StreamStart on
    // the bus with protocol=DExtra, module=C, a cached header.
    let header = test_header(*b"W1AW    ");
    let mut hdr_buf = [0u8; 64];
    let hdr_n = encode_voice_header(&mut hdr_buf, sid(), &header)?;
    let hdr_slice = hdr_buf.get(..hdr_n).ok_or("hdr bytes out of range")?;
    drop(
        dextra_ep
            .handle_inbound(hdr_slice, peer_dextra, std::time::Instant::now())
            .await?,
    );

    let event_start = rx.try_recv().ok().ok_or("StreamStart not published")?;
    assert_eq!(event_start.source_protocol, ProtocolKind::DExtra);
    assert_eq!(event_start.source_peer, peer_dextra);
    assert_eq!(event_start.module, Module::C);
    assert!(
        matches!(event_start.event, VoiceEvent::StreamStart { .. }),
        "first published event is StreamStart"
    );
    assert!(
        event_start.cached_header.is_some(),
        "StreamStart carries cached header"
    );

    // Transcode the StreamStart into each target protocol. The
    // header frame has known wire sizes: 56 bytes for DExtra, 58
    // bytes for DPlus, 100 bytes for DCS (DCS fuses header+AMBE).
    let mut out = [0u8; 256];
    let dplus_hdr_len = transcode_voice(
        ProtocolKind::DPlus,
        &event_start.event,
        event_start.cached_header.as_ref(),
        &mut out,
    )?;
    assert_eq!(dplus_hdr_len, 58, "DPlus voice header is 58 bytes");
    let dcs_hdr_len = transcode_voice(
        ProtocolKind::Dcs,
        &event_start.event,
        event_start.cached_header.as_ref(),
        &mut out,
    )?;
    assert_eq!(dcs_hdr_len, 100, "DCS voice frame is 100 bytes");

    // Two voice data frames. Each should publish a Frame event on
    // the bus, and each should re-encode cleanly in both target
    // protocols.
    let frame = VoiceFrame::silence();
    for seq in 0_u8..2 {
        let mut data_buf = [0u8; 64];
        let data_n = encode_voice_data(&mut data_buf, sid(), seq, &frame)?;
        let data_slice = data_buf.get(..data_n).ok_or("data bytes out of range")?;
        drop(
            dextra_ep
                .handle_inbound(data_slice, peer_dextra, std::time::Instant::now())
                .await?,
        );

        let event_frame = rx.try_recv().ok().ok_or("Frame not published")?;
        assert_eq!(event_frame.source_protocol, ProtocolKind::DExtra);
        assert_eq!(event_frame.module, Module::C);
        assert!(matches!(event_frame.event, VoiceEvent::Frame { .. }));
        assert!(
            event_frame.cached_header.is_some(),
            "voice data frame carries cached header (for DCS targets)"
        );

        // Transcode each Frame event into both target protocols.
        let dplus_data_len = transcode_voice(
            ProtocolKind::DPlus,
            &event_frame.event,
            event_frame.cached_header.as_ref(),
            &mut out,
        )?;
        assert_eq!(dplus_data_len, 29, "DPlus voice data is 29 bytes");
        let dcs_data_len = transcode_voice(
            ProtocolKind::Dcs,
            &event_frame.event,
            event_frame.cached_header.as_ref(),
            &mut out,
        )?;
        assert_eq!(dcs_data_len, 100, "DCS voice frame is 100 bytes");
    }

    // Voice EOT. Endpoint publishes StreamEnd on the bus.
    let mut eot_buf = [0u8; 64];
    let eot_n = encode_voice_eot(&mut eot_buf, sid(), 2)?;
    let eot_slice = eot_buf.get(..eot_n).ok_or("eot bytes out of range")?;
    drop(
        dextra_ep
            .handle_inbound(eot_slice, peer_dextra, std::time::Instant::now())
            .await?,
    );

    let event_end = rx.try_recv().ok().ok_or("StreamEnd not published")?;
    assert_eq!(event_end.source_protocol, ProtocolKind::DExtra);
    assert!(matches!(event_end.event, VoiceEvent::StreamEnd { .. }));

    // StreamEnd re-encodes into both target protocols. DCS needs
    // the cached header (every DCS voice packet embeds one). The
    // cache is cleared by `update_stream_cache_dextra` on EOT, so
    // `cached_header` in the published event MAY be `None` — but
    // the test is still useful for the DPlus path which doesn't
    // need the header.
    let dplus_eot_len = transcode_voice(
        ProtocolKind::DPlus,
        &event_end.event,
        event_end.cached_header.as_ref(),
        &mut out,
    )?;
    assert_eq!(dplus_eot_len, 32, "DPlus voice eot is 32 bytes");

    // No more events on the bus.
    assert!(rx.try_recv().is_err(), "no further events after StreamEnd");
    Ok(())
}
