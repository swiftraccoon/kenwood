// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! End-to-end: a fake `DExtra` reflector pushes one transmission and
//! the supervisor writes the three recording files.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dstar_gateway_core::codec::dextra as dextra_codec;
use dstar_gateway_core::{
    Callsign, DstarHeader, FrameSequence, HeaderFlags, Module, RepeaterIdentity, RoutingCallsign,
    StreamId, Suffix, VoiceFrame,
};
use stargazer::config::Config;
use stargazer::control::Coordinator;
use stargazer::writer::Writer;
use tokio::net::UdpSocket;

// Compilation-unit dep acknowledgements (unused_crate_dependencies):
use chrono as _;
use clap as _;
use dstar_gateway as _;
use mbelib_rs as _;
use reqwest as _;
use serde as _;
use thiserror as _;
use tracing as _;
use tracing_subscriber as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Minimal `DExtra` reflector: ACKs the 11-byte LINK, then pushes a
/// header, three voice frames, and an EOT to the linked client.
async fn fake_reflector()
-> Result<(SocketAddr, tokio::task::JoinHandle<std::io::Result<()>>), std::io::Error> {
    const SID: StreamId = StreamId::new(0x1234).unwrap();
    let sock = UdpSocket::bind("127.0.0.1:0").await?;
    let addr = sock.local_addr()?;
    let handle = tokio::spawn(async move {
        let mut buf = [0u8; 256];
        loop {
            let (n, src) = sock.recv_from(&mut buf).await?;
            if n == 11 && buf.get(9) == Some(&b'B') {
                // Acknowledge LINK, then send one transmission.
                let identity = RepeaterIdentity {
                    callsign: RoutingCallsign::new("N0CALL").map_err(std::io::Error::other)?,
                    module: Module::D,
                };
                let mut out = [0u8; 64];
                let length = dextra_codec::encode_connect_ack(&mut out, identity, Module::B)
                    .map_err(std::io::Error::other)?;
                send_packet(&sock, src, &out, length).await?;
                let header = DstarHeader {
                    flags: HeaderFlags::NONE,
                    rpt2: Callsign::from_wire_bytes(*b"XRF001 G"),
                    rpt1: Callsign::from_wire_bytes(*b"XRF001 B"),
                    ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
                    my_call: Callsign::from_wire_bytes(*b"W1AW    "),
                    my_suffix: Suffix::from_wire_bytes(*b"D75 "),
                };
                let length = dextra_codec::encode_voice_header(&mut out, SID, &header)
                    .map_err(std::io::Error::other)?;
                send_packet(&sock, src, &out, length).await?;
                for seq in 0..3u8 {
                    let vf = VoiceFrame::silence();
                    let seq = FrameSequence::new(seq)
                        .ok_or_else(|| std::io::Error::other("fixture sequence"))?;
                    let length = dextra_codec::encode_voice_data(&mut out, SID, seq, &vf)
                        .map_err(std::io::Error::other)?;
                    send_packet(&sock, src, &out, length).await?;
                }
                let seq = FrameSequence::new(3)
                    .ok_or_else(|| std::io::Error::other("fixture EOT sequence"))?;
                let length = dextra_codec::encode_voice_eot(&mut out, SID, seq)
                    .map_err(std::io::Error::other)?;
                send_packet(&sock, src, &out, length).await?;
            }
            // Ignore polls/unlinks; the test shuts the client down.
        }
    });
    Ok((addr, handle))
}

async fn send_packet(
    socket: &UdpSocket,
    peer: SocketAddr,
    bytes: &[u8],
    length: usize,
) -> std::io::Result<()> {
    let _sent = socket
        .send_to(
            bytes
                .get(..length)
                .ok_or_else(|| std::io::Error::other("encoded packet length"))?,
            peer,
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn records_one_transmission_end_to_end() -> TestResult {
    let (reflector_addr, mut reflector) = fake_reflector().await?;
    let dir = tempfile::tempdir()?;

    let config = Config::from_toml(&format!(
        r#"
callsign = "N0CALL"
recordings_dir = {}
max_capture_seconds = 60
max_concurrent_captures = 2
[[record]]
reflector = "XRF001"
protocol = "dextra"
host = "{}"
port = {}
modules = ["B"]
"#,
        toml::Value::String(dir.path().to_string_lossy().into_owned()),
        reflector_addr.ip(),
        reflector_addr.port()
    ))?;
    let writer = Arc::new(Writer::new(dir.path().to_path_buf(), true));
    let mut coordinator = Coordinator::new(config, dir.path().join("recorder.toml"), writer)?;
    coordinator.spawn_initial();

    // Wait for the JSON commit marker to appear (up to 10 s).
    let json_path = wait_for_json(dir.path().to_path_buf())
        .await?
        .ok_or("no recording appeared")?;

    let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(&json_path)?)?;
    let at = |path: &str| {
        doc.pointer(path)
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    };
    assert_eq!(at("/reflector"), "XRF001");
    assert_eq!(at("/module"), "B");
    assert_eq!(at("/protocol"), "dextra");
    assert_eq!(at("/stream_id"), "1234");
    assert_eq!(at("/header/my_callsign"), "W1AW");
    assert_eq!(at("/frames/received"), 3);
    assert_eq!(at("/end_reason"), "eot");
    assert!(json_path.with_extension("ambe").exists());
    assert!(json_path.with_extension("wav").exists());

    let completed =
        tokio::time::timeout(Duration::from_secs(5), coordinator.shutdown_all()).await?;
    let summary = completed
        .into_iter()
        .next()
        .ok_or("supervisor completion")?
        .result?;
    assert_eq!(summary.recordings_written(), 1);
    assert_eq!(summary.failed_writes(), 0);
    reflector.abort();
    match (&mut reflector).await {
        Err(error) if error.is_cancelled() => {}
        Ok(result) => result?,
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

async fn wait_for_json(base: PathBuf) -> Result<Option<PathBuf>, std::io::Error> {
    for _ in 0..100u32 {
        if let Some(found) = find_json(&base)? {
            return Ok(Some(found));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(None)
}

fn find_json(dir: &Path) -> std::io::Result<Option<PathBuf>> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            if let Some(found) = find_json(&path)? {
                return Ok(Some(found));
            }
        } else if path.extension().is_some_and(|e| e == "json") {
            return Ok(Some(path));
        }
    }
    Ok(None)
}
