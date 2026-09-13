//! Negative lifecycle boundaries through the same fake backend as success.

use super::*;

#[tokio::test]
async fn changed_control_identity_or_gateway_prevents_programming() -> TestResult {
    let plan = plan(0)?;
    let mut wrong_identity = MockTransport::new();
    wrong_identity.expect(b"ID\r", b"ID TM-D750\r");
    wrong_identity.expect(b"FV\r", b"FV 1.01\r");
    wrong_identity.expect(b"TY\r", b"TY K,2,1\r");
    for script in [wrong_identity, identity_script(Some(2))] {
        let mut harness = Harness::new()?;
        harness.add(false, script, Fault::None);
        let result = harness.run(&plan).await?;
        assert!(!result.succeeded());
        assert_eq!(result.restoration, Restoration::NotOwed);
        assert!(result.probe.is_none());
        assert!(result.restore.is_none());
        let entry = result.entry.ok_or("entry evidence missing")?;
        assert_eq!(entry.exchange.exit, evidence::Exit::NotEntered);
        assert!(!entry.exchange.intent_recorded);
        assert!(entry.readiness.is_none());
        assert!(entry.verification.is_none());
        harness.assert_retired(1, 0)?;
    }
    Ok(())
}

#[tokio::test]
async fn silent_programming_entry_is_uncertain_and_sends_no_escape_or_retry() -> TestResult {
    let plan = plan(0)?;
    let mut script = identity_script(Some(0));
    script.expect_hang(b"0M PROGRAM\r");
    let mut harness = Harness::new()?;
    harness.add(false, script, Fault::None);
    let result = harness.run(&plan).await?;
    assert!(!result.succeeded());
    assert_eq!(result.restoration, Restoration::NotOwed);
    assert!(result.probe.is_none());
    assert!(result.restore.is_none());
    let entry = result.entry.ok_or("entry evidence missing")?;
    assert_eq!(entry.exchange.exit, evidence::Exit::Uncertain);
    assert!(!entry.exchange.intent_recorded);
    assert!(entry.readiness.is_none());
    harness.assert_retired(1, 0)
}

#[tokio::test]
async fn changed_inverse_before_image_preserves_debt_without_overwriting() -> TestResult {
    let plan = plan(0)?;
    let reverse = plan.restoration()?;
    let target = reverse
        .replacements()
        .last()
        .ok_or("target guard missing")?
        .page();
    let mut script = identity_script(Some(2));
    script.expect(b"0M PROGRAM\r", b"0M\r");
    for page in reverse.replacements() {
        let mut bytes = page.expected().to_vec();
        if page.page() == target {
            *bytes.last_mut().ok_or("target before-image empty")? ^= 1;
        }
        read(&mut script, page.page(), &bytes);
    }
    script.expect(b"E", &[ACK]);
    let mut harness = Harness::new()?;
    harness.phase(&plan, Fault::None);
    harness.add(true, modem_script(), Fault::None);
    harness.add(false, script, Fault::None);
    let result = harness.run(&plan).await?;
    assert!(!result.succeeded());
    assert_eq!(result.restoration, Restoration::Owed);
    let restore = result.restore.ok_or("restoration evidence missing")?;
    assert_eq!(restore.exchange.exit, evidence::Exit::Acknowledged);
    assert!(!restore.exchange.intent_recorded);
    assert!(restore.exchange.possible.is_empty());
    assert!(restore.exchange.verified.is_empty());
    assert!(restore.readiness.is_none());
    harness.assert_retired(5, 1)
}

#[tokio::test]
async fn fresh_gateway_mismatch_cannot_clear_verified_write_debt() -> TestResult {
    let plan = plan(0)?;
    let mut harness = Harness::new()?;
    harness.phase(&plan, Fault::None);
    harness.add(true, modem_script(), Fault::None);
    harness.add(false, complete(&plan.restoration()?), Fault::None);
    harness.add(false, identity_script(None), Fault::None);
    harness.add(false, identity_script(Some(2)), Fault::None);
    let result = harness.run(&plan).await?;
    assert!(!result.succeeded());
    assert_eq!(result.restoration, Restoration::Owed);
    let restore = result.restore.ok_or("restoration evidence missing")?;
    assert_eq!(restore.exchange.verified.len(), 1);
    let verification = restore.verification.ok_or("fresh verification missing")?;
    assert_eq!(verification.gateway, Some(2));
    assert!(!verification.succeeded());
    harness.assert_retired(7, 2)
}

#[tokio::test]
async fn cat_reply_on_modem_is_not_success_and_still_restores() -> TestResult {
    let plan = plan(0)?;
    let mut harness = Harness::new()?;
    harness.phase(&plan, Fault::None);
    harness.add(true, identity_script(Some(2)), Fault::None);
    harness.phase(&plan.restoration()?, Fault::None);
    let result = harness.run(&plan).await?;
    assert!(!result.succeeded());
    assert_eq!(result.restoration, Restoration::Verified);
    let probe = result.probe.ok_or("diagnostic evidence missing")?;
    assert!(probe.succeeded());
    assert!(matches!(
        probe.outcome,
        super::super::super::Outcome::CatObserved { .. }
    ));
    harness.assert_retired(7, 2)
}

fn assert_never_opened(harness: &Harness) -> TestResult {
    let observed = harness
        .backend
        .observed
        .lock()
        .map_err(|error| io::Error::other(error.to_string()))?;
    assert!(observed.events.is_empty());
    assert_eq!(observed.opens, 0);
    assert_eq!(observed.live, 0);
    assert_eq!(observed.writes, 0);
    drop(observed);
    assert_eq!(
        harness.backend.scripts.len(),
        1,
        "even the first open must remain undispatched"
    );
    Ok(())
}

#[tokio::test]
async fn initial_journal_failure_prevents_any_open() -> TestResult {
    let mut harness = Harness::new()?;
    harness.add(false, MockTransport::new(), Fault::None);
    harness.journal = Recorder::named(
        File::open(&harness.backend.journal)?,
        Arc::clone(&harness.cancelled),
        "terminal-journal.jsonl",
    );
    let result = harness.run(&plan(0)?).await?;
    assert!(!result.succeeded());
    assert_eq!(result.restoration, Restoration::NotOwed);
    assert!(result.entry.is_none());
    assert_eq!(result.problems.len(), 1);
    assert_never_opened(&harness)
}

#[tokio::test]
async fn initial_exchange_capture_failure_prevents_any_open() -> TestResult {
    let mut harness = Harness::new()?;
    harness.add(false, MockTransport::new(), Fault::None);
    let captures = harness.captures.as_mut().ok_or("captures missing")?;
    captures.entry.exchange = Recorder::named(
        File::open(harness.directory.path().join("capture/transcript.jsonl"))?,
        Arc::clone(&harness.cancelled),
        "transcript.jsonl",
    );
    let result = harness.run(&plan(0)?).await?;
    assert!(!result.succeeded());
    assert_eq!(result.restoration, Restoration::NotOwed);
    let entry = result.entry.ok_or("entry evidence missing")?;
    assert!(!entry.exchange.transcript.complete);
    assert!(entry.readiness.is_none());
    assert_never_opened(&harness)
}
