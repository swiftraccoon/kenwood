//! Fixed PM1 wire scope, durable-intent ordering, and fail-closed lifecycle.
//!
//! Persistence cases model separate MCP sessions after exit and fresh identity
//! verification. They do not simulate or independently prove a physical reboot.

use mcp_d75_extract as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use std::cell::Cell;
use std::future::{Future, poll_fn};
use std::io;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use std::time::Duration;

use kenwood_tmd750::memory::{
    PmNameTrial, PmNameTrialError, PmNameTrialEvent, PmNameTrialSession, PmNameTrialStatus,
    PmNameTrialWrite,
};
use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::{
    Address, Error, FirmwareIdentity, Identity, McpError, McpProbeExit, Page,
    PmNameTrialSessionError, PmNameTrialSessionOutcome, PmNameTrialSessionReport,
    PmNameTrialSessionStage, PmNameTrialWriteDisposition, Radio, RadioModel, RadioType,
};
use kenwood_transport::{MockTransport, Transport, TransportError};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture() -> Result<PmNameTrial, Box<dyn std::error::Error>> {
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    let mut original = [0xA5; 256];
    original.get_mut(10..26).ok_or("PM1 range missing")?.fill(0);
    original
        .get_mut(10..13)
        .ok_or("PM1 text missing")?
        .copy_from_slice(b"PM1");
    Ok(PmNameTrial::prepare_unqualified_offline(
        &identity, &original, "PM1",
    )?)
}

fn format_page() -> Result<Page, kenwood_tmd750::ValidationError> {
    Page::new(Address::new(8)?, 40)
}

fn identity(mock: &mut MockTransport, firmware: &str) {
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", format!("FV {firmware}\r").as_bytes());
    mock.expect(b"TY\r", b"TY K,2,1\r");
}

fn entry(mock: &mut MockTransport) {
    identity(mock, "1.02");
    mock.expect(b"0M PROGRAM\r", b"0M\r");
}

fn frame(page: Page, data: &[u8]) -> Vec<u8> {
    let mut bytes = write_request(page).to_vec();
    bytes.extend_from_slice(data);
    bytes
}

fn read(mock: &mut MockTransport, page: Page, data: &[u8]) {
    mock.expect(&read_request(page), &frame(page, data));
    mock.expect(&[ACK], &[ACK]);
}

fn preflight(mock: &mut MockTransport, trial: &PmNameTrial, before: &[u8]) -> TestResult {
    entry(mock);
    read(mock, format_page()?, &[0; 40]);
    read(mock, trial.page(), before);
    Ok(())
}

fn session_script(trial: &PmNameTrial) -> Result<MockTransport, Box<dyn std::error::Error>> {
    let mut mock = MockTransport::new();
    let (before, after) = match trial.next_session()? {
        PmNameTrialSession::Rename => (trial.original_page(), Some(trial.expected_page())),
        PmNameTrialSession::Restore => (trial.expected_page(), Some(trial.original_page())),
        PmNameTrialSession::VerifyRestoration => (trial.original_page(), None),
    };
    preflight(&mut mock, trial, before)?;
    if let Some(after) = after {
        mock.expect(&frame(trial.page(), after), &[ACK]);
        read(&mut mock, trial.page(), after);
    }
    mock.expect(b"E", &[ACK]);
    Ok(mock)
}

#[derive(Debug)]
struct AuditedTransport {
    mock: MockTransport,
    durable_intents: Arc<AtomicUsize>,
    memory_writes: usize,
    exited: bool,
    baud_changes: Vec<u32>,
    close_calls: usize,
}

impl AuditedTransport {
    fn new(mock: MockTransport, durable_intents: &Arc<AtomicUsize>) -> Self {
        Self {
            mock,
            durable_intents: Arc::clone(durable_intents),
            memory_writes: 0,
            exited: false,
            baud_changes: Vec::new(),
            close_calls: 0,
        }
    }
}

