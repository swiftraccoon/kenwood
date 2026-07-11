//! End-to-end TX queue space-gating tests.
//!
//! Unit tests on [`TxQueue`](mmdvm::tokio_shell::TxQueue) itself live
//! inline in `src/tokio_shell/tx_queue.rs`. These integration tests
//! drive the full [`AsyncModem`] loop through a fake modem transport
//! and verify the behavior a consumer would actually observe: queued
//! frames release only when the modem reports enough FIFO space.

use std::time::Duration;

use mmdvm::{AsyncModem, ShellError};
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
    //  ysf=0, p25=0, nxdn=0, reserved=0, fm=0, pocsag=0
    vec![1, 0, 0, dstar_space, 0, 0, 0, 0, 0, 0, 0, 0]
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
async fn header_needs_five_slots() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init = drain_frames(&mut modem_side, Duration::from_millis(100)).await;

    // Report exactly 4 — the reference requires MORE than the header's
    // 4-slot cost (Modem.cpp:1053 `m_dstarSpace > 4U`), so nothing
    // may drain yet.
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, status_v2(4)),
    )
    .await?;
    modem.send_dstar_header([2u8; 41]).await?;

    tokio::time::advance(Duration::from_millis(200)).await;
    let frames = drain_frames(&mut modem_side, Duration::from_millis(100)).await;
    assert!(
        !frames.iter().any(|f| f.command == MMDVM_DSTAR_HEADER),
        "header must NOT drain with only 4 slots: {:?}",
        frames.iter().map(|f| f.command).collect::<Vec<_>>()
    );

    // Now bump space to 5 — header should drain on the next playout
    // tick.
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, status_v2(5)),
    )
    .await?;
    tokio::time::advance(Duration::from_millis(200)).await;
    let frames = drain_frames(&mut modem_side, Duration::from_millis(100)).await;
    assert!(
        frames.iter().any(|f| f.command == MMDVM_DSTAR_HEADER),
        "header must drain once 5 slots reported: {:?}",
        frames.iter().map(|f| f.command).collect::<Vec<_>>()
    );
    Ok(())
}

