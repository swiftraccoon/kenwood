// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! End-to-end: a fake `DExtra` reflector pushes one transmission and
//! the supervisor writes the three recording files.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dstar_gateway_core::codec::dextra as dextra_codec;
use dstar_gateway_core::{Callsign, DstarHeader, Module, StreamId, Suffix, VoiceFrame};
use stargazer::capture::{CaptureDurationLimit, ConcurrentCaptureLimit};
use stargazer::config::{ProtocolChoice, Target};
use stargazer::session::run_supervisor;
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
use toml as _;
use tracing as _;
use tracing_subscriber as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Minimal `DExtra` reflector: ACKs the 11-byte LINK, then pushes a
/// header, three voice frames, and an EOT to the linked client.
async fn fake_reflector() -> Result<(SocketAddr, tokio::task::JoinHandle<()>), std::io::Error> {
    let sock = UdpSocket::bind("127.0.0.1:0").await?;
    let addr = sock.local_addr()?;
    let handle = tokio::spawn(async move {
        let mut buf = [0u8; 256];
        loop {
            let Ok((n, src)) = sock.recv_from(&mut buf).await else {
                return;
            };
            if n == 11 {
                // LINK → ACK, then push one transmission.
                let refl = Callsign::from_wire_bytes(*b"XRF001  ");
                let mut out = [0u8; 64];
                if let Ok(len) = dextra_codec::encode_connect_ack(&mut out, &refl, Module::B)
                    && let Some(pkt) = out.get(..len)
                {
                    let _unused = sock.send_to(pkt, src).await;
                }

                let Some(sid) = StreamId::new(0x1234) else {
                    return;
                };
                let header = DstarHeader {
                    flag1: 0,
                    flag2: 0,
                    flag3: 0,
                    rpt2: Callsign::from_wire_bytes(*b"XRF001 G"),
                    rpt1: Callsign::from_wire_bytes(*b"XRF001 B"),
                    ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
                    my_call: Callsign::from_wire_bytes(*b"W1AW    "),
                    my_suffix: Suffix::from_wire_bytes(*b"D75 "),
                };
                if let Ok(len) = dextra_codec::encode_voice_header(&mut out, sid, &header)
                    && let Some(pkt) = out.get(..len)
                {
                    let _unused = sock.send_to(pkt, src).await;
                }
                for seq in 0..3u8 {
                    let vf = VoiceFrame::silence();
                    if let Ok(len) = dextra_codec::encode_voice_data(&mut out, sid, seq, &vf)
                        && let Some(pkt) = out.get(..len)
                    {
                        let _unused = sock.send_to(pkt, src).await;
                    }
                }
                if let Ok(len) = dextra_codec::encode_voice_eot(&mut out, sid, 3)
                    && let Some(pkt) = out.get(..len)
                {
                    let _unused = sock.send_to(pkt, src).await;
                }
            }
            // Ignore polls/unlinks; the test shuts the client down.
        }
    });
    Ok((addr, handle))
}

#[tokio::test]
async fn records_one_transmission_end_to_end() -> TestResult {
    let (reflector_addr, _reflector) = fake_reflector().await?;
    let dir = tempfile::tempdir()?;

    let target = Target {
        reflector: "XRF001".to_string(),
        reflector_callsign: Callsign::try_from_str("XRF001")?,
        protocol: ProtocolChoice::Dextra,
        host: reflector_addr.ip().to_string(),
        port: reflector_addr.port(),
        module: Module::B,
    };
    let writer = Arc::new(Writer::new(dir.path().to_path_buf(), true));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let sup = tokio::spawn(run_supervisor(
        target,
        Callsign::try_from_str("N0CALL")?,
        Module::D,
        CaptureDurationLimit::try_from_seconds(60)?,
        ConcurrentCaptureLimit::try_from_count(2)?,
        Arc::clone(&writer),
        shutdown_rx,
    ));

    // Wait for the JSON commit marker to appear (up to 10 s).
    let json_path = wait_for_json(dir.path().to_path_buf())
        .await
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

    let _unused = shutdown_tx.send(true);
    let _joined = tokio::time::timeout(Duration::from_secs(5), sup).await;
    Ok(())
}

async fn wait_for_json(base: PathBuf) -> Option<PathBuf> {
    for _ in 0..100u32 {
        if let Some(found) = find_json(&base) {
            return Some(found);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

fn find_json(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_json(&path) {
                return Some(found);
            }
        } else if path.extension().is_some_and(|e| e == "json") {
            return Some(path);
        }
    }
    None
}
