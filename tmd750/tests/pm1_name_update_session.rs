//! PM1 update wire scope, immutable before-images, and fail-closed lifecycle.
//!
//! Transports are scripted. The test calls the finalize hook itself, because
//! the driver never finalizes a session on its caller's behalf.

use kenwood_schema as _;
use mcp_d75_extract as _;
use mmdvm as _;
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
    Pm1Name, Pm1NameUpdate, Pm1NameUpdateError, Pm1NameUpdateEvent, Pm1NameUpdateSession,
    Pm1NameUpdateStatus,
};
use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::{
    Address, Error, FirmwareIdentity, Identity, McpError, McpProbeExit, Page,
    Pm1NameUpdateSessionError, Pm1NameUpdateSessionOutcome, Pm1NameUpdateSessionReport,
    Pm1NameUpdateSessionStage, Pm1NameUpdateWriteDisposition, Radio, RadioModel, RadioType,
};
use kenwood_transport::{MockTransport, Transport, TransportError};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture() -> Result<Pm1NameUpdate, Box<dyn std::error::Error>> {
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
    Ok(Pm1NameUpdate::prepare(
        &identity,
        &original,
        &Pm1Name::new("PM1")?,
        &Pm1Name::new("BASE")?,
    )?)
}

fn format_page() -> Result<Page, kenwood_tmd750::ValidationError> {
    Page::new(Address::new(8)?, 40)
}

fn identity(mock: &mut MockTransport, firmware: &str, radio_type: &str) {
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", format!("FV {firmware}\r").as_bytes());
    mock.expect(b"TY\r", format!("TY {radio_type}\r").as_bytes());
}

fn entry(mock: &mut MockTransport) {
    identity(mock, "1.02", "K,2,1");
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

fn preflight(mock: &mut MockTransport, update: &Pm1NameUpdate, before: &[u8]) -> TestResult {
    entry(mock);
    read(mock, format_page()?, &[0; 40]);
    read(mock, update.page(), before);
    Ok(())
}

fn session_script(update: &Pm1NameUpdate) -> Result<MockTransport, Box<dyn std::error::Error>> {
    let mut mock = MockTransport::new();
    match update.next_session()? {
        Pm1NameUpdateSession::Apply => {
            preflight(&mut mock, update, update.original_page())?;
            mock.expect(&frame(update.page(), update.desired_page()), &[ACK]);
            read(&mut mock, update.page(), update.desired_page());
        }
        Pm1NameUpdateSession::Verify => {
            preflight(&mut mock, update, update.desired_page())?;
        }
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
    write_failure: Option<WriteFailure>,
}

#[derive(Debug, Clone, Copy)]
enum WriteFailure {
    Error,
    Timeout,
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
            write_failure: None,
        }
    }
}

impl Transport for AuditedTransport {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        assert!(!self.exited, "the original handle must be retired after E");
        if bytes.first() == Some(&b'W') {
            assert_eq!(
                self.durable_intents.load(Ordering::SeqCst),
                1,
                "the durable callback must precede W"
            );
            assert_eq!(
                bytes.len(),
                261,
                "one write must contain header and payload"
            );
            self.memory_writes += 1;
        }
        self.mock.write(bytes).await?;
        if bytes.first() == Some(&b'W') {
            match self.write_failure {
                Some(WriteFailure::Error) => {
                    return Err(TransportError::Write(io::Error::other(
                        "write failed after dispatch began",
                    )));
                }
                Some(WriteFailure::Timeout) => return std::future::pending().await,
                None => {}
            }
        }
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
        assert!(!self.exited, "no baud operation may follow E");
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
        "CAT must refuse an uncertain or retired handle"
    );
    assert!(
        matches!(
            radio.mcp_session(),
            Err(Error::Mcp(
                McpError::RecoveryRequired | McpError::ConnectionRetired
            ))
        ),
        "MCP must refuse an uncertain or retired handle"
    );
}

