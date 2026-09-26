//! Channel name update wire scope, immutable before-images, and fail-closed
//! lifecycle.
//!
//! Transports are scripted. The test calls the finalize hook itself, because
//! the driver never finalizes a session on its caller's behalf.

use kenwood_schema as _;
use mcp_d75_extract as _;
use mmdvm as _;
use proptest as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use std::cell::Cell;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kenwood_tmd750::memory::{
    ChannelNameText, ChannelNameUpdate, TextFieldUpdateError, TextFieldUpdateEvent,
    TextFieldUpdateSession, TextFieldUpdateStatus,
};
use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::types::PhysicalChannel;
use kenwood_tmd750::{
    Address, Error, FirmwareIdentity, Identity, McpError, McpProbeExit, Page, Radio, RadioModel,
    RadioType, TextFieldUpdateSessionError, TextFieldUpdateSessionOutcome,
    TextFieldUpdateSessionReport, TextFieldUpdateSessionStage, TextFieldUpdateWriteDisposition,
};
use kenwood_transport::{MockTransport, Transport, TransportError};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Channel 999 lives at offset 112 of the name page starting at 81,408.
const CHANNEL: u16 = 999;
const NAME_PAGE_ADDRESS: u32 = 81_408;
const NAME_OFFSET: usize = 112;

fn identity_tuple() -> Result<Identity, Box<dyn std::error::Error>> {
    Ok(Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    })
}

