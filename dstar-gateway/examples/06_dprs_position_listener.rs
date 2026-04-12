#![cfg(feature = "examples-network")]
//! Listen to a busy `DPlus` reflector and log every DPRS position
//! report seen in the slow-data stream.
//!
//! Feeds each inbound voice frame's 3-byte slow-data payload into a
//! [`SlowDataAssembler`]. When the assembler returns a complete
//! [`SlowDataBlock::Gps`] (type byte `0x3X`), the example attempts
//! to parse it as a DPRS `$$CRC...` sentence via [`parse_dprs`], and
//! logs the resulting [`Latitude`] / [`Longitude`] pair.
//!
//! Runs for 60 seconds and then disconnects cleanly. Intended as a
//! minimal demo of the slowdata + dprs layers working together; the
//! production TUI builds on the same pattern.
//!
//! Gated behind the `examples-network` feature.
//!
//! ```text
//! REFLECTOR_HOST=ref030.example.com:20001 \
//!     cargo run -p dstar-gateway --example 06_dprs_position_listener \
//!     --features examples-network
//! ```

use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dstar_gateway::auth::AuthClient;
use dstar_gateway::tokio_shell::AsyncSession;
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{
    Authenticated, ClientStateKind, Configured, Connecting, DPlus, Event, Session,
};
use dstar_gateway_core::slowdata::{SlowDataAssembler, SlowDataBlock};
use dstar_gateway_core::types::{Callsign, Module, StreamId};
use dstar_gateway_core::{DprsReport, parse_dprs};
use tokio::net::UdpSocket;
use tokio::time::timeout;

// Acknowledged workspace dev-deps.
use pcap_parser as _;
use trybuild as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let callsign = Callsign::try_from_str("W1AW")?;
    let reflector_host =
        env::var("REFLECTOR_HOST").unwrap_or_else(|_| "ref030.example.com:20001".to_string());

    // 1. `DPlus` requires TCP auth before the UDP handshake.
    let auth_client = AuthClient::new();
    let host_list = auth_client.authenticate(callsign).await?;

    // 2. Resolve peer + bind socket.
    let peer = tokio::net::lookup_host(&reflector_host)
        .await?
        .next()
        .ok_or("resolve")?;
    let sock = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

    // 3. Typestate handshake.
    let session: Session<DPlus, Configured> = Session::<DPlus, Configured>::builder()
        .callsign(callsign)
        .local_module(Module::try_from_char('B')?)
        .reflector_module(Module::try_from_char('C')?)
        .peer(peer)
        .build();
    let authed: Session<DPlus, Authenticated> = session
        .authenticate(host_list)
        .map_err(|f| format!("authenticate: {}", f.error))?;
    let mut connecting: Session<DPlus, Connecting> = authed
        .connect(Instant::now())
        .map_err(|f| format!("connect: {}", f.error))?;

    for _ in 0..4_u8 {
        if let Some(tx) = connecting.poll_transmit(Instant::now()) {
            let _ = sock.send_to(tx.payload, tx.dst).await?;
        }
        let mut buf = [0u8; 256];
        let Ok(recv) = timeout(Duration::from_secs(5), sock.recv_from(&mut buf)).await else {
            eprintln!("timeout on handshake");
            return Ok(());
        };
        let (n, src) = recv?;
        connecting.handle_input(Instant::now(), src, &buf[..n])?;
        if connecting.state_kind() == ClientStateKind::Connected {
            break;
        }
    }
    if connecting.state_kind() != ClientStateKind::Connected {
        eprintln!("handshake did not complete");
        return Ok(());
    }

    let mut async_session = AsyncSession::spawn(
        connecting
            .promote()
            .map_err(|f| format!("promote: {}", f.error))?,
        Arc::clone(&sock),
    );

    // 4. Per-stream slow-data assemblers. Each stream id gets its
    //    own accumulator so interleaved streams don't corrupt each
    //    other's blocks.
    let mut assemblers: HashMap<StreamId, SlowDataAssembler> = HashMap::new();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => break,
            ev = async_session.next_event() => {
                let Some(event) = ev else { break };
                match event {
                    Event::VoiceFrame { stream_id, frame, .. } => {
                        let asm = assemblers.entry(stream_id).or_default();
                        if let Some(block) = asm.push(frame.slow_data) {
                            // Only GPS blocks carry DPRS sentences; other
                            // kinds (text, header retx, squelch) are logged
                            // at debug level but not parsed further.
                            match block {
                                SlowDataBlock::Gps(sentence) => {
                                    match parse_dprs(&sentence) {
                                        Ok(report) => log_report(&report),
                                        Err(e) => tracing::debug!(?e, sentence, "dprs parse failed"),
                                    }
                                }
                                other => tracing::debug!(?other, "non-gps slow data block"),
                            }
                        }
                    }
                    Event::VoiceEnd { stream_id, .. } => {
                        let _ = assemblers.remove(&stream_id);
                    }
                    _ => tracing::debug!(?event, "event"),
                }
            }
        }
    }

    async_session.disconnect().await?;
    Ok(())
}

/// Pretty-print one parsed DPRS report.
fn log_report(report: &DprsReport) {
    tracing::info!(
        callsign = %report.callsign.as_str().trim(),
        lat_deg = report.latitude.degrees(),
        lon_deg = report.longitude.degrees(),
        symbol = %report.symbol,
        comment = ?report.comment,
        "dprs position"
    );
}