impl Transport for AuditedTransport {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        assert!(
            !self.exited,
            "no protocol write may follow E on this handle"
        );
        if bytes.first() == Some(&b'W') {
            assert_eq!(
                self.durable_intents.load(Ordering::SeqCst),
                1,
                "W requires a successful intent callback"
            );
            self.memory_writes += 1;
        }
        self.mock.write(bytes).await?;
        if bytes == b"E" {
            self.exited = true;
        }
        Ok(())
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        self.mock.read(bytes).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.close_calls += 1;
        self.mock.close().await
    }

    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        assert!(!self.exited, "no baud ioctl may follow E on this handle");
        self.baud_changes.push(baud);
        self.mock.set_baud_rate(baud)
    }
}

async fn assert_blocked<T: Transport>(radio: &mut Radio<T>) {
    assert!(
        matches!(
            radio.identify().await,
            Err(Error::Mcp(
                McpError::RecoveryRequired | McpError::ConnectionRetired
            ))
        ),
        "CAT must not use an uncertain or retired handle"
    );
    assert!(
        matches!(
            radio.mcp_session(),
            Err(Error::Mcp(
                McpError::RecoveryRequired | McpError::ConnectionRetired
            ))
        ),
        "MCP must not reborrow an uncertain or retired handle"
    );
}

fn assert_session_success(report: &PmNameTrialSessionReport, trial: &PmNameTrial) {
    assert!(
        matches!(
            report.outcome,
            PmNameTrialSessionOutcome::AwaitingCatVerification
        ),
        "only the external CAT and cleanup proof may remain"
    );
    assert_eq!(
        report.identity.as_ref(),
        Some(trial.identity()),
        "complete identity must match"
    );
    assert_eq!(
        report.entry_reply.as_deref(),
        Some(b"0M".as_slice()),
        "entry reply must be exact"
    );
    assert_eq!(
        report.exit,
        McpProbeExit::Acknowledged,
        "E acknowledgment is required"
    );
    assert!(
        report.cleanup_error.is_none(),
        "successful sessions have no cleanup failure"
    );
    assert_eq!(
        report.status,
        PmNameTrialStatus::PossiblyChanged,
        "the session driver cannot attest external finalization"
    );
}