fn fixture() -> Result<ChannelNameUpdate, Box<dyn std::error::Error>> {
    let mut original = [0xA5; 256];
    original
        .get_mut(NAME_OFFSET..NAME_OFFSET + 16)
        .ok_or("channel 999 field missing")?
        .fill(0);
    Ok(ChannelNameUpdate::prepare(
        &identity_tuple()?,
        &original,
        PhysicalChannel::new(CHANNEL)?,
        None,
        Some(&ChannelNameText::new("Repeater 1")?),
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

fn preflight(mock: &mut MockTransport, update: &ChannelNameUpdate, before: &[u8]) -> TestResult {
    entry(mock);
    read(mock, format_page()?, &[0; 40]);
    read(mock, update.page(), before);
    Ok(())
}

fn session_script(update: &ChannelNameUpdate) -> Result<MockTransport, Box<dyn std::error::Error>> {
    let mut mock = MockTransport::new();
    match update.next_session()? {
        TextFieldUpdateSession::Apply => {
            preflight(&mut mock, update, update.original_page())?;
            mock.expect(&frame(update.page(), update.desired_page()), &[ACK]);
            read(&mut mock, update.page(), update.desired_page());
        }
        TextFieldUpdateSession::Verify => {
            preflight(&mut mock, update, update.desired_page())?;
        }
    }
    mock.expect(b"E", &[ACK]);
    Ok(mock)
}

/// Scripted transport that audits the wire discipline around the sole W frame.
#[derive(Debug)]
struct AuditedTransport {
    mock: MockTransport,
    durable_intents: Arc<AtomicUsize>,
    memory_writes: usize,
    written_page: Option<u32>,
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
            written_page: None,
            exited: false,
            baud_changes: Vec::new(),
            close_calls: 0,
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
            let address = bytes.get(1..4).map(|address| {
                address
                    .iter()
                    .fold(0_u32, |acc, byte| acc << 8 | u32::from(*byte))
            });
            self.written_page = address;
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

fn assert_session_success(report: &TextFieldUpdateSessionReport, update: &ChannelNameUpdate) {
    assert!(
        matches!(
            report.outcome,
            TextFieldUpdateSessionOutcome::AwaitingCatVerification
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
        TextFieldUpdateStatus::PossiblyChanged,
        "external finalization is still required"
    );
    assert!(
        report.gateway_mode.is_none(),
        "the channel name field reads no Gateway state"
    );
}

#[tokio::test]
async fn exact_two_session_scope_writes_the_name_page_once_and_requires_external_finalization()
-> TestResult {
    let mut update = fixture()?;
    assert_eq!(update.page().address().as_u32(), NAME_PAGE_ADDRESS);
    for phase in [
        TextFieldUpdateSession::Apply,
        TextFieldUpdateSession::Verify,
    ] {
        let intents = Arc::new(AtomicUsize::new(0));
        let mut radio = Radio::new(AuditedTransport::new(session_script(&update)?, &intents));
        let report = radio
            .set_text_field_session_until_exit(
                &mut update,
                || false,
                |state| {
                    assert_eq!(phase, TextFieldUpdateSession::Apply);
                    assert_eq!(state.status(), TextFieldUpdateStatus::NotWritten);
                    let _previous = intents.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .await;
        assert_session_success(&report, &update);
        assert_eq!(report.session, Some(phase));
        let apply = phase == TextFieldUpdateSession::Apply;
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
                TextFieldUpdateWriteDisposition::Acknowledged
            } else {
                TextFieldUpdateWriteDisposition::NotAttempted
            }
        );
        assert_blocked(&mut radio).await;
        let transport = radio.into_transport();
        assert_eq!(transport.close_calls, 0, "caller owns original close/drop");
        assert_eq!(transport.baud_changes, [9600]);
        assert_eq!(transport.memory_writes, usize::from(apply));
        assert_eq!(
            transport.written_page,
            apply.then_some(NAME_PAGE_ADDRESS),
            "the sole W frame must address the channel's name page"
        );
        transport.mock.assert_complete();
        update.record(TextFieldUpdateEvent::SessionFinalized {
            id: report.session_id().ok_or("missing session ID")?,
            guards: (),
        })?;
    }
    assert_eq!(
        update.status(),
        TextFieldUpdateStatus::VerifiedAcrossSessions
    );
    Ok(())
}

#[tokio::test]
async fn firmware_or_type_mismatch_prevents_entry_and_durable_intent() -> TestResult {
    for (firmware, radio_type) in [("1.03", "K,2,1"), ("1.02", "J,2,1")] {
        let mut update = fixture()?;
        let mut mock = MockTransport::new();
        identity(&mut mock, firmware, radio_type);
        let mut radio = Radio::new(mock);
        let intents = Cell::new(0);
        let report = radio
            .set_text_field_session_until_exit(
                &mut update,
                || false,
                |_state| {
                    intents.set(intents.get() + 1);
                    Ok(())
                },
            )
            .await;
        assert!(
            matches!(
                report.outcome,
                TextFieldUpdateSessionOutcome::Failed {
                    stage: TextFieldUpdateSessionStage::Identity,
                    error: TextFieldUpdateSessionError::Evidence(
                        TextFieldUpdateError::IdentityMismatch
                    ),
                }
            ),
            "{:?}",
            report.outcome
        );
        assert!(report.entry_reply.is_none(), "MCP must not be entered");
        assert_eq!(report.exit, McpProbeExit::NotEntered);
        assert_eq!(report.write, TextFieldUpdateWriteDisposition::NotAttempted);
        assert_eq!(report.status, TextFieldUpdateStatus::NotWritten);
        assert_eq!(intents.get(), 0, "no intent may be recorded");
        assert!(update.next_session().is_err(), "the update must halt");
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn whole_page_drift_refuses_w_without_merging_and_still_exits() -> TestResult {
    let mut update = fixture()?;
    let mut drifted = *update.original_page();
    *drifted.get_mut(0).ok_or("first byte")? ^= 1;
    let mut mock = MockTransport::new();
    preflight(&mut mock, &update, &drifted)?;
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let intents = Cell::new(0);
    let report = radio
        .set_text_field_session_until_exit(
            &mut update,
            || false,
            |_state| {
                intents.set(intents.get() + 1);
                Ok(())
            },
        )
        .await;
    assert!(
        matches!(
            report.outcome,
            TextFieldUpdateSessionOutcome::Failed {
                stage: TextFieldUpdateSessionStage::FreshComparison,
                error: TextFieldUpdateSessionError::Evidence(TextFieldUpdateError::PageMismatch),
            }
        ),
        "{:?}",
        report.outcome
    );
    assert_eq!(report.write, TextFieldUpdateWriteDisposition::NotAttempted);
    assert_eq!(report.exit, McpProbeExit::Acknowledged);
    assert_eq!(report.status, TextFieldUpdateStatus::NotWritten);
    assert_eq!(intents.get(), 0, "no intent may be recorded");
    assert_eq!(report.segments.len(), 2);
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn failed_durable_intent_prevents_w_and_exits() -> TestResult {
    let mut update = fixture()?;
    let mut mock = MockTransport::new();
    preflight(&mut mock, &update, update.original_page())?;
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let report = radio
        .set_text_field_session_until_exit(
            &mut update,
            || false,
            |_state| Err(io::Error::other("journal disk full")),
        )
        .await;
    assert!(
        matches!(
            report.outcome,
            TextFieldUpdateSessionOutcome::Failed {
                stage: TextFieldUpdateSessionStage::DurableIntent,
                error: TextFieldUpdateSessionError::DurableIntent(_),
            }
        ),
        "{:?}",
        report.outcome
    );
    assert_eq!(report.write, TextFieldUpdateWriteDisposition::NotAttempted);
    assert_eq!(
        report.status,
        TextFieldUpdateStatus::NotWritten,
        "an unrecorded intent leaves no write risk"
    );
    assert_eq!(report.exit, McpProbeExit::Acknowledged);
    assert!(update.next_session().is_err(), "the update must halt");
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn missing_write_ack_sends_no_readback_exit_retry_or_rollback() -> TestResult {
    let mut update = fixture()?;
    let mut mock = MockTransport::new();
    preflight(&mut mock, &update, update.original_page())?;
    mock.expect(&frame(update.page(), update.desired_page()), &[0x15]);
    let intents = Arc::new(AtomicUsize::new(0));
    let mut radio = Radio::new(AuditedTransport::new(mock, &intents));
    let report = radio
        .set_text_field_session_until_exit(
            &mut update,
            || false,
            |_state| {
                let _previous = intents.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .await;
    assert!(
        matches!(
            report.outcome,
            TextFieldUpdateSessionOutcome::Failed {
                stage: TextFieldUpdateSessionStage::Write,
                error: TextFieldUpdateSessionError::Io(_),
            }
        ),
        "{:?}",
        report.outcome
    );
    assert_eq!(
        report.write,
        TextFieldUpdateWriteDisposition::PossiblyDispatched
    );
    assert_eq!(
        report.status,
        TextFieldUpdateStatus::PossiblyChanged,
        "a dispatched frame keeps the write risk"
    );
    assert_eq!(
        report.exit,
        McpProbeExit::RecoveryRequired,
        "an uncertain exchange sends no E"
    );
    assert_eq!(
        report.segments.len(),
        2,
        "no readback follows a missing ACK"
    );
    assert_blocked(&mut radio).await;
    let transport = radio.into_transport();
    assert_eq!(transport.memory_writes, 1);
    assert!(!transport.exited, "no E after an uncertain write");
    transport.mock.assert_complete();
    Ok(())
}

#[tokio::test]
async fn pre_write_cancellation_prevents_intent_at_the_first_and_last_boundary() -> TestResult {
    // Before any traffic.
    let mut update = fixture()?;
    let mut radio = Radio::new(MockTransport::new());
    let report = radio
        .set_text_field_session_until_exit(&mut update, || true, |_state| Ok(()))
        .await;
    assert!(
        matches!(report.outcome, TextFieldUpdateSessionOutcome::Cancelled),
        "{:?}",
        report.outcome
    );
    assert!(report.identity.is_none(), "no identity exchange");
    assert_eq!(report.status, TextFieldUpdateStatus::NotWritten);
    radio.into_transport().assert_complete();

    // After the fresh comparison, the last boundary before intent: the
    // session still exits cleanly and records no intent.
    let mut update = fixture()?;
    let mut mock = MockTransport::new();
    preflight(&mut mock, &update, update.original_page())?;
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let checks = Cell::new(0);
    let intents = Cell::new(0);
    let report = radio
        .set_text_field_session_until_exit(
            &mut update,
            || {
                checks.set(checks.get() + 1);
                checks.get() >= 5
            },
            |_state| {
                intents.set(intents.get() + 1);
                Ok(())
            },
        )
        .await;
    assert!(
        matches!(report.outcome, TextFieldUpdateSessionOutcome::Cancelled),
        "{:?}",
        report.outcome
    );
    assert_eq!(intents.get(), 0, "no intent may be recorded");
    assert_eq!(report.write, TextFieldUpdateWriteDisposition::NotAttempted);
    assert_eq!(report.exit, McpProbeExit::Acknowledged);
    assert_eq!(report.status, TextFieldUpdateStatus::NotWritten);
    assert!(update.next_session().is_err(), "a cancelled update halts");
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn halted_update_is_rejected_before_any_radio_traffic() -> TestResult {
    let mut update = fixture()?;
    update.halt();
    let mut radio = Radio::new(MockTransport::new());
    let report = radio
        .set_text_field_session_until_exit(&mut update, || false, |_state| Ok(()))
        .await;
    assert!(
        matches!(
            report.outcome,
            TextFieldUpdateSessionOutcome::Failed {
                stage: TextFieldUpdateSessionStage::Preparation,
                error: TextFieldUpdateSessionError::Evidence(TextFieldUpdateError::TerminalState),
            }
        ),
        "{:?}",
        report.outcome
    );
    assert!(report.session.is_none());
    assert!(report.identity.is_none(), "no identity exchange");
    radio.into_transport().assert_complete();
    Ok(())
}
