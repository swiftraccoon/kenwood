//! In-memory loopback tests for the MMDVM async shell.
//!
//! Uses [`tokio::io::duplex`] as a fake transport and drives a minimal
//! simulated modem on one end, real [`AsyncModem`] on the other.

use std::time::Duration;

use mmdvm::{AsyncModem, Event, ShellError};
use mmdvm_core::{
    MMDVM_ACK, MMDVM_DSTAR_EOT, MMDVM_DSTAR_HEADER, MMDVM_GET_STATUS, MMDVM_GET_VERSION, MMDVM_NAK,
    MMDVM_SET_MODE, MmdvmFrame, ModemMode, NakReason, decode_frame, encode_frame,
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
                    // Bad frame: resync one byte.
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

    // Advance time so interval_at/interval clocks wake up; otherwise
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

    // Enqueue a header BEFORE sending any status reply: the loop has
    // no space info yet (dstar_space = 0), so the header must sit in
    // the queue.
    modem.send_dstar_header([0u8; 41]).await?;

    // Advance playout tick a few times; the loop should NOT write the
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
    //  mode=Dstar(1), state=0, reserved=0, dstar=10, dmr1=0, dmr2=0,
    //  ysf=0, p25=0, nxdn=0, reserved=0, fm=0, pocsag=0
    let status_payload = vec![1u8, 0, 0, 10, 0, 0, 0, 0, 0, 0, 0, 0];
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

    // Drain events until we see DstarHeaderRx or a shutdown.
    let mut seen = false;
    for _ in 0..20 {
        tokio::time::advance(Duration::from_millis(11)).await;
        if let Ok(Some(Event::DstarHeaderRx { bytes })) =
            timeout(Duration::from_millis(50), modem.next_event()).await
        {
            assert_eq!(bytes.as_slice(), header.as_slice());
            seen = true;
            break;
        }
    }
    assert!(seen, "expected a DstarHeaderRx event");
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn frame_fragmented_byte_at_a_time_reassembles() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init =
        collect_frames_until(&mut modem_side, |_| None::<()>, Duration::from_millis(100)).await;

    // Serial and Bluetooth SPP transports routinely deliver a frame
    // in arbitrary fragments; feed a D-STAR data frame one byte at
    // a time.
    let wire = encode_frame(&MmdvmFrame::with_payload(
        mmdvm_core::MMDVM_DSTAR_DATA,
        (0..12u8).collect(),
    ))?;
    for byte in wire {
        modem_side.write_all(&[byte]).await?;
        modem_side.flush().await?;
        tokio::time::advance(Duration::from_millis(1)).await;
    }

    let mut seen = false;
    for _ in 0..20 {
        tokio::time::advance(Duration::from_millis(11)).await;
        if let Ok(Some(Event::DstarDataRx { bytes })) =
            timeout(Duration::from_millis(50), modem.next_event()).await
        {
            assert_eq!(
                bytes,
                core::array::from_fn(|i| { u8::try_from(i).unwrap_or(u8::MAX) })
            );
            seen = true;
            break;
        }
    }
    assert!(seen, "fragmented frame must reassemble into one event");
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn two_frames_in_one_write_produce_ordered_events() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init =
        collect_frames_until(&mut modem_side, |_| None::<()>, Duration::from_millis(100)).await;

    // USB-CDC coalesces: one read can deliver several frames.
    let mut batch = encode_frame(&MmdvmFrame::with_payload(
        mmdvm_core::MMDVM_DSTAR_DATA,
        vec![0x11; 12],
    ))?;
    batch.extend(encode_frame(&MmdvmFrame::new(MMDVM_DSTAR_EOT))?);
    modem_side.write_all(&batch).await?;
    modem_side.flush().await?;

    let mut events = Vec::new();
    for _ in 0..20 {
        tokio::time::advance(Duration::from_millis(11)).await;
        match timeout(Duration::from_millis(50), modem.next_event()).await {
            Ok(Some(Event::DstarDataRx { .. })) => events.push("data"),
            Ok(Some(Event::DstarEot)) => {
                events.push("eot");
                break;
            }
            _ => {}
        }
    }
    assert_eq!(
        events,
        vec!["data", "eot"],
        "both frames from one read must decode, in order"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn v1_handshake_selects_v1_status_offsets() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init =
        collect_frames_until(&mut modem_side, |_| None::<()>, Duration::from_millis(100)).await;

    // Protocol-v1 version response: [proto=1, description...].
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_VERSION, b"\x01MMDVM 2018".to_vec()),
    )
    .await?;

    // v1 status layout: [unused, mode(1), state(2), dstar(3),
    // dmr1(4), dmr2(5), ysf(6)]; mode Dstar, CD set, dstar=12.
    // Misparsed as v2 this would read mode=Idle from payload[0] and
    // reject the 7-byte payload as too short.
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, vec![0, 1, 0x40, 12, 0, 0, 0]),
    )
    .await?;

    let mut saw_version = false;
    let mut status = None;
    for _ in 0..30 {
        tokio::time::advance(Duration::from_millis(11)).await;
        match timeout(Duration::from_millis(50), modem.next_event()).await {
            Ok(Some(Event::Version(v))) => {
                assert_eq!(v.protocol, 1);
                saw_version = true;
            }
            Ok(Some(Event::Status(s))) => {
                status = Some(s);
                break;
            }
            Ok(Some(Event::ProtocolViolation { command, detail })) => {
                return Err(format!(
                    "v1 status must not be rejected after a v1 handshake: 0x{command:02X} {detail}"
                )
                .into());
            }
            _ => {}
        }
    }
    assert!(saw_version, "expected the v1 Version event first");
    let status = status.ok_or("expected a Status event parsed with v1 offsets")?;
    assert_eq!(status.mode, ModemMode::Dstar);
    assert!(status.cd(), "CD flag lives at v1 offset 2");
    assert!(!status.tx());
    assert_eq!(status.dstar_space, 12);
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ack_and_nak_decode_exact_fields() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init =
        collect_frames_until(&mut modem_side, |_| None::<()>, Duration::from_millis(100)).await;

    // Wire layouts per firmware sendACK/sendNAK
    // (MMDVM/SerialPort.cpp): ACK carries the command byte, NAK
    // carries command + reason.
    modem_side.write_all(&[0xE0, 4, 0x70, 0x02]).await?;
    modem_side.write_all(&[0xE0, 5, 0x7F, 0x03, 0x02]).await?;
    modem_side.flush().await?;

    let mut ack = None;
    let mut nak = None;
    for _ in 0..30 {
        tokio::time::advance(Duration::from_millis(11)).await;
        match timeout(Duration::from_millis(50), modem.next_event()).await {
            Ok(Some(Event::Ack { command })) => ack = Some(command),
            Ok(Some(Event::Nak { command, reason })) => {
                nak = Some((command, reason));
                break;
            }
            _ => {}
        }
    }
    assert_eq!(ack, Some(0x02), "ACK must carry the ACK'd command byte");
    assert_eq!(
        nak,
        Some((0x03, NakReason::WrongMode)),
        "NAK must decode command and reason exactly"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn resync_recovers_past_spurious_frame_start_in_garbage() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init =
        collect_frames_until(&mut modem_side, |_| None::<()>, Duration::from_millis(100)).await;

    // Garbage containing a spurious 0xE0 with an invalid length (2),
    // immediately followed by a real EOT frame in the same write.
    // The resync must skip the phantom and decode the real frame.
    let mut junk = vec![0x13u8, 0x37, 0xE0, 0x02, 0xDE];
    junk.extend(encode_frame(&MmdvmFrame::new(MMDVM_DSTAR_EOT))?);
    modem_side.write_all(&junk).await?;
    modem_side.flush().await?;

    let mut saw_eot = false;
    for _ in 0..30 {
        tokio::time::advance(Duration::from_millis(11)).await;
        if matches!(
            timeout(Duration::from_millis(50), modem.next_event()).await,
            Ok(Some(Event::DstarEot))
        ) {
            saw_eot = true;
            break;
        }
    }
    assert!(
        saw_eot,
        "resync must recover past an embedded spurious 0xE0"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn extended_length_frame_decodes_through_the_loop() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init =
        collect_frames_until(&mut modem_side, |_| None::<()>, Duration::from_millis(100)).await;

    // Extended frame as the firmware sends for FM audio >252 B:
    // [0xE0, 0x00, len2, cmd, payload...], total = len2 + 255.
    // len2=45 → total 300 → 296 payload bytes. FM data is not a
    // modeled mode, so it must surface as UnhandledResponse intact,
    // NOT shredded by the resync path.
    let total = 255 + 45;
    let mut wire = vec![0xE0, 0x00, 45, mmdvm_core::MMDVM_FM_DATA];
    wire.resize(total, 0x5A);
    modem_side.write_all(&wire).await?;
    modem_side.flush().await?;

    let mut payload_len = None;
    for _ in 0..30 {
        tokio::time::advance(Duration::from_millis(11)).await;
        if let Ok(Some(Event::UnhandledResponse { command, payload })) =
            timeout(Duration::from_millis(50), modem.next_event()).await
        {
            assert_eq!(command, mmdvm_core::MMDVM_FM_DATA);
            payload_len = Some(payload.len());
            break;
        }
    }
    assert_eq!(
        payload_len,
        Some(total - 4),
        "extended frame must decode intact through the loop"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn set_mode_resolves_ok_on_ack() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init =
        collect_frames_until(&mut modem_side, |_| None::<()>, Duration::from_millis(100)).await;

    let (set_result, drive_result) = tokio::join!(modem.set_mode(ModemMode::Dstar), async {
        // Wait for the SetMode frame on the wire, then ACK it.
        let hit = collect_frames_until(
            &mut modem_side,
            |f| (f.command == MMDVM_SET_MODE).then_some(()),
            Duration::from_millis(500),
        )
        .await;
        if hit.and_then(|(_, h)| h).is_none() {
            return Err::<(), Box<dyn std::error::Error>>("SetMode frame never seen".into());
        }
        modem_write(
            &mut modem_side,
            &MmdvmFrame::with_payload(MMDVM_ACK, vec![MMDVM_SET_MODE]),
        )
        .await
    });
    drive_result?;
    assert!(
        set_result.is_ok(),
        "set_mode must resolve Ok on modem ACK: {set_result:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn set_mode_resolves_err_on_nak() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init =
        collect_frames_until(&mut modem_side, |_| None::<()>, Duration::from_millis(100)).await;

    let (set_result, drive_result) = tokio::join!(modem.set_mode(ModemMode::Dstar), async {
        let hit = collect_frames_until(
            &mut modem_side,
            |f| (f.command == MMDVM_SET_MODE).then_some(()),
            Duration::from_millis(500),
        )
        .await;
        if hit.and_then(|(_, h)| h).is_none() {
            return Err::<(), Box<dyn std::error::Error>>("SetMode frame never seen".into());
        }
        // NAK with reason 2 = wrong mode.
        modem_write(
            &mut modem_side,
            &MmdvmFrame::with_payload(MMDVM_NAK, vec![MMDVM_SET_MODE, 2]),
        )
        .await
    });
    drive_result?;
    assert!(
        matches!(
            set_result,
            Err(ShellError::Nak {
                command: MMDVM_SET_MODE,
                reason: NakReason::WrongMode,
            })
        ),
        "a NAK'd set_mode must fail with the correlated reason: {set_result:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn set_mode_times_out_on_silent_modem() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init =
        collect_frames_until(&mut modem_side, |_| None::<()>, Duration::from_millis(100)).await;

    // The modem never replies. set_mode must not hang; it must fail
    // with a response timeout in bounded time.
    let result = timeout(Duration::from_secs(30), modem.set_mode(ModemMode::Dstar)).await;
    let inner = result.map_err(|_| "set_mode must not hang on a silent modem")?;
    assert!(
        matches!(inner, Err(ShellError::ResponseTimeout)),
        "silent modem must produce ResponseTimeout: {inner:?}"
    );
    Ok(())
}

/// An ACK correlated to a DIFFERENT command must not resolve a
/// pending `set_mode`: a stray ACK (raw command, firmware quirk)
/// would otherwise report success while the mode never changed. The
/// wrong-command ACK is delivered, then the modem stays silent;
/// `set_mode` must still end in `ResponseTimeout`.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn set_mode_ignores_ack_for_a_different_command() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init =
        collect_frames_until(&mut modem_side, |_| None::<()>, Duration::from_millis(100)).await;

    let (set_result, drive_result) = tokio::join!(
        timeout(Duration::from_secs(30), modem.set_mode(ModemMode::Dstar)),
        async {
            let hit = collect_frames_until(
                &mut modem_side,
                |f| (f.command == MMDVM_SET_MODE).then_some(()),
                Duration::from_millis(500),
            )
            .await;
            if hit.and_then(|(_, h)| h).is_none() {
                return Err::<(), Box<dyn std::error::Error>>("SetMode frame never seen".into());
            }
            // ACK correlated to GET_STATUS, not SET_MODE.
            modem_write(
                &mut modem_side,
                &MmdvmFrame::with_payload(MMDVM_ACK, vec![MMDVM_GET_STATUS]),
            )
            .await
        }
    );
    drive_result?;
    let inner = set_result.map_err(|_| "set_mode must not hang")?;
    assert!(
        matches!(inner, Err(ShellError::ResponseTimeout)),
        "an ACK for another command must not resolve set_mode: {inner:?}"
    );
    Ok(())
}

/// Test transport whose read side fails immediately with an I/O
/// error; writes are swallowed. Used to drive the loop's fatal-error
/// exit path (a `DuplexStream` can only EOF, never error).
#[derive(Debug, Default)]
struct FailingReadTransport;

impl tokio::io::AsyncRead for FailingReadTransport {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Err(std::io::Error::other("injected read failure")))
    }
}

impl tokio::io::AsyncWrite for FailingReadTransport {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

/// Test transport whose write side never completes (kernel TX buffer
/// full, serial flow control asserted, hung USB endpoint). Reads pend
/// forever too, so the only way the loop can make progress is by
/// bounding the write.
#[derive(Debug, Default)]
struct WedgedWriteTransport;

impl tokio::io::AsyncRead for WedgedWriteTransport {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Pending
    }
}

impl tokio::io::AsyncWrite for WedgedWriteTransport {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Pending
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Pending
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn wedged_transport_write_times_out_with_fatal_event() -> TestResult {
    // The very first handshake write wedges. Without a write
    // deadline the loop freezes forever (no reads, no commands, no
    // shutdown) and the consumer waits on next_event() for eternity.
    let mut modem = AsyncModem::spawn(WedgedWriteTransport);

    let mut fatal_message = None;
    loop {
        match timeout(Duration::from_secs(60), modem.next_event()).await {
            Ok(Some(Event::Fatal { message })) => {
                fatal_message = Some(message);
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => return Err("loop must not freeze forever on a wedged write".into()),
        }
    }
    let message = fatal_message.ok_or("a wedged write must surface as Event::Fatal")?;
    assert!(
        message.contains("timed out"),
        "Fatal must describe the write timeout, got: {message}"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn transport_error_emits_fatal_event() -> TestResult {
    let mut modem = AsyncModem::spawn(FailingReadTransport);

    let mut fatal_message = None;
    while let Some(event) = modem.next_event().await {
        if let Event::Fatal { message } = event {
            fatal_message = Some(message);
        }
    }
    let message = fatal_message.ok_or("transport error must surface as Event::Fatal")?;
    assert!(
        message.contains("injected read failure"),
        "Fatal must carry the underlying error, got: {message}"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn queued_frames_dropped_on_eof_surface_tx_dropped() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init =
        collect_frames_until(&mut modem_side, |_| None::<()>, Duration::from_millis(100)).await;

    // No status was ever delivered, so dstar_space is 0 and these
    // stay queued.
    modem.send_dstar_data([1u8; 12]).await?;
    modem.send_dstar_data([2u8; 12]).await?;

    // EOF the transport: the loop exits with the queue non-empty.
    drop(modem_side);

    let mut dropped = None;
    let mut saw_closed = false;
    while let Some(event) = modem.next_event().await {
        match event {
            Event::TxDropped { frames } => dropped = Some(frames),
            Event::TransportClosed => saw_closed = true,
            _ => {}
        }
    }
    assert!(saw_closed, "EOF must still emit TransportClosed");
    assert_eq!(
        dropped,
        Some(2),
        "discarded TX frames must surface as TxDropped"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn malformed_responses_emit_protocol_violation() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init =
        collect_frames_until(&mut modem_side, |_| None::<()>, Duration::from_millis(100)).await;

    // Status response too short for the v2 layout (5-byte payload).
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_GET_STATUS, vec![1, 0, 0, 4, 0]),
    )
    .await?;
    // D-STAR header with the wrong payload size (10 bytes, not 41).
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(MMDVM_DSTAR_HEADER, vec![0u8; 10]),
    )
    .await?;
    // ACK with an empty payload (reference always carries the
    // ACK'd command byte).
    modem_write(&mut modem_side, &MmdvmFrame::new(MMDVM_ACK)).await?;

    let mut violations = Vec::new();
    for _ in 0..30 {
        tokio::time::advance(Duration::from_millis(11)).await;
        match timeout(Duration::from_millis(50), modem.next_event()).await {
            Ok(Some(Event::ProtocolViolation { command, .. })) => {
                violations.push(command);
                if violations.len() == 3 {
                    break;
                }
            }
            Ok(Some(Event::Ack { command })) => {
                return Err(
                    format!("empty ACK must not decode as Ack {{ command: {command} }}").into(),
                );
            }
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break,
        }
    }
    assert_eq!(
        violations,
        vec![MMDVM_GET_STATUS, MMDVM_DSTAR_HEADER, MMDVM_ACK],
        "each malformed frame must surface as a ProtocolViolation"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn lagging_event_consumer_gets_exact_loss_without_blocking_loop() -> TestResult {
    let (client_side, mut modem_side) = duplex_pair();
    let mut modem = AsyncModem::spawn(client_side);

    tokio::time::advance(Duration::from_millis(5)).await;
    let _init =
        collect_frames_until(&mut modem_side, |_| None::<()>, Duration::from_millis(100)).await;

    // Flood the loop with 300 inbound EOT frames without consuming a
    // single event, 44 more than the 256-slot event ring can hold. A loop
    // that blocks on event delivery wedges here and can never process
    // another command (the deadlock: consumer waits on the loop, the
    // loop waits on the consumer).
    for _ in 0..300 {
        modem_write(&mut modem_side, &MmdvmFrame::new(MMDVM_DSTAR_EOT)).await?;
    }
    tokio::time::advance(Duration::from_millis(50)).await;

    // The loop must still process commands.
    let result = timeout(Duration::from_millis(500), modem.request_status()).await;
    assert!(
        matches!(result, Ok(Ok(()))),
        "loop must stay responsive with a full event channel: {result:?}"
    );

    // The gap is part of the typed event stream. It must not be
    // relegated to a log line or silently inferred from missing EOTs.
    let loss = timeout(Duration::from_millis(500), modem.next_event()).await;
    assert!(
        matches!(loss, Ok(Some(Event::EventsDropped { count: 44 }))),
        "receiver must report the exact overwritten count: {loss:?}"
    );
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

    // Write garbage bytes: invalid start byte, invalid length, etc.
    modem_side
        .write_all(&[0x13, 0x37, 0xDE, 0xAD, 0xBE, 0xEF])
        .await?;
    // Then an actually-valid DstarEot frame.
    modem_write(&mut modem_side, &MmdvmFrame::new(MMDVM_DSTAR_EOT)).await?;

    // Loop should still be alive and able to emit events.
    let mut saw_eot = false;
    for _ in 0..30 {
        tokio::time::advance(Duration::from_millis(11)).await;
        if matches!(
            timeout(Duration::from_millis(50), modem.next_event()).await,
            Ok(Some(Event::DstarEot))
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
