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

#[cfg(feature = "hosts-fetcher")]
use reqwest as _;

use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dstar_gateway::tokio_shell::AsyncSession;
use dstar_gateway_core::header::DStarHeader;
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{
    ClientStateKind, Configured, Connected, DExtra, Event, Session,
};
use dstar_gateway_core::types::{Callsign, Module, StreamId};
use dstar_gateway_core::voice::VoiceFrame;
use tokio::net::UdpSocket;
use tokio::time::timeout;

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

    let callsign = Callsign::try_from_str(&env::var("DSTAR_CALLSIGN")?)?;
    let reflector_a =
        env::var("REFLECTOR_A").unwrap_or_else(|_| "xrf030.example.com:30001".to_string());
    let reflector_b =
        env::var("REFLECTOR_B").unwrap_or_else(|_| "xrf040.example.com:30001".to_string());
    let reflector_callsigns = (
        Callsign::try_from_str(
            &env::var("REFLECTOR_A_CALLSIGN").unwrap_or_else(|_| "XRF030".to_string()),
        )?,
        Callsign::try_from_str(
            &env::var("REFLECTOR_B_CALLSIGN").unwrap_or_else(|_| "XRF040".to_string()),
        )?,
    );

    // Connect to both reflectors in parallel to minimize startup
    // latency. If either fails the example aborts; a production
    // bridge would retry the failed side while keeping the other
    // side open.
    let (session_a, session_b) = tokio::try_join!(
        connect(callsign, &reflector_a, 'C'),
        connect(callsign, &reflector_b, 'C'),
    )?;
    let mut async_a = session_a;
    let mut async_b = session_b;
    let route_to_a = RelayRoute {
        operator: callsign,
        local_module: Module::B,
        reflector: reflector_callsigns.0,
        reflector_module: Module::C,
    };
    let route_to_b = RelayRoute {
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

    // Clean shutdown, ignoring errors because either side may
    // already be dead.
    drop(async_a.disconnect().await);
    drop(async_b.disconnect().await);
    Ok(())
}

#[derive(Clone, Copy)]
struct RelayRoute {
    operator: Callsign,
    local_module: Module,
    reflector: Callsign,
    reflector_module: Module,
}

/// Forward one event from the RX side to the TX side, rewriting the
/// header for the destination reflector and tracking the active stream.
async fn forward(
    event: &Event<DExtra>,
    tx: &mut AsyncSession<DExtra>,
    tx_stream: &mut Option<StreamId>,
    route: RelayRoute,
) -> Result<(), Box<dyn std::error::Error>> {
    match event {
        Event::VoiceStart {
            stream_id, header, ..
        } => {
            let mut outbound_header = DStarHeader::for_relay(
                route.operator,
                route.local_module,
                route.reflector,
                route.reflector_module,
                header.my_call,
                header.my_suffix,
            );
            outbound_header.flag1 = header.flag1;
            outbound_header.flag2 = header.flag2;
            outbound_header.flag3 = header.flag3;
            *tx_stream = Some(*stream_id);
            tx.send_header(outbound_header, *stream_id).await?;
        }
        Event::VoiceFrame {
            stream_id,
            seq,
            frame,
        } => {
            if tx_stream.is_some_and(|active| active == *stream_id) {
                // Forward the frame verbatim. `VoiceFrame` is `Copy` so
                // this is a bitwise move, no alloc.
                let outgoing: VoiceFrame = *frame;
                tx.send_voice(*stream_id, *seq, outgoing).await?;
            }
        }
        Event::VoiceEnd { stream_id, .. } => {
            // If we had a matching header cached, emit an EOT on the
            // TX side. Seq on EOT is advisory: MMDVMHost uses 0 in
            // the common case, which the core codec accepts.
            if *tx_stream == Some(*stream_id) {
                tx.send_eot(*stream_id, 0).await?;
                *tx_stream = None;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Spawn one `AsyncSession<DExtra>` to the named reflector.
async fn connect(
    callsign: Callsign,
    reflector_host: &str,
    module_char: char,
) -> Result<AsyncSession<DExtra>, Box<dyn std::error::Error>> {
    let peer = tokio::net::lookup_host(reflector_host)
        .await?
        .next()
        .ok_or("resolve")?;
    let sock = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

    let session: Session<DExtra, Configured> = Session::<DExtra, Configured>::builder()
        .callsign(callsign)
        .local_module(Module::try_from_char('B')?)
        .reflector_module(Module::try_from_char(module_char)?)
        .peer(peer)
        .build();

    let now = Instant::now();
    let mut connecting = session
        .connect(now)
        .map_err(|f| format!("connect: {}", f.error))?;
    if let Some(tx) = connecting.poll_transmit(now) {
        let _ = sock.send_to(tx.payload, tx.dst).await?;
    }
    let mut buf = [0u8; 64];
    let (n, src) = timeout(Duration::from_secs(5), sock.recv_from(&mut buf))
        .await
        .map_err(|_| "handshake timeout")??;
    let slice = buf.get(..n).unwrap_or(&[]);
    connecting.handle_input(Instant::now(), src, slice)?;
    if connecting.state_kind() != ClientStateKind::Connected {
        return Err("handshake did not complete".into());
    }
    let connected: Session<DExtra, Connected> = connecting
        .promote()
        .map_err(|f| format!("promote: {}", f.error))?;
    Ok(AsyncSession::spawn(connected, sock))
}
