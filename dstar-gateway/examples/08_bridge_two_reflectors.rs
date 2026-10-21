#![cfg(feature = "examples-network")]
//! Bridge voice traffic between two `DExtra` reflectors (A <-> B).
//!
//! Spawns two `AsyncSession<DExtra>` connections, one to "reflector
//! A" and one to "reflector B", and forwards every inbound voice
//! event from A to B and vice versa. A `tokio::select!` over both
//! event streams keeps the forwarding fair (one call from A does not
//! starve the next call from B).
//!
//! This is the minimal form of a reflector-to-reflector "bridge".
//! It rewrites routing headers for each destination, but deliberately
//! omits loop detection, per-module policy, and transcoding. Run it
//! only against controlled endpoints where you have permission to
//! forward traffic; do not point both sides at public reflectors.
//!
//! Gated behind the `examples-network` feature.
//!
//! ```text
//! DSTAR_CALLSIGN=N0CALL ACTUALLY_BRIDGE=1 \
//! REFLECTOR_A=xrf030.example.com:30001 REFLECTOR_A_CALLSIGN=XRF030 \
//! REFLECTOR_B=xrf040.example.com:30001 \
//! REFLECTOR_B_CALLSIGN=XRF040 \
//!     cargo run -p dstar-gateway --example 08_bridge_two_reflectors \
//!     --features examples-network
//! ```

#[cfg(feature = "insecure-plaintext-xlx-directory")]
use reqwest as _;

use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dstar_gateway::tokio_shell::{AsyncSession, drive_connecting};
use dstar_gateway_core::RelayRouting;
use dstar_gateway_core::header::DstarHeader;
use dstar_gateway_core::session::client::{Configured, DExtra, Event, Session};
use dstar_gateway_core::types::{FrameSequence, Module, RoutingCallsign, StreamId};
use dstar_gateway_core::voice::VoiceFrame;
use tokio::net::UdpSocket;

// Acknowledged workspace dev-deps.
use pcap_parser as _;
use thiserror as _;
use trybuild as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    if env::var("ACTUALLY_BRIDGE").ok().as_deref() != Some("1") {
        eprintln!(
            "refusing to forward traffic: set ACTUALLY_BRIDGE=1 after reviewing the safety notes"
        );
        return Ok(());
    }

    let callsign = RoutingCallsign::new(&env::var("DSTAR_CALLSIGN")?)?;
    let reflector_a =
        env::var("REFLECTOR_A").unwrap_or_else(|_| "xrf030.example.com:30001".to_string());
    let reflector_b =
        env::var("REFLECTOR_B").unwrap_or_else(|_| "xrf040.example.com:30001".to_string());
    let reflector_callsigns = (
        RoutingCallsign::new(
            &env::var("REFLECTOR_A_CALLSIGN").unwrap_or_else(|_| "XRF030".to_string()),
        )?,
        RoutingCallsign::new(
            &env::var("REFLECTOR_B_CALLSIGN").unwrap_or_else(|_| "XRF040".to_string()),
        )?,
    );

    // Connect to both reflectors in parallel to minimize startup
    // latency. If either fails the example aborts; a production
    // bridge would retry the failed side while keeping the other
    // side open.
    let (session_a, session_b) = tokio::try_join!(
        connect(callsign, &reflector_a, Module::C),
        connect(callsign, &reflector_b, Module::C),
    )?;
    let mut async_a = session_a;
    let mut async_b = session_b;
    let route_to_a = RelayRouting {
        operator: callsign,
        local_module: Module::B,
        reflector: reflector_callsigns.0,
        reflector_module: Module::C,
    };
    let route_to_b = RelayRouting {
        operator: callsign,
        local_module: Module::B,
        reflector: reflector_callsigns.1,
        reflector_module: Module::C,
    };

    tracing::info!("bridge up, forwarding both directions");

    // Forwarding loop. On every `VoiceStart` we record the header
    // for that stream; on subsequent `VoiceFrame`s we push the frame
    // through to the other side; on `VoiceEnd` we send an EOT. The
    // outbound stream id is preserved from the inbound id so a
    // reflector-side dedup by stream id still works.
    let mut tx_stream_a_to_b: Option<StreamId> = None;
    let mut tx_stream_b_to_a: Option<StreamId> = None;

    let forwarding = async {
        loop {
            tokio::select! {
                ev = async_a.next_event() => {
                    let Some(event) = ev else {
                        tracing::warn!("A stream closed");
                        break;
                    };
                    forward(
                        &event,
                        &mut async_b,
                        &mut tx_stream_a_to_b,
                        route_to_b,
                    )
                    .await?;
                }
                ev = async_b.next_event() => {
                    let Some(event) = ev else {
                        tracing::warn!("B stream closed");
                        break;
                    };
                    forward(
                        &event,
                        &mut async_a,
                        &mut tx_stream_b_to_a,
                        route_to_a,
                    )
                    .await?;
                }
            }
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let (left, right) = tokio::join!(async_a.disconnect(), async_b.disconnect());
    for result in [&left, &right] {
        if let Err(error) = result {
            tracing::warn!(%error, "bridge unlink failed");
        }
    }
    forwarding?;
    left?;
    right?;
    Ok(())
}

/// Forward one event from the RX side to the TX side, rewriting the
/// header for the destination reflector and tracking the active stream.
async fn forward(
    event: &Event<DExtra>,
    tx: &mut AsyncSession<DExtra>,
    tx_stream: &mut Option<StreamId>,
    route: RelayRouting,
) -> Result<(), Box<dyn std::error::Error>> {
    match event {
        Event::VoiceStart {
            stream_id, header, ..
        } => {
            let outbound_header = DstarHeader::for_relay(route, header.my_call, header.my_suffix)
                .with_flags(header.flags);
            *tx_stream = Some(*stream_id);
            tx.send_header(outbound_header, *stream_id).await?;
        }
        Event::VoiceFrame {
            stream_id,
            seq,
            frame,
        } => {
            if tx_stream.is_some_and(|active| active == *stream_id) {
                let Some(index) = FrameSequence::new(*seq) else {
                    tracing::warn!(
                        sequence = seq,
                        "dropping a received frame with an invalid transmit index"
                    );
                    return Ok(());
                };
                let outgoing: VoiceFrame = *frame;
                tx.send_voice(*stream_id, index, outgoing).await?;
            }
        }
        Event::VoiceEnd { stream_id, .. } => {
            // If a matching header is cached, emit an EOT on the
            // TX side. Seq on EOT is advisory: MMDVMHost uses 0 in
            // the common case, which the core codec accepts.
            if *tx_stream == Some(*stream_id) {
                tx.send_eot(*stream_id, FrameSequence::ZERO).await?;
                *tx_stream = None;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Spawn one `AsyncSession<DExtra>` to the named reflector.
async fn connect(
    callsign: RoutingCallsign,
    reflector_host: &str,
    module: Module,
) -> Result<AsyncSession<DExtra>, Box<dyn std::error::Error>> {
    let peer = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::lookup_host(reflector_host),
    )
    .await??
    .next()
    .ok_or("resolve")?;
    let bind = if peer.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let sock = Arc::new(UdpSocket::bind(bind).await?);

    let session: Session<DExtra, Configured> = Session::<DExtra, Configured>::builder()
        .callsign(callsign)
        .local_module(Module::try_from_char('B')?)
        .reflector_module(module)
        .peer(peer)
        .build();

    let connecting = session.connect(Instant::now())?;
    let connected = drive_connecting(connecting, &sock, Duration::from_secs(5)).await?;
    Ok(AsyncSession::spawn(connected, sock))
}