#[tokio::test]
async fn all_three_sessions_have_exact_wire_scope_and_require_external_finalization() -> TestResult
{
    let mut trial = fixture()?;
    for (phase, expected_write) in [
        (PmNameTrialSession::Rename, Some(PmNameTrialWrite::Rename)),
        (PmNameTrialSession::Restore, Some(PmNameTrialWrite::Restore)),
        (PmNameTrialSession::VerifyRestoration, None),
    ] {
        let intents = Arc::new(AtomicUsize::new(0));
        let mut radio = Radio::new(AuditedTransport::new(session_script(&trial)?, &intents));
        let mut observed_write = None;
        let report = radio
            .run_approved_pm1_trial_session_until_exit(
                &mut trial,
                || false,
                |state, write| {
                    assert_eq!(
                        state.status(),
                        if phase == PmNameTrialSession::Rename {
                            PmNameTrialStatus::NotWritten
                        } else {
                            PmNameTrialStatus::PossiblyChanged
                        }
                    );
                    observed_write = Some(write);
                    let _previous = intents.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .await;
        assert_session_success(&report, &trial);
        assert_eq!(report.session, Some(phase));
        assert_eq!(observed_write, expected_write);
        assert_eq!(
            report.segments.len(),
            if expected_write.is_some() { 3 } else { 2 }
        );
        assert_eq!(
            report.segments.first().map(|segment| segment.page),
            Some(format_page()?)
        );
        assert!(matches!(
            trial.next_session(),
            Err(PmNameTrialError::UnexpectedEvent)
        ));
        assert_eq!(
            report.write,
            expected_write.map_or(PmNameTrialWriteDisposition::NotAttempted, |write| {
                PmNameTrialWriteDisposition::Acknowledged { write }
            })
        );
        assert_blocked(&mut radio).await;
        let transport = radio.into_transport();
        assert_eq!(transport.close_calls, 0);
        assert_eq!(transport.baud_changes, [9600]);
        assert_eq!(
            transport.memory_writes,
            usize::from(expected_write.is_some())
        );
        transport.mock.assert_complete();
        // The test attests external cleanup; the driver must never do so itself.
        trial.record(PmNameTrialEvent::SessionFinalized {
            id: report.session_id().ok_or("session ID missing")?,
        })?;
    }
    assert_eq!(trial.status(), PmNameTrialStatus::RestorationVerified);
    Ok(())
}

#[tokio::test]
async fn wrong_fresh_identity_prevents_entry_and_intent() -> TestResult {
    let mut trial = fixture()?;
    let mut mock = MockTransport::new();
    identity(&mut mock, "1.03");
    let mut radio = Radio::new(mock);
    let mut called = false;
    let report = radio
        .run_approved_pm1_trial_session_until_exit(
            &mut trial,
            || false,
            |_, _| {
                called = true;
                Ok(())
            },
        )
        .await;
    assert!(matches!(
        report.outcome,
        PmNameTrialSessionOutcome::Failed {
            stage: PmNameTrialSessionStage::Identity,
            error: PmNameTrialSessionError::Evidence(PmNameTrialError::IdentityMismatch),
        }
    ));
    assert_eq!(report.exit, McpProbeExit::NotEntered);
    assert_eq!(report.status, PmNameTrialStatus::NotWritten);
    assert_eq!(
        report
            .identity
            .as_ref()
            .map(|value| value.firmware.as_str()),
        Some("1.03")
    );
    assert!(!called);
    let mock = radio.into_transport();
    assert_eq!(mock.writes().len(), 3);
    mock.assert_complete();
    Ok(())
}

#[tokio::test]
async fn stale_page_or_format_refuses_writing_but_exits_at_a_known_boundary() -> TestResult {
    for changed_format in [false, true] {
        let mut trial = fixture()?;
        let mut mock = MockTransport::new();
        entry(&mut mock);
        let mut format = [0; 40];
        let mut page = *trial.original_page();
        if changed_format {
            *format.get_mut(2).ok_or("version missing")? = 1;
        } else {
            *page.get_mut(9).ok_or("adjacent PM selector missing")? ^= 1;
        }
        read(&mut mock, format_page()?, &format);
        read(&mut mock, trial.page(), &page);
        mock.expect(b"E", &[ACK]);
        let mut radio = Radio::new(mock);
        let mut called = false;
        let report = radio
            .run_approved_pm1_trial_session_until_exit(
                &mut trial,
                || false,
                |_, _| {
                    called = true;
                    Ok(())
                },
            )
            .await;
        assert!(matches!(
            report.outcome,
            PmNameTrialSessionOutcome::Failed {
                stage: PmNameTrialSessionStage::FreshComparison,
                error: PmNameTrialSessionError::Evidence(_),
            }
        ));
        assert_eq!(report.exit, McpProbeExit::Acknowledged);
        assert_eq!(report.segments.len(), 2);
        assert_eq!(report.write, PmNameTrialWriteDisposition::NotAttempted);
        assert_eq!(report.status, PmNameTrialStatus::NotWritten);
        assert!(!called);
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn durable_callback_failure_prevents_w_and_preserves_additional_exit_failure() -> TestResult {
    for exit_ack in [ACK, 0x15] {
        let mut trial = fixture()?;
        let mut mock = MockTransport::new();
        preflight(&mut mock, &trial, trial.original_page())?;
        mock.expect(b"E", &[exit_ack]);
        let mut radio = Radio::new(mock);
        let report = radio
            .run_approved_pm1_trial_session_until_exit(
                &mut trial,
                || false,
                |_, _| Err(io::Error::other("durable sync failed")),
            )
            .await;
        assert!(matches!(
            report.outcome,
            PmNameTrialSessionOutcome::Failed {
                stage: PmNameTrialSessionStage::DurableIntent,
                error: PmNameTrialSessionError::DurableIntent(_),
            }
        ));
        assert_eq!(report.cleanup_error.is_some(), exit_ack != ACK);
        assert_eq!(
            report.exit,
            if exit_ack == ACK {
                McpProbeExit::Acknowledged
            } else {
                McpProbeExit::NotAcknowledged
            }
        );
        assert_eq!(report.write, PmNameTrialWriteDisposition::NotAttempted);
        assert_eq!(report.status, PmNameTrialStatus::NotWritten);
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_at_every_pre_write_boundary_exits_without_an_intent() -> TestResult {
    for boundary in 0..5 {
        let mut trial = fixture()?;
        let mut mock = MockTransport::new();
        if boundary > 0 {
            identity(&mut mock, "1.02");
        }
        if boundary > 1 {
            mock.expect(b"0M PROGRAM\r", b"0M\r");
        }
        if boundary > 2 {
            read(&mut mock, format_page()?, &[0; 40]);
        }
        if boundary > 3 {
            read(&mut mock, trial.page(), trial.original_page());
        }
        if boundary > 1 {
            mock.expect(b"E", &[ACK]);
        }
        let mut radio = Radio::new(mock);
        let mut checks = 0;
        let mut called = false;
        let report = radio
            .run_approved_pm1_trial_session_until_exit(
                &mut trial,
                || {
                    let cancel = checks == boundary;
                    checks += 1;
                    cancel
                },
                |_, _| {
                    called = true;
                    Ok(())
                },
            )
            .await;
        assert!(matches!(
            report.outcome,
            PmNameTrialSessionOutcome::Cancelled
        ));
        assert_eq!(report.status, PmNameTrialStatus::NotWritten);
        assert_eq!(
            report.exit,
            if boundary > 1 {
                McpProbeExit::Acknowledged
            } else {
                McpProbeExit::NotEntered
            }
        );
        assert!(!called);
        assert!(trial.next_session().is_err());
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_after_intent_cannot_abandon_rename_restore_or_persistence() -> TestResult {
    let mut trial = fixture()?;
    let cancel = Cell::new(false);
    for _ in 0..3 {
        let mut radio = Radio::new(session_script(&trial)?);
        let report = radio
            .run_approved_pm1_trial_session_until_exit(
                &mut trial,
                || cancel.get(),
                |_, _| {
                    cancel.set(true);
                    Ok(())
                },
            )
            .await;
        assert_session_success(&report, &trial);
        trial.record(PmNameTrialEvent::SessionFinalized {
            id: report.session_id().ok_or("session ID missing")?,
        })?;
        radio.into_transport().assert_complete();
    }
    assert!(cancel.get());
    assert_eq!(trial.status(), PmNameTrialStatus::RestorationVerified);
    Ok(())
}

#[tokio::test]
async fn missing_write_ack_retains_obligation_and_sends_no_readback_or_exit() -> TestResult {
    for hangs in [false, true] {
        let mut trial = fixture()?;
        let mut mock = MockTransport::new();
        preflight(&mut mock, &trial, trial.original_page())?;
        let write = frame(trial.page(), trial.expected_page());
        if hangs {
            mock.expect_hang(&write);
        } else {
            mock.expect(&write, &[0x15]);
        }
        let mut radio = Radio::new(mock);
        radio.set_timeout(Duration::from_millis(1));
        let report = radio
            .run_approved_pm1_trial_session_until_exit(&mut trial, || false, |_, _| Ok(()))
            .await;
        assert!(matches!(
            report.outcome,
            PmNameTrialSessionOutcome::Failed {
                stage: PmNameTrialSessionStage::Write,
                ..
            }
        ));
        assert_eq!(report.exit, McpProbeExit::RecoveryRequired);
        assert_eq!(report.status, PmNameTrialStatus::PossiblyChanged);
        assert_eq!(
            report.write,
            PmNameTrialWriteDisposition::PossiblyDispatched {
                write: PmNameTrialWrite::Rename
            }
        );
        assert_eq!(report.segments.len(), 2);
        assert!(trial.next_session().is_err());
        assert_blocked(&mut radio).await;
        let mock = radio.into_transport();
        assert_eq!(mock.writes().last(), Some(&write));
        mock.assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn complete_wrong_readback_exits_but_never_clears_restoration_obligation() -> TestResult {
    let mut trial = fixture()?;
    let mut mock = MockTransport::new();
    preflight(&mut mock, &trial, trial.original_page())?;
    mock.expect(&frame(trial.page(), trial.expected_page()), &[ACK]);
    let mut wrong = *trial.expected_page();
    *wrong.get_mut(200).ok_or("unrelated byte missing")? ^= 1;
    read(&mut mock, trial.page(), &wrong);
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let report = radio
        .run_approved_pm1_trial_session_until_exit(&mut trial, || false, |_, _| Ok(()))
        .await;
    assert!(matches!(
        report.outcome,
        PmNameTrialSessionOutcome::Failed {
            stage: PmNameTrialSessionStage::ImmediateReadback,
            error: PmNameTrialSessionError::Evidence(PmNameTrialError::PageMismatch),
        }
    ));
    assert_eq!(report.exit, McpProbeExit::Acknowledged);
    assert_eq!(report.status, PmNameTrialStatus::PossiblyChanged);
    assert_eq!(
        report
            .segments
            .last()
            .map(|segment| segment.data.as_slice()),
        Some(wrong.as_slice())
    );
    assert!(trial.next_session().is_err());
    assert_blocked(&mut radio).await;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn failed_preflight_exchange_keeps_only_acknowledged_reads_and_sends_no_exit() -> TestResult {
    for target_page in [false, true] {
        let mut trial = fixture()?;
        let mut mock = MockTransport::new();
        entry(&mut mock);
        let page = if target_page {
            read(&mut mock, format_page()?, &[0; 40]);
            trial.page()
        } else {
            format_page()?
        };
        mock.expect(&read_request(page), &[b'X', 0, 0, 8, 40]);
        let mut radio = Radio::new(mock);
        let report = radio
            .run_approved_pm1_trial_session_until_exit(&mut trial, || false, |_, _| Ok(()))
            .await;
        assert!(matches!(report.outcome, PmNameTrialSessionOutcome::Failed {
            stage: PmNameTrialSessionStage::Read { page: failed }, ..
        } if failed == page));
        assert_eq!(report.segments.len(), usize::from(target_page));
        assert_eq!(report.exit, McpProbeExit::RecoveryRequired);
        assert_eq!(report.status, PmNameTrialStatus::NotWritten);
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn failed_entry_never_attempts_exit_or_records_a_write() -> TestResult {
    let mut trial = fixture()?;
    let mut mock = MockTransport::new();
    identity(&mut mock, "1.02");
    mock.expect(b"0M PROGRAM\r", b"wrong\r");
    let mut radio = Radio::new(mock);
    let report = radio
        .run_approved_pm1_trial_session_until_exit(&mut trial, || false, |_, _| Ok(()))
        .await;
    assert!(matches!(
        report.outcome,
        PmNameTrialSessionOutcome::Failed {
            stage: PmNameTrialSessionStage::Entry,
            ..
        }
    ));
    assert_eq!(report.exit, McpProbeExit::RecoveryRequired);
    assert_eq!(report.status, PmNameTrialStatus::NotWritten);
    assert!(report.entry_reply.is_none());
    assert!(report.segments.is_empty());
    assert_blocked(&mut radio).await;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn partial_post_write_readback_retains_obligation_and_prohibits_exit() -> TestResult {
    let mut trial = fixture()?;
    let mut mock = MockTransport::new();
    preflight(&mut mock, &trial, trial.original_page())?;
    mock.expect(&frame(trial.page(), trial.expected_page()), &[ACK]);
    mock.expect_partial_then_hang(&read_request(trial.page()), b"W\x04");
    let mut radio = Radio::new(mock);
    radio.set_timeout(Duration::from_millis(1));
    let report = radio
        .run_approved_pm1_trial_session_until_exit(&mut trial, || false, |_, _| Ok(()))
        .await;
    assert!(matches!(
        report.outcome,
        PmNameTrialSessionOutcome::Failed {
            stage: PmNameTrialSessionStage::ImmediateReadback,
            ..
        }
    ));
    assert_eq!(report.exit, McpProbeExit::RecoveryRequired);
    assert_eq!(report.status, PmNameTrialStatus::PossiblyChanged);
    assert_eq!(report.segments.len(), 2);
    assert_eq!(
        report.write,
        PmNameTrialWriteDisposition::Acknowledged {
            write: PmNameTrialWrite::Rename
        }
    );
    assert_blocked(&mut radio).await;
    radio.into_transport().assert_complete();
    Ok(())
}

async fn drop_pending(future: impl Future) {
    let mut future = pin!(future);
    let result = poll_fn(|context| Poll::Ready(future.as_mut().poll(context))).await;
    assert!(
        result.is_pending(),
        "the scripted exchange must still be in flight"
    );
}

#[tokio::test]
async fn dropping_entry_write_or_exit_blocks_all_protocol_reuse() -> TestResult {
    for phase in 0..3 {
        let mut trial = fixture()?;
        let mut mock = MockTransport::new();
        if phase == 0 {
            identity(&mut mock, "1.02");
            mock.expect_partial_then_hang(b"0M PROGRAM\r", b"0");
        } else {
            preflight(&mut mock, &trial, trial.original_page())?;
            let write = frame(trial.page(), trial.expected_page());
            if phase == 1 {
                mock.expect_hang(&write);
            } else {
                mock.expect(&write, &[ACK]);
                read(&mut mock, trial.page(), trial.expected_page());
                mock.expect_hang(b"E");
            }
        }
        let mut radio = Radio::new(mock);
        drop_pending(radio.run_approved_pm1_trial_session_until_exit(
            &mut trial,
            || false,
            |_, _| Ok(()),
        ))
        .await;
        assert_eq!(
            trial.status(),
            if phase == 0 {
                PmNameTrialStatus::NotWritten
            } else {
                PmNameTrialStatus::PossiblyChanged
            }
        );
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn unexpected_persisted_page_never_restores_a_stale_baseline() -> TestResult {
    let mut trial = fixture()?;
    let mut first = Radio::new(session_script(&trial)?);
    let report = first
        .run_approved_pm1_trial_session_until_exit(&mut trial, || false, |_, _| Ok(()))
        .await;
    assert_session_success(&report, &trial);
    trial.record(PmNameTrialEvent::SessionFinalized {
        id: report.session_id().ok_or("session ID missing")?,
    })?;
    first.into_transport().assert_complete();
    let mut mock = MockTransport::new();
    let mut drifted = *trial.expected_page();
    *drifted.get_mut(9).ok_or("PM selector missing")? ^= 1;
    preflight(&mut mock, &trial, &drifted)?;
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let mut called = false;
    let report = radio
        .run_approved_pm1_trial_session_until_exit(
            &mut trial,
            || true,
            |_, _| {
                called = true;
                Ok(())
            },
        )
        .await;
    assert!(matches!(
        report.outcome,
        PmNameTrialSessionOutcome::Failed {
            stage: PmNameTrialSessionStage::FreshComparison,
            error: PmNameTrialSessionError::Evidence(PmNameTrialError::PageMismatch),
        }
    ));
    assert!(!called);
    assert_eq!(report.write, PmNameTrialWriteDisposition::NotAttempted);
    assert_eq!(report.status, PmNameTrialStatus::PossiblyChanged);
    assert_eq!(report.exit, McpProbeExit::Acknowledged);
    assert_blocked(&mut radio).await;
    radio.into_transport().assert_complete();
    Ok(())
}
