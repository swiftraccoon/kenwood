//! Fixed Terminal-exit wire scope, immutable guards, and fail-closed lifecycle.
//!
//! Separate mocked connections model the caller-finalized sequence; the tests
//! use no hardware.

use kenwood_schema as _;
use mcp_d75_extract as _;
use mmdvm as _;
use proptest as _;
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
    TerminalExitTrial, TerminalExitTrialError, TerminalExitTrialEvent, TerminalExitTrialSession,
    TerminalExitTrialStatus,
};
use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::{
    Address, DvGatewayMode, Error, FirmwareIdentity, Identity, McpError, McpProbeExit, Page, Radio,
    RadioModel, RadioType, TerminalExitTrialSessionError, TerminalExitTrialSessionOutcome,
    TerminalExitTrialSessionReport, TerminalExitTrialSessionStage,
    TerminalExitTrialWriteDisposition,
};
use kenwood_transport::{MockTransport, Transport, TransportError};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture() -> Result<TerminalExitTrial, Box<dyn std::error::Error>> {
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    let mut off = [0xA5; 256];
    *off.first_mut().ok_or("Gateway byte missing")? = 0;
    *off.get_mut(2).ok_or("Terminal subtype missing")? = 0;
    let mut control = [0x5A; 256];
    *control.get_mut(9).ok_or("PM selector missing")? = 0;
    let mut routing = [0x3C; 256];
    *routing.get_mut(71).ok_or("USB function missing")? = 0;
    *routing.get_mut(77).ok_or("Gateway route missing")? = 1;
    Ok(TerminalExitTrial::prepare_unqualified_offline(
        &identity, &off, &control, &routing,
    )?)
}

fn format_page() -> Result<Page, kenwood_tmd750::ValidationError> {
    Page::new(Address::new(8)?, 40)
}

fn identify(mock: &mut MockTransport, firmware: &str, radio_type: &str) {
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", format!("FV {firmware}\r").as_bytes());
    mock.expect(b"TY\r", format!("TY {radio_type}\r").as_bytes());
}

