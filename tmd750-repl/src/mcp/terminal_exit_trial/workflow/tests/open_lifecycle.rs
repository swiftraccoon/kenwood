//! On-disk original-connection evidence without opening real radio interfaces.

use super::*;

#[derive(Debug, serde::Deserialize)]
struct TranscriptRecord {
    sequence: u64,
    utc_unix_nanoseconds: String,
    elapsed_microseconds: u128,
    event: CapturedEvent,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CapturedEvent {
    OpenRequested { path: String, baud: u32 },
    OpenCompleted,
    OpenFailed { error: CapturedFailure },
    WriteRequested { bytes: Vec<u8> },
    WriteCompleted,
    WriteFailed,
    ReadCompleted,
    ReadFailed,
    InvalidReadCount,
    BaudRequested,
    BaudCompleted,
    BaudFailed,
    CloseRequested,
    CloseCompleted,
    CloseFailed,
}

#[derive(Debug, serde::Deserialize)]
struct CapturedFailure {
    message: String,
    causes: Vec<String>,
}

fn transcript(path: &Path) -> Result<Vec<TranscriptRecord>, TestError> {
    let content = std::fs::read_to_string(path)?;
    assert!(
        content.ends_with('\n'),
        "record boundaries must be complete"
    );
    let records = content
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<TranscriptRecord>, _>>()?;
    for (sequence, record) in records.iter().enumerate() {
        assert_eq!(
            record.sequence,
            u64::try_from(sequence)?,
            "original capture sequence numbers must remain contiguous"
        );
        let timestamp = record.utc_unix_nanoseconds.parse::<i128>()?;
        assert!(timestamp > 0, "each event requires an actual UTC timestamp");
        let _valid_timestamp = time::OffsetDateTime::from_unix_timestamp_nanos(timestamp)?;
    }
    assert!(
        records.windows(2).all(|pair| {
            matches!(pair, [before, after] if before.elapsed_microseconds <= after.elapsed_microseconds)
        }),
        "monotonic observation timestamps must retain event ordering"
    );
    Ok(records)
}

fn original_path(connection: &Connection) -> Result<PathBuf, TestError> {
    let directory = connection
        .journal
        .path
        .parent()
        .ok_or("journal parent missing")?;
    Ok(directory
        .join(format!("session-{}", connection.id / 2))
        .join("transcript.jsonl"))
}

fn assert_request(record: &TranscriptRecord) -> TestResult {
    let CapturedEvent::OpenRequested { path, baud } = &record.event else {
        return Err("the first record must describe the pending original open".into());
    };
    assert_eq!(
        path,
        &endpoint().path,
        "record the selected endpoint exactly"
    );
    assert_eq!(*baud, 9600, "record the actual requested baud");
    Ok(())
}

pub(super) fn verify_before_open(connection: &Connection) -> TestResult {
    if !connection.id.is_multiple_of(2) {
        return Ok(());
    }
    let records = transcript(&original_path(connection)?)?;
    let [request] = records.as_slice() else {
        return Err("exactly one complete request record must exist before original open".into());
    };
    assert_request(request)
}

pub(super) fn verify_before_first_write(connection: &Connection, bytes: &[u8]) -> TestResult {
    if !connection.id.is_multiple_of(2) {
        return Ok(());
    }
    let records = transcript(&original_path(connection)?)?;
    let [request, completed, command] = records.as_slice() else {
        return Err("both original open records must precede the first CAT request".into());
    };
    assert_request(request)?;
    assert!(
        matches!(completed.event, CapturedEvent::OpenCompleted),
        "the successful original opening must be recorded before CAT dispatch"
    );
    assert!(
        matches!(&command.event, CapturedEvent::WriteRequested { bytes: recorded } if recorded == bytes),
        "the first dispatched CAT request must match its prior on-disk record"
    );
    assert_eq!(
        bytes, b"ID\r",
        "the first dispatched request remains identity"
    );
    Ok(())
}

#[tokio::test]
async fn both_original_opens_are_timestamped_before_their_first_cat_request() -> TestResult {
    let mut harness = Harness::new()?;
    let result = harness.run().await?;
    assert!(result.succeeded(&harness.trial));
    assert_eq!(result.sessions.len(), 2);
    for (phase, session) in result.sessions.iter().enumerate() {
        let path = harness
            .directory
            .path()
            .join(format!("session-{phase}/transcript.jsonl"));
        let records = transcript(&path)?;
        let [request, completed, first_command, ..] = records.as_slice() else {
            return Err("original lifecycle records missing".into());
        };
        assert_request(request)?;
        assert!(matches!(completed.event, CapturedEvent::OpenCompleted));
        assert!(
            matches!(&first_command.event, CapturedEvent::WriteRequested { bytes } if bytes == b"ID\r")
        );
        assert!(matches!(
            records.last().map(|record| &record.event),
            Some(CapturedEvent::CloseCompleted)
        ));
        assert!(session.transcript.complete);
    }
    Ok(())
}

#[tokio::test]
async fn either_original_open_failure_retains_its_error_without_protocol_or_retry() -> TestResult {
    for failed_id in [0, 2] {
        let mut harness = Harness::new()?;
        harness.backend.open_failure_at = Some(failed_id);
        let result = harness.run().await?;
        assert!(!result.succeeded(&harness.trial));
        assert_eq!(harness.backend.opens, failed_id + 1);
        assert_eq!(
            harness.trial.status(),
            if failed_id == 0 {
                TerminalExitTrialStatus::NotWritten
            } else {
                TerminalExitTrialStatus::PossiblyChanged
            }
        );
        let events = harness.events()?;
        assert_eq!(
            events.get(position(&events, &Event::Open(failed_id))?..),
            Some([Event::Open(failed_id)].as_slice()),
            "failed open cannot dispatch commands, close a nonexistent handle, or retry"
        );
        assert_eq!(memory_writes(&events), failed_id / 2);
        let session = result.sessions.last().ok_or("failed session missing")?;
        assert!(session.core.is_none());
        assert!(session.close_error.is_none());
        assert!(session.synchronization_error.is_none());
        assert!(session.post_exit.gateway_off_evidence().is_none());
        let error = session
            .open_error
            .as_ref()
            .ok_or("actual open error missing")?;
        assert_eq!(error.causes, ["scripted original open failure"]);
        let records = transcript(
            &harness
                .directory
                .path()
                .join(format!("session-{}/transcript.jsonl", failed_id / 2)),
        )?;
        let [request, failed] = records.as_slice() else {
            return Err("failed original open must retain exactly two records".into());
        };
        assert_request(request)?;
        let CapturedEvent::OpenFailed { error: captured } = &failed.event else {
            return Err("second record must preserve the actual open failure".into());
        };
        assert_eq!(captured.message, error.message);
        assert_eq!(captured.causes, error.causes);
        assert!(session.transcript.complete);
        let encoded = serde_json::to_value(session)?;
        assert_eq!(
            encoded
                .pointer("/transcript/events")
                .and_then(serde_json::Value::as_u64),
            Some(2)
        );
        assert_eq!(
            encoded
                .pointer("/post_exit/transcript/events")
                .and_then(serde_json::Value::as_u64),
            Some(0)
        );
    }
    Ok(())
}

#[tokio::test]
async fn failed_open_completion_record_retires_the_acquired_handle_without_cat() -> TestResult {
    use crate::capture::Event as CaptureEvent;
    use crate::mcp::reconnect::{PostExitVerification, SkipReason};

    let mut harness = Harness::new()?;
    harness.connection(0)?.mock = MockTransport::new();
    let [
        SessionCaptures {
            mut original,
            post_exit,
        },
        _unused,
    ] = harness.captures.take().ok_or("capture pairs missing")?;
    let selected = endpoint();
    original.record(CaptureEvent::OpenRequested {
        path: &selected.path,
        baud: 9600,
    });
    original.synchronize()?;
    let connection = harness.backend.open(&selected, 9600)?;
    drop(original);

    let path = harness.directory.path().join("session-0/transcript.jsonl");
    let failed = Arc::new(AtomicBool::new(false));
    let mut original = Recorder::named(
        File::open(&path)?,
        Arc::clone(&failed),
        "read-only-open-completion.jsonl",
    );
    original.record(CaptureEvent::OpenCompleted);
    assert!(
        failed.load(Ordering::Relaxed),
        "the read-only recorder must actually fail"
    );
    let mut session = super::super::SessionEvidence {
        core: None,
        transcript: original.summary(),
        open_error: None,
        close_error: None,
        synchronization_error: None,
        post_exit: PostExitVerification::skipped(
            SkipReason::OriginalCaptureIncomplete,
            post_exit.summary(),
        ),
    };
    session.synchronize_original(&mut original);
    let first_error = serde_json::to_value(
        session
            .synchronization_error
            .as_ref()
            .ok_or("recording failure missing")?,
    )?;
    let admitted = session.admit_original(connection, original).await;
    assert!(
        admitted.is_none(),
        "failed opening evidence must never admit CAT"
    );
    assert_retired_after_capture_failure(&harness, &session, &first_error, &path)
}

fn assert_retired_after_capture_failure(
    harness: &Harness,
    session: &super::super::SessionEvidence,
    first_error: &serde_json::Value,
    path: &Path,
) -> TestResult {
    assert_eq!(
        harness.events()?,
        [Event::Open(0), Event::Close(0), Event::Dropped(0)],
        "the acquired handle must only close and drop, with no commands or retry"
    );
    assert_eq!(
        harness.backend.opens, 1,
        "the acquired handle must not be reopened"
    );
    assert!(session.open_error.is_none(), "the actual open succeeded");
    let close_error = session
        .close_error
        .as_ref()
        .ok_or("close capture error missing")?;
    assert!(
        close_error
            .causes
            .iter()
            .any(|cause| cause.contains("required capture failed after close dispatch")),
        "the mock close succeeded, but its missing required capture must remain explicit"
    );
    assert!(
        session.core.is_none(),
        "no protocol evidence may be manufactured"
    );
    assert!(
        !session.transcript.complete,
        "the failed capture must remain incomplete"
    );
    assert_eq!(
        &serde_json::to_value(
            session
                .synchronization_error
                .as_ref()
                .ok_or("original error lost")?,
        )?,
        first_error,
        "cleanup synchronization must preserve the first recording failure"
    );
    assert!(
        session.post_exit.gateway_off_evidence().is_none(),
        "an unproven original handle cannot supply fresh Gateway evidence"
    );
    assert_eq!(
        transcript(path)?.len(),
        1,
        "only the successful open request is on disk"
    );
    Ok(())
}
