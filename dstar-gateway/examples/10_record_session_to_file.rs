#![cfg(feature = "examples-network")]
//! Capture inbound reflector traffic to a length-prefixed binary file.
//!
//! Connects to a `DExtra` reflector, then taps the raw UDP socket to
//! write every incoming datagram to `session.bin` for offline replay.
//! The file format is intentionally minimal — each record is a big-
//! endian 4-byte length followed by exactly that many bytes. The
//! conformance harness already supports a compatible shape via
//! `replay_pcap_file`, so the output can be fed back into the test
//! corpus with only a small adapter.
//!
//! The capture runs for 60 seconds and then disconnects cleanly.
//!
//! Gated behind the `examples-network` feature.
//!
//! ```text
//! REFLECTOR_HOST=xrf030.example.com:30001 OUTPUT=/tmp/session.bin \
//!     cargo run -p dstar-gateway --example 10_record_session_to_file \
//!     --features examples-network
//! ```

use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{ClientStateKind, Configured, DExtra, Session};
use dstar_gateway_core::types::{Callsign, Module};
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
        env::var("REFLECTOR_HOST").unwrap_or_else(|_| "xrf030.example.com:30001".to_string());
    let output: PathBuf = env::var("OUTPUT")
        .unwrap_or_else(|_| "session.bin".to_string())
        .into();

    // Resolve + bind.
    let peer = tokio::net::lookup_host(&reflector_host)
        .await?
        .next()
        .ok_or("resolve")?;
    let sock = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

    // Open the capture file. Use a buffered writer so every small
    // record doesn't hammer the kernel; the `drop(writer)` at the
    // end flushes implicitly, and we also flush explicitly below.
    let file = File::create(&output)?;
    let mut writer = BufWriter::new(file);
    tracing::info!("recording into {}", output.display());

    // Drive the handshake on the same socket we'll tap.
    let session: Session<DExtra, Configured> = Session::<DExtra, Configured>::builder()
        .callsign(callsign)
        .local_module(Module::try_from_char('B')?)
        .reflector_module(Module::try_from_char('C')?)
        .peer(peer)
        .build();
    let now = Instant::now();
    let mut connecting = session
        .connect(now)
        .map_err(|f| format!("connect: {}", f.error))?;
    if let Some(tx) = connecting.poll_transmit(now) {
        let _ = sock.send_to(tx.payload, tx.dst).await?;
    }

    // Recv ACK. We write the ACK to the capture too — the file
    // records EVERY inbound datagram, including the handshake reply.
    let mut buf = [0u8; 2048];
    let (n, src) = timeout(Duration::from_secs(5), sock.recv_from(&mut buf))
        .await
        .map_err(|_| "handshake timeout")??;
    write_record(&mut writer, &buf[..n])?;
    connecting.handle_input(Instant::now(), src, &buf[..n])?;
    if connecting.state_kind() != ClientStateKind::Connected {
        eprintln!("handshake did not complete");
        return Ok(());
    }

    // After the handshake we bypass the tokio shell entirely — it
    // owns the socket so we can't tap it. The tap is a pure
    // recv/drive loop that forwards bytes into the sans-io core
    // directly. This is the simplest way to tee inbound traffic.
    let mut connected = connecting
        .promote()
        .map_err(|f| format!("promote: {}", f.error))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        // Drain outbound (keepalives).
        while let Some(tx) = connected.poll_transmit(Instant::now()) {
            let _ = sock.send_to(tx.payload, tx.dst).await?;
        }
        // Wait for the next inbound packet or ~250 ms.
        let recv_fut = sock.recv_from(&mut buf);
        match timeout(Duration::from_millis(250), recv_fut).await {
            Ok(Ok((n, src))) => {
                write_record(&mut writer, &buf[..n])?;
                connected.handle_input(Instant::now(), src, &buf[..n])?;
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                // Timeout — let the session advance its internal
                // timers. No bytes to record this iteration.
                connected.handle_timeout(Instant::now());
            }
        }
        // Drain events so the session doesn't block on a full buffer.
        while let Some(event) = connected.poll_event() {
            tracing::debug!(?event, "event");
        }
    }

    writer.flush()?;
    drop(writer);
    tracing::info!("recording complete");

    // Best-effort disconnect on the sans-io session. We don't
    // unwrap the result — the recording is the point of this
    // example, so even a failing disconnect should not drop the
    // captured file on the floor.
    if let Err(e) = connected.disconnect_in_place(Instant::now()) {
        tracing::warn!(?e, "disconnect_in_place failed");
    }
    while let Some(tx) = connected.poll_transmit(Instant::now()) {
        let _ = sock.send_to(tx.payload, tx.dst).await?;
    }

    Ok(())
}

/// Write one length-prefixed record: 4-byte BE length followed by
/// `bytes`. Matches the format expected by the conformance replay
/// harness (modulo a thin adapter that wraps each record in pcap
/// framing if needed).
fn write_record<W: Write>(writer: &mut W, bytes: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(bytes.len()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "record too large for u32")
    })?;
    writer.write_all(&len.to_be_bytes())?;
    writer.write_all(bytes)?;
    Ok(())
}