/// The local space ledger must deplete between status polls.
///
/// The modem reports FIFO space only in status replies; between polls
/// the loop must debit its local `dstar_space` for every frame it
/// drains (`MMDVMHost`'s `Modem.cpp` keeps the same local ledger).
/// Without the debit, a burst queued between polls would all drain at
/// the last-reported space and overrun the modem's TX FIFO. Every
/// other test in this file refreshes status before its expectation,
/// so this is the only test that fails if the debit disappears.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn local_space_depletes_between_status_polls() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init = drain_frames(&mut modem_side, Duration::from_millis(100)).await;

    // One status report: 3 slots. The fake modem then stays silent,
    // so the loop has only its local ledger to go on.
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, status_v2(3)),
    )
    .await?;

    // Queue three data frames. Data drains only while space exceeds
    // one slot, so exactly two may go out (3 → 2 → 1, hold at 1).
    modem.send_dstar_data([1u8; 12]).await?;
    modem.send_dstar_data([2u8; 12]).await?;
    modem.send_dstar_data([3u8; 12]).await?;

    tokio::time::advance(Duration::from_millis(1000)).await;
    let frames = drain_frames(&mut modem_side, Duration::from_millis(100)).await;
    let data_count = frames
        .iter()
        .filter(|f| f.command == MMDVM_DSTAR_DATA)
        .count();
    assert_eq!(
        data_count,
        2,
        "exactly two data frames may drain from 3 reported slots: {:?}",
        frames.iter().map(|f| f.command).collect::<Vec<_>>()
    );

    // A fresh status report replenishes the ledger — the held frame
    // drains on the next playout tick.
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, status_v2(3)),
    )
    .await?;
    tokio::time::advance(Duration::from_millis(500)).await;
    let frames = drain_frames(&mut modem_side, Duration::from_millis(100)).await;
    let data_count = frames
        .iter()
        .filter(|f| f.command == MMDVM_DSTAR_DATA)
        .count();
    assert_eq!(
        data_count, 1,
        "the held frame drains after a fresh status report"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn data_needs_two_slots() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init = drain_frames(&mut modem_side, Duration::from_millis(100)).await;

    // 1 slot — below the strict `> 1` data gate (Modem.cpp:1054).
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, status_v2(1)),
    )
    .await?;
    modem.send_dstar_data([3u8; 12]).await?;

    tokio::time::advance(Duration::from_millis(200)).await;
    let frames = drain_frames(&mut modem_side, Duration::from_millis(100)).await;
    assert!(
        !frames.iter().any(|f| f.command == MMDVM_DSTAR_DATA),
        "data frame must NOT drain with only 1 slot: {:?}",
        frames.iter().map(|f| f.command).collect::<Vec<_>>()
    );

    // 2 slots satisfy the strict gate.
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, status_v2(2)),
    )
    .await?;
    tokio::time::advance(Duration::from_millis(200)).await;
    let frames = drain_frames(&mut modem_side, Duration::from_millis(100)).await;
    assert!(
        frames.iter().any(|f| f.command == MMDVM_DSTAR_DATA),
        "data frame must drain with 2 slots: {:?}",
        frames.iter().map(|f| f.command).collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn eot_needs_two_slots() -> TestResult {
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
        !frames.iter().any(|f| f.command == MMDVM_DSTAR_EOT),
        "EOT must NOT drain with only 1 slot: {:?}",
        frames.iter().map(|f| f.command).collect::<Vec<_>>()
    );

    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, status_v2(2)),
    )
    .await?;
    tokio::time::advance(Duration::from_millis(200)).await;
    let frames = drain_frames(&mut modem_side, Duration::from_millis(100)).await;
    assert!(
        frames.iter().any(|f| f.command == MMDVM_DSTAR_EOT),
        "EOT must drain with 2 slots: {:?}",
        frames.iter().map(|f| f.command).collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn playout_paces_one_frame_per_tick() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init = drain_frames(&mut modem_side, Duration::from_millis(100)).await;

    // Plenty of space, then queue three data frames back-to-back.
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, status_v2(20)),
    )
    .await?;
    // Let the loop ingest the status, then clear the wire.
    tokio::time::advance(Duration::from_millis(11)).await;
    let _cleared = drain_frames(&mut modem_side, Duration::from_millis(2)).await;

    modem.send_dstar_data([1u8; 12]).await?;
    modem.send_dstar_data([2u8; 12]).await?;
    modem.send_dstar_data([3u8; 12]).await?;

    // One playout tick must release exactly one frame, mirroring the
    // reference's one-write-per-playout pacing (Modem.cpp:1049-1084).
    tokio::time::advance(Duration::from_millis(10)).await;
    let first = drain_frames(&mut modem_side, Duration::from_millis(2)).await;
    let first_data = first
        .iter()
        .filter(|f| f.command == MMDVM_DSTAR_DATA)
        .count();
    assert_eq!(
        first_data, 1,
        "exactly one data frame per playout tick, got {first_data}: {first:?}"
    );

    // The next tick releases the next frame.
    tokio::time::advance(Duration::from_millis(10)).await;
    let second = drain_frames(&mut modem_side, Duration::from_millis(2)).await;
    let second_data = second
        .iter()
        .filter(|f| f.command == MMDVM_DSTAR_DATA)
        .count();
    assert_eq!(second_data, 1, "second tick must release the second frame");
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn queue_cap_returns_buffer_full() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init = drain_frames(&mut modem_side, Duration::from_millis(100)).await;

    // Space stays 0 — nothing drains, so the queue fills.
    for i in 0..64 {
        modem
            .send_dstar_data([0u8; 12])
            .await
            .map_err(|e| format!("send {i} failed early: {e}"))?;
    }
    let overflow = modem.send_dstar_data([0u8; 12]).await;
    assert!(
        matches!(overflow, Err(ShellError::BufferFull { .. })),
        "send past the cap must fail with BufferFull, got {overflow:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn queue_blocked_then_replenished_drains_in_order() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init = drain_frames(&mut modem_side, Duration::from_millis(100)).await;

    // 2 free slots: enough for data (needs >1) but the HEAD of the
    // queue is a header (needs >4), so nothing may drain — FIFO
    // order must not be bypassed.
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, status_v2(2)),
    )
    .await?;
    modem.send_dstar_header([1u8; 41]).await?;
    modem.send_dstar_data([2u8; 12]).await?;
    modem.send_dstar_eot().await?;

    tokio::time::advance(Duration::from_millis(100)).await;
    let blocked = drain_frames(&mut modem_side, Duration::from_millis(50)).await;
    assert!(
        !blocked.iter().any(|f| matches!(
            f.command,
            MMDVM_DSTAR_HEADER | MMDVM_DSTAR_DATA | MMDVM_DSTAR_EOT
        )),
        "a blocked head must hold the whole queue: {blocked:?}"
    );

    // Fresh status grants plenty of space mid-transmission — the
    // queue must now release in FIFO order across playout ticks.
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, status_v2(10)),
    )
    .await?;
    tokio::time::advance(Duration::from_millis(100)).await;
    let released = drain_frames(&mut modem_side, Duration::from_millis(100)).await;
    let seq: Vec<u8> = released
        .iter()
        .filter_map(|f| match f.command {
            MMDVM_DSTAR_HEADER | MMDVM_DSTAR_DATA | MMDVM_DSTAR_EOT => Some(f.command),
            _ => None,
        })
        .collect();
    assert_eq!(
        seq,
        vec![MMDVM_DSTAR_HEADER, MMDVM_DSTAR_DATA, MMDVM_DSTAR_EOT],
        "replenished queue must drain in FIFO order"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn shutdown_with_stuck_queue_returns_within_flush_deadline() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init = drain_frames(&mut modem_side, Duration::from_millis(100)).await;

    // No space ever granted — the frame can never drain. A shutdown
    // must still complete once the flush deadline expires instead of
    // hanging forever (the modem may be unplugged or wedged).
    modem.send_dstar_data([7u8; 12]).await?;

    let result = timeout(Duration::from_secs(30), modem.shutdown()).await;
    let transport = result
        .map_err(|_| "shutdown must not hang forever on a stuck TX queue")?
        .map_err(|e| format!("shutdown must recover the transport: {e}"))?;
    drop(transport);
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn shutdown_flush_drains_when_modem_grants_space() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init = drain_frames(&mut modem_side, Duration::from_millis(100)).await;

    // Queue a frame the modem has no space for yet.
    modem.send_dstar_data([9u8; 12]).await?;

    // Start the shutdown concurrently — the loop enters its flush
    // phase with the frame still queued.
    let shutdown_task = tokio::spawn(modem.shutdown());
    tokio::time::advance(Duration::from_millis(50)).await;

    // Status polling must CONTINUE during the flush (it is the only
    // way to learn that space freed up). Answer the poll with space.
    let polls = {
        let mut buf = Vec::new();
        let mut scratch = [0u8; 512];
        let deadline = tokio::time::Instant::now() + Duration::from_millis(400);
        let mut frames = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match timeout(remaining, modem_side.read(&mut scratch)).await {
                Ok(Ok(0) | Err(_)) | Err(_) => break,
                Ok(Ok(n)) => {
                    if let Some(slice) = scratch.get(..n) {
                        buf.extend_from_slice(slice);
                    }
                }
            }
            while let Ok(Some((frame, consumed))) = decode_frame(&buf) {
                frames.push(frame);
                drop(buf.drain(..consumed));
            }
            if frames.iter().any(|f| f.command == MMDVM_GET_STATUS) {
                break;
            }
        }
        frames
    };
    assert!(
        polls.iter().any(|f| f.command == MMDVM_GET_STATUS),
        "status polling must continue during the shutdown flush: {polls:?}"
    );

    // Grant space — the queued frame must now drain and shutdown
    // must complete well before the flush deadline.
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, status_v2(10)),
    )
    .await?;

    let flushed = drain_frames(&mut modem_side, Duration::from_millis(300)).await;
    assert!(
        flushed.iter().any(|f| f.command == MMDVM_DSTAR_DATA),
        "queued frame must drain once space is granted during shutdown: {flushed:?}"
    );

    let transport = timeout(Duration::from_secs(30), shutdown_task)
        .await
        .map_err(|_| "shutdown must complete after the queue drains")?
        .map_err(|e| format!("shutdown task panicked: {e}"))?
        .map_err(|e| format!("shutdown must succeed: {e}"))?;
    drop(transport);
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn shutdown_after_loop_exit_still_recovers_transport() -> TestResult {
    let (client_side, modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;

    // EOF the transport; the loop exits cleanly on its own carrying
    // the recovered transport in its JoinHandle.
    drop(modem_side);
    // Drain events until the loop's exit is visible.
    while let Ok(Some(_ev)) = timeout(Duration::from_millis(200), modem.next_event()).await {}

    // shutdown() after the fact must still hand the transport back
    // rather than failing with SessionClosed and dropping it.
    let transport = timeout(Duration::from_secs(5), modem.shutdown())
        .await
        .map_err(|_| "shutdown after loop exit must not hang")?
        .map_err(|e| format!("shutdown after loop exit must recover the transport: {e}"))?;
    drop(transport);
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
