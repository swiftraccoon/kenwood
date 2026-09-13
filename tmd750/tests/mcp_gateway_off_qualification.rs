//! Fixed Gateway-Off probe admission, wire scope, cancellation, and retirement.

use kenwood_schema as _;
use mcp_d75_extract as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use std::io;

use kenwood_tmd750::protocol::mcp::{ACK, ENTER, EXIT, read_request, write_request};
use kenwood_tmd750::{
    Address, DvGatewayMode, Error, McpError, McpGatewayOffProbeReport, McpProbeExit,
    McpProbeOutcome, McpProbeStage, Page, Radio,
};
use kenwood_transport::{MockTransport, Transport, TransportError};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug)]
struct Witness {
    mock: MockTransport,
    baud_changes: Vec<u32>,
    entry_read_error: Option<io::Error>,
    awaiting_entry: bool,
}

impl Witness {
    const fn new(mock: MockTransport) -> Self {
        Self {
            mock,
            baud_changes: Vec::new(),
            entry_read_error: None,
            awaiting_entry: false,
        }
    }
}

impl Transport for Witness {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.mock.write(bytes).await?;
        self.awaiting_entry = bytes == ENTER;
        Ok(())
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        if self.awaiting_entry
            && let Some(error) = self.entry_read_error.take()
        {
            return Err(TransportError::Read(error));
        }
        self.mock.read(bytes).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.mock.close().await
    }

    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        self.baud_changes.push(baud);
        self.mock.set_baud_rate(baud)
    }
}

fn pages() -> Result<[Page; 2], Error> {
    Ok([
        Page::new(Address::new(8)?, 40)?,
        Page::new(Address::new(327_681)?, 255)?,
    ])
}

fn identity(mock: &mut MockTransport, firmware: &[u8], radio_type: &[u8]) {
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", firmware);
    mock.expect(b"TY\r", radio_type);
}

fn preflight(mock: &mut MockTransport) {
    identity(mock, b"FV 1.02\r", b"TY K,2,1\r");
    mock.expect(b"GW\r", b"GW 0\r");
}

fn read(mock: &mut MockTransport, page: Page, fill: u8) {
    let mut response = write_request(page).to_vec();
    response.extend(vec![fill; page.len()]);
    mock.expect(&read_request(page), &response);
    mock.expect(&[ACK], &[ACK]);
}

fn complete_script() -> Result<MockTransport, Error> {
    let mut mock = MockTransport::new();
    preflight(&mut mock);
    mock.expect(ENTER, b"0M\r");
    for (page, fill) in pages()?.into_iter().zip([0x12, 0x34]) {
        read(&mut mock, page, fill);
    }
    mock.expect(&[EXIT], &[ACK]);
    Ok(mock)
}

fn assert_no_memory_write(mock: &MockTransport) {
    assert!(
        mock.writes()
            .iter()
            .all(|bytes| !matches!(bytes.first(), Some(b'W' | b'Z'))),
        "fixed qualification must never dispatch a memory-write or fill frame"
    );
    mock.assert_complete();
}

async fn assert_retired(radio: &mut Radio<Witness>) {
    assert!(
        matches!(
            radio.identify().await,
            Err(Error::Mcp(
                McpError::RecoveryRequired | McpError::ConnectionRetired
            ))
        ),
        "CAT must remain blocked on an uncertain or detached handle"
    );
    assert!(
        matches!(
            radio.enter_mcp().await,
            Err(Error::Mcp(
                McpError::RecoveryRequired | McpError::ConnectionRetired
            ))
        ),
        "another entry must not reuse the retired handle"
    );
    assert!(
        matches!(
            radio.mcp_session(),
            Err(Error::Mcp(
                McpError::RecoveryRequired | McpError::ConnectionRetired
            ))
        ),
        "a retired handle cannot be borrowed as a synchronized MCP session"
    );
}

fn assert_not_entered(report: &McpGatewayOffProbeReport, stage: McpProbeStage) {
    assert!(
        matches!(&report.probe.outcome, McpProbeOutcome::Failed { stage: actual, .. } if *actual == stage),
        "retain the actual admission failure stage: {report:?}"
    );
    assert_eq!(
        report.probe.exit,
        McpProbeExit::NotEntered,
        "refused admission must not attempt programming entry"
    );
    assert!(
        report.probe.entry_reply.is_none(),
        "refusal is not entry acceptance"
    );
    assert!(
        report.probe.segments.is_empty(),
        "refused entry permits no page reads"
    );
}

