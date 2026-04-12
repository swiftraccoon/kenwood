#![cfg(feature = "examples-network")]
//! Bridge voice traffic between two `DExtra` reflectors (A <-> B).
//!
//! Spawns two `AsyncSession<DExtra>` connections — one to "reflector
//! A" and one to "reflector B" — and forwards every inbound voice
//! event from A to B and vice versa. A `tokio::select!` over both
//! event streams keeps the forwarding fair (one call from A does not
//! starve the next call from B).
//!
//! This is the minimal form of the "bridge" pattern from the
//! `dstar-gateway` specification section 7; a production bridge adds
//! loop detection (don't forward traffic that originated on the
//! other side), per-module policy, and transcoding for mixed
//! protocols. For clarity, this example omits all of that — it
//! just illustrates the two-session wiring.
//!
//! Gated behind the `examples-network` feature.
//!
//! ```text
//! REFLECTOR_A=xrf030.example.com:30001 \
//! REFLECTOR_B=xrf040.example.com:30001 \
//!     cargo run -p dstar-gateway --example 08_bridge_two_reflectors \
//!     --features examples-network
//! ```

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
use trybuild as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let callsign = Callsign::try_from_str("W1AW")?;
    let reflector_a =
        env::var("REFLECTOR_A").unwrap_or_else(|_| "xrf030.example.com:30001".to_string());
    let reflector_b =
        env::var("REFLECTOR_B").unwrap_or_else(|_| "xrf040.example.com:30001".to_string());

    // Connect to both reflectors in parallel to minimize startup
    // latency. If either fails the example aborts — a production
    // bridge would retry the failed side while keeping the other
    // side open.
    let (session_a, session_b) = tokio::try_join!(
        connect(callsign, &reflector_a, 'C'),
        connect(callsign, &reflector_b, 'C'),
    )?;
    let mut async_a = session_a;
    let mut async_b = session_b;

    tracing::info!("bridge up — forwarding both directions");

    // Forwarding loop. On every `VoiceStart` we record the header
    // for that stream; on subsequent `VoiceFrame`s we push the frame
    // through to the other side; on `VoiceEnd` we send an EOT. The
    // outbound stream id is preserved from the inbound id so a
    // reflector-side dedup by stream id still works.
    let mut tx_header_a_to_b: Option<(StreamId, DStarHeader)> = None;
    let mut tx_header_b_to_a: Option<(StreamId, DStarHeader)> = None;

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
                    &mut tx_header_a_to_b,
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
                    &mut tx_header_b_to_a,
                )
                .await?;
            }
        }
    }

    // Clean shutdown, ignoring errors because either side may
    // already be dead.
    let _ = async_a.disconnect().await;
    let _ = async_b.disconnect().await;
    Ok(())
}

/// Forward one event from the RX side to the TX side, tracking the
/// current stream header so `send_voice` / `send_eot` can reference
/// it if needed (DCS would; `DExtra` does not — but the pattern scales
/// to any TX side).
async fn forward(
    event: &Event<DExtra>,
    tx: &mut AsyncSession<DExtra>,
    tx_header: &mut Option<(StreamId, DStarHeader)>,
) -> Result<(), Box<dyn std::error::Error>> {
    match event {
        Event::VoiceStart {
            stream_id, header, ..
        } => {
            *tx_header = Some((*stream_id, *header));
            tx.send_header(*header, *stream_id).await?;
        }
        Event::VoiceFrame {
            stream_id,
            seq,
            frame,
        } => {
            // Forward the frame verbatim. `VoiceFrame` is `Copy` so
            // this is a bitwise move, no alloc.
            let outgoing: VoiceFrame = *frame;
            tx.send_voice(*stream_id, *seq, outgoing).await?;
        }
        Event::VoiceEnd { stream_id, .. } => {
            // If we had a matching header cached, emit an EOT on the
            // TX side. Seq on EOT is advisory — MMDVMHost uses 0 in
            // the common case, which the core codec accepts.
            if let Some((sid, _)) = tx_header.take()
                && sid == *stream_id
            {
                tx.send_eot(*stream_id, 0).await?;
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
    connecting.handle_input(Instant::now(), src, &buf[..n])?;
    if connecting.state_kind() != ClientStateKind::Connected {
        return Err("handshake did not complete".into());
    }
    let connected: Session<DExtra, Connected> = connecting
        .promote()
        .map_err(|f| format!("promote: {}", f.error))?;
    Ok(AsyncSession::spawn(connected, sock))
}
