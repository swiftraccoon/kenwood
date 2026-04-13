//! End-to-end TX queue space-gating tests.
//!
//! Unit tests on [`TxQueue`](mmdvm::tokio_shell::TxQueue) itself live
//! inline in `src/tokio_shell/tx_queue.rs`. These integration tests
//! drive the full [`AsyncModem`] loop through a fake modem transport
//! and verify the behavior a consumer would actually observe: queued
//! frames release only when the modem reports enough FIFO space.

use std::time::Duration;

use mmdvm::AsyncModem;
use mmdvm_core::{
    MMDVM_DSTAR_DATA, MMDVM_DSTAR_EOT, MMDVM_DSTAR_HEADER, MMDVM_GET_STATUS, MmdvmFrame,
    decode_frame, encode_frame,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::time::timeout;

// Acknowledge workspace dev-deps so `-D unused-crate-dependencies`
// doesn't fire across each integration binary.
use thiserror as _;
use tracing as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Build a 4096-byte duplex + split.
fn duplex_pair() -> (DuplexStream, DuplexStream) {
    tokio::io::duplex(4096)
}

/// Drain every complete MMDVM frame the client has written so far.
async fn drain_frames(stream: &mut DuplexStream, deadline: Duration) -> Vec<MmdvmFrame> {
    let mut buf = Vec::with_capacity(4096);
    let mut out = Vec::new();
    let mut scratch = [0u8; 512];
    let deadline_at = tokio::time::Instant::now() + deadline;

    loop {
        let remaining = deadline_at.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match timeout(remaining, stream.read(&mut scratch)).await {
            Ok(Ok(0) | Err(_)) | Err(_) => break,
            Ok(Ok(n)) => {
                if let Some(slice) = scratch.get(..n) {
                    buf.extend_from_slice(slice);
                }
            }
        }
        loop {
            match decode_frame(&buf) {
                Ok(Some((frame, consumed))) => {
                    out.push(frame);
                    drop(buf.drain(..consumed));
                }
                Ok(None) => break,
                Err(_) => {
                    if buf.is_empty() {
                        break;
                    }
                    let _discarded = buf.remove(0);
                }
            }
        }
    }
    out
}

async fn modem_write(stream: &mut DuplexStream, frame: &MmdvmFrame) -> TestResult {
    let bytes = encode_frame(frame)?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

/// v2 status payload with the given `dstar_space`.
fn status_v2(dstar_space: u8) -> Vec<u8> {
    //  mode=DStar(1), state=0, reserved=0, dstar=N, dmr1=0, dmr2=0,
    //  ysf=0, p25=0, nxdn=0
    vec![1, 0, 0, dstar_space, 0, 0, 0, 0, 0]
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn zero_space_means_no_header_drained() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    // Drain the initial handshake.
    let _init = drain_frames(&mut modem_side, Duration::from_millis(100)).await;

    // Report zero space — the loop must not emit our header.
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, status_v2(0)),
    )
    .await?;

    modem.send_dstar_header([1u8; 41]).await?;

    // Advance a full second of playout + status ticks.
    tokio::time::advance(Duration::from_millis(1000)).await;
    let frames = drain_frames(&mut modem_side, Duration::from_millis(100)).await;

    assert!(
        !frames.iter().any(|f| f.command == MMDVM_DSTAR_HEADER),
        "header MUST NOT drain with 0 dstar_space: {:?}",
        frames.iter().map(|f| f.command).collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn header_needs_four_slots() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init = drain_frames(&mut modem_side, Duration::from_millis(100)).await;

    // Report 3 — below the 4-slot header threshold.
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, status_v2(3)),
    )
    .await?;
    modem.send_dstar_header([2u8; 41]).await?;

    tokio::time::advance(Duration::from_millis(200)).await;
    let frames = drain_frames(&mut modem_side, Duration::from_millis(100)).await;
    assert!(
        !frames.iter().any(|f| f.command == MMDVM_DSTAR_HEADER),
        "header must NOT drain with only 3 slots: {:?}",
        frames.iter().map(|f| f.command).collect::<Vec<_>>()
    );

    // Now bump space to 4 — header should drain on next playout tick.
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, status_v2(4)),
    )
    .await?;
    // Playout tick is 10 ms; give it a few cycles.
    tokio::time::advance(Duration::from_millis(200)).await;
    let frames = drain_frames(&mut modem_side, Duration::from_millis(100)).await;
    assert!(
        frames.iter().any(|f| f.command == MMDVM_DSTAR_HEADER),
        "header must drain once 4 slots reported: {:?}",
        frames.iter().map(|f| f.command).collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn data_needs_one_slot() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init = drain_frames(&mut modem_side, Duration::from_millis(100)).await;

    // 1 slot — enough for a data frame but not a header.
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, status_v2(1)),
    )
    .await?;
    modem.send_dstar_data([3u8; 12]).await?;

    tokio::time::advance(Duration::from_millis(200)).await;
    let frames = drain_frames(&mut modem_side, Duration::from_millis(100)).await;
    assert!(
        frames.iter().any(|f| f.command == MMDVM_DSTAR_DATA),
        "data frame must drain with 1 slot: {:?}",
        frames.iter().map(|f| f.command).collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn eot_needs_one_slot() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init = drain_frames(&mut modem_side, Duration::from_millis(100)).await;

    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, status_v2(1)),
    )
    .await?;
    modem.send_dstar_eot().await?;

    tokio::time::advance(Duration::from_millis(200)).await;
    let frames = drain_frames(&mut modem_side, Duration::from_millis(100)).await;
    assert!(
        frames.iter().any(|f| f.command == MMDVM_DSTAR_EOT),
        "EOT must drain with 1 slot: {:?}",
        frames.iter().map(|f| f.command).collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn fifo_order_preserved_end_to_end() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init = drain_frames(&mut modem_side, Duration::from_millis(100)).await;

    // Report plenty of space.
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, status_v2(20)),
    )
    .await?;

    modem.send_dstar_header([1u8; 41]).await?;
    modem.send_dstar_data([2u8; 12]).await?;
    modem.send_dstar_data([3u8; 12]).await?;
    modem.send_dstar_eot().await?;

    tokio::time::advance(Duration::from_millis(200)).await;
    let frames = drain_frames(&mut modem_side, Duration::from_millis(200)).await;

    // Collect only the D-STAR-related commands (filter out the GetStatus
    // pokes that fire on the 250 ms timer).
    let seq: Vec<u8> = frames
        .iter()
        .filter_map(|f| match f.command {
            MMDVM_DSTAR_HEADER | MMDVM_DSTAR_DATA | MMDVM_DSTAR_EOT => Some(f.command),
            _ => None,
        })
        .collect();
    assert_eq!(
        seq,
        vec![
            MMDVM_DSTAR_HEADER,
            MMDVM_DSTAR_DATA,
            MMDVM_DSTAR_DATA,
            MMDVM_DSTAR_EOT
        ],
        "FIFO order must be preserved end-to-end"
    );
    Ok(())
}
