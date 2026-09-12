//! Historical success facts must not hide incomplete exit verification.

use super::{Harness, Scenario, TestResult, fresh_script, mcp_script};

#[tokio::test]
async fn failed_verify_entry_keeps_prior_off_observations_without_claiming_completion() -> TestResult
{
    let mut harness = Harness::new()?;
    let mut script = fresh_script(0);
    script.expect(b"0M PROGRAM\r", b"");
    let connection = harness.connection(2)?;
    connection.mock = script;
    connection.entry_read_error = Some(std::io::Error::from_raw_os_error(6));
    let result = harness.run().await?;
    let text = result.observation_lines(&harness.trial).join("\n");
    for required in [
        "historical, not a current-state check",
        "Session 1 memory write: acknowledged.",
        "Session 1: immediate acknowledged full Gateway-page readback matched the exact Off page.",
        "Session 1: fresh matching CAT identity and Gateway Off were captured",
        "Session 2 memory write: not attempted.",
    ] {
        assert!(
            text.contains(required),
            "missing retained observation: {required}"
        );
    }
    assert!(
        !text.contains("Session 2: fresh matching CAT"),
        "the first session's Off must not become the second session's evidence"
    );
    assert!(
        !result.succeeded(&harness.trial),
        "observation rendering cannot complete the trial"
    );
    Ok(())
}

#[tokio::test]
async fn unacknowledged_write_never_claims_readback_or_fresh_off() -> TestResult {
    let mut harness = Harness::new()?;
    harness.connection(0)?.mock = mcp_script(&harness.trial, 0, Scenario::BadWriteAck)?;
    let result = harness.run().await?;
    let text = result.observation_lines(&harness.trial).join("\n");
    assert!(
        text.contains("possibly dispatched; no acknowledgment proved"),
        "uncertain dispatch must remain explicit"
    );
    assert!(
        !text.contains("readback matched"),
        "an ACK failure supplies no readback"
    );
    assert!(
        !text.contains("fresh matching CAT"),
        "uncertain framing must not claim a fresh Off check"
    );
    Ok(())
}

#[tokio::test]
async fn readback_summary_requires_every_byte_and_complete_capture() -> TestResult {
    let mut harness = Harness::new()?;
    let mut result = harness.run().await?;
    assert!(
        result
            .observation_lines(&harness.trial)
            .join("\n")
            .contains("readback matched"),
        "the unmodified successful fixture must expose its readback"
    );
    let first = result.sessions.first_mut().ok_or("first session missing")?;
    let core = first.core.as_mut().ok_or("first core report missing")?;
    let readback = core
        .segments
        .last_mut()
        .ok_or("immediate readback missing")?;
    *readback
        .data
        .last_mut()
        .ok_or("last readback byte missing")? ^= 1;
    assert!(
        !result
            .observation_lines(&harness.trial)
            .join("\n")
            .contains("readback matched"),
        "changing an unrelated final byte must remove the matching-readback claim"
    );
    let first = result.sessions.first_mut().ok_or("first session missing")?;
    first.transcript.complete = false;
    let text = result.observation_lines(&harness.trial).join("\n");
    assert!(
        text.contains("Session 1: original capture is incomplete"),
        "capture loss must be visible"
    );
    assert!(
        !text.contains("Session 1 memory write: acknowledged"),
        "incomplete capture must not yield qualified observations"
    );
    assert!(
        !text.contains("Session 1: fresh matching CAT"),
        "a failed required capture must not produce a session success summary"
    );
    Ok(())
}
