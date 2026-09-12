//! Bounded MY1 update wire scope, fresh guards, and fail-closed lifecycle.
//!
//! All transports are scripted. Explicit test finalization is a caller
//! attestation, not hardware, capture-durability, or power-cycle evidence.

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
    My1Callsign, My1CallsignUpdate, My1CallsignUpdateError, My1CallsignUpdateEvent,
    My1CallsignUpdateSession as Session, My1CallsignUpdateStatus as Status,
};
use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::{
    Address, DvGatewayMode, Error, FirmwareIdentity, Identity, McpError, McpProbeExit,
    My1CallsignUpdateSessionError as SessionError, My1CallsignUpdateSessionOutcome as Outcome,
    My1CallsignUpdateSessionReport as Report, My1CallsignUpdateSessionStage as Stage,
    My1CallsignUpdateWriteDisposition as WriteDisposition, Page, Radio, RadioModel, RadioType,
};
use kenwood_transport::{MockTransport, Transport, TransportError};

type TestError = Box<dyn std::error::Error>;
type TestResult = Result<(), TestError>;

fn fixture() -> Result<My1CallsignUpdate, TestError> {
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    let mut target = [0xA5; 256];
    target.get_mut(..3).ok_or("target guards missing")?.fill(0);
    target.get_mut(8..16).ok_or("MY1 field missing")?.fill(0);
    let mut control = [0x5A; 256];
    *control.get_mut(9).ok_or("PM selector missing")? = 0;
    Ok(My1CallsignUpdate::prepare(
        &identity,
        &target,
        &control,
        None,
        &My1Callsign::new("KQ4NIT")?,
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
    mock.expect(b"GW\r", b"GW 0\r");
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

fn preflight(mock: &mut MockTransport, update: &My1CallsignUpdate, target: &[u8]) -> TestResult {
    entry(mock);
    read(mock, format_page()?, &[0; 40]);
    read(mock, update.control_page_spec(), update.control_page());
    read(mock, update.page(), target);
    Ok(())
}

fn session_script(update: &My1CallsignUpdate) -> Result<MockTransport, TestError> {
    let mut mock = MockTransport::new();
    match update.next_session()? {
        Session::Apply => {
            preflight(&mut mock, update, update.original_page())?;
            mock.expect(&frame(update.page(), update.desired_page()), &[ACK]);
            read(&mut mock, update.page(), update.desired_page());
        }
        Session::Verify => preflight(&mut mock, update, update.desired_page())?,
    }
    mock.expect(b"E", &[ACK]);
    Ok(mock)
}

/// Independent literals keep shared framing helpers from masking scope drift.
fn expected_wire(update: &My1CallsignUpdate, phase: Session) -> Vec<Vec<u8>> {
    let mut writes: Vec<Vec<u8>> = [
        b"ID\r".as_slice(),
        b"FV\r",
        b"TY\r",
        b"GW\r",
        b"0M PROGRAM\r",
        b"R\0\0\x08\x28",
        &[ACK],
        b"R\x04\xF0\0\0",
        &[ACK],
        b"R\x05\x10\0\0",
        &[ACK],
    ]
    .into_iter()
    .map(<[u8]>::to_vec)
    .collect();
    if phase == Session::Apply {
        let mut write = b"W\x05\x10\0\0".to_vec();
        write.extend_from_slice(update.desired_page());
        writes.extend([write, b"R\x05\x10\0\0".to_vec(), vec![ACK]]);
    }
    writes.push(b"E".to_vec());
    writes
}

#[derive(Debug, Clone, Copy)]
enum DispatchFailure {
    Error,
    Timeout,
}

#[derive(Debug)]
struct AuditedTransport {
    mock: MockTransport,
    intents: Arc<AtomicUsize>,
    exited: bool,
    baud_changes: Vec<u32>,
    close_calls: usize,
    dispatch_failure: Option<DispatchFailure>,
}

impl AuditedTransport {
    fn new(mock: MockTransport, intents: &Arc<AtomicUsize>) -> Self {
        Self {
            mock,
            intents: Arc::clone(intents),
            exited: false,
            baud_changes: Vec::new(),
            close_calls: 0,
            dispatch_failure: None,
        }
    }
}

impl Transport for AuditedTransport {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        assert!(
            !self.exited,
            "no command may reuse the original handle after E"
        );
        if bytes.first() == Some(&b'W') {
            assert_eq!(
                self.intents.load(Ordering::SeqCst),
                1,
                "exactly one completed durable callback must precede W"
            );
            assert_eq!(
                bytes.len(),
                261,
                "header and full payload must share one write"
            );
            assert_eq!(
                bytes.get(..5),
                Some(b"W\x05\x10\0\0".as_slice()),
                "the only admitted write is the canonical 256-byte MY1 page"
            );
        }
        self.mock.write(bytes).await?;
        if bytes.first() == Some(&b'W') {
            match self.dispatch_failure {
                Some(DispatchFailure::Error) => {
                    return Err(TransportError::Write(io::Error::other("dispatch failed")));
                }
                Some(DispatchFailure::Timeout) => return std::future::pending().await,
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
        assert!(
            !self.exited,
            "no baud operation may follow E on the original handle"
        );
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
        "uncertain or retired handles must refuse CAT without transport I/O"
    );
    assert!(
        matches!(
            radio.mcp_session(),
            Err(Error::Mcp(
                McpError::RecoveryRequired | McpError::ConnectionRetired
            ))
        ),
        "uncertain or retired handles must refuse MCP reborrowing"
    );
}

fn assert_failed(report: &Report, stage: Stage) {
    assert!(
        matches!(&report.outcome, Outcome::Failed { stage: actual, .. } if *actual == stage),
        "retain the first failed requirement {stage:?}: {report:?}"
    );
}

fn assert_success(report: &Report, update: &My1CallsignUpdate) {
    assert!(
        matches!(report.outcome, Outcome::AwaitingCatVerification),
        "only independently captured lifecycle and CAT finalization may remain: {report:?}"
    );
    assert_eq!(
        report.identity.as_ref(),
        Some(update.identity()),
        "retain fresh identity"
    );
    assert_eq!(
        report.gateway_mode,
        Some(DvGatewayMode::Off),
        "retain fresh Gateway Off"
    );
    assert_eq!(
        report.entry_reply.as_deref(),
        Some(b"0M".as_slice()),
        "require exact entry"
    );
    assert_eq!(
        report.exit,
        McpProbeExit::Acknowledged,
        "require E/ACK before retirement"
    );
    assert_eq!(
        report.status,
        Status::PossiblyChanged,
        "caller finalization remains outstanding"
    );
    assert!(
        report.cleanup_error.is_none(),
        "successful cleanup has no secondary error"
    );
}

fn finalize(update: &mut My1CallsignUpdate, report: &Report) -> TestResult {
    let identity = update.identity().clone();
    update.record(My1CallsignUpdateEvent::SessionFinalized {
        id: report.session_id().ok_or("session ID missing")?,
        identity: &identity,
        gateway_mode: DvGatewayMode::Off,
    })?;
    Ok(())
}

async fn apply_and_finalize(update: &mut My1CallsignUpdate) -> TestResult {
    let mut radio = Radio::new(session_script(update)?);
    let report = radio
        .set_my1_callsign_session_until_exit(update, || false, |_| Ok(()))
        .await;
    assert_success(&report, update);
    assert_blocked(&mut radio).await;
    radio.into_transport().assert_complete();
    finalize(update, &report)
}

async fn assert_completed_original(
    mut radio: Radio<AuditedTransport>,
    report: &Report,
    update: &My1CallsignUpdate,
    phase: Session,
    intents: &AtomicUsize,
) -> TestResult {
    assert_success(report, update);
    assert_eq!(
        report.session,
        Some(phase),
        "the engine alone selects apply then verification"
    );
    assert_eq!(
        report.segments.len(),
        if phase == Session::Apply { 4 } else { 3 },
        "retain every acknowledged read"
    );
    assert_eq!(
        report.segments.first().map(|segment| segment.page),
        Some(format_page()?),
        "format is always the first read"
    );
    assert_eq!(
        report
            .segments
            .get(1)
            .map(|segment| segment.data.as_slice()),
        Some(update.control_page().as_slice()),
        "retain every control-page byte"
    );
    assert_eq!(
        report.write,
        if phase == Session::Apply {
            WriteDisposition::Acknowledged
        } else {
            WriteDisposition::NotAttempted
        },
        "only apply may dispatch the requested page"
    );
    assert!(
        update.next_session().is_err(),
        "driver cannot advance without external finalization"
    );
    assert_blocked(&mut radio).await;
    let transport = radio.into_transport();
    assert_eq!(
        transport.mock.writes(),
        expected_wire(update, phase),
        "the entire wire schedule is closed and literal-pinned"
    );
    assert_eq!(
        transport.baud_changes,
        [9600],
        "only entry may configure the baud rate"
    );
    assert_eq!(transport.close_calls, 0, "caller owns original close/drop");
    assert_eq!(
        intents.load(Ordering::SeqCst),
        usize::from(phase == Session::Apply),
        "only apply accepts one durable intent"
    );
    transport.mock.assert_complete();
    Ok(())
}

#[tokio::test]
async fn two_sessions_pin_every_command_and_preserve_non_callsign_bytes() -> TestResult {
    let mut update = fixture()?;
    let original = *update.original_page();
    let desired = *update.desired_page();
    let control = *update.control_page();
    assert_eq!(
        desired.get(8..16),
        Some(b"KQ4NIT\0\0".as_slice()),
        "MY1 uses exact NUL-padded bytes"
    );
    for (offset, (before, after)) in original.iter().zip(&desired).enumerate() {
        assert!(
            before == after || (8..14).contains(&offset),
            "non-callsign byte {offset} changed"
        );
    }
    for phase in [Session::Apply, Session::Verify] {
        let intents = Arc::new(AtomicUsize::new(0));
        let mut radio = Radio::new(AuditedTransport::new(session_script(&update)?, &intents));
        let report = radio
            .set_my1_callsign_session_until_exit(
                &mut update,
                || false,
                |state| {
                    assert_eq!(
                        phase,
                        Session::Apply,
                        "verification cannot accept an intent"
                    );
                    assert_eq!(
                        state.status(),
                        Status::NotWritten,
                        "intent precedes possible dispatch"
                    );
                    assert_eq!(
                        state.original_page(),
                        &original,
                        "intent keeps the original image"
                    );
                    assert_eq!(
                        state.desired_page(),
                        &desired,
                        "intent keeps the desired image"
                    );
                    assert_eq!(
                        state.control_page(),
                        &control,
                        "intent keeps the control image"
                    );
                    let _previous = intents.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .await;
        assert_completed_original(radio, &report, &update, phase, &intents).await?;
        finalize(&mut update, &report)?;
    }
    assert_eq!(
        update.status(),
        Status::VerifiedAcrossSessions,
        "both externally finalized sessions are required"
    );
    assert_eq!(
        update.original_page(),
        &original,
        "the original image must never be rebased"
    );
    assert_eq!(
        update.desired_page(),
        &desired,
        "the desired image must remain immutable"
    );
    assert_eq!(
        update.control_page(),
        &control,
        "the control image must remain immutable"
    );
    Ok(())
}

#[tokio::test]
async fn mismatching_fresh_identity_blocks_gateway_entry_and_intent() -> TestResult {
    for (firmware, radio_type) in [("1.03", "K,2,1"), ("1.02", "J,2,1")] {
        let mut update = fixture()?;
        let mut mock = MockTransport::new();
        identity(&mut mock, firmware, radio_type);
        let mut radio = Radio::new(mock);
        let called = Cell::new(false);
        let report = radio
            .set_my1_callsign_session_until_exit(
                &mut update,
                || false,
                |_| {
                    called.set(true);
                    Ok(())
                },
            )
            .await;
        assert_failed(&report, Stage::Identity);
        assert!(
            matches!(
                report.outcome,
                Outcome::Failed {
                    error: SessionError::Evidence(My1CallsignUpdateError::IdentityMismatch),
                    ..
                }
            ),
            "retain the typed identity mismatch"
        );
        assert!(
            report.identity.is_some(),
            "retain the observed mismatching identity"
        );
        assert_eq!(
            report.gateway_mode, None,
            "identity mismatch must prevent the Gateway query"
        );
        assert_eq!(
            report.exit,
            McpProbeExit::NotEntered,
            "identity mismatch must prevent MCP entry"
        );
        assert_eq!(
            report.status,
            Status::NotWritten,
            "identity mismatch creates no write obligation"
        );
        assert!(
            !called.get(),
            "identity mismatch must prevent durable intent"
        );
        let mock = radio.into_transport();
        assert_eq!(
            mock.writes().len(),
            3,
            "only the three identity queries are allowed"
        );
        mock.assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn active_unknown_rejected_and_partial_gateway_replies_prevent_entry() -> TestResult {
    for response in [
        Some(b"GW 2\r".as_slice()),
        Some(b"GW 255\r"),
        Some(b"N\r"),
        None,
    ] {
        let mut update = fixture()?;
        let mut mock = MockTransport::new();
        identity(&mut mock, "1.02", "K,2,1");
        if let Some(response) = response {
            mock.expect(b"GW\r", response);
        } else {
            mock.expect_partial_then_hang(b"GW\r", b"GW ");
        }
        let mut radio = Radio::new(mock);
        radio.set_timeout(Duration::from_millis(1));
        let called = Cell::new(false);
        let report = radio
            .set_my1_callsign_session_until_exit(
                &mut update,
                || false,
                |_| {
                    called.set(true);
                    Ok(())
                },
            )
            .await;
        assert_failed(&report, Stage::GatewayGuard);
        let expected = match response {
            Some(b"GW 2\r") => Some(DvGatewayMode::Terminal),
            Some(b"GW 255\r") => Some(DvGatewayMode::Unqualified(255)),
            _ => None,
        };
        assert_eq!(
            report.gateway_mode, expected,
            "do not invent or discard Gateway observations"
        );
        if let Some(expected) = expected {
            assert!(
                matches!(report.outcome, Outcome::Failed { error: SessionError::Evidence(My1CallsignUpdateError::GatewayMode { actual }), .. } if actual == expected),
                "retain the exact refused typed Gateway mode"
            );
        } else {
            assert!(
                matches!(
                    report.outcome,
                    Outcome::Failed {
                        error: SessionError::Io(_),
                        ..
                    }
                ),
                "rejected or incomplete CAT replies must retain their I/O cause"
            );
        }
        assert_eq!(
            report.exit,
            McpProbeExit::NotEntered,
            "Gateway refusal prohibits MCP entry"
        );
        assert_eq!(
            report.status,
            Status::NotWritten,
            "Gateway refusal creates no write obligation"
        );
        assert_eq!(
            report.write,
            WriteDisposition::NotAttempted,
            "Gateway refusal prohibits W"
        );
        assert!(!called.get(), "Gateway refusal must prevent durable intent");
        if response.is_none() {
            assert_blocked(&mut radio).await;
        }
        let mock = radio.into_transport();
        assert_eq!(
            mock.writes().len(),
            4,
            "only identity and the one Gateway query are allowed"
        );
        mock.assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn format_control_and_target_drift_prevent_intent_without_rebasing() -> TestResult {
    for mutation in 0..7 {
        let mut update = fixture()?;
        let mut format = [0; 40];
        let mut control = *update.control_page();
        let mut target = *update.original_page();
        match mutation {
            0 => *format.get_mut(2).ok_or("format byte missing")? = 1,
            1 => *control.get_mut(9).ok_or("PM selector missing")? = 1,
            2 => *control.last_mut().ok_or("control tail missing")? ^= 1,
            3 => *target.get_mut(0).ok_or("Gateway byte missing")? = 2,
            4 => *target.get_mut(1).ok_or("MY selection missing")? = 1,
            5 => *target.get_mut(16).ok_or("MY1 memo missing")? ^= 1,
            _ => *target.last_mut().ok_or("target tail missing")? ^= 1,
        }
        let mut mock = MockTransport::new();
        entry(&mut mock);
        read(&mut mock, format_page()?, &format);
        read(&mut mock, update.control_page_spec(), &control);
        read(&mut mock, update.page(), &target);
        mock.expect(b"E", &[ACK]);
        let mut radio = Radio::new(mock);
        let called = Cell::new(false);
        let report = radio
            .set_my1_callsign_session_until_exit(
                &mut update,
                || false,
                |_| {
                    called.set(true);
                    Ok(())
                },
            )
            .await;
        assert_failed(&report, Stage::FreshComparison);
        assert_eq!(
            report.exit,
            McpProbeExit::Acknowledged,
            "complete scope mismatches permit detached exit"
        );
        assert_eq!(
            report.status,
            Status::NotWritten,
            "scope mismatches create no write obligation"
        );
        assert_eq!(
            report.write,
            WriteDisposition::NotAttempted,
            "scope mismatches prohibit W"
        );
        assert_eq!(
            report.segments.len(),
            3,
            "retain all complete guard observations"
        );
        assert!(
            !called.get(),
            "fresh drift {mutation} must prevent durable intent"
        );
        assert!(
            update.next_session().is_err(),
            "scope drift permanently halts the update"
        );
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn failed_durable_intent_preserves_the_first_error_and_any_exit_error() -> TestResult {
    for exit_ack in [ACK, 0x15] {
        let mut update = fixture()?;
        let mut mock = MockTransport::new();
        preflight(&mut mock, &update, update.original_page())?;
        mock.expect(b"E", &[exit_ack]);
        let mut radio = Radio::new(mock);
        let report = radio
            .set_my1_callsign_session_until_exit(
                &mut update,
                || false,
                |_| Err(io::Error::other("durable intent synchronization failed")),
            )
            .await;
        assert_failed(&report, Stage::DurableIntent);
        let Outcome::Failed {
            error: SessionError::DurableIntent(error),
            ..
        } = &report.outcome
        else {
            return Err("durable intent failure lost its original typed error".into());
        };
        assert_eq!(
            error.to_string(),
            "durable intent synchronization failed",
            "preserve the original callback failure"
        );
        assert_eq!(
            report.cleanup_error.is_some(),
            exit_ack != ACK,
            "secondary exit errors must not overwrite the original failure"
        );
        assert_eq!(
            report.exit,
            if exit_ack == ACK {
                McpProbeExit::Acknowledged
            } else {
                McpProbeExit::NotAcknowledged
            },
            "report the actual detached-exit result"
        );
        assert_eq!(
            report.write,
            WriteDisposition::NotAttempted,
            "failed durability prohibits W"
        );
        assert_eq!(
            report.status,
            Status::NotWritten,
            "a rejected intent cannot establish possible dispatch"
        );
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn every_pre_intent_cancellation_boundary_stops_at_complete_exchanges() -> TestResult {
    for boundary in 0..7 {
        let mut update = fixture()?;
        let mut mock = MockTransport::new();
        if boundary > 0 {
            identity(&mut mock, "1.02", "K,2,1");
        }
        if boundary > 1 {
            mock.expect(b"GW\r", b"GW 0\r");
        }
        if boundary > 2 {
            mock.expect(b"0M PROGRAM\r", b"0M\r");
        }
        if boundary > 3 {
            read(&mut mock, format_page()?, &[0; 40]);
        }
        if boundary > 4 {
            read(&mut mock, update.control_page_spec(), update.control_page());
        }
        if boundary > 5 {
            read(&mut mock, update.page(), update.original_page());
        }
        if boundary > 2 {
            mock.expect(b"E", &[ACK]);
        }
        let mut radio = Radio::new(mock);
        let mut checks = 0;
        let called = Cell::new(false);
        let report = radio
            .set_my1_callsign_session_until_exit(
                &mut update,
                || {
                    let cancelled = checks == boundary;
                    checks += 1;
                    cancelled
                },
                |_| {
                    called.set(true);
                    Ok(())
                },
            )
            .await;
        assert!(
            matches!(report.outcome, Outcome::Cancelled),
            "boundary {boundary}: {report:?}"
        );
        assert_eq!(
            report.status,
            Status::NotWritten,
            "pre-intent cancellation creates no write obligation"
        );
        assert_eq!(
            report.write,
            WriteDisposition::NotAttempted,
            "pre-intent cancellation prohibits W"
        );
        assert_eq!(
            report.exit,
            if boundary > 2 {
                McpProbeExit::Acknowledged
            } else {
                McpProbeExit::NotEntered
            },
            "only entered complete sessions require E/ACK"
        );
        assert!(
            !called.get(),
            "safe cancellation cannot accept a write intent"
        );
        assert!(
            update.next_session().is_err(),
            "cancelled updates must remain halted"
        );
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_after_intent_finishes_both_sessions_without_another_write() -> TestResult {
    let mut update = fixture()?;
    let cancel = Cell::new(false);
    let intents = Cell::new(0);
    for phase in [Session::Apply, Session::Verify] {
        let mut radio = Radio::new(session_script(&update)?);
        let report = radio
            .set_my1_callsign_session_until_exit(
                &mut update,
                || cancel.get(),
                |_| {
                    cancel.set(true);
                    intents.set(intents.get() + 1);
                    Ok(())
                },
            )
            .await;
        assert_success(&report, &update);
        assert_eq!(
            report.session,
            Some(phase),
            "late cancellation cannot abandon the required verification phase"
        );
        let mock = radio.into_transport();
        assert_eq!(
            mock.writes(),
            expected_wire(&update, phase),
            "late cancellation must not add rollback or retry commands"
        );
        mock.assert_complete();
        finalize(&mut update, &report)?;
    }
    assert!(
        cancel.get(),
        "the apply intent must have requested cancellation"
    );
    assert_eq!(
        intents.get(),
        1,
        "verification must not accept another intent"
    );
    assert_eq!(
        update.status(),
        Status::VerifiedAcrossSessions,
        "both required phases finish despite late cancellation"
    );
    Ok(())
}

#[tokio::test]
async fn failed_dispatch_or_ack_never_sends_readback_exit_retry_or_rollback() -> TestResult {
    for (dispatch, ack) in [
        (Some(DispatchFailure::Error), None),
        (Some(DispatchFailure::Timeout), None),
        (None, Some(0x15)),
        (None, None),
    ] {
        let mut update = fixture()?;
        let mut mock = MockTransport::new();
        preflight(&mut mock, &update, update.original_page())?;
        let write = frame(update.page(), update.desired_page());
        if dispatch.is_some() {
            mock.expect(&write, &[]);
        } else if let Some(ack) = ack {
            mock.expect(&write, &[ack]);
        } else {
            mock.expect_hang(&write);
        }
        let intents = Arc::new(AtomicUsize::new(0));
        let mut transport = AuditedTransport::new(mock, &intents);
        transport.dispatch_failure = dispatch;
        let mut radio = Radio::new(transport);
        radio.set_timeout(Duration::from_millis(1));
        let report = radio
            .set_my1_callsign_session_until_exit(
                &mut update,
                || false,
                |_| {
                    let _previous = intents.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .await;
        assert_failed(&report, Stage::Write);
        assert!(
            matches!(
                report.outcome,
                Outcome::Failed {
                    error: SessionError::Io(_),
                    ..
                }
            ),
            "dispatch or ACK failure must retain its I/O cause"
        );
        assert_eq!(
            report.write,
            WriteDisposition::PossiblyDispatched,
            "failure cannot prove zero delivery"
        );
        assert_eq!(
            report.status,
            Status::PossiblyChanged,
            "possible write remains material evidence"
        );
        assert_eq!(
            report.exit,
            McpProbeExit::RecoveryRequired,
            "uncertain exchange prohibits speculative E"
        );
        assert_eq!(
            report.segments.len(),
            3,
            "retain only complete pre-write reads"
        );
        assert!(
            update.next_session().is_err(),
            "uncertain write must permanently halt the update"
        );
        assert_blocked(&mut radio).await;
        let transport = radio.into_transport();
        assert_eq!(
            transport.mock.writes().last(),
            Some(&write),
            "W must be the last attempted command"
        );
        assert_eq!(
            intents.load(Ordering::SeqCst),
            1,
            "there must be no retry intent"
        );
        transport.mock.assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn complete_last_byte_readback_mismatch_exits_without_rollback_or_verification() -> TestResult
{
    let mut update = fixture()?;
    let mut mock = MockTransport::new();
    preflight(&mut mock, &update, update.original_page())?;
    mock.expect(&frame(update.page(), update.desired_page()), &[ACK]);
    let mut wrong = *update.desired_page();
    *wrong.last_mut().ok_or("target tail missing")? ^= 1;
    read(&mut mock, update.page(), &wrong);
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let report = radio
        .set_my1_callsign_session_until_exit(&mut update, || false, |_| Ok(()))
        .await;
    assert_failed(&report, Stage::ImmediateReadback);
    assert!(
        matches!(
            report.outcome,
            Outcome::Failed {
                error: SessionError::Evidence(My1CallsignUpdateError::PageMismatch),
                ..
            }
        ),
        "the final unrelated byte must participate in full-page comparison"
    );
    assert_eq!(
        report.exit,
        McpProbeExit::Acknowledged,
        "complete readback leaves a known exit boundary"
    );
    assert_eq!(
        report.status,
        Status::PossiblyChanged,
        "mismatching readback cannot clear possible change"
    );
    assert_eq!(
        report.write,
        WriteDisposition::Acknowledged,
        "preserve the earlier successful write ACK"
    );
    assert_eq!(
        report
            .segments
            .last()
            .map(|segment| segment.data.as_slice()),
        Some(wrong.as_slice()),
        "retain actual mismatching bytes"
    );
    assert!(
        update.next_session().is_err(),
        "readback mismatch must halt without a repair attempt"
    );
    assert_blocked(&mut radio).await;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn uncertain_entry_or_guard_reads_prohibit_intent_exit_and_handle_reuse() -> TestResult {
    for boundary in 0_usize..5 {
        let mut update = fixture()?;
        let mut mock = MockTransport::new();
        let expected_stage = if boundary < 2 {
            identity(&mut mock, "1.02", "K,2,1");
            mock.expect(b"GW\r", b"GW 0\r");
            if boundary == 0 {
                mock.expect(b"0M PROGRAM\r", b"wrong\r");
            } else {
                mock.expect_partial_then_hang(b"0M PROGRAM\r", b"0");
            }
            Stage::Entry
        } else {
            entry(&mut mock);
            if boundary > 2 {
                read(&mut mock, format_page()?, &[0; 40]);
            }
            if boundary > 3 {
                read(&mut mock, update.control_page_spec(), update.control_page());
            }
            let page = match boundary {
                2 => format_page()?,
                3 => update.control_page_spec(),
                _ => update.page(),
            };
            mock.expect_partial_then_hang(&read_request(page), b"W");
            Stage::Read { page }
        };
        let mut radio = Radio::new(mock);
        radio.set_timeout(Duration::from_millis(1));
        let called = Cell::new(false);
        let report = radio
            .set_my1_callsign_session_until_exit(
                &mut update,
                || false,
                |_| {
                    called.set(true);
                    Ok(())
                },
            )
            .await;
        assert_failed(&report, expected_stage);
        assert_eq!(
            report.exit,
            McpProbeExit::RecoveryRequired,
            "uncertain entry/read prohibits E"
        );
        assert_eq!(
            report.status,
            Status::NotWritten,
            "no durable write intent was reached"
        );
        assert_eq!(
            report.write,
            WriteDisposition::NotAttempted,
            "uncertain preflight cannot dispatch W"
        );
        assert_eq!(
            report.segments.len(),
            boundary.saturating_sub(2),
            "retain only complete acknowledged pages"
        );
        assert!(
            !called.get(),
            "entry/read failure must prevent durable intent"
        );
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn readback_and_exit_uncertainty_preserve_distinct_completed_evidence() -> TestResult {
    for boundary in 0..3 {
        let mut update = fixture()?;
        let mut mock = MockTransport::new();
        preflight(&mut mock, &update, update.original_page())?;
        mock.expect(&frame(update.page(), update.desired_page()), &[ACK]);
        if boundary == 0 {
            mock.expect_partial_then_hang(&read_request(update.page()), b"W\x05");
        } else {
            read(&mut mock, update.page(), update.desired_page());
            if boundary == 1 {
                mock.expect(b"E", &[0x15]);
            } else {
                mock.expect_hang(b"E");
            }
        }
        let mut radio = Radio::new(mock);
        radio.set_timeout(Duration::from_millis(1));
        let report = radio
            .set_my1_callsign_session_until_exit(&mut update, || false, |_| Ok(()))
            .await;
        assert_failed(
            &report,
            if boundary == 0 {
                Stage::ImmediateReadback
            } else {
                Stage::Exit
            },
        );
        assert_eq!(
            report.status,
            Status::PossiblyChanged,
            "partial lifecycle cannot finalize the change"
        );
        assert_eq!(
            report.write,
            WriteDisposition::Acknowledged,
            "preserve the observed write ACK"
        );
        assert_eq!(
            report.segments.len(),
            if boundary == 0 { 3 } else { 4 },
            "never retain incomplete readback as a segment"
        );
        assert_eq!(
            report.exit,
            if boundary == 0 {
                McpProbeExit::RecoveryRequired
            } else {
                McpProbeExit::NotAcknowledged
            },
            "distinguish no safe exit from a failed attempted exit"
        );
        assert!(
            report.cleanup_error.is_none(),
            "the exit failure is primary after successful readback"
        );
        assert!(
            update.next_session().is_err(),
            "partial lifecycle must halt further sessions"
        );
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn separate_session_final_byte_drift_never_rewrites_despite_late_cancellation() -> TestResult
{
    for change_control in [false, true] {
        let mut update = fixture()?;
        apply_and_finalize(&mut update).await?;
        let mut control = *update.control_page();
        let mut target = *update.desired_page();
        if change_control {
            *control.last_mut().ok_or("control tail missing")? ^= 1;
        } else {
            *target.last_mut().ok_or("target tail missing")? ^= 1;
        }
        let mut mock = MockTransport::new();
        entry(&mut mock);
        read(&mut mock, format_page()?, &[0; 40]);
        read(&mut mock, update.control_page_spec(), &control);
        read(&mut mock, update.page(), &target);
        mock.expect(b"E", &[ACK]);
        let mut radio = Radio::new(mock);
        let called = Cell::new(false);
        let report = radio
            .set_my1_callsign_session_until_exit(
                &mut update,
                || true,
                |_| {
                    called.set(true);
                    Ok(())
                },
            )
            .await;
        assert_failed(&report, Stage::FreshComparison);
        assert_eq!(
            report.session,
            Some(Session::Verify),
            "only independent read-only verification was requested"
        );
        assert_eq!(
            report.status,
            Status::PossiblyChanged,
            "drift cannot establish requested-page persistence"
        );
        assert_eq!(
            report.write,
            WriteDisposition::NotAttempted,
            "verification cannot repair fresh drift"
        );
        assert_eq!(
            report.exit,
            McpProbeExit::Acknowledged,
            "complete mismatching pages permit detached exit"
        );
        assert!(
            !called.get(),
            "verification must never offer another write intent"
        );
        assert!(
            update.next_session().is_err(),
            "late cancellation does not permit retrying a failed verification"
        );
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn halted_or_unfinalized_updates_refuse_even_a_fresh_transport() -> TestResult {
    for unfinalized in [false, true] {
        let mut update = fixture()?;
        if unfinalized {
            let mut first = Radio::new(session_script(&update)?);
            let report = first
                .set_my1_callsign_session_until_exit(&mut update, || false, |_| Ok(()))
                .await;
            assert_success(&report, &update);
            first.into_transport().assert_complete();
        } else {
            update.halt();
        }
        let mut radio = Radio::new(MockTransport::new());
        let called = Cell::new(false);
        let report = radio
            .set_my1_callsign_session_until_exit(
                &mut update,
                || false,
                |_| {
                    called.set(true);
                    Ok(())
                },
            )
            .await;
        assert_failed(&report, Stage::Preparation);
        assert_eq!(
            report.session_id(),
            None,
            "no session may be selected from stale engine state"
        );
        assert_eq!(
            report.exit,
            McpProbeExit::NotEntered,
            "preparation refusal cannot enter MCP"
        );
        assert_eq!(
            report.status,
            if unfinalized {
                Status::PossiblyChanged
            } else {
                Status::NotWritten
            },
            "retain prior possible dispatch without inventing a new one"
        );
        assert!(
            !called.get(),
            "stale engine state must prevent durable intent"
        );
        let mock = radio.into_transport();
        assert!(
            mock.writes().is_empty(),
            "fresh transport does not override engine finalization"
        );
        mock.assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn a_finalized_apply_still_cannot_reuse_its_retired_handle_for_verification() -> TestResult {
    let mut update = fixture()?;
    let mut radio = Radio::new(session_script(&update)?);
    let first = radio
        .set_my1_callsign_session_until_exit(&mut update, || false, |_| Ok(()))
        .await;
    assert_success(&first, &update);
    finalize(&mut update, &first)?;
    let called = Cell::new(false);
    let report = radio
        .set_my1_callsign_session_until_exit(
            &mut update,
            || true,
            |_| {
                called.set(true);
                Ok(())
            },
        )
        .await;
    assert_failed(&report, Stage::Identity);
    assert!(
        matches!(
            report.outcome,
            Outcome::Failed {
                error: SessionError::Io(Error::Mcp(McpError::ConnectionRetired)),
                ..
            }
        ),
        "engine finalization cannot make the retired serial handle reusable"
    );
    assert_eq!(
        report.status,
        Status::PossiblyChanged,
        "failed verification preserves the earlier possible change"
    );
    assert_eq!(
        report.write,
        WriteDisposition::NotAttempted,
        "retired-handle refusal must not write again"
    );
    assert!(
        !called.get(),
        "retired-handle refusal must not request another intent"
    );
    let mock = radio.into_transport();
    assert_eq!(
        mock.writes(),
        expected_wire(&update, Session::Apply),
        "verification on a retired handle must add no traffic"
    );
    mock.assert_complete();
    Ok(())
}

async fn drop_pending(future: impl Future) {
    let mut future = pin!(future);
    let result = poll_fn(|context| Poll::Ready(future.as_mut().poll(context))).await;
    assert!(
        result.is_pending(),
        "the scripted incomplete exchange must still be pending"
    );
}

#[tokio::test]
async fn dropped_entry_write_or_exit_cannot_reuse_the_protocol_handle() -> TestResult {
    for boundary in 0..3 {
        let mut update = fixture()?;
        let mut mock = MockTransport::new();
        if boundary == 0 {
            identity(&mut mock, "1.02", "K,2,1");
            mock.expect(b"GW\r", b"GW 0\r");
            mock.expect_partial_then_hang(b"0M PROGRAM\r", b"0");
        } else {
            preflight(&mut mock, &update, update.original_page())?;
            let write = frame(update.page(), update.desired_page());
            if boundary == 1 {
                mock.expect_hang(&write);
            } else {
                mock.expect(&write, &[ACK]);
                read(&mut mock, update.page(), update.desired_page());
                mock.expect_hang(b"E");
            }
        }
        let mut radio = Radio::new(mock);
        drop_pending(radio.set_my1_callsign_session_until_exit(&mut update, || false, |_| Ok(())))
            .await;
        assert_eq!(
            update.status(),
            if boundary == 0 {
                Status::NotWritten
            } else {
                Status::PossiblyChanged
            },
            "dropping a future cannot erase prior dispatch evidence"
        );
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}