#[tokio::test]
async fn gateway_off_probe_reads_exactly_two_fragments_and_retires_the_handle() -> TestResult {
    let mut radio = Radio::new(Witness::new(complete_script()?));
    let report = radio.probe_mcp_gateway_off_until_exit(|| false).await;
    assert!(
        matches!(
            report.probe.outcome,
            McpProbeOutcome::AwaitingCatVerification
        ),
        "complete fixed reads still require independent fresh verification: {report:?}"
    );
    assert_eq!(report.gateway_mode, Some(DvGatewayMode::Off));
    assert_eq!(report.probe.exit, McpProbeExit::Acknowledged);
    assert_eq!(report.probe.entry_reply.as_deref(), Some(b"0M".as_slice()));
    assert_eq!(report.probe.segments.len(), 2);
    for (segment, (page, fill)) in report
        .probe
        .segments
        .iter()
        .zip(pages()?.into_iter().zip([0x12, 0x34]))
    {
        assert_eq!(
            segment.page, page,
            "fragment addresses and lengths are fixed"
        );
        assert_eq!(
            segment.data,
            vec![fill; page.len()],
            "all read bytes remain evidence"
        );
    }
    assert_retired(&mut radio).await;
    let witness = radio.into_transport();
    let [global, slot] = pages()?;
    assert_eq!(
        witness.mock.writes(),
        &[
            b"ID\r".to_vec(),
            b"FV\r".to_vec(),
            b"TY\r".to_vec(),
            b"GW\r".to_vec(),
            ENTER.to_vec(),
            read_request(global).to_vec(),
            vec![ACK],
            read_request(slot).to_vec(),
            vec![ACK],
            vec![EXIT],
        ],
        "exact scheduler permits no duplicated identity, unrelated request, or post-exit command"
    );
    assert_eq!(
        witness.baud_changes,
        [9600],
        "only entry may configure baud"
    );
    assert_no_memory_write(&witness.mock);
    Ok(())
}

#[tokio::test]
async fn exact_identity_pin_refuses_other_firmware_or_type_before_gateway() {
    for (firmware, radio_type) in [
        (b"FV 1.00\r".as_slice(), b"TY K,2,1\r".as_slice()),
        (b"FV 1.020\r".as_slice(), b"TY K,2,1\r".as_slice()),
        (b"FV 1.02\r".as_slice(), b"TY J,2,1\r".as_slice()),
        (b"FV 1.02\r".as_slice(), b"TY K,2,2\r".as_slice()),
    ] {
        let mut mock = MockTransport::new();
        identity(&mut mock, firmware, radio_type);
        let mut radio = Radio::new(Witness::new(mock));
        let report = radio.probe_mcp_gateway_off_until_exit(|| false).await;
        assert_not_entered(&report, McpProbeStage::Identity);
        assert!(
            report.probe.identity.is_some(),
            "retain the actual rejected complete identity"
        );
        assert!(
            report.gateway_mode.is_none(),
            "identity mismatch must prevent even GW"
        );
        let witness = radio.into_transport();
        assert_eq!(witness.mock.writes().len(), 3);
        assert!(
            witness.baud_changes.is_empty(),
            "refusal precedes programming baud configuration"
        );
        assert_no_memory_write(&witness.mock);
    }
}

#[tokio::test]
async fn another_radio_model_is_refused_before_any_remaining_preflight() {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TH-D75\r");
    let mut radio = Radio::new(Witness::new(mock));
    let report = radio.probe_mcp_gateway_off_until_exit(|| false).await;
    assert_not_entered(&report, McpProbeStage::Identity);
    assert!(
        report.probe.identity.is_none(),
        "a wrong model is not a valid TM-D750 identity"
    );
    assert!(
        report.gateway_mode.is_none(),
        "model refusal precedes the Gateway request"
    );
    let witness = radio.into_transport();
    assert_eq!(witness.mock.writes(), &[b"ID\r".to_vec()]);
    assert_no_memory_write(&witness.mock);
}

#[tokio::test]
async fn terminal_and_unnamed_gateway_states_are_retained_but_never_enter_mcp() {
    for (reply, expected) in [
        (b"GW 2\r".as_slice(), DvGatewayMode::Terminal),
        (b"GW 1\r".as_slice(), DvGatewayMode::Unqualified(1)),
        (b"GW 255\r".as_slice(), DvGatewayMode::Unqualified(255)),
    ] {
        let mut mock = MockTransport::new();
        identity(&mut mock, b"FV 1.02\r", b"TY K,2,1\r");
        mock.expect(b"GW\r", reply);
        let mut radio = Radio::new(Witness::new(mock));
        let report = radio.probe_mcp_gateway_off_until_exit(|| false).await;
        assert_not_entered(&report, McpProbeStage::Gateway);
        assert_eq!(
            report.gateway_mode,
            Some(expected),
            "retain the rejected raw state"
        );
        let witness = radio.into_transport();
        assert_eq!(witness.mock.writes().len(), 4);
        assert!(
            witness.baud_changes.is_empty(),
            "non-Off admission must fail before entry"
        );
        assert_no_memory_write(&witness.mock);
    }
}

