//! Fixed MY1 wire scope and guards, using no hardware or radio discovery.

use kenwood_thd75 as _;
use mcp_d75_extract as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use std::cell::Cell;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use kenwood_tmd750::memory::{
    MyCallsignTrial, PmNameTrialError, PmNameTrialSession, PmNameTrialStatus, PmNameTrialWrite,
};
use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::transport::{MockTransport, Transport, TransportError};
use kenwood_tmd750::{
    Address, DvGatewayMode, Error, FirmwareIdentity, Identity, McpError, McpProbeExit, Page,
    PmNameTrialSessionError, PmNameTrialSessionOutcome, PmNameTrialSessionReport,
    PmNameTrialSessionStage, PmNameTrialWriteDisposition, Radio, RadioModel, RadioType,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture() -> Result<MyCallsignTrial, Box<dyn std::error::Error>> {
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    let mut target = [0xA5; 256];
    target.get_mut(..3).ok_or("target guards missing")?.fill(0);
    target.get_mut(8..16).ok_or("MY1 field missing")?.fill(0);
    let mut control = [0x5A; 256];
    *control.get_mut(9).ok_or("active PM byte missing")? = 0;
    Ok(MyCallsignTrial::prepare_unqualified_offline(
        &identity, &target, &control,
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

fn preflight(mock: &mut MockTransport, trial: &MyCallsignTrial, before: &[u8]) -> TestResult {
    entry(mock);
    read(mock, format_page()?, &[0; 40]);
    read(
        mock,
        MyCallsignTrial::required_control_page()?,
        trial.control_page(),
    );
    read(mock, trial.page(), before);
    Ok(())
}

fn session_script(trial: &MyCallsignTrial) -> Result<MockTransport, Box<dyn std::error::Error>> {
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
    intents: Arc<AtomicUsize>,
    exited: bool,
}

impl Transport for AuditedTransport {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        assert!(
            !self.exited,
            "the retired handle must receive no command after E"
        );
        if bytes.first() == Some(&b'W') {
            assert_eq!(
                self.intents.load(Ordering::SeqCst),
                1,
                "the exact durable intent must precede the only session W"
            );
            assert_eq!(
                bytes.len(),
                261,
                "W must be a single complete header/data frame"
            );
            assert_eq!(
                bytes.get(..5),
                Some(b"W\x05\x10\x00\x00".as_slice()),
                "the only admitted MY1 write is the pinned 256-byte page"
            );
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
        self.mock.close().await
    }

    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        assert!(
            !self.exited,
            "the retired handle must receive no baud operation"
        );
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
        "an uncertain or retired handle must refuse CAT without I/O"
    );
    assert!(
        matches!(
            radio.mcp_session(),
            Err(Error::Mcp(
                McpError::RecoveryRequired | McpError::ConnectionRetired
            ))
        ),
        "an uncertain or retired handle must refuse MCP reborrowing"
    );
}

fn assert_session_success(report: &PmNameTrialSessionReport, trial: &MyCallsignTrial) {
    assert!(
        matches!(
            report.outcome,
            PmNameTrialSessionOutcome::AwaitingCatVerification
        ),
        "only independent CAT, capture, and durable finalization may remain: {report:?}"
    );
    assert_eq!(
        report.identity.as_ref(),
        Some(trial.identity()),
        "fresh identity must match"
    );
    assert_eq!(
        report.gateway_mode,
        Some(DvGatewayMode::Off),
        "fresh GW Off must be retained"
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
        PmNameTrialStatus::PossiblyChanged,
        "the radio driver cannot attest external finalization or clear the obligation"
    );
}

#[tokio::test]
async fn all_three_sessions_pin_guards_and_restore_the_exact_page() -> TestResult {
    let mut trial = fixture()?;
    let original = *trial.original_page();
    let expected = *trial.expected_page();
    assert_eq!(
        expected.get(8..16),
        Some(b"KQ4NIT\0\0".as_slice()),
        "MY1 requires exactly six ASCII bytes and two NUL padding bytes"
    );
    for (offset, (before, after)) in original.iter().zip(&expected).enumerate() {
        assert!(
            before == after || (8..14).contains(&offset),
            "only the six fixed callsign bytes may change, including no memo change"
        );
    }
    let cancel = Cell::new(false);
    let mut writes = 0;
    let mut reads = 0;
    for phase in [
        PmNameTrialSession::Rename,
        PmNameTrialSession::Restore,
        PmNameTrialSession::VerifyRestoration,
    ] {
        let intents = Arc::new(AtomicUsize::new(0));
        let mut radio = Radio::new(AuditedTransport {
            mock: session_script(&trial)?,
            intents: Arc::clone(&intents),
            exited: false,
        });
        let report = radio
            .run_approved_my1_trial_session_until_exit(
                &mut trial,
                || cancel.get(),
                |state, _| {
                    assert_eq!(
                        state.original_page(),
                        &original,
                        "recovery bytes must stay immutable"
                    );
                    assert_eq!(
                        state.expected_page(),
                        &expected,
                        "temporary bytes must stay immutable"
                    );
                    let _previous = intents.fetch_add(1, Ordering::SeqCst);
                    cancel.set(true);
                    Ok(())
                },
            )
            .await;
        assert_session_success(&report, &trial);
        assert_eq!(
            report.session,
            Some(phase),
            "only the engine chooses each session"
        );
        reads += report.segments.len();
        assert_blocked(&mut radio).await;
        let transport = radio.into_transport();
        writes += transport
            .mock
            .writes()
            .iter()
            .filter(|bytes| bytes.first() == Some(&b'W'))
            .count();
        assert_eq!(
            transport
                .mock
                .writes()
                .iter()
                .filter(|bytes| bytes.as_slice() == b"GW\r")
                .count(),
            1,
            "each fresh original connection must issue exactly one read-only GW"
        );
        transport.mock.assert_complete();
        trial.finalize_session(report.session_id().ok_or("session ID missing")?)?;
    }
    assert_eq!(
        writes, 2,
        "only temporary entry and exact restoration may write"
    );
    assert_eq!(
        reads, 11,
        "all guard and target reads must be retained across three sessions"
    );
    assert_eq!(
        trial.original_page(),
        &original,
        "the original page must never be rebased"
    );
    assert_eq!(
        trial.status(),
        PmNameTrialStatus::RestorationVerified,
        "all three externally finalized sessions are required for restoration evidence"
    );
    Ok(())
}

#[tokio::test]
async fn mismatching_identity_prevents_gateway_query_and_mcp_entry() -> TestResult {
    let mut trial = fixture()?;
    let mut mock = MockTransport::new();
    identity(&mut mock, "1.03");
    let mut radio = Radio::new(mock);
    let mut called = false;
    let report = radio
        .run_approved_my1_trial_session_until_exit(
            &mut trial,
            || false,
            |_, _| {
                called = true;
                Ok(())
            },
        )
        .await;
    assert!(
        matches!(
            report.outcome,
            PmNameTrialSessionOutcome::Failed {
                stage: PmNameTrialSessionStage::Identity,
                error: PmNameTrialSessionError::Evidence(PmNameTrialError::IdentityMismatch),
            }
        ),
        "the exact firmware identity must be checked before all further commands"
    );
    assert!(
        !called,
        "identity mismatch must prevent the durable intent callback"
    );
    assert_eq!(
        report.gateway_mode, None,
        "GW must not be queried after identity mismatch"
    );
    assert_eq!(
        report.exit,
        McpProbeExit::NotEntered,
        "MCP must not be entered"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn every_nonzero_gateway_value_is_retained_and_refused_before_entry() -> TestResult {
    for (raw, expected) in [
        (1, DvGatewayMode::Unqualified(1)),
        (2, DvGatewayMode::Terminal),
        (3, DvGatewayMode::Unqualified(3)),
        (u8::MAX, DvGatewayMode::Unqualified(u8::MAX)),
    ] {
        let mut trial = fixture()?;
        let mut mock = MockTransport::new();
        identity(&mut mock, "1.02");
        mock.expect(b"GW\r", format!("GW {raw}\r").as_bytes());
        let mut radio = Radio::new(mock);
        let mut called = false;
        let report = radio
            .run_approved_my1_trial_session_until_exit(
                &mut trial,
                || false,
                |_, _| {
                    called = true;
                    Ok(())
                },
            )
            .await;
        assert!(
            matches!(report.outcome, PmNameTrialSessionOutcome::Failed {
            stage: PmNameTrialSessionStage::GatewayGuard,
            error: PmNameTrialSessionError::Evidence(PmNameTrialError::GatewayMode { actual }),
        } if actual == expected),
            "only typed Off can pass the CAT guard"
        );
        assert_eq!(
            report.gateway_mode,
            Some(expected),
            "refused gateway observations must not disappear from evidence"
        );
        assert_eq!(
            report.exit,
            McpProbeExit::NotEntered,
            "active or unknown GW must prevent MCP entry"
        );
        assert_eq!(
            report.status,
            PmNameTrialStatus::NotWritten,
            "no write obligation may be created"
        );
        assert!(
            !called,
            "nonzero GW must prevent the durable intent callback"
        );
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn incomplete_gateway_reply_blocks_entry_and_further_handle_use() -> TestResult {
    let mut trial = fixture()?;
    let mut mock = MockTransport::new();
    identity(&mut mock, "1.02");
    mock.expect_partial_then_hang(b"GW\r", b"GW ");
    let mut radio = Radio::new(mock);
    radio.set_timeout(Duration::from_millis(1));
    let report = radio
        .run_approved_my1_trial_session_until_exit(&mut trial, || false, |_, _| Ok(()))
        .await;
    assert!(
        matches!(
            report.outcome,
            PmNameTrialSessionOutcome::Failed {
                stage: PmNameTrialSessionStage::GatewayGuard,
                ..
            }
        ),
        "incomplete CAT GW is a gateway guard failure"
    );
    assert_eq!(
        report.gateway_mode, None,
        "partial CAT bytes are not an accepted observation"
    );
    assert_eq!(
        report.exit,
        McpProbeExit::NotEntered,
        "there must be no speculative MCP entry or E"
    );
    assert_eq!(
        report.status,
        PmNameTrialStatus::NotWritten,
        "no intent was accepted"
    );
    assert_blocked(&mut radio).await;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn any_guard_or_target_drift_refuses_intent_at_a_known_boundary() -> TestResult {
    for mutation in 0..5 {
        let mut trial = fixture()?;
        let mut format = [0; 40];
        let mut control = *trial.control_page();
        let mut target = *trial.original_page();
        match mutation {
            0 => *format.get_mut(2).ok_or("format missing")? = 1,
            1 => *control.get_mut(9).ok_or("PM selector missing")? = 1,
            2 => {
                *control
                    .get_mut(200)
                    .ok_or("unrelated control byte missing")? ^= 1;
            }
            3 => *target.get_mut(16).ok_or("memo byte missing")? ^= 1,
            _ => *target.get_mut(0).ok_or("gateway byte missing")? = 1,
        }
        let mut mock = MockTransport::new();
        entry(&mut mock);
        read(&mut mock, format_page()?, &format);
        read(
            &mut mock,
            MyCallsignTrial::required_control_page()?,
            &control,
        );
        read(&mut mock, trial.page(), &target);
        mock.expect(b"E", &[ACK]);
        let mut radio = Radio::new(mock);
        let mut called = false;
        let report = radio
            .run_approved_my1_trial_session_until_exit(
                &mut trial,
                || false,
                |_, _| {
                    called = true;
                    Ok(())
                },
            )
            .await;
        assert!(
            matches!(
                report.outcome,
                PmNameTrialSessionOutcome::Failed {
                    stage: PmNameTrialSessionStage::FreshComparison,
                    ..
                }
            ),
            "every complete but mismatching observation must fail before intent"
        );
        assert_eq!(
            report.segments.len(),
            3,
            "all complete observations must be retained"
        );
        assert_eq!(
            report.exit,
            McpProbeExit::Acknowledged,
            "known-boundary failure permits detached E"
        );
        assert_eq!(
            report.write,
            PmNameTrialWriteDisposition::NotAttempted,
            "drift must prohibit W"
        );
        assert_eq!(
            report.status,
            PmNameTrialStatus::NotWritten,
            "fresh drift creates no write obligation"
        );
        assert!(
            !called,
            "fresh guard or target drift must prevent journal intent"
        );
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn every_pre_intent_cancellation_boundary_retains_safe_cleanup() -> TestResult {
    for boundary in 0..7 {
        let mut trial = fixture()?;
        let mut mock = MockTransport::new();
        if boundary > 0 {
            identity(&mut mock, "1.02");
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
            read(
                &mut mock,
                MyCallsignTrial::required_control_page()?,
                trial.control_page(),
            );
        }
        if boundary > 5 {
            read(&mut mock, trial.page(), trial.original_page());
        }
        if boundary > 2 {
            mock.expect(b"E", &[ACK]);
        }
        let mut radio = Radio::new(mock);
        let mut checks = 0;
        let mut called = false;
        let report = radio
            .run_approved_my1_trial_session_until_exit(
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
        assert!(
            matches!(report.outcome, PmNameTrialSessionOutcome::Cancelled),
            "pre-intent cancellation must remain distinguishable from a possible write"
        );
        assert_eq!(
            report.status,
            PmNameTrialStatus::NotWritten,
            "safe cancellation creates no obligation"
        );
        assert_eq!(
            report.exit,
            if boundary > 2 {
                McpProbeExit::Acknowledged
            } else {
                McpProbeExit::NotEntered
            },
            "only an entered, synchronized MCP session should receive E"
        );
        assert!(
            !called,
            "pre-intent cancellation must not call the durable intent callback"
        );
        assert!(
            trial.next_session().is_err(),
            "cancelled instances must not silently restart"
        );
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn failed_durable_intent_prevents_write_and_retains_additional_exit_error() -> TestResult {
    let mut trial = fixture()?;
    let mut mock = MockTransport::new();
    preflight(&mut mock, &trial, trial.original_page())?;
    mock.expect(b"E", &[0x15]);
    let mut radio = Radio::new(mock);
    let report = radio
        .run_approved_my1_trial_session_until_exit(
            &mut trial,
            || false,
            |_, _| Err(io::Error::other("journal synchronization failed")),
        )
        .await;
    assert!(
        matches!(
            report.outcome,
            PmNameTrialSessionOutcome::Failed {
                stage: PmNameTrialSessionStage::DurableIntent,
                error: PmNameTrialSessionError::DurableIntent(_),
            }
        ),
        "the first failure must remain the unsynchronized intent"
    );
    assert!(
        report.cleanup_error.is_some(),
        "the independent failed exit must also be retained"
    );
    assert_eq!(
        report.exit,
        McpProbeExit::NotAcknowledged,
        "missing E ACK cannot be success"
    );
    assert_eq!(
        report.write,
        PmNameTrialWriteDisposition::NotAttempted,
        "failed journal must prohibit W"
    );
    assert_eq!(
        report.status,
        PmNameTrialStatus::NotWritten,
        "unsynchronized intent must not advance the engine"
    );
    assert_blocked(&mut radio).await;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn missing_write_ack_keeps_obligation_and_prohibits_speculative_cleanup() -> TestResult {
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
            .run_approved_my1_trial_session_until_exit(&mut trial, || false, |_, _| Ok(()))
            .await;
        assert!(
            matches!(
                report.outcome,
                PmNameTrialSessionOutcome::Failed {
                    stage: PmNameTrialSessionStage::Write,
                    ..
                }
            ),
            "missing W acknowledgment must be retained as a write failure"
        );
        assert_eq!(
            report.write,
            PmNameTrialWriteDisposition::PossiblyDispatched {
                write: PmNameTrialWrite::Rename
            },
            "silence or rejection cannot prove that the write never reached the device"
        );
        assert_eq!(
            report.status,
            PmNameTrialStatus::PossiblyChanged,
            "restoration remains owed"
        );
        assert_eq!(
            report.exit,
            McpProbeExit::RecoveryRequired,
            "uncertain W prohibits speculative E"
        );
        assert_eq!(
            report.segments.len(),
            3,
            "no speculative readback may follow uncertain W"
        );
        assert_blocked(&mut radio).await;
        let mock = radio.into_transport();
        assert_eq!(
            mock.writes().last(),
            Some(&write),
            "nothing may follow the uncertain write"
        );
        mock.assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn incomplete_guard_read_keeps_only_complete_evidence_and_sends_no_exit() -> TestResult {
    let mut trial = fixture()?;
    let mut mock = MockTransport::new();
    entry(&mut mock);
    read(&mut mock, format_page()?, &[0; 40]);
    let guard_page = MyCallsignTrial::required_control_page()?;
    mock.expect_partial_then_hang(&read_request(guard_page), b"W\x04");
    let mut radio = Radio::new(mock);
    radio.set_timeout(Duration::from_millis(1));
    let report = radio
        .run_approved_my1_trial_session_until_exit(&mut trial, || false, |_, _| Ok(()))
        .await;
    assert!(
        matches!(report.outcome, PmNameTrialSessionOutcome::Failed {
        stage: PmNameTrialSessionStage::Read { page }, ..
    } if page == guard_page),
        "incomplete guard bytes cannot be accepted as a full-page observation"
    );
    assert_eq!(
        report.segments.len(),
        1,
        "only the completely acknowledged format read is evidence"
    );
    assert_eq!(
        report.exit,
        McpProbeExit::RecoveryRequired,
        "uncertain read prohibits E"
    );
    assert_eq!(
        report.status,
        PmNameTrialStatus::NotWritten,
        "no durable intent was reached"
    );
    assert_blocked(&mut radio).await;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn incomplete_or_mismatching_readback_never_clears_the_obligation() -> TestResult {
    for incomplete in [false, true] {
        let mut trial = fixture()?;
        let mut mock = MockTransport::new();
        preflight(&mut mock, &trial, trial.original_page())?;
        mock.expect(&frame(trial.page(), trial.expected_page()), &[ACK]);
        if incomplete {
            mock.expect_partial_then_hang(&read_request(trial.page()), b"W\x05");
        } else {
            let mut changed_memo = *trial.expected_page();
            *changed_memo.get_mut(16).ok_or("memo byte missing")? ^= 1;
            read(&mut mock, trial.page(), &changed_memo);
            mock.expect(b"E", &[ACK]);
        }
        let mut radio = Radio::new(mock);
        radio.set_timeout(Duration::from_millis(1));
        let report = radio
            .run_approved_my1_trial_session_until_exit(&mut trial, || false, |_, _| Ok(()))
            .await;
        assert!(
            matches!(
                report.outcome,
                PmNameTrialSessionOutcome::Failed {
                    stage: PmNameTrialSessionStage::ImmediateReadback,
                    ..
                }
            ),
            "complete mismatches and incomplete pages must both fail immediate verification"
        );
        assert_eq!(
            report.status,
            PmNameTrialStatus::PossiblyChanged,
            "W acknowledgment alone does not establish the intended page or restoration"
        );
        assert_eq!(
            report.write,
            PmNameTrialWriteDisposition::Acknowledged {
                write: PmNameTrialWrite::Rename
            },
            "the earlier complete W acknowledgment must survive readback failure"
        );
        assert_eq!(
            report.exit,
            if incomplete {
                McpProbeExit::RecoveryRequired
            } else {
                McpProbeExit::Acknowledged
            },
            "only a known complete read boundary permits detached exit"
        );
        assert_eq!(
            report.segments.len(),
            if incomplete { 3 } else { 4 },
            "only complete acknowledged readback may join the retained preflight observations"
        );
        assert!(
            trial.next_session().is_err(),
            "readback failure must halt without a stale restore"
        );
        assert_blocked(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn changed_page_drift_in_restore_session_forbids_a_stale_restoration() -> TestResult {
    let mut trial = fixture()?;
    let mut first = Radio::new(session_script(&trial)?);
    let first_report = first
        .run_approved_my1_trial_session_until_exit(&mut trial, || false, |_, _| Ok(()))
        .await;
    assert_session_success(&first_report, &trial);
    trial.finalize_session(
        first_report
            .session_id()
            .ok_or("first session ID missing")?,
    )?;
    first.into_transport().assert_complete();
    let mut drifted = *trial.expected_page();
    *drifted
        .get_mut(200)
        .ok_or("unrelated target byte missing")? ^= 1;
    let mut mock = MockTransport::new();
    preflight(&mut mock, &trial, &drifted)?;
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let mut called = false;
    let report = radio
        .run_approved_my1_trial_session_until_exit(
            &mut trial,
            || true,
            |_, _| {
                called = true;
                Ok(())
            },
        )
        .await;
    assert!(
        matches!(
            report.outcome,
            PmNameTrialSessionOutcome::Failed {
                stage: PmNameTrialSessionStage::FreshComparison,
                error: PmNameTrialSessionError::Evidence(PmNameTrialError::PageMismatch),
            }
        ),
        "restoration must compare the entire expected changed page without rebasing"
    );
    assert!(
        !called,
        "stale restoration must never reach durable intent or W"
    );
    assert_eq!(
        report.write,
        PmNameTrialWriteDisposition::NotAttempted,
        "no stale original page may be replayed"
    );
    assert_eq!(
        report.status,
        PmNameTrialStatus::PossiblyChanged,
        "failed restoration cannot erase its obligation"
    );
    assert_eq!(
        report.exit,
        McpProbeExit::Acknowledged,
        "known-boundary failure still completes safe E"
    );
    assert!(
        trial.next_session().is_err(),
        "mismatching restoration must permanently halt this instance"
    );
    assert_blocked(&mut radio).await;
    radio.into_transport().assert_complete();
    Ok(())
}