fn entry(mock: &mut MockTransport, gateway: u8) {
    identify(mock, "1.02", "K,2,1");
    mock.expect(b"GW\r", format!("GW {gateway}\r").as_bytes());
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

fn preflight(mock: &mut MockTransport, trial: &TerminalExitTrial, target: &[u8]) -> TestResult {
    entry(
        mock,
        match trial.next_session()? {
            TerminalExitTrialSession::Apply => 2,
            TerminalExitTrialSession::Verify => 0,
        },
    );
    read(mock, format_page()?, &[0; 40]);
    read(mock, trial.control_page_spec(), trial.control_page());
    read(mock, trial.routing_page_spec(), trial.routing_page());
    read(mock, trial.page(), target);
    Ok(())
}

fn session_script(trial: &TerminalExitTrial) -> Result<MockTransport, Box<dyn std::error::Error>> {
    let mut mock = MockTransport::new();
    match trial.next_session()? {
        TerminalExitTrialSession::Apply => {
            preflight(&mut mock, trial, trial.expected_active_page())?;
            mock.expect(&frame(trial.page(), trial.off_page()), &[ACK]);
            read(&mut mock, trial.page(), trial.off_page());
        }
        TerminalExitTrialSession::Verify => preflight(&mut mock, trial, trial.off_page())?,
    }
    mock.expect(b"E", &[ACK]);
    Ok(mock)
}

#[derive(Debug, Default)]
struct Audit {
    intents: AtomicUsize,
    completed_reads: AtomicUsize,
}

#[derive(Debug)]
struct AuditedTransport {
    mock: MockTransport,
    audit: Arc<Audit>,
    pending_read_ack: bool,
    exited: bool,
    baud_changes: Vec<u32>,
    close_calls: usize,
}

impl AuditedTransport {
    fn new(mock: MockTransport, audit: &Arc<Audit>) -> Self {
        Self {
            mock,
            audit: Arc::clone(audit),
            pending_read_ack: false,
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
            "no command may follow E on the retired handle"
        );
        if bytes.first() == Some(&b'W') {
            assert_eq!(
                self.audit.intents.load(Ordering::SeqCst),
                1,
                "the sole W requires exactly one successful durable-intent callback"
            );
            assert_eq!(
                self.audit.completed_reads.load(Ordering::SeqCst),
                4,
                "all four complete fresh read exchanges must precede W"
            );
            assert_eq!(bytes.len(), 261, "W must be one complete header/data frame");
            assert_eq!(
                bytes.get(..5),
                Some(b"W\x05\x10\x00\x00".as_slice()),
                "only the pinned 256-byte Gateway target may be written"
            );
        }
        self.mock.write(bytes).await?;
        self.pending_read_ack = bytes == [ACK];
        if bytes == b"E" {
            self.exited = true;
        }
        Ok(())
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        let count = self.mock.read(bytes).await?;
        if self.pending_read_ack && bytes.get(..count) == Some([ACK].as_slice()) {
            let _previous = self.audit.completed_reads.fetch_add(1, Ordering::SeqCst);
            self.pending_read_ack = false;
        }
        Ok(count)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.close_calls += 1;
        self.mock.close().await
    }

    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        assert!(
            !self.exited,
            "no baud operation may follow E on the retired handle"
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
        "CAT must refuse uncertain or retired handles without I/O"
    );
    assert!(
        matches!(
            radio.mcp_session(),
            Err(Error::Mcp(
                McpError::RecoveryRequired | McpError::ConnectionRetired
            ))
        ),
        "MCP must refuse uncertain or retired handles without I/O"
    );
}

fn assert_success(report: &TerminalExitTrialSessionReport, trial: &TerminalExitTrial) {
    assert!(
        matches!(
            report.outcome,
            TerminalExitTrialSessionOutcome::AwaitingCatVerification
        ),
        "successful wire exchanges still require external finalization: {report:?}"
    );
    assert_eq!(
        report.identity.as_ref(),
        Some(trial.identity()),
        "fresh identity must match"
    );
    assert_eq!(
        report.entry_reply.as_deref(),
        Some(b"0M".as_slice()),
        "entry must be exact"
    );
    assert_eq!(
        report.exit,
        McpProbeExit::Acknowledged,
        "E must be acknowledged"
    );
    assert!(
        report.cleanup_error.is_none(),
        "successful cleanup cannot contain an error"
    );
    assert_eq!(
        report.status,
        TerminalExitTrialStatus::PossiblyChanged,
        "the driver cannot clear the obligation without external finalization"
    );
}

fn finalize(trial: &mut TerminalExitTrial, report: &TerminalExitTrialSessionReport) -> TestResult {
    let identity = trial.identity().clone();
    trial.record(TerminalExitTrialEvent::SessionFinalized {
        id: report.session_id().ok_or("session ID missing")?,
        identity: &identity,
        gateway_mode: DvGatewayMode::Off,
    })?;
    Ok(())
}

async fn applied_trial() -> Result<TerminalExitTrial, Box<dyn std::error::Error>> {
    let mut trial = fixture()?;
    let mut radio = Radio::new(session_script(&trial)?);
    let report = radio
        .run_approved_terminal_exit_trial_session_until_exit(&mut trial, || false, |_| Ok(()))
        .await;
    assert_success(&report, &trial);
    finalize(&mut trial, &report)?;
    radio.into_transport().assert_complete();
    Ok(trial)
}

fn assert_session_report(
    report: &TerminalExitTrialSessionReport,
    trial: &TerminalExitTrial,
    phase: TerminalExitTrialSession,
) -> TestResult {
    let (id, gateway, reads, write) = match phase {
        TerminalExitTrialSession::Apply => (
            1,
            DvGatewayMode::Terminal,
            5,
            TerminalExitTrialWriteDisposition::Acknowledged,
        ),
        TerminalExitTrialSession::Verify => (
            2,
            DvGatewayMode::Off,
            4,
            TerminalExitTrialWriteDisposition::NotAttempted,
        ),
    };
    assert_success(report, trial);
    assert_eq!(
        report.session,
        Some(phase),
        "the engine alone selects each session"
    );
    assert_eq!(
        report.session_id().map(std::num::NonZeroU64::get),
        Some(id),
        "separate sessions need distinct fixed IDs"
    );
    assert_eq!(
        report.gateway_mode,
        Some(gateway),
        "each pre-entry Gateway state must be retained"
    );
    assert_eq!(
        report.write, write,
        "only Apply may dispatch a memory write"
    );
    assert_eq!(
        report.segments.len(),
        reads,
        "every acknowledged read must be retained"
    );
    let expected_pages = [
        format_page()?,
        trial.control_page_spec(),
        trial.routing_page_spec(),
        trial.page(),
    ];
    let observed: Vec<_> = report
        .segments
        .iter()
        .take(4)
        .map(|segment| segment.page)
        .collect();
    assert_eq!(
        observed, expected_pages,
        "format, control, routing, and target must be read in order"
    );
    Ok(())
}

#[tokio::test]
async fn exact_two_session_sequence_writes_only_off_and_requires_external_finalization()
-> TestResult {
    let mut trial = fixture()?;
    let off = *trial.off_page();
    let active = *trial.expected_active_page();
    let differences: Vec<_> = off
        .iter()
        .zip(&active)
        .enumerate()
        .filter_map(|(offset, (before, after))| {
            (before != after).then_some((offset, *before, *after))
        })
        .collect();
    assert_eq!(
        differences,
        [(0, 0, 2)],
        "the active expectation must change only Gateway mode"
    );
    let cancel = Cell::new(false);
    for phase in [
        TerminalExitTrialSession::Apply,
        TerminalExitTrialSession::Verify,
    ] {
        let audit = Arc::new(Audit::default());
        let mut radio = Radio::new(AuditedTransport::new(session_script(&trial)?, &audit));
        let report = radio
            .run_approved_terminal_exit_trial_session_until_exit(
                &mut trial,
                || cancel.get(),
                |state| {
                    assert_eq!(
                        audit.completed_reads.load(Ordering::SeqCst),
                        4,
                        "intent follows every fresh read ACK"
                    );
                    assert_eq!(state.off_page(), &off, "the Off page must never be rebased");
                    assert_eq!(
                        state.expected_active_page(),
                        &active,
                        "the active expectation must stay immutable"
                    );
                    assert_eq!(
                        state.status(),
                        TerminalExitTrialStatus::NotWritten,
                        "the callback precedes accepted intent"
                    );
                    let _previous = audit.intents.fetch_add(1, Ordering::SeqCst);
                    cancel.set(true);
                    Ok(())
                },
            )
            .await;
        assert_session_report(&report, &trial, phase)?;
        assert!(
            trial.next_session().is_err(),
            "the driver cannot attest a new post-exit connection itself"
        );
        assert_blocked(&mut radio).await;
        let transport = radio.into_transport();
        assert_eq!(
            transport.close_calls, 0,
            "original close remains an external obligation"
        );
        assert_eq!(
            transport.baud_changes,
            [9600],
            "only pre-entry baud setup is permitted"
        );
        assert_eq!(
            transport.audit.intents.load(Ordering::SeqCst),
            usize::from(phase == TerminalExitTrialSession::Apply),
            "Verify cannot request another intent"
        );
        transport.mock.assert_complete();
        finalize(&mut trial, &report)?;
    }
    assert!(
        cancel.get(),
        "post-intent cancellation was requested throughout verification"
    );
    assert_eq!(
        trial.status(),
        TerminalExitTrialStatus::OffVerifiedAcrossSessions,
        "only both external finalizations complete the trial"
    );
    Ok(())
}

#[tokio::test]
async fn wrong_complete_identity_prevents_gateway_entry_and_intent() -> TestResult {
    for (firmware, radio_type) in [("1.03", "K,2,1"), ("1.02", "E,2,1")] {
        let mut trial = fixture()?;
        let mut mock = MockTransport::new();
        identify(&mut mock, firmware, radio_type);
        let mut radio = Radio::new(mock);
        let mut called = false;
        let report = radio
            .run_approved_terminal_exit_trial_session_until_exit(
                &mut trial,
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
                TerminalExitTrialSessionOutcome::Failed {
                    stage: TerminalExitTrialSessionStage::Identity,
                    error: TerminalExitTrialSessionError::Evidence(
                        TerminalExitTrialError::IdentityMismatch
                    ),
                }
            ),
            "all complete identity components must match before further commands"
        );
        assert!(
            report.identity.is_some(),
            "the rejected complete identity must be retained"
        );
        assert_eq!(
            report.gateway_mode, None,
            "GW cannot follow an identity mismatch"
        );
        assert_eq!(
            report.exit,
            McpProbeExit::NotEntered,
            "MCP must not be entered"
        );
        assert!(
            !called,
            "identity mismatch must prevent the intent callback"
        );
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn wrong_gateway_is_retained_and_refused_in_both_sessions() -> TestResult {
    for phase in [
        TerminalExitTrialSession::Apply,
        TerminalExitTrialSession::Verify,
    ] {
        for raw in [0, 1, 2, 3, u8::MAX] {
            let expected = if phase == TerminalExitTrialSession::Apply {
                DvGatewayMode::Terminal
            } else {
                DvGatewayMode::Off
            };
            let actual = DvGatewayMode::from(raw);
            if actual == expected {
                continue;
            }
            let mut trial = if phase == TerminalExitTrialSession::Apply {
                fixture()?
            } else {
                applied_trial().await?
            };
            let mut mock = MockTransport::new();
            identify(&mut mock, "1.02", "K,2,1");
            mock.expect(b"GW\r", format!("GW {raw}\r").as_bytes());
            let mut radio = Radio::new(mock);
            let mut called = false;
            let report = radio
                .run_approved_terminal_exit_trial_session_until_exit(
                    &mut trial,
                    || false,
                    |_| {
                        called = true;
                        Ok(())
                    },
                )
                .await;
            assert!(
                matches!(report.outcome, TerminalExitTrialSessionOutcome::Failed {
                stage: TerminalExitTrialSessionStage::Gateway,
                error: TerminalExitTrialSessionError::Evidence(TerminalExitTrialError::GatewayModeMismatch { expected: wanted, actual: observed }),
            } if wanted == expected && observed == actual),
                "each session must reject every wrong Gateway state before MCP"
            );
            assert_eq!(
                report.gateway_mode,
                Some(actual),
                "refused raw values must survive in the report"
            );
            assert_eq!(
                report.exit,
                McpProbeExit::NotEntered,
                "no MCP entry may follow a failed Gateway guard"
            );
            assert_eq!(
                report.write,
                TerminalExitTrialWriteDisposition::NotAttempted,
                "a wrong Gateway state cannot authorize W"
            );
            assert!(!called, "the wrong Gateway state must prevent intent");
            radio.into_transport().assert_complete();
        }
    }
    Ok(())
}

#[tokio::test]
async fn partial_gateway_reply_prohibits_entry_and_handle_reuse() -> TestResult {
    let mut trial = fixture()?;
    let mut mock = MockTransport::new();
    identify(&mut mock, "1.02", "K,2,1");
    mock.expect_partial_then_hang(b"GW\r", b"GW ");
    let mut radio = Radio::new(mock);
    radio.set_timeout(Duration::from_millis(1));
    let report = radio
        .run_approved_terminal_exit_trial_session_until_exit(&mut trial, || false, |_| Ok(()))
        .await;
    assert!(
        matches!(
            report.outcome,
            TerminalExitTrialSessionOutcome::Failed {
                stage: TerminalExitTrialSessionStage::Gateway,
                ..
            }
        ),
        "partial Gateway bytes cannot authorize MCP entry"
    );
    assert_eq!(
        report.gateway_mode, None,
        "a partial reply is not an accepted observation"
    );
    assert_eq!(
        report.exit,
        McpProbeExit::NotEntered,
        "no speculative E may be sent"
    );
    assert_blocked(&mut radio).await;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn unsupported_format_stops_before_control_routing_and_target_reads() -> TestResult {
    let mut trial = fixture()?;
    let mut mock = MockTransport::new();
    entry(&mut mock, 2);
    let mut format = [0; 40];
    *format.get_mut(2).ok_or("format byte missing")? = 1;
    read(&mut mock, format_page()?, &format);
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let mut called = false;
    let report = radio
        .run_approved_terminal_exit_trial_session_until_exit(
            &mut trial,
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
            TerminalExitTrialSessionOutcome::Failed {
                stage: TerminalExitTrialSessionStage::FreshComparison,
                error: TerminalExitTrialSessionError::Evidence(
                    TerminalExitTrialError::MemoryFormat { actual: 1 }
                ),
            }
        ),
        "unknown memory format must stop interpretation immediately"
    );
    assert_eq!(
        report.segments.len(),
        1,
        "only the complete format fragment was read"
    );
    assert_eq!(
        report.exit,
        McpProbeExit::Acknowledged,
        "a complete rejected format still permits detached exit"
    );
    assert!(!called, "unknown format cannot reach intent");
    assert_blocked(&mut radio).await;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn every_guard_and_unrelated_page_byte_is_immutable_before_intent() -> TestResult {
    for (page_index, offset) in [
        (0, 9),
        (0, 200),
        (1, 71),
        (1, 77),
        (1, 200),
        (2, 0),
        (2, 2),
        (2, 8),
        (2, 200),
    ] {
        let mut trial = fixture()?;
        let mut pages = [
            *trial.control_page(),
            *trial.routing_page(),
            *trial.expected_active_page(),
        ];
        *pages
            .get_mut(page_index)
            .and_then(|page| page.get_mut(offset))
            .ok_or("mutation byte missing")? ^= 1;
        let mut mock = MockTransport::new();
        entry(&mut mock, 2);
        read(&mut mock, format_page()?, &[0; 40]);
        for (page, data) in [
            trial.control_page_spec(),
            trial.routing_page_spec(),
            trial.page(),
        ]
        .into_iter()
        .zip(&pages)
        {
            read(&mut mock, page, data);
        }
        mock.expect(b"E", &[ACK]);
        let mut radio = Radio::new(mock);
        let mut called = false;
        let report = radio
            .run_approved_terminal_exit_trial_session_until_exit(
                &mut trial,
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
                TerminalExitTrialSessionOutcome::Failed {
                    stage: TerminalExitTrialSessionStage::FreshComparison,
                    ..
                }
            ),
            "every complete guard or target mismatch must fail before intent"
        );
        assert_eq!(
            report.segments.len(),
            4,
            "complete mismatching evidence must remain available"
        );
        assert_eq!(
            report.write,
            TerminalExitTrialWriteDisposition::NotAttempted,
            "no stale page may be written"
        );
        assert_eq!(
            report.status,
            TerminalExitTrialStatus::NotWritten,
            "drift cannot create a write obligation"
        );
        assert_eq!(
            report.exit,
            McpProbeExit::Acknowledged,
            "known-boundary refusal should exit safely"
        );
        assert!(!called, "any drift must prevent durable intent");
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn each_pre_intent_cancellation_boundary_exits_without_writing() -> TestResult {
    for boundary in 0..8 {
        let mut trial = fixture()?;
        let mut mock = MockTransport::new();
        if boundary > 0 {
            identify(&mut mock, "1.02", "K,2,1");
        }
        if boundary > 1 {
            mock.expect(b"GW\r", b"GW 2\r");
        }
        if boundary > 2 {
            mock.expect(b"0M PROGRAM\r", b"0M\r");
        }
        if boundary > 3 {
            read(&mut mock, format_page()?, &[0; 40]);
        }
        if boundary > 4 {
            read(&mut mock, trial.control_page_spec(), trial.control_page());
        }
        if boundary > 5 {
            read(&mut mock, trial.routing_page_spec(), trial.routing_page());
        }
        if boundary > 6 {
            read(&mut mock, trial.page(), trial.expected_active_page());
        }
        if boundary > 2 {
            mock.expect(b"E", &[ACK]);
        }
        let mut radio = Radio::new(mock);
        let mut checks = 0;
        let mut called = false;
        let report = radio
            .run_approved_terminal_exit_trial_session_until_exit(
                &mut trial,
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
            matches!(report.outcome, TerminalExitTrialSessionOutcome::Cancelled),
            "pre-intent cancellation must be reported distinctly"
        );
        assert_eq!(
            report.status,
            TerminalExitTrialStatus::NotWritten,
            "cancellation before intent creates no obligation"
        );
        assert_eq!(
            report.write,
            TerminalExitTrialWriteDisposition::NotAttempted,
            "cancellation must prevent W"
        );
        assert_eq!(
            report.exit,
            if boundary > 2 {
                McpProbeExit::Acknowledged
            } else {
                McpProbeExit::NotEntered
            },
            "only entered synchronized MCP sessions receive E"
        );
        assert!(
            !called,
            "cancelled sessions cannot invoke the intent callback"
        );
        assert!(
            trial.next_session().is_err(),
            "cancellation cannot silently restart this instance"
        );
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn failed_intent_prevents_write_and_preserves_any_independent_exit_error() -> TestResult {
    for exit_ack in [ACK, 0x15] {
        let mut trial = fixture()?;
        let mut mock = MockTransport::new();
        preflight(&mut mock, &trial, trial.expected_active_page())?;
        mock.expect(b"E", &[exit_ack]);
        let mut radio = Radio::new(mock);
        let report = radio
            .run_approved_terminal_exit_trial_session_until_exit(
                &mut trial,
                || false,
                |_| Err(io::Error::other("intent synchronization failed")),
            )
            .await;
        assert!(
            matches!(
                report.outcome,
                TerminalExitTrialSessionOutcome::Failed {
                    stage: TerminalExitTrialSessionStage::DurableIntent,
                    error: TerminalExitTrialSessionError::DurableIntent(_),
                }
            ),
            "the intent failure must remain the first failure"
        );
        assert_eq!(
            report.cleanup_error.is_some(),
            exit_ack != ACK,
            "an independent exit error must not replace or disappear behind the intent failure"
        );
        assert_eq!(
            report.exit,
            if exit_ack == ACK {
                McpProbeExit::Acknowledged
            } else {
                McpProbeExit::NotAcknowledged
            },
            "exit evidence must match its actual acknowledgment"
        );
        assert_eq!(
            report.write,
            TerminalExitTrialWriteDisposition::NotAttempted,
            "failed synchronization prohibits W"
        );
        assert_eq!(
            report.status,
            TerminalExitTrialStatus::NotWritten,
            "an unsuccessful callback is not an accepted intent"
        );
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn failed_or_incomplete_entry_prohibits_reads_exit_and_reuse() -> TestResult {
    for partial in [false, true] {
        let mut trial = fixture()?;
        let mut mock = MockTransport::new();
        identify(&mut mock, "1.02", "K,2,1");
        mock.expect(b"GW\r", b"GW 2\r");
        if partial {
            mock.expect_partial_then_hang(b"0M PROGRAM\r", b"0");
        } else {
            mock.expect(b"0M PROGRAM\r", b"wrong\r");
        }
        let mut radio = Radio::new(mock);
        radio.set_timeout(Duration::from_millis(1));
        let report = radio
            .run_approved_terminal_exit_trial_session_until_exit(&mut trial, || false, |_| Ok(()))
            .await;
        assert!(
            matches!(
                report.outcome,
                TerminalExitTrialSessionOutcome::Failed {
                    stage: TerminalExitTrialSessionStage::Entry,
                    ..
                }
            ),
            "only the exact complete entry reply may admit page traffic"
        );
        assert_eq!(
            report.exit,
            McpProbeExit::RecoveryRequired,
            "uncertain entry prohibits speculative exit"
        );
        assert!(
            report.entry_reply.is_none(),
            "wrong or partial entry cannot be recorded as accepted"
        );
        assert!(
            report.segments.is_empty(),
            "no page read may follow rejected entry"
        );
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn incomplete_preflight_reads_retain_only_complete_evidence_and_send_no_exit() -> TestResult {
    for failed_index in 0..4 {
        let mut trial = fixture()?;
        let mut mock = MockTransport::new();
        entry(&mut mock, 2);
        let pages = [
            format_page()?,
            trial.control_page_spec(),
            trial.routing_page_spec(),
            trial.page(),
        ];
        let data = [
            [0; 40].as_slice(),
            trial.control_page().as_slice(),
            trial.routing_page().as_slice(),
            trial.expected_active_page().as_slice(),
        ];
        for (page, bytes) in pages.iter().copied().zip(data).take(failed_index) {
            read(&mut mock, page, bytes);
        }
        let failed_page = *pages.get(failed_index).ok_or("failed page missing")?;
        mock.expect_partial_then_hang(&read_request(failed_page), b"W\x05");
        let mut radio = Radio::new(mock);
        radio.set_timeout(Duration::from_millis(1));
        let report = radio
            .run_approved_terminal_exit_trial_session_until_exit(&mut trial, || false, |_| Ok(()))
            .await;
        assert!(
            matches!(report.outcome, TerminalExitTrialSessionOutcome::Failed { stage: TerminalExitTrialSessionStage::Read { page }, .. } if page == failed_page),
            "the exact incomplete read must be identified"
        );
        assert_eq!(
            report.segments.len(),
            failed_index,
            "partial page bytes cannot become complete evidence"
        );
        assert_eq!(
            report.exit,
            McpProbeExit::RecoveryRequired,
            "an incomplete read prohibits E"
        );
        assert_eq!(
            report.status,
            TerminalExitTrialStatus::NotWritten,
            "partial guard evidence cannot create intent"
        );
        assert_blocked(&mut radio).await;
        let mock = radio.into_transport();
        assert_eq!(
            mock.writes().last().map(Vec::as_slice),
            Some(read_request(failed_page).as_slice()),
            "nothing may follow the uncertain read"
        );
        mock.assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn a_complete_payload_without_the_read_ack_cannot_authorize_intent() -> TestResult {
    let mut trial = fixture()?;
    let mut mock = MockTransport::new();
    entry(&mut mock, 2);
    read(&mut mock, format_page()?, &[0; 40]);
    read(&mut mock, trial.control_page_spec(), trial.control_page());
    read(&mut mock, trial.routing_page_spec(), trial.routing_page());
    mock.expect(
        &read_request(trial.page()),
        &frame(trial.page(), trial.expected_active_page()),
    );
    mock.expect_hang(&[ACK]);
    let mut radio = Radio::new(mock);
    radio.set_timeout(Duration::from_millis(1));
    let mut called = false;
    let report = radio
        .run_approved_terminal_exit_trial_session_until_exit(
            &mut trial,
            || false,
            |_| {
                called = true;
                Ok(())
            },
        )
        .await;
    assert!(
        matches!(report.outcome, TerminalExitTrialSessionOutcome::Failed {
            stage: TerminalExitTrialSessionStage::Read { page }, ..
        } if page == trial.page()),
        "full payload bytes without the final ACK remain an incomplete exchange"
    );
    assert_eq!(
        report.segments.len(),
        3,
        "the unacknowledged target must not join the evidence"
    );
    assert_eq!(
        report.write,
        TerminalExitTrialWriteDisposition::NotAttempted,
        "an incomplete handshake cannot authorize W"
    );
    assert_eq!(
        report.exit,
        McpProbeExit::RecoveryRequired,
        "the incomplete ACK phase prohibits speculative E"
    );
    assert!(
        !called,
        "the durable intent cannot precede the target read ACK"
    );
    assert_blocked(&mut radio).await;
    let mock = radio.into_transport();
    assert_eq!(
        mock.writes().last().map(Vec::as_slice),
        Some([ACK].as_slice()),
        "nothing may follow the uncertain ACK exchange"
    );
    mock.assert_complete();
    Ok(())
}

#[tokio::test]
async fn missing_write_ack_retains_possible_change_and_sends_no_readback_exit_or_retry()
-> TestResult {
    for hangs in [false, true] {
        let mut trial = fixture()?;
        let mut mock = MockTransport::new();
        preflight(&mut mock, &trial, trial.expected_active_page())?;
        let write = frame(trial.page(), trial.off_page());
        if hangs {
            mock.expect_hang(&write);
        } else {
            mock.expect(&write, &[0x15]);
        }
        let mut radio = Radio::new(mock);
        radio.set_timeout(Duration::from_millis(1));
        let report = radio
            .run_approved_terminal_exit_trial_session_until_exit(&mut trial, || false, |_| Ok(()))
            .await;
        assert!(
            matches!(
                report.outcome,
                TerminalExitTrialSessionOutcome::Failed {
                    stage: TerminalExitTrialSessionStage::Write,
                    ..
                }
            ),
            "an absent or wrong W acknowledgment must fail the write"
        );
        assert_eq!(
            report.write,
            TerminalExitTrialWriteDisposition::PossiblyDispatched,
            "failure cannot establish that no write bytes reached the radio"
        );
        assert_eq!(
            report.status,
            TerminalExitTrialStatus::PossiblyChanged,
            "uncertain dispatch must retain the obligation"
        );
        assert_eq!(
            report.exit,
            McpProbeExit::RecoveryRequired,
            "uncertain W prohibits speculative cleanup"
        );
        assert_eq!(
            report.segments.len(),
            4,
            "no readback may follow uncertain W"
        );
        assert!(
            trial.next_session().is_err(),
            "an uncertain write must permanently halt this attempt"
        );
        assert_blocked(&mut radio).await;
        let mock = radio.into_transport();
        assert_eq!(
            mock.writes().last(),
            Some(&write),
            "nothing may follow uncertain dispatch"
        );
        mock.assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn mismatching_or_incomplete_immediate_readback_never_clears_possible_change() -> TestResult {
    for incomplete in [false, true] {
        let mut trial = fixture()?;
        let mut mock = MockTransport::new();
        preflight(&mut mock, &trial, trial.expected_active_page())?;
        mock.expect(&frame(trial.page(), trial.off_page()), &[ACK]);
        if incomplete {
            mock.expect_partial_then_hang(&read_request(trial.page()), b"W\x05");
        } else {
            let mut wrong = *trial.off_page();
            *wrong
                .get_mut(200)
                .ok_or("unrelated readback byte missing")? ^= 1;
            read(&mut mock, trial.page(), &wrong);
            mock.expect(b"E", &[ACK]);
        }
        let mut radio = Radio::new(mock);
        radio.set_timeout(Duration::from_millis(1));
        let report = radio
            .run_approved_terminal_exit_trial_session_until_exit(&mut trial, || false, |_| Ok(()))
            .await;
        assert!(
            matches!(
                report.outcome,
                TerminalExitTrialSessionOutcome::Failed {
                    stage: TerminalExitTrialSessionStage::ImmediateReadback,
                    ..
                }
            ),
            "every readback byte must match before verification can continue"
        );
        assert_eq!(
            report.status,
            TerminalExitTrialStatus::PossiblyChanged,
            "acknowledgment is not complete Off-page evidence"
        );
        assert_eq!(
            report.write,
            TerminalExitTrialWriteDisposition::Acknowledged,
            "the accepted W ACK must survive later failure"
        );
        assert_eq!(
            report.exit,
            if incomplete {
                McpProbeExit::RecoveryRequired
            } else {
                McpProbeExit::Acknowledged
            },
            "only complete readback boundaries permit E"
        );
        assert_eq!(
            report.segments.len(),
            if incomplete { 4 } else { 5 },
            "only fully acknowledged readback joins the evidence"
        );
        assert!(
            trial.next_session().is_err(),
            "readback failure must halt without retrying the write"
        );
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn fresh_verification_drift_cannot_rewrite_or_rebase_any_page() -> TestResult {
    for page_index in 0..3 {
        let mut trial = applied_trial().await?;
        let mut pages = [
            *trial.control_page(),
            *trial.routing_page(),
            *trial.off_page(),
        ];
        *pages
            .get_mut(page_index)
            .and_then(|page| page.get_mut(200))
            .ok_or("unrelated verification byte missing")? ^= 1;
        let mut mock = MockTransport::new();
        entry(&mut mock, 0);
        read(&mut mock, format_page()?, &[0; 40]);
        for (page, bytes) in [
            trial.control_page_spec(),
            trial.routing_page_spec(),
            trial.page(),
        ]
        .into_iter()
        .zip(&pages)
        {
            read(&mut mock, page, bytes);
        }
        mock.expect(b"E", &[ACK]);
        let mut radio = Radio::new(mock);
        let mut called = false;
        let report = radio
            .run_approved_terminal_exit_trial_session_until_exit(
                &mut trial,
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
                TerminalExitTrialSessionOutcome::Failed {
                    stage: TerminalExitTrialSessionStage::FreshComparison,
                    ..
                }
            ),
            "fresh verification compares every immutable guard and target byte"
        );
        assert!(!called, "verification must never request a second intent");
        assert_eq!(
            report.write,
            TerminalExitTrialWriteDisposition::NotAttempted,
            "verification cannot repair unexpected state"
        );
        assert_eq!(
            report.status,
            TerminalExitTrialStatus::PossiblyChanged,
            "failed verification cannot clear the accepted intent"
        );
        assert_eq!(
            report.exit,
            McpProbeExit::Acknowledged,
            "complete mismatches permit detached exit"
        );
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn a_second_connection_cannot_skip_external_finalization() -> TestResult {
    let mut trial = fixture()?;
    let mut first = Radio::new(session_script(&trial)?);
    let first_report = first
        .run_approved_terminal_exit_trial_session_until_exit(&mut trial, || false, |_| Ok(()))
        .await;
    assert_success(&first_report, &trial);
    first.into_transport().assert_complete();
    let mut fresh = Radio::new(MockTransport::new());
    let report = fresh
        .run_approved_terminal_exit_trial_session_until_exit(&mut trial, || false, |_| Ok(()))
        .await;
    assert!(
        matches!(
            report.outcome,
            TerminalExitTrialSessionOutcome::Failed {
                stage: TerminalExitTrialSessionStage::Preparation,
                error: TerminalExitTrialSessionError::Evidence(
                    TerminalExitTrialError::UnexpectedEvent
                ),
            }
        ),
        "a new driver call cannot substitute for the externally finalized lifecycle"
    );
    assert_eq!(report.session, None, "no fresh session was admitted");
    assert_eq!(
        report.write,
        TerminalExitTrialWriteDisposition::NotAttempted,
        "missing external evidence cannot trigger another W"
    );
    assert_eq!(
        report.status,
        TerminalExitTrialStatus::PossiblyChanged,
        "missing external evidence cannot clear the accepted intent"
    );
    let mock = fresh.into_transport();
    assert!(
        mock.writes().is_empty(),
        "preparation failure must send no bytes on the new handle"
    );
    mock.assert_complete();
    Ok(())
}

#[tokio::test]
async fn failed_exit_after_successful_readback_retains_the_obligation() -> TestResult {
    let mut trial = fixture()?;
    let mut mock = MockTransport::new();
    preflight(&mut mock, &trial, trial.expected_active_page())?;
    mock.expect(&frame(trial.page(), trial.off_page()), &[ACK]);
    read(&mut mock, trial.page(), trial.off_page());
    mock.expect_hang(b"E");
    let mut radio = Radio::new(mock);
    radio.set_timeout(Duration::from_millis(1));
    let report = radio
        .run_approved_terminal_exit_trial_session_until_exit(&mut trial, || false, |_| Ok(()))
        .await;
    assert!(
        matches!(
            report.outcome,
            TerminalExitTrialSessionOutcome::Failed {
                stage: TerminalExitTrialSessionStage::Exit,
                ..
            }
        ),
        "successful readback cannot hide failed exit"
    );
    assert_eq!(
        report.exit,
        McpProbeExit::NotAcknowledged,
        "missing E ACK is not completed exit"
    );
    assert_eq!(
        report.status,
        TerminalExitTrialStatus::PossiblyChanged,
        "external verification remains unproven"
    );
    assert!(
        trial.next_session().is_err(),
        "failed exit cannot progress into a new session"
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
async fn dropping_entry_write_or_exit_permanently_retires_the_inflight_handle() -> TestResult {
    for phase in 0..3 {
        let mut trial = fixture()?;
        let mut mock = MockTransport::new();
        if phase == 0 {
            identify(&mut mock, "1.02", "K,2,1");
            mock.expect(b"GW\r", b"GW 2\r");
            mock.expect_partial_then_hang(b"0M PROGRAM\r", b"0");
        } else {
            preflight(&mut mock, &trial, trial.expected_active_page())?;
            let write = frame(trial.page(), trial.off_page());
            if phase == 1 {
                mock.expect_hang(&write);
            } else {
                mock.expect(&write, &[ACK]);
                read(&mut mock, trial.page(), trial.off_page());
                mock.expect_hang(b"E");
            }
        }
        let mut radio = Radio::new(mock);
        drop_pending(radio.run_approved_terminal_exit_trial_session_until_exit(
            &mut trial,
            || false,
            |_| Ok(()),
        ))
        .await;
        assert_eq!(
            trial.status(),
            if phase == 0 {
                TerminalExitTrialStatus::NotWritten
            } else {
                TerminalExitTrialStatus::PossiblyChanged
            },
            "dropping a future cannot erase an already accepted intent"
        );
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}