#[tokio::test]
async fn rejected_gateway_query_is_not_fabricated_as_off() {
    let mut mock = MockTransport::new();
    identity(&mut mock, b"FV 1.02\r", b"TY K,2,1\r");
    mock.expect(b"GW\r", b"N\r");
    let mut radio = Radio::new(Witness::new(mock));
    let report = radio.probe_mcp_gateway_off_until_exit(|| false).await;
    assert_not_entered(&report, McpProbeStage::Gateway);
    assert!(
        report.gateway_mode.is_none(),
        "a rejected query has no Gateway observation"
    );
    let witness = radio.into_transport();
    assert_eq!(witness.mock.writes().len(), 4);
    assert_no_memory_write(&witness.mock);
}

#[tokio::test]
async fn cancellation_before_identity_between_identity_and_gateway_or_before_entry_sends_no_mcp() {
    for cancel_at in 1..=3 {
        let mut mock = MockTransport::new();
        if cancel_at > 1 {
            identity(&mut mock, b"FV 1.02\r", b"TY K,2,1\r");
        }
        if cancel_at > 2 {
            mock.expect(b"GW\r", b"GW 0\r");
        }
        let mut radio = Radio::new(Witness::new(mock));
        let mut checkpoints = 0;
        let report = radio
            .probe_mcp_gateway_off_until_exit(|| {
                checkpoints += 1;
                checkpoints >= cancel_at
            })
            .await;
        assert!(
            matches!(report.probe.outcome, McpProbeOutcome::Cancelled),
            "cancel at boundary {cancel_at}"
        );
        assert_eq!(report.probe.exit, McpProbeExit::NotEntered);
        assert_eq!(
            report.gateway_mode,
            (cancel_at == 3).then_some(DvGatewayMode::Off)
        );
        let witness = radio.into_transport();
        assert_eq!(
            witness.mock.writes().len(),
            match cancel_at {
                1 => 0,
                2 => 3,
                _ => 4,
            }
        );
        assert!(
            witness.baud_changes.is_empty(),
            "pre-entry cancellation must avoid baud operations"
        );
        assert_no_memory_write(&witness.mock);
    }
}

#[tokio::test]
async fn cancellation_at_each_read_boundary_exits_once_without_old_handle_cat() -> TestResult {
    for read_count in 0..=2 {
        let mut mock = MockTransport::new();
        preflight(&mut mock);
        mock.expect(ENTER, b"0M\r");
        for page in pages()?.into_iter().take(read_count) {
            read(&mut mock, page, 0x4A);
        }
        mock.expect(&[EXIT], &[ACK]);
        let mut radio = Radio::new(Witness::new(mock));
        let mut checkpoints = 0;
        let report = radio
            .probe_mcp_gateway_off_until_exit(|| {
                checkpoints += 1;
                checkpoints >= 4 + read_count
            })
            .await;
        assert!(
            matches!(report.probe.outcome, McpProbeOutcome::Cancelled),
            "a requested stop remains cancelled even after clean exit"
        );
        assert_eq!(report.probe.exit, McpProbeExit::Acknowledged);
        assert_eq!(report.probe.segments.len(), read_count);
        assert_eq!(report.gateway_mode, Some(DvGatewayMode::Off));
        assert_retired(&mut radio).await;
        let witness = radio.into_transport();
        assert_eq!(
            witness.mock.writes().last().map(Vec::as_slice),
            Some([EXIT].as_slice())
        );
        assert_eq!(witness.baud_changes, [9600]);
        assert_no_memory_write(&witness.mock);
    }
    Ok(())
}

