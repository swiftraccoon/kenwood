//! Pure qualification prerequisites and independent artifact success checks.

use super::*;
use kenwood_tmd750::{Address, FirmwareIdentity, McpProbeSegment, Page, RadioModel, RadioType};
use reconnect::VerificationOutcome;

type TestResult = Result<(), Box<dyn StdError + Send + Sync>>;

fn probe() -> Result<McpProbeReport, Box<dyn StdError + Send + Sync>> {
    Ok(McpProbeReport {
        identity: Some(Identity {
            model: RadioModel::TmD750,
            firmware: FirmwareIdentity::new("1.02")?,
            radio_type: RadioType::new("K,2,1")?,
        }),
        entry_reply: Some(b"0M".to_vec()),
        segments: vec![
            McpProbeSegment {
                page: Page::new(Address::new(8)?, 40)?,
                data: vec![0x42; 40],
            },
            McpProbeSegment {
                page: Page::new(Address::new(327_681)?, 255)?,
                data: vec![0x42; 255],
            },
        ],
        exit: McpProbeExit::Acknowledged,
        outcome: McpProbeOutcome::AwaitingCatVerification,
    })
}

fn summary() -> Result<TranscriptSummary, Box<dyn StdError + Send + Sync>> {
    let directory = tempfile::tempdir()?;
    let artifacts = Artifacts::create(
        Some(&directory.path().join("capture")),
        Arc::new(AtomicBool::new(false)),
    )?;
    Ok(artifacts.transcript.summary())
}

#[test]
fn only_complete_detached_evidence_permits_verification() -> TestResult {
    let transcript = summary()?;
    let cancelled = AtomicBool::new(false);
    let complete = probe()?;
    assert_eq!(
        verification_eligibility(&complete, None, &transcript, &cancelled).ok(),
        complete.identity.as_ref()
    );
    for outcome in [
        McpProbeOutcome::Cancelled,
        McpProbeOutcome::Failed {
            stage: McpProbeStage::Exit,
            error: kenwood_tmd750::Error::Timeout {
                operation: "test exit",
                millis: 1,
            },
        },
    ] {
        let mut incomplete = probe()?;
        incomplete.outcome = outcome;
        assert!(matches!(
            verification_eligibility(&incomplete, None, &transcript, &cancelled),
            Err(SkipReason::OriginalProbeIncomplete)
        ));
    }
    for exit in [
        McpProbeExit::NotEntered,
        McpProbeExit::NotAcknowledged,
        McpProbeExit::RecoveryRequired,
    ] {
        let mut incomplete = probe()?;
        incomplete.exit = exit;
        assert!(matches!(
            verification_eligibility(&incomplete, None, &transcript, &cancelled),
            Err(SkipReason::OriginalProbeIncomplete)
        ));
    }
    Ok(())
}

#[test]
fn identity_and_exact_fragment_shapes_are_required() -> TestResult {
    let transcript = summary()?;
    let cancelled = AtomicBool::new(false);
    let mut missing_identity = probe()?;
    missing_identity.identity = None;
    let mut missing_page = probe()?;
    let _removed = missing_page.segments.pop();
    let mut reversed = probe()?;
    reversed.segments.reverse();
    let mut short_data = probe()?;
    let _removed = short_data
        .segments
        .first_mut()
        .ok_or("missing global fragment")?
        .data
        .pop();
    let mut wrong_address = probe()?;
    wrong_address
        .segments
        .first_mut()
        .ok_or("missing global fragment")?
        .page = Page::new(Address::new(9)?, 40)?;
    for incomplete in [
        missing_identity,
        missing_page,
        reversed,
        short_data,
        wrong_address,
    ] {
        assert!(matches!(
            verification_eligibility(&incomplete, None, &transcript, &cancelled),
            Err(SkipReason::OriginalProbeIncomplete)
        ));
    }
    Ok(())
}

#[test]
fn close_capture_and_cancellation_failures_refuse_verification() -> TestResult {
    let complete = probe()?;
    let mut transcript = summary()?;
    let cancelled = AtomicBool::new(false);
    let failure = Failure::from_error(&io::Error::other("close failed"));
    assert!(matches!(
        verification_eligibility(&complete, Some(&failure), &transcript, &cancelled),
        Err(SkipReason::OriginalCloseFailed)
    ));
    transcript.complete = false;
    assert!(matches!(
        verification_eligibility(&complete, None, &transcript, &cancelled),
        Err(SkipReason::OriginalCaptureIncomplete)
    ));
    transcript.complete = true;
    cancelled.store(true, Ordering::Relaxed);
    assert!(matches!(
        verification_eligibility(&complete, None, &transcript, &cancelled),
        Err(SkipReason::Cancelled)
    ));
    Ok(())
}

fn artifact() -> Result<ArtifactReport, Box<dyn StdError + Send + Sync>> {
    Ok(ArtifactReport {
        format_version: 3,
        software_version: "test",
        started_at_utc: "2026-09-07T00:00:00Z".to_owned(),
        finished_at_utc: "2026-09-07T00:00:01Z".to_owned(),
        endpoint: Endpoint {
            path: "test-radio".to_owned(),
            usb_vendor_id: Some(0x2166),
            usb_product_id: Some(0x9030),
            cat_baud: 9600,
        },
        transcript: summary()?,
        probe: Some(ProbeEvidence::from(&probe()?)),
        open_error: None,
        signal_error: None,
        close_error: None,
        post_exit_verification: ReadinessVerification::skipped(
            SkipReason::OriginalProbeIncomplete,
            summary()?,
        ),
    })
}

#[test]
fn probe_artifact_always_records_the_required_fresh_verification() -> TestResult {
    let report = artifact()?;
    let json = serde_json::to_value(&report)?;
    assert_eq!(
        json.get("format_version"),
        Some(&serde_json::json!(3)),
        "probe reports retain every bounded readiness attempt"
    );
    assert_eq!(
        json.pointer("/post_exit_verification/outcome/status"),
        Some(&serde_json::json!("skipped")),
        "even skipped verification must have explicit evidence"
    );
    assert!(
        !report.succeeded(),
        "read and exit evidence alone cannot complete the workflow"
    );
    Ok(())
}

#[test]
fn fresh_match_does_not_erase_original_failure_or_capture_failure() -> TestResult {
    let mut report = artifact()?;
    let mut verification = ReadinessVerification::skipped(SkipReason::Cancelled, summary()?);
    verification.outcome = VerificationOutcome::Matched;
    report.post_exit_verification = verification;
    assert!(report.succeeded());
    let json = serde_json::to_value(&report)?;
    assert_eq!(
        json.pointer("/probe/outcome/status"),
        Some(&serde_json::json!("awaiting_cat_verification"))
    );
    assert!(
        json.pointer("/probe/cat_identity").is_none(),
        "original-session evidence must not suggest old-handle CAT was queried"
    );
    assert_eq!(
        json.pointer("/post_exit_verification/identity_assurance"),
        Some(&serde_json::json!("endpoint_and_cat_tuple_only"))
    );
    report.transcript.complete = false;
    assert!(!report.succeeded());
    report.transcript.complete = true;
    report.close_error = Some(Failure::from_error(&io::Error::other(
        "original close failed",
    )));
    assert!(!report.succeeded());
    report.close_error = None;
    report.post_exit_verification.transcript.complete = false;
    assert!(!report.succeeded());
    report.post_exit_verification.transcript.complete = true;
    report.probe.as_mut().ok_or("missing probe")?.outcome = Outcome::Cancelled;
    assert!(!report.succeeded());
    Ok(())
}