fn assert_session_success(report: &Pm1NameUpdateSessionReport, update: &Pm1NameUpdate) {
    assert!(
        matches!(
            report.outcome,
            Pm1NameUpdateSessionOutcome::AwaitingCatVerification
        ),
        "only external CAT and lifecycle proof may remain"
    );
    assert_eq!(
        report.identity.as_ref(),
        Some(update.identity()),
        "new identity must match"
    );
    assert_eq!(
        report.entry_reply.as_deref(),
        Some(b"0M".as_slice()),
        "entry must match exactly"
    );
    assert_eq!(
        report.exit,
        McpProbeExit::Acknowledged,
        "exit must be acknowledged"
    );
    assert!(
        report.cleanup_error.is_none(),
        "successful sessions have no cleanup failure"
    );
    assert_eq!(
        report.status,
        Pm1NameUpdateStatus::PossiblyChanged,
        "external finalization is still required"
    );
}

#[tokio::test]
async fn exact_two_session_scope_writes_once_and_requires_external_finalization() -> TestResult {
    let mut update = fixture()?;
    for phase in [Pm1NameUpdateSession::Apply, Pm1NameUpdateSession::Verify] {
        let intents = Arc::new(AtomicUsize::new(0));
        let mut radio = Radio::new(AuditedTransport::new(session_script(&update)?, &intents));
        let report = radio
            .set_pm1_name_session_until_exit(
                &mut update,
                || false,
                |state| {
                    assert_eq!(phase, Pm1NameUpdateSession::Apply);
                    assert_eq!(state.status(), Pm1NameUpdateStatus::NotWritten);
                    let _previous = intents.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .await;
        assert_session_success(&report, &update);
        assert_eq!(report.session, Some(phase));
        let apply = phase == Pm1NameUpdateSession::Apply;
        assert_eq!(report.segments.len(), if apply { 3 } else { 2 });
        assert_eq!(
            report.segments.first().map(|segment| segment.page),
            Some(format_page()?)
        );
        assert!(
            update.next_session().is_err(),
            "caller finalization must precede the next session"
        );
        assert_eq!(
            report.write,
            if apply {
                Pm1NameUpdateWriteDisposition::Acknowledged
            } else {
                Pm1NameUpdateWriteDisposition::NotAttempted
            }
        );
        assert_blocked(&mut radio).await;
        let transport = radio.into_transport();
        assert_eq!(transport.close_calls, 0, "caller owns original close/drop");
        assert_eq!(transport.baud_changes, [9600]);
        assert_eq!(transport.memory_writes, usize::from(apply));
        transport.mock.assert_complete();
        update.record(Pm1NameUpdateEvent::SessionFinalized {
            id: report.session_id().ok_or("missing session ID")?,
        })?;
    }
    assert_eq!(update.status(), Pm1NameUpdateStatus::VerifiedAcrossSessions);
    Ok(())
}

#[tokio::test]
async fn firmware_or_type_mismatch_prevents_entry_and_durable_intent() -> TestResult {
    for (firmware, radio_type) in [("1.03", "K,2,1"), ("1.02", "J,2,1")] {
        let mut update = fixture()?;
        let mut mock = MockTransport::new();
        identity(&mut mock, firmware, radio_type);
        let mut radio = Radio::new(mock);
        let mut called = false;
        let report = radio
            .set_pm1_name_session_until_exit(
                &mut update,
                || false,
                |_| {
                    called = true;
                    Ok(())
                },
            )
            .await;
        assert!(
            matches!(
                report.outcome,
                Pm1NameUpdateSessionOutcome::Failed {
                    stage: Pm1NameUpdateSessionStage::Identity,
                    error: Pm1NameUpdateSessionError::Evidence(
                        Pm1NameUpdateError::IdentityMismatch
                    ),
                }
            ),
            "identity mismatch must stop at identity: {report:?}"
        );
        assert_eq!(report.exit, McpProbeExit::NotEntered);
        assert_eq!(report.status, Pm1NameUpdateStatus::NotWritten);
        assert!(!called, "identity mismatch must prevent intent");
        assert!(
            report.identity.is_some(),
            "retain the observed mismatching identity"
        );
        let mock = radio.into_transport();
        assert_eq!(mock.writes().len(), 3);
        mock.assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn whole_page_drift_or_unsupported_format_refuses_w_without_merging() -> TestResult {
    for changed_format in [false, true] {
        let mut update = fixture()?;
        let mut mock = MockTransport::new();
        entry(&mut mock);
        let mut format = [0; 40];
        let mut page = *update.original_page();
        if changed_format {
            *format.get_mut(2).ok_or("format byte missing")? = 1;
        } else {
            *page.get_mut(9).ok_or("adjacent PM selector missing")? ^= 1;
        }
        read(&mut mock, format_page()?, &format);
        read(&mut mock, update.page(), &page);
        mock.expect(b"E", &[ACK]);
        let mut radio = Radio::new(mock);
        let mut called = false;
        let report = radio
            .set_pm1_name_session_until_exit(
                &mut update,
                || false,
                |_| {
                    called = true;
                    Ok(())
                },
            )
            .await;
        assert!(
            matches!(
                report.outcome,
                Pm1NameUpdateSessionOutcome::Failed {
                    stage: Pm1NameUpdateSessionStage::FreshComparison,
                    error: Pm1NameUpdateSessionError::Evidence(_),
                }
            ),
            "fresh evidence must reject drift before intent: {report:?}"
        );
        assert_eq!(report.exit, McpProbeExit::Acknowledged);
        assert_eq!(report.write, Pm1NameUpdateWriteDisposition::NotAttempted);
        assert_eq!(report.status, Pm1NameUpdateStatus::NotWritten);
        assert_eq!(report.segments.len(), 2);
        assert!(!called, "page drift must prevent intent");
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn failed_durable_intent_prevents_w_and_retains_an_additional_exit_error() -> TestResult {
    for exit_ack in [ACK, 0x15] {
        let mut update = fixture()?;
        let mut mock = MockTransport::new();
        preflight(&mut mock, &update, update.original_page())?;
        mock.expect(b"E", &[exit_ack]);
        let mut radio = Radio::new(mock);
        let report = radio
            .set_pm1_name_session_until_exit(
                &mut update,
                || false,
                |_| Err(io::Error::other("durable synchronization failed")),
            )
            .await;
        assert!(
            matches!(
                report.outcome,
                Pm1NameUpdateSessionOutcome::Failed {
                    stage: Pm1NameUpdateSessionStage::DurableIntent,
                    error: Pm1NameUpdateSessionError::DurableIntent(_),
                }
            ),
            "preserve the original durable-intent failure: {report:?}"
        );
        assert_eq!(report.cleanup_error.is_some(), exit_ack != ACK);
        assert_eq!(
            report.exit,
            if exit_ack == ACK {
                McpProbeExit::Acknowledged
            } else {
                McpProbeExit::NotAcknowledged
            }
        );
        assert_eq!(report.write, Pm1NameUpdateWriteDisposition::NotAttempted);
        assert_eq!(report.status, Pm1NameUpdateStatus::NotWritten);
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn every_pre_write_cancellation_boundary_prevents_intent() -> TestResult {
    for boundary in 0..5 {
        let mut update = fixture()?;
        let mut mock = MockTransport::new();
        if boundary > 0 {
            identity(&mut mock, "1.02", "K,2,1");
        }
        if boundary > 1 {
            mock.expect(b"0M PROGRAM\r", b"0M\r");
        }
        if boundary > 2 {
            read(&mut mock, format_page()?, &[0; 40]);
        }
        if boundary > 3 {
            read(&mut mock, update.page(), update.original_page());
        }
        if boundary > 1 {
            mock.expect(b"E", &[ACK]);
        }
        let mut radio = Radio::new(mock);
        let mut checks = 0;
        let mut called = false;
        let report = radio
            .set_pm1_name_session_until_exit(
                &mut update,
                || {
                    let cancel = checks == boundary;
                    checks += 1;
                    cancel
                },
                |_| {
                    called = true;
                    Ok(())
                },
            )
            .await;
        assert!(
            matches!(report.outcome, Pm1NameUpdateSessionOutcome::Cancelled),
            "boundary {boundary} must honor safe cancellation: {report:?}"
        );
        assert_eq!(report.status, Pm1NameUpdateStatus::NotWritten);
        assert_eq!(
            report.exit,
            if boundary > 1 {
                McpProbeExit::Acknowledged
            } else {
                McpProbeExit::NotEntered
            }
        );
        assert!(!called, "safe cancellation must not journal an intent");
        assert!(
            update.next_session().is_err(),
            "cancelled updates must halt permanently"
        );
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_after_intent_cannot_abandon_apply_or_verification() -> TestResult {
    let mut update = fixture()?;
    let cancel = Cell::new(false);
    for _ in 0..2 {
        let mut radio = Radio::new(session_script(&update)?);
        let report = radio
            .set_pm1_name_session_until_exit(
                &mut update,
                || cancel.get(),
                |_| {
                    cancel.set(true);
                    Ok(())
                },
            )
            .await;
        assert_session_success(&report, &update);
        update.record(Pm1NameUpdateEvent::SessionFinalized {
            id: report.session_id().ok_or("missing session ID")?,
        })?;
        radio.into_transport().assert_complete();
    }
    assert!(
        cancel.get(),
        "the intent callback must have requested cancellation"
    );
    assert_eq!(update.status(), Pm1NameUpdateStatus::VerifiedAcrossSessions);
    Ok(())
}

#[tokio::test]
async fn missing_write_ack_sends_no_readback_exit_retry_or_rollback() -> TestResult {
    for hangs in [false, true] {
        let mut update = fixture()?;
        let mut mock = MockTransport::new();
        preflight(&mut mock, &update, update.original_page())?;
        let write = frame(update.page(), update.desired_page());
        if hangs {
            mock.expect_hang(&write);
        } else {
            mock.expect(&write, &[0x15]);
        }
        let mut radio = Radio::new(mock);
        radio.set_timeout(Duration::from_millis(1));
        let report = radio
            .set_pm1_name_session_until_exit(&mut update, || false, |_| Ok(()))
            .await;
        assert!(
            matches!(
                report.outcome,
                Pm1NameUpdateSessionOutcome::Failed {
                    stage: Pm1NameUpdateSessionStage::Write,
                    ..
                }
            ),
            "missing ACK must fail the write exchange: {report:?}"
        );
        assert_eq!(report.exit, McpProbeExit::RecoveryRequired);
        assert_eq!(report.status, Pm1NameUpdateStatus::PossiblyChanged);
        assert_eq!(
            report.write,
            Pm1NameUpdateWriteDisposition::PossiblyDispatched
        );
        assert_eq!(report.segments.len(), 2);
        assert!(
            update.next_session().is_err(),
            "uncertain writes must prevent further sessions"
        );
        assert_blocked(&mut radio).await;
        let mock = radio.into_transport();
        assert_eq!(mock.writes().last(), Some(&write));
        mock.assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn failed_or_timed_out_dispatch_preserves_uncertainty_without_followup_traffic() -> TestResult
{
    for write_failure in [WriteFailure::Error, WriteFailure::Timeout] {
        let mut update = fixture()?;
        let mut mock = MockTransport::new();
        preflight(&mut mock, &update, update.original_page())?;
        let write = frame(update.page(), update.desired_page());
        mock.expect(&write, &[]);
        let intents = Arc::new(AtomicUsize::new(0));
        let mut transport = AuditedTransport::new(mock, &intents);
        transport.write_failure = Some(write_failure);
        let mut radio = Radio::new(transport);
        radio.set_timeout(Duration::from_millis(1));
        let report = radio
            .set_pm1_name_session_until_exit(
                &mut update,
                || false,
                |_| {
                    let _previous = intents.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .await;
        assert!(
            matches!(
                report.outcome,
                Pm1NameUpdateSessionOutcome::Failed {
                    stage: Pm1NameUpdateSessionStage::Write,
                    error: Pm1NameUpdateSessionError::Io(_),
                }
            ),
            "dispatch failure must remain a write I/O error: {report:?}"
        );
        assert_eq!(
            report.write,
            Pm1NameUpdateWriteDisposition::PossiblyDispatched
        );
        assert_eq!(report.status, Pm1NameUpdateStatus::PossiblyChanged);
        assert_eq!(report.exit, McpProbeExit::RecoveryRequired);
        assert_blocked(&mut radio).await;
        let transport = radio.into_transport();
        assert_eq!(transport.memory_writes, 1);
        assert_eq!(transport.mock.writes().last(), Some(&write));
        transport.mock.assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn halted_update_is_rejected_before_any_radio_traffic() -> TestResult {
    let mut update = fixture()?;
    update.halt();
    let mut radio = Radio::new(MockTransport::new());
    let mut called = false;
    let report = radio
        .set_pm1_name_session_until_exit(
            &mut update,
            || false,
            |_| {
                called = true;
                Ok(())
            },
        )
        .await;
    assert!(
        matches!(
            report.outcome,
            Pm1NameUpdateSessionOutcome::Failed {
                stage: Pm1NameUpdateSessionStage::Preparation,
                ..
            }
        ),
        "halted updates must fail before identity I/O: {report:?}"
    );
    assert_eq!(report.session_id(), None);
    assert_eq!(report.exit, McpProbeExit::NotEntered);
    assert_eq!(report.status, Pm1NameUpdateStatus::NotWritten);
    assert!(!called, "halted updates must not journal an intent");
    let mock = radio.into_transport();
    assert!(
        mock.writes().is_empty(),
        "halted updates must send no bytes"
    );
    mock.assert_complete();
    Ok(())
}

#[tokio::test]
async fn complete_wrong_readback_exits_without_claiming_verification_or_rolling_back() -> TestResult
{
    let mut update = fixture()?;
    let mut mock = MockTransport::new();
    preflight(&mut mock, &update, update.original_page())?;
    mock.expect(&frame(update.page(), update.desired_page()), &[ACK]);
    let mut wrong = *update.desired_page();
    *wrong.get_mut(200).ok_or("unrelated byte missing")? ^= 1;
    read(&mut mock, update.page(), &wrong);
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let report = radio
        .set_pm1_name_session_until_exit(&mut update, || false, |_| Ok(()))
        .await;
    assert!(
        matches!(
            report.outcome,
            Pm1NameUpdateSessionOutcome::Failed {
                stage: Pm1NameUpdateSessionStage::ImmediateReadback,
                error: Pm1NameUpdateSessionError::Evidence(Pm1NameUpdateError::PageMismatch),
            }
        ),
        "wrong immediate bytes must remain a readback mismatch: {report:?}"
    );
    assert_eq!(report.exit, McpProbeExit::Acknowledged);
    assert_eq!(report.status, Pm1NameUpdateStatus::PossiblyChanged);
    assert_eq!(
        report
            .segments
            .last()
            .map(|segment| segment.data.as_slice()),
        Some(wrong.as_slice())
    );
    assert!(
        update.next_session().is_err(),
        "readback mismatch must permanently halt the update"
    );
    assert_blocked(&mut radio).await;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn incomplete_entry_or_preflight_sends_no_exit() -> TestResult {
    for stage in 0..3 {
        let mut update = fixture()?;
        let mut mock = MockTransport::new();
        if stage == 0 {
            identity(&mut mock, "1.02", "K,2,1");
            mock.expect(b"0M PROGRAM\r", b"wrong\r");
        } else {
            entry(&mut mock);
            let page = if stage == 1 {
                format_page()?
            } else {
                read(&mut mock, format_page()?, &[0; 40]);
                update.page()
            };
            mock.expect(&read_request(page), &[b'X', 0, 0, 8, 40]);
        }
        let mut radio = Radio::new(mock);
        let report = radio
            .set_pm1_name_session_until_exit(&mut update, || false, |_| Ok(()))
            .await;
        assert!(
            matches!(report.outcome, Pm1NameUpdateSessionOutcome::Failed { .. }),
            "incomplete preflight must fail without speculative cleanup: {report:?}"
        );
        assert_eq!(report.exit, McpProbeExit::RecoveryRequired);
        assert_eq!(report.status, Pm1NameUpdateStatus::NotWritten);
        assert_eq!(report.segments.len(), usize::from(stage == 2));
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn partial_post_write_readback_preserves_possible_change_and_refuses_exit() -> TestResult {
    let mut update = fixture()?;
    let mut mock = MockTransport::new();
    preflight(&mut mock, &update, update.original_page())?;
    mock.expect(&frame(update.page(), update.desired_page()), &[ACK]);
    mock.expect_partial_then_hang(&read_request(update.page()), b"W\x04");
    let mut radio = Radio::new(mock);
    radio.set_timeout(Duration::from_millis(1));
    let report = radio
        .set_pm1_name_session_until_exit(&mut update, || false, |_| Ok(()))
        .await;
    assert!(
        matches!(
            report.outcome,
            Pm1NameUpdateSessionOutcome::Failed {
                stage: Pm1NameUpdateSessionStage::ImmediateReadback,
                ..
            }
        ),
        "partial readback must fail at the readback stage: {report:?}"
    );
    assert_eq!(report.exit, McpProbeExit::RecoveryRequired);
    assert_eq!(report.status, Pm1NameUpdateStatus::PossiblyChanged);
    assert_eq!(report.segments.len(), 2);
    assert_eq!(report.write, Pm1NameUpdateWriteDisposition::Acknowledged);
    assert_blocked(&mut radio).await;
    radio.into_transport().assert_complete();
    Ok(())
}

async fn drop_pending(future: impl Future) {
    let mut future = pin!(future);
    let result = poll_fn(|context| Poll::Ready(future.as_mut().poll(context))).await;
    assert!(
        result.is_pending(),
        "scripted exchange must still be pending"
    );
}

#[tokio::test]
async fn dropped_entry_write_or_exit_prevents_protocol_reuse() -> TestResult {
    for phase in 0..3 {
        let mut update = fixture()?;
        let mut mock = MockTransport::new();
        if phase == 0 {
            identity(&mut mock, "1.02", "K,2,1");
            mock.expect_partial_then_hang(b"0M PROGRAM\r", b"0");
        } else {
            preflight(&mut mock, &update, update.original_page())?;
            let write = frame(update.page(), update.desired_page());
            if phase == 1 {
                mock.expect_hang(&write);
            } else {
                mock.expect(&write, &[ACK]);
                read(&mut mock, update.page(), update.desired_page());
                mock.expect_hang(b"E");
            }
        }
        let mut radio = Radio::new(mock);
        drop_pending(radio.set_pm1_name_session_until_exit(&mut update, || false, |_| Ok(())))
            .await;
        assert_eq!(
            update.status(),
            if phase == 0 {
                Pm1NameUpdateStatus::NotWritten
            } else {
                Pm1NameUpdateStatus::PossiblyChanged
            }
        );
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn separate_session_drift_never_rewrites_or_attests_verification() -> TestResult {
    let mut update = fixture()?;
    let mut first = Radio::new(session_script(&update)?);
    let report = first
        .set_pm1_name_session_until_exit(&mut update, || false, |_| Ok(()))
        .await;
    assert_session_success(&report, &update);
    update.record(Pm1NameUpdateEvent::SessionFinalized {
        id: report.session_id().ok_or("missing session ID")?,
    })?;
    first.into_transport().assert_complete();
    let mut mock = MockTransport::new();
    let mut drifted = *update.desired_page();
    *drifted.get_mut(9).ok_or("adjacent PM selector missing")? ^= 1;
    preflight(&mut mock, &update, &drifted)?;
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let mut called = false;
    let report = radio
        .set_pm1_name_session_until_exit(
            &mut update,
            || true,
            |_| {
                called = true;
                Ok(())
            },
        )
        .await;
    assert!(
        matches!(
            report.outcome,
            Pm1NameUpdateSessionOutcome::Failed {
                stage: Pm1NameUpdateSessionStage::FreshComparison,
                error: Pm1NameUpdateSessionError::Evidence(Pm1NameUpdateError::PageMismatch),
            }
        ),
        "fresh-session drift must remain a full-page mismatch: {report:?}"
    );
    assert!(!called, "verification cannot journal a second intent");
    assert_eq!(report.write, Pm1NameUpdateWriteDisposition::NotAttempted);
    assert_eq!(report.status, Pm1NameUpdateStatus::PossiblyChanged);
    assert_eq!(report.exit, McpProbeExit::Acknowledged);
    assert_blocked(&mut radio).await;
    radio.into_transport().assert_complete();
    Ok(())
}