#[tokio::test]
async fn completed_entry_write_followed_by_enxio_sends_no_exit_or_retry() {
    let mut mock = MockTransport::new();
    preflight(&mut mock);
    mock.expect(ENTER, b"");
    let mut witness = Witness::new(mock);
    witness.entry_read_error = Some(io::Error::from_raw_os_error(6));
    let mut radio = Radio::new(witness);
    let report = radio.probe_mcp_gateway_off_until_exit(|| false).await;
    assert!(
        matches!(&report.probe.outcome, McpProbeOutcome::Failed {
            stage: McpProbeStage::Entry,
            error: Error::Transport(TransportError::Read(error)),
        } if error.raw_os_error() == Some(6)),
        "preserve the actual zero-response entry read failure: {report:?}"
    );
    assert_eq!(report.gateway_mode, Some(DvGatewayMode::Off));
    assert_eq!(report.probe.exit, McpProbeExit::RecoveryRequired);
    assert!(
        report.probe.entry_reply.is_none(),
        "a completed entry write is not a reply"
    );
    assert!(
        report.probe.segments.is_empty(),
        "uncertain entry permits no reads"
    );
    assert_retired(&mut radio).await;
    let witness = radio.into_transport();
    assert_eq!(
        witness.mock.writes().len(),
        5,
        "identity, GW, one entry only"
    );
    assert_eq!(witness.mock.writes().last().map(Vec::as_slice), Some(ENTER));
    assert_no_memory_write(&witness.mock);
}

#[tokio::test]
async fn a_complete_payload_with_bad_read_ack_never_joins_the_evidence() -> TestResult {
    let mut mock = MockTransport::new();
    preflight(&mut mock);
    mock.expect(ENTER, b"0M\r");
    let [global, _] = pages()?;
    let mut reply = write_request(global).to_vec();
    reply.extend(vec![0x55; global.len()]);
    mock.expect(&read_request(global), &reply);
    mock.expect(&[ACK], &[0x15]);
    let mut radio = Radio::new(Witness::new(mock));
    let report = radio.probe_mcp_gateway_off_until_exit(|| false).await;
    assert!(
        matches!(
            report.probe.outcome,
            McpProbeOutcome::Failed {
                stage: McpProbeStage::GlobalRead,
                ..
            }
        ),
        "read acknowledgment failure must remain a read failure"
    );
    assert_eq!(report.probe.exit, McpProbeExit::RecoveryRequired);
    assert!(
        report.probe.segments.is_empty(),
        "payload alone is not an acknowledged fragment"
    );
    assert_retired(&mut radio).await;
    let witness = radio.into_transport();
    assert_eq!(
        witness.mock.writes().last().map(Vec::as_slice),
        Some([ACK].as_slice()),
        "no speculative E after an uncertain read ACK"
    );
    assert_no_memory_write(&witness.mock);
    Ok(())
}

#[tokio::test]
async fn second_fragment_failure_retains_only_the_first_acknowledged_fragment() -> TestResult {
    let mut mock = MockTransport::new();
    preflight(&mut mock);
    mock.expect(ENTER, b"0M\r");
    let [global, slot] = pages()?;
    read(&mut mock, global, 0x33);
    mock.expect(&read_request(slot), b"W\0\0\0\0");
    let mut radio = Radio::new(Witness::new(mock));
    let report = radio.probe_mcp_gateway_off_until_exit(|| false).await;
    assert!(
        matches!(
            report.probe.outcome,
            McpProbeOutcome::Failed {
                stage: McpProbeStage::SlotRead,
                ..
            }
        ),
        "second fragment error must preserve its stage"
    );
    assert_eq!(report.probe.exit, McpProbeExit::RecoveryRequired);
    assert_eq!(report.probe.segments.len(), 1);
    assert_eq!(
        report.probe.segments.first().map(|segment| segment.page),
        Some(global)
    );
    assert_retired(&mut radio).await;
    let witness = radio.into_transport();
    assert_eq!(
        witness.mock.writes().last().map(Vec::as_slice),
        Some(read_request(slot).as_slice()),
        "uncertain second read permits no ACK, exit, or CAT"
    );
    assert_no_memory_write(&witness.mock);
    Ok(())
}

#[tokio::test]
async fn a_bad_exit_ack_never_becomes_successful_deferred_verification() -> TestResult {
    let mut mock = MockTransport::new();
    preflight(&mut mock);
    mock.expect(ENTER, b"0M\r");
    for page in pages()? {
        read(&mut mock, page, 0x22);
    }
    mock.expect(&[EXIT], &[0x15]);
    let mut radio = Radio::new(Witness::new(mock));
    let report = radio.probe_mcp_gateway_off_until_exit(|| false).await;
    assert!(
        matches!(
            report.probe.outcome,
            McpProbeOutcome::Failed {
                stage: McpProbeStage::Exit,
                ..
            }
        ),
        "retain exit failure rather than awaiting fresh verification"
    );
    assert_eq!(report.probe.exit, McpProbeExit::NotAcknowledged);
    assert_eq!(report.probe.segments.len(), 2);
    assert_retired(&mut radio).await;
    let witness = radio.into_transport();
    assert_eq!(
        witness.baud_changes,
        [9600],
        "exit failure cannot change baud or restore CAT"
    );
    assert_no_memory_write(&witness.mock);
    Ok(())
}
