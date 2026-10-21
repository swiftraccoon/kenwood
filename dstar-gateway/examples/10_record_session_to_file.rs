#![cfg(feature = "examples-network")]
//! Record post-handshake UDP datagrams to a newly created binary file.
//!
//! Each record is a four-byte big-endian length followed by exact bytes.
//! `OUTPUT` defaults to `session.bin`; existing files are refused. The capture
//! lasts 60 seconds, then awaits unlink and background task completion.
//! The pcap conformance harness requires a separate framing adapter.
//!
//! ```text
//! DSTAR_CALLSIGN=N0CALL REFLECTOR_HOST=127.0.0.1:30001 OUTPUT=session.bin \
//! cargo run -p dstar-gateway --example 10_record_session_to_file --features examples-network
//! ```

#[cfg(feature = "insecure-plaintext-xlx-directory")]
use reqwest as _;

use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dstar_gateway::tokio_shell::{AsyncSession, drive_connecting};
use dstar_gateway_core::Driver;
use dstar_gateway_core::session::client::{Connected, DExtra, Session};
use dstar_gateway_core::{Module, RoutingCallsign};
use tokio::net::UdpSocket;

use pcap_parser as _;
use thiserror as _;
use trybuild as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let callsign = RoutingCallsign::new(&env::var("DSTAR_CALLSIGN")?)?;
    let host = env::var("REFLECTOR_HOST")?;
    let output = PathBuf::from(env::var("OUTPUT").unwrap_or_else(|_| "session.bin".to_owned()));
    let file = File::options().write(true).create_new(true).open(&output)?;
    let mut writer = BufWriter::new(file);
    let peer = tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host(host))
        .await??
        .next()
        .ok_or("reflector hostname resolved to no addresses")?;
    let bind = if peer.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = Arc::new(UdpSocket::bind(bind).await?);
    let connecting = Session::<DExtra, _>::builder()
        .callsign(callsign)
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer(peer)
        .build()
        .connect(Instant::now())?;
    let mut connected = drive_connecting(connecting, &socket, Duration::from_secs(5)).await?;
    let result = record(&mut connected, &socket, &mut writer).await;
    let mut client = AsyncSession::spawn(connected, socket);
    let shutdown = client.disconnect().await;
    if let Err(error) = &shutdown {
        tracing::warn!(%error, "capture unlink failed");
    }
    result?;
    shutdown?;
    tracing::info!(path = %output.display(), "recording complete");
    Ok(())
}

/// Record received bytes while driving due core work; blocking file writes
/// run on this example's runtime thread and every write/flush error is returned.
async fn record(
    session: &mut Session<DExtra, Connected>,
    socket: &UdpSocket,
    writer: &mut BufWriter<File>,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut buffer = vec![0_u8; 65_536].into_boxed_slice();
    loop {
        session.handle_timeout(Instant::now());
        while let Some(transmit) = session.poll_transmit(Instant::now()) {
            let _sent =
                tokio::time::timeout_at(deadline, socket.send_to(transmit.payload, transmit.dst))
                    .await??;
        }
        while let Some(event) = session.poll_event() {
            tracing::debug!(?event, "capture event");
        }
        let wake = session.poll_timeout().map_or(deadline, |wake| {
            tokio::time::Instant::from_std(wake).min(deadline)
        });
        match tokio::time::timeout_at(wake, socket.recv_from(&mut buffer)).await {
            Ok(Ok((length, source))) => {
                let bytes = buffer.get(..length).ok_or("invalid receive byte count")?;
                write_record(writer, bytes)?;
                session.handle_input(Instant::now(), source, bytes)?;
            }
            Ok(Err(error)) => return Err(error.into()),
            Err(_) if tokio::time::Instant::now() >= deadline => break,
            Err(_) => {}
        }
    }
    writer.flush()?;
    Ok(())
}

/// Write one complete length-prefixed record with fallible size conversion.
fn write_record<W: Write>(writer: &mut W, bytes: &[u8]) -> std::io::Result<()> {
    let length = u32::try_from(bytes.len()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "record too large for u32")
    })?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(bytes)
}
