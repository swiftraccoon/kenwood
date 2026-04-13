//! In-memory loopback tests for the MMDVM async shell.
//!
//! Uses [`tokio::io::duplex`] as a fake transport and drives a minimal
//! simulated modem on one end, real [`AsyncModem`] on the other.

use std::time::Duration;

use mmdvm::{AsyncModem, Event};
use mmdvm_core::{
    MMDVM_DSTAR_EOT, MMDVM_DSTAR_HEADER, MMDVM_GET_STATUS, MMDVM_GET_VERSION, MmdvmFrame,
    decode_frame, encode_frame,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::time::timeout;

// Acknowledge workspace dev-deps so `-D unused-crate-dependencies`
// doesn't fire across each integration binary.
use thiserror as _;
use tracing as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Build a 4096-byte duplex + split so "modem side" and "client side"
/// can be driven independently.
fn duplex_pair() -> (DuplexStream, DuplexStream) {
    tokio::io::duplex(4096)
}

/// Drain frames from a stream until a predicate returns `Some(T)` or
/// the timeout elapses.
async fn collect_frames_until<F, T>(
    stream: &mut DuplexStream,
    mut pred: F,
    deadline: Duration,
) -> Option<(Vec<MmdvmFrame>, Option<T>)>
where
    F: FnMut(&MmdvmFrame) -> Option<T>,
{
    let mut buf = Vec::with_capacity(4096);
    let mut out = Vec::new();
    let mut scratch = [0u8; 512];

    let deadline_at = tokio::time::Instant::now() + deadline;

    loop {
        let remaining = deadline_at.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Some((out, None));
        }
        match timeout(remaining, stream.read(&mut scratch)).await {
            Ok(Ok(0) | Err(_)) | Err(_) => return Some((out, None)),
            Ok(Ok(n)) => {
                if let Some(slice) = scratch.get(..n) {
                    buf.extend_from_slice(slice);
                }
            }
        }

        // Try decoding as many frames as possible.
        loop {
            match decode_frame(&buf) {
                Ok(Some((frame, consumed))) => {
                    let maybe_hit = pred(&frame);
                    out.push(frame);
                    drop(buf.drain(..consumed));
                    if let Some(v) = maybe_hit {
                        return Some((out, Some(v)));
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    // Bad frame — resync one byte.
                    if buf.is_empty() {
                        break;
                    }
                    let _discarded = buf.remove(0);
                }
            }
        }
    }
}

/// Shorthand: write one frame from the modem side.
async fn modem_write(stream: &mut DuplexStream, frame: &MmdvmFrame) -> TestResult {
    let bytes = encode_frame(frame)?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn spawn_issues_initial_version_and_status_probes() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let _modem = AsyncModem::spawn(client_side);

    // Advance time so interval_at/interval clocks wake up — otherwise
    // the paused clock means nothing elapses. The initial probes are
    // emitted before any timer fires, but we still need to yield.
    tokio::time::advance(Duration::from_millis(10)).await;

    let (frames, _) =
        collect_frames_until(&mut modem_side, |_| None::<()>, Duration::from_millis(500))
            .await
            .ok_or("collect timed out")?;

    let saw_version = frames.iter().any(|f| f.command == MMDVM_GET_VERSION);
    let saw_status = frames.iter().any(|f| f.command == MMDVM_GET_STATUS);

    assert!(
        saw_version,
        "expected GetVersion at startup, got: {:?}",
        frames.iter().map(|f| f.command).collect::<Vec<_>>()
    );
    assert!(
        saw_status,
        "expected GetStatus at startup, got: {:?}",
        frames.iter().map(|f| f.command).collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn status_poll_fires_every_250ms() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let _modem = AsyncModem::spawn(client_side);

    // Let the initial handshake drain first.
    tokio::time::advance(Duration::from_millis(5)).await;
    let (initial, _) =
        collect_frames_until(&mut modem_side, |_| None::<()>, Duration::from_millis(100))
            .await
            .ok_or("initial collect timed out")?;
    let initial_status = initial
        .iter()
        .filter(|f| f.command == MMDVM_GET_STATUS)
        .count();

    // Advance in 250 ms slices and drain each time so the duplex
    // channel doesn't back up. Under paused time, tokio fires all
    // eligible timers during `advance`, but the channel write must
    // make progress between advances.
    let mut periodic_status = 0usize;
    for _ in 0..6 {
        tokio::time::advance(Duration::from_millis(260)).await;
        let (batch, _) =
            collect_frames_until(&mut modem_side, |_| None::<()>, Duration::from_millis(50))
                .await
                .ok_or("batch collect timed out")?;
        periodic_status += batch
            .iter()
            .filter(|f| f.command == MMDVM_GET_STATUS)
            .count();
    }

    assert!(
        periodic_status >= 3,
        "expected >=3 periodic status polls in ~1.5 s (plus initial={initial_status}), saw {periodic_status}"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn send_dstar_header_writes_after_space_reported() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    // Drain the initial GetVersion + GetStatus.
    tokio::time::advance(Duration::from_millis(5)).await;
    let (initial, _) = collect_frames_until(
        &mut modem_side,
        |f| {
            if f.command == MMDVM_GET_STATUS {
                Some(())
            } else {
                None
            }
        },
        Duration::from_millis(100),
    )
    .await
    .ok_or("initial handshake collect timed out")?;
    assert!(
        initial.iter().any(|f| f.command == MMDVM_GET_VERSION),
        "expected GetVersion"
    );

    // Enqueue a header BEFORE sending any status reply — the loop has
    // no space info yet (dstar_space = 0), so the header must sit in
    // the queue.
    modem.send_dstar_header([0u8; 41]).await?;

    // Advance playout tick a few times — loop should NOT write the
    // header yet because space is 0.
    for _ in 0..5 {
        tokio::time::advance(Duration::from_millis(11)).await;
    }
    let (pre_status, _) =
        collect_frames_until(&mut modem_side, |_| None::<()>, Duration::from_millis(50))
            .await
            .ok_or("pre-status collect timed out")?;
    assert!(
        !pre_status.iter().any(|f| f.command == MMDVM_DSTAR_HEADER),
        "header must NOT be written before space is known: {pre_status:?}"
    );

    // Now simulate the modem reporting dstar_space=10 (v2 layout).
    //  mode=DStar(1), state=0, reserved=0, dstar=10, dmr1=0, dmr2=0,
    //  ysf=0, p25=0, nxdn=0
    let status_payload = vec![1u8, 0, 0, 10, 0, 0, 0, 0, 0];
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, status_payload),
    )
    .await?;

    // Give the loop a moment to ingest + drain.
    for _ in 0..5 {
        tokio::time::advance(Duration::from_millis(11)).await;
    }
    let (post_status, _) = collect_frames_until(
        &mut modem_side,
        |f| {
            if f.command == MMDVM_DSTAR_HEADER {
                Some(())
            } else {
                None
            }
        },
        Duration::from_millis(200),
    )
    .await
    .ok_or("post-status collect timed out")?;

    assert!(
        post_status.iter().any(|f| f.command == MMDVM_DSTAR_HEADER),
        "header MUST be written after space reported: {:?}",
        post_status.iter().map(|f| f.command).collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn dstar_header_rx_emits_event() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    // Let startup handshake drain off the modem side.
    tokio::time::advance(Duration::from_millis(5)).await;
    let _drained = collect_frames_until(
        &mut modem_side,
        |f| {
            if f.command == MMDVM_GET_STATUS {
                Some(())
            } else {
                None
            }
        },
        Duration::from_millis(100),
    )
    .await;

    // Inject a D-STAR header from the modem.
    let header = vec![0xAAu8; 41];
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_DSTAR_HEADER, header.clone()),
    )
    .await?;

    // Drain events until we see DStarHeaderRx or a shutdown.
    let mut seen = false;
    for _ in 0..20 {
        tokio::time::advance(Duration::from_millis(11)).await;
        if let Ok(Some(Event::DStarHeaderRx { bytes })) =
            timeout(Duration::from_millis(50), modem.next_event()).await
        {
            assert_eq!(bytes.as_slice(), header.as_slice());
            seen = true;
            break;
        }
    }
    assert!(seen, "expected a DStarHeaderRx event");
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn malformed_bytes_are_swallowed() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;

    // Drain initial frames.
    let _drained = collect_frames_until(
        &mut modem_side,
        |f| {
            if f.command == MMDVM_GET_STATUS {
                Some(())
            } else {
                None
            }
        },
        Duration::from_millis(100),
    )
    .await;

    // Write garbage bytes — invalid start byte, invalid length, etc.
    modem_side
        .write_all(&[0x13, 0x37, 0xDE, 0xAD, 0xBE, 0xEF])
        .await?;
    // Then an actually-valid DStarEot frame.
    modem_write(&mut modem_side, &MmdvmFrame::new(MMDVM_DSTAR_EOT)).await?;

    // Loop should still be alive and able to emit events.
    let mut saw_eot = false;
    for _ in 0..30 {
        tokio::time::advance(Duration::from_millis(11)).await;
        if matches!(
            timeout(Duration::from_millis(50), modem.next_event()).await,
            Ok(Some(Event::DStarEot))
        ) {
            saw_eot = true;
            break;
        }
    }
    assert!(
        saw_eot,
        "session must survive garbage and still decode real frames"
    );
    Ok(())
}
