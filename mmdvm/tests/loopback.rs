//! In-memory loopback tests for the MMDVM async shell.
//!
//! Uses [`tokio::io::duplex`] as a fake transport and drives a minimal
//! simulated modem on one end, real [`AsyncModem`] on the other.

use std::time::Duration;

use mmdvm::{AsyncModem, Event, ShellError};
use mmdvm_core::{Command, MmdvmFrame, ModemMode, NakReason};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::time::timeout;

// Acknowledge workspace dev-deps so `-D unused-crate-dependencies`
// doesn't fire across each integration binary.
#[cfg(feature = "dstar")]
use dstar_gateway_core as _;
use kenwood_transport as _;
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
            match MmdvmFrame::from_bytes(&buf) {
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
    let bytes = frame.to_bytes();
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

    let saw_version = frames.iter().any(|f| f.command() == Command::GET_VERSION);
    let saw_status = frames.iter().any(|f| f.command() == Command::GET_STATUS);

    assert!(
        saw_version,
        "expected GetVersion at startup, got: {:?}",
        frames.iter().map(MmdvmFrame::command).collect::<Vec<_>>()
    );
    assert!(
        saw_status,
        "expected GetStatus at startup, got: {:?}",
        frames.iter().map(MmdvmFrame::command).collect::<Vec<_>>()
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
        .filter(|f| f.command() == Command::GET_STATUS)
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
            .filter(|f| f.command() == Command::GET_STATUS)
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
            if f.command() == Command::GET_STATUS {
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
        initial.iter().any(|f| f.command() == Command::GET_VERSION),
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
        !pre_status
            .iter()
            .any(|f| f.command() == Command::DSTAR_HEADER),
        "header must NOT be written before space is known: {pre_status:?}"
    );

    let mut version = vec![0_u8; 20];
    *version.first_mut().ok_or("version header byte")? = 2;
    version.extend_from_slice(b"TEST");
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(Command::GET_VERSION, version)?,
    )
    .await?;

    // Now simulate the modem reporting dstar_space=10 (v2 layout).
    //  mode=Dstar(1), state=0, reserved=0, dstar=10, dmr1=0, dmr2=0,
    //  ysf=0, p25=0, nxdn=0, reserved=0, fm=0, pocsag=0
    let status_payload = vec![1u8, 0, 0, 10, 0, 0, 0, 0, 0, 0, 0, 0];
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(Command::GET_STATUS, status_payload)?,
    )
    .await?;

    // Give the loop a moment to ingest + drain.
    for _ in 0..5 {
        tokio::time::advance(Duration::from_millis(11)).await;
    }
    let (post_status, _) = collect_frames_until(
        &mut modem_side,
        |f| {
            if f.command() == Command::DSTAR_HEADER {
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
        post_status
            .iter()
            .any(|f| f.command() == Command::DSTAR_HEADER),
        "header MUST be written after space reported: {:?}",
        post_status
            .iter()
            .map(MmdvmFrame::command)
            .collect::<Vec<_>>()
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
            if f.command() == Command::GET_STATUS {
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
        &MmdvmFrame::with_payload(Command::DSTAR_HEADER, header.clone())?,
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
    let wire = MmdvmFrame::with_payload(Command::DSTAR_DATA, (0..12u8).collect())?.to_bytes();
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
    let mut batch = MmdvmFrame::with_payload(Command::DSTAR_DATA, vec![0x11; 12])?.to_bytes();
    batch.extend(MmdvmFrame::new(Command::DSTAR_EOT).to_bytes());
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
        &MmdvmFrame::with_payload(Command::GET_VERSION, b"\x01MMDVM 2018".to_vec())?,
    )
    .await?;

    // v1 status layout: [unused, mode(1), state(2), dstar(3),
    // dmr1(4), dmr2(5), ysf(6)]; mode Dstar, CD set, dstar=12.
    // Misparsed as v2 this would read mode=Idle from payload[0] and
    // reject the 7-byte payload as too short.
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(Command::GET_STATUS, vec![0, 1, 0x40, 12, 0, 0, 0])?,
    )
    .await?;

    let mut saw_version = false;
    let mut status = None;
    for _ in 0..30 {
        tokio::time::advance(Duration::from_millis(11)).await;
        match timeout(Duration::from_millis(50), modem.next_event()).await {
            Ok(Some(Event::Version(v))) => {
                assert_eq!(v.protocol().get(), 1);
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
    assert_eq!(
        ack,
        Some(Command::SET_CONFIG),
        "ACK must carry the ACK'd command byte"
    );
    assert_eq!(
        nak,
        Some((Command::SET_MODE, NakReason::CommandUnavailable)),
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
    junk.extend(MmdvmFrame::new(Command::DSTAR_EOT).to_bytes());
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
    let mut wire = vec![0xE0, 0x00, 45, Command::FM_DATA.get()];
    wire.resize(total, 0x5A);
    modem_side.write_all(&wire).await?;
    modem_side.flush().await?;

    let mut payload_len = None;
    for _ in 0..30 {
        tokio::time::advance(Duration::from_millis(11)).await;
        if let Ok(Some(Event::UnhandledResponse { command, payload })) =
            timeout(Duration::from_millis(50), modem.next_event()).await
        {
            assert_eq!(command, Command::FM_DATA);
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
            |f| (f.command() == Command::SET_MODE).then_some(()),
            Duration::from_millis(500),
        )
        .await;
        if hit.and_then(|(_, h)| h).is_none() {
            return Err::<(), Box<dyn std::error::Error>>("SetMode frame never seen".into());
        }
        modem_write(
            &mut modem_side,
            &MmdvmFrame::with_payload(Command::ACK, vec![Command::SET_MODE.get()])?,
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
            |f| (f.command() == Command::SET_MODE).then_some(()),
            Duration::from_millis(500),
        )
        .await;
        if hit.and_then(|(_, h)| h).is_none() {
            return Err::<(), Box<dyn std::error::Error>>("SetMode frame never seen".into());
        }
        // NAK code 2 reports that the command is unavailable in the active mode.
        modem_write(
            &mut modem_side,
            &MmdvmFrame::with_payload(Command::NAK, vec![Command::SET_MODE.get(), 2])?,
        )
        .await
    });
    drive_result?;
    assert!(
        matches!(
            set_result,
            Err(ShellError::Nak {
                command: Command::SET_MODE,
                reason: NakReason::CommandUnavailable,
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

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn cancelled_or_timed_out_mode_change_cannot_be_overwritten() -> TestResult {
    for wait in [Duration::from_millis(25), Duration::from_secs(3)] {
        let (client, mut peer) = duplex_pair();
        let mut modem = AsyncModem::spawn(client);
        let _initial =
            collect_frames_until(&mut peer, |_| None::<()>, Duration::from_millis(10)).await;
        let first = timeout(wait, modem.set_mode(ModemMode::Dstar)).await;
        assert!(
            matches!(first, Err(_) | Ok(Err(ShellError::ResponseTimeout))),
            "{first:?}"
        );
        let blocked = modem.set_mode(ModemMode::Idle).await;
        assert!(
            matches!(blocked, Err(ShellError::ModeChangePending)),
            "{blocked:?}"
        );
        let raw = modem.send_raw(Command::SET_MODE, vec![0]).await;
        assert!(
            matches!(
                raw,
                Err(ShellError::ReservedCommand {
                    command: Command::SET_MODE
                })
            ),
            "{raw:?}"
        );
        let (written, _) =
            collect_frames_until(&mut peer, |_| None::<()>, Duration::from_millis(10))
                .await
                .ok_or("missing write collection")?;
        let changes: Vec<_> = written
            .iter()
            .filter(|frame| frame.command() == Command::SET_MODE)
            .collect();
        assert_eq!(
            changes.len(),
            1,
            "a second mode change reached the wire: {written:?}"
        );
        assert_eq!(changes.first().ok_or("first mode missing")?.payload(), [1]);

        modem_write(&mut peer, &MmdvmFrame::with_payload(Command::ACK, vec![3])?).await?;
        loop {
            let event = timeout(Duration::from_millis(50), modem.next_event())
                .await?
                .ok_or("session closed before late ACK")?;
            if matches!(
                event,
                Event::Ack {
                    command: Command::SET_MODE
                }
            ) {
                break;
            }
        }
        let (change, response) = tokio::join!(modem.set_mode(ModemMode::Idle), async {
            let next = collect_frames_until(
                &mut peer,
                |frame| (frame.command() == Command::SET_MODE).then_some(frame.payload().to_vec()),
                Duration::from_millis(50),
            )
            .await
            .ok_or("missing next mode write")?;
            assert_eq!(next.1, Some(vec![0]));
            modem_write(&mut peer, &MmdvmFrame::with_payload(Command::ACK, vec![3])?).await
        });
        response?;
        change?;
    }
    Ok(())
}

/// Complete the startup writes, then reject the first explicit request.
#[derive(Debug)]
struct FailThirdWrite {
    attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl tokio::io::AsyncRead for FailThirdWrite {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Pending
    }
}

impl tokio::io::AsyncWrite for FailThirdWrite {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buffer: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let attempt = self
            .attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if attempt == 2 {
            return std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "request write failed after startup",
            )));
        }
        std::task::Poll::Ready(Ok(buffer.len()))
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

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn request_write_failure_retires_the_stream_before_another_write() -> TestResult {
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut modem = AsyncModem::spawn(FailThirdWrite {
        attempts: attempts.clone(),
    });
    tokio::task::yield_now().await;
    let result = modem.request_version().await;
    assert!(
        matches!(result, Err(ShellError::Io(ref error))
        if error.kind() == std::io::ErrorKind::BrokenPipe
            && error.to_string() == "request write failed after startup"),
        "{result:?}"
    );
    let terminal = timeout(Duration::from_millis(50), modem.next_event())
        .await?
        .ok_or("missing fatal event")?;
    assert!(
        matches!(terminal, Event::Fatal { ref message } if message.contains("request write failed")),
        "{terminal:?}"
    );
    assert_eq!(
        timeout(Duration::from_millis(50), modem.next_event()).await?,
        None
    );
    assert!(matches!(
        modem.request_status().await,
        Err(ShellError::SessionClosed)
    ));
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert!(matches!(modem.shutdown().await, Err(error)
        if matches!(error.error(), ShellError::Io(source)
            if source.kind() == std::io::ErrorKind::BrokenPipe)));
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn debug_responses_preserve_binary_payloads_and_identifiers() -> TestResult {
    let (client, mut peer) = duplex_pair();
    let mut modem = AsyncModem::spawn(client);
    let _initial = collect_frames_until(&mut peer, |_| None::<()>, Duration::from_millis(10)).await;
    for command in [Command::DEBUG1, Command::DEBUG5, Command::DEBUG_DUMP] {
        let payload = vec![b'T', b'X', 0, 0xFF, 0x80, 0x01];
        modem_write(
            &mut peer,
            &MmdvmFrame::with_payload(command, payload.clone())?,
        )
        .await?;
        let event = timeout(Duration::from_millis(50), modem.next_event())
            .await?
            .ok_or("missing debug response")?;
        assert_eq!(event, Event::Debug { command, payload });
    }
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn raw_payload_bounds_are_checked_before_submission() -> TestResult {
    let (client, mut peer) = duplex_pair();
    let mut modem = AsyncModem::spawn(client);
    let invalid = modem.send_raw(Command::FM_DATA, vec![0; 507]).await;
    assert!(
        matches!(
            invalid,
            Err(ShellError::Core(mmdvm_core::MmdvmError::PayloadTooLarge {
                len: 507
            }))
        ),
        "{invalid:?}"
    );
    modem.send_raw(Command::FM_DATA, vec![0xA5; 506]).await?;
    let (frames, payload) = collect_frames_until(
        &mut peer,
        |frame| (frame.command() == Command::FM_DATA).then_some(frame.payload().to_vec()),
        Duration::from_millis(50),
    )
    .await
    .ok_or("missing maximum raw frame")?;
    assert_eq!(payload, Some(vec![0xA5; 506]));
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame.command() == Command::FM_DATA)
            .count(),
        1
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
                |f| (f.command() == Command::SET_MODE).then_some(()),
                Duration::from_millis(500),
            )
            .await;
            if hit.and_then(|(_, h)| h).is_none() {
                return Err::<(), Box<dyn std::error::Error>>("SetMode frame never seen".into());
            }
            // ACK correlated to GET_STATUS, not SET_MODE.
            modem_write(
                &mut modem_side,
                &MmdvmFrame::with_payload(Command::ACK, vec![Command::GET_STATUS.get()])?,
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
async fn mode_deadline_includes_command_channel_backpressure() -> TestResult {
    let mut modem = AsyncModem::spawn(WedgedWriteTransport);
    for _ in 0..32 {
        let queued = timeout(Duration::from_millis(1), modem.request_version()).await;
        assert!(
            queued.is_err(),
            "startup write must still be pending: {queued:?}"
        );
    }
    let started = tokio::time::Instant::now();
    let result = timeout(Duration::from_secs(3), modem.set_mode(ModemMode::Dstar)).await?;
    assert!(
        matches!(result, Err(ShellError::ResponseTimeout)),
        "{result:?}"
    );
    assert_eq!(started.elapsed(), Duration::from_secs(2));
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn configuration_deadline_includes_command_channel_backpressure() -> TestResult {
    let mut modem = AsyncModem::spawn(WedgedWriteTransport);
    for _ in 0..32 {
        let queued = timeout(Duration::from_millis(1), modem.request_version()).await;
        assert!(
            queued.is_err(),
            "startup write must leave the request queue full"
        );
    }
    let configuration = mmdvm::DstarConfiguration::new(
        mmdvm::ConfigurationProfile::MmdvmV1,
        mmdvm::TxDelay::try_from(10)?,
        mmdvm::SignalLevels {
            receive: mmdvm::SignalLevel::new(128),
            transmit: mmdvm::SignalLevel::new(128),
        },
    )?;
    let started = tokio::time::Instant::now();
    let result = timeout(Duration::from_secs(3), modem.configure_dstar(configuration)).await?;
    assert!(
        matches!(result, Err(ShellError::ResponseTimeout)),
        "{result:?}"
    );
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(2),
        "configuration submission shares the response deadline"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn expired_unwritten_configuration_cannot_start_after_a_late_version() -> TestResult {
    let (client, mut peer) = duplex_pair();
    let mut modem = AsyncModem::spawn(client);
    let _initial = collect_frames_until(&mut peer, |_| None::<()>, Duration::from_millis(1)).await;
    let configuration = mmdvm::DstarConfiguration::new(
        mmdvm::ConfigurationProfile::MmdvmV1,
        mmdvm::TxDelay::try_from(10)?,
        mmdvm::SignalLevels {
            receive: mmdvm::SignalLevel::new(128),
            transmit: mmdvm::SignalLevel::new(128),
        },
    )?;
    let started = tokio::time::Instant::now();
    let result = modem.configure_dstar(configuration).await;
    assert!(
        matches!(result, Err(ShellError::ResponseTimeout)),
        "{result:?}"
    );
    assert_eq!(started.elapsed(), Duration::from_secs(2));
    modem_write(
        &mut peer,
        &MmdvmFrame::with_payload(Command::GET_VERSION, b"\x01TEST".to_vec())?,
    )
    .await?;
    let (frames, _) = collect_frames_until(&mut peer, |_| None::<()>, Duration::from_millis(20))
        .await
        .ok_or("configuration observations")?;
    assert!(
        frames
            .iter()
            .all(|frame| frame.command() != Command::SET_CONFIG),
        "a version arriving after timeout must not dispatch a discarded request"
    );
    assert!(
        matches!(
            modem.send_raw(Command::SET_CONFIG, vec![]).await,
            Err(ShellError::ReservedCommand {
                command: Command::SET_CONFIG
            })
        ),
        "raw configuration cannot bypass correlation even with an empty payload"
    );
    let completed = modem.shutdown().await?;
    assert_eq!(completed.summary().dropped_frames(), Some(0));
    Ok(())
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

    let mut version = vec![0_u8; 20];
    *version.first_mut().ok_or("version header byte")? = 2;
    version.extend_from_slice(b"TEST");
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(Command::GET_VERSION, version)?,
    )
    .await?;

    // Status response too short for the v2 layout (5-byte payload).
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(Command::GET_STATUS, vec![1, 0, 0, 4, 0])?,
    )
    .await?;
    // D-STAR header with the wrong payload size (10 bytes, not 41).
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(Command::DSTAR_HEADER, vec![0u8; 10])?,
    )
    .await?;
    // ACK with an empty payload (reference always carries the
    // ACK'd command byte).
    modem_write(&mut modem_side, &MmdvmFrame::new(Command::ACK)).await?;
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(Command::ACK, vec![3, 0])?,
    )
    .await?;
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(Command::NAK, vec![3])?,
    )
    .await?;
    modem_write(
        &mut modem_side,
        &MmdvmFrame::with_payload(Command::NAK, vec![3, 2, 0])?,
    )
    .await?;

    let mut violations = Vec::new();
    for _ in 0..30 {
        tokio::time::advance(Duration::from_millis(11)).await;
        match timeout(Duration::from_millis(50), modem.next_event()).await {
            Ok(Some(Event::ProtocolViolation { command, .. })) => {
                violations.push(command);
                if violations.len() == 6 {
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
        vec![
            Command::GET_STATUS,
            Command::DSTAR_HEADER,
            Command::ACK,
            Command::ACK,
            Command::NAK,
            Command::NAK
        ],
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
        modem_write(&mut modem_side, &MmdvmFrame::new(Command::DSTAR_EOT)).await?;
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
            if f.command() == Command::GET_STATUS {
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
    modem_write(&mut modem_side, &MmdvmFrame::new(Command::DSTAR_EOT)).await?;

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
