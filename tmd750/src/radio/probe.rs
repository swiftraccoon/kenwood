//! Fixed-scope, read-only probes of the MCP transport lifecycle.
//!
//! A probe reads only the two fixed fragments named below and acknowledges
//! exit; it sends no memory-write, fill, or RF command.

use super::{Identity, Radio};
use crate::error::{Error, ProtocolError};
use crate::types::{DvGatewayMode, Page, RadioModel, Region};
use kenwood_transport::Transport;

/// A step in the fixed MCP probe sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpProbeStage {
    /// Fresh CAT model, firmware, and radio-type proof before entry.
    Identity,
    /// Fresh Gateway query and required Off comparison before entry.
    Gateway,
    /// Programming-mode entry and its exact expected response.
    Entry,
    /// The 40-byte global fragment at address 8.
    GlobalRead,
    /// The 255-byte PM-off fragment at address 327681.
    SlotRead,
    /// Programming-mode exit and its acknowledgment.
    Exit,
}

/// Whether programming mode was exited during a probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpProbeExit {
    /// Programming-mode entry was not attempted.
    NotEntered,
    /// Entry or a page exchange failed; no speculative exit bytes were sent.
    ///
    /// Stop using the connection and restore the radio to normal operation
    /// before opening a fresh connection. The radio's current mode is unknown.
    RecoveryRequired,
    /// Exit was attempted but its complete success could not be established.
    ///
    /// No subsequent CAT commands or baud changes were sent.
    NotAcknowledged,
    /// The radio acknowledged exit.
    ///
    /// The old handle is retired without further I/O: the caller closes and
    /// drops it, then verifies identity on a fresh connection.
    Acknowledged,
}

/// One acknowledged fragment: the exact page requested and the bytes returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProbeSegment {
    /// Exact address and length requested.
    pub page: Page,
    /// Bytes returned by the radio, expanding a uniform-fill response if used.
    pub data: Vec<u8>,
}

/// Completion, cooperative cancellation, or the first failed probe step.
#[derive(Debug)]
pub enum McpProbeOutcome {
    /// Both fragments were read and exit succeeded; CAT was not attempted.
    ///
    /// Returned by [`Radio::probe_mcp`]. The caller closes this transport before
    /// verifying identity on a fresh connection.
    AwaitingCatVerification,
    /// The caller requested cancellation at a complete exchange boundary.
    ///
    /// If programming mode had been entered, exit still completed. The caller
    /// releases the original handle; any further CAT verification needs a fresh
    /// connection. A cleanup failure is [`Self::Failed`] instead.
    Cancelled,
    /// The probe stopped at a failed step, retaining earlier observations.
    Failed {
        /// Step that failed.
        stage: McpProbeStage,
        /// Original typed failure, including its error source chain.
        error: Error,
    },
}

/// The wire exchanges of a fixed-scope MCP probe, including incomplete runs.
///
/// It contains only the two fragments listed below, so it is not a configuration
/// backup. Raw wire capture belongs in a transport wrapper, so that rejected or
/// incomplete replies are preserved even when this report is not returned.
#[derive(Debug)]
pub struct McpProbeReport {
    /// Identity proven immediately before programming-mode entry, if reached.
    pub identity: Option<Identity>,
    /// Accepted programming-entry reply, without its carriage return.
    ///
    /// Unexpected replies remain in the entry error and raw wire transcript;
    /// this field is populated only after the entry reply has been accepted.
    pub entry_reply: Option<Vec<u8>>,
    /// Fragments whose payload and complete ACK exchange were validated.
    pub segments: Vec<McpProbeSegment>,
    /// Exit disposition, independent of whether all fragments were read.
    pub exit: McpProbeExit,
    /// Overall result; a completed read with failed cleanup is a failure.
    pub outcome: McpProbeOutcome,
}

/// A fixed read-only MCP probe guarded by an additional Gateway-Off check.
///
/// The Gateway value comes from this session's pre-entry CAT query, so it
/// describes the radio before entry, not after exit. After an acknowledged
/// exit the caller retires this transport and verifies a fresh connection.
#[derive(Debug)]
pub struct McpGatewayOffProbeReport {
    /// The fixed fragments, original identity, entry, exit, and first failure.
    pub probe: McpProbeReport,
    /// Actual pre-entry Gateway reply, including a rejected non-Off value.
    ///
    /// Absent when identity, cancellation, or the Gateway exchange prevented
    /// obtaining a valid reply. No Gateway setter is sent.
    pub gateway_mode: Option<DvGatewayMode>,
}

impl McpProbeReport {
    const fn pending() -> Self {
        Self {
            identity: None,
            entry_reply: None,
            segments: Vec::new(),
            exit: McpProbeExit::NotEntered,
            outcome: McpProbeOutcome::AwaitingCatVerification,
        }
    }

    fn fail(&mut self, stage: McpProbeStage, error: Error) {
        self.outcome = McpProbeOutcome::Failed { stage, error };
    }
}

impl<T: Transport> Radio<T> {
    /// Read two fixed MCP fragments and acknowledge exit without post-exit CAT.
    ///
    /// Reads exactly `8..48` and `327681..327936`, using the official transfer
    /// boundaries. Sends identity, entry, read requests, protocol ACKs, and exit;
    /// never sends memory-write, fill, or RF-transmit commands. Every failure
    /// preserves earlier completed observations without inventing gap bytes.
    ///
    /// The exit ACK retires this handle. The official program closes its
    /// transfer handle at that point, and on firmware 1.02 over main-unit USB
    /// the serial endpoint re-enumerates shortly afterwards, invalidating the
    /// old handle. This method leaves handle release and fresh-connection
    /// identity proof to the caller: it never closes, reopens, enumerates, or
    /// selects a transport, performs no baud-rate change after the ACK, and
    /// blocks further protocol access on this handle.
    ///
    /// Success returns [`McpProbeOutcome::AwaitingCatVerification`]: the report
    /// holds the original identity, both acknowledged fragments, and the exit
    /// disposition. CAT readiness on this handle is not tested.
    ///
    /// # Cancellation
    ///
    /// Await the future to completion and request cancellation through the
    /// callback. A synchronized session is exited before returning; no CAT is
    /// sent on this connection, even during cleanup. Close the transport before
    /// any subsequent operation. After an incomplete exchange, restore normal
    /// radio operation before opening another connection.
    pub async fn probe_mcp(&mut self, mut should_cancel: impl FnMut() -> bool) -> McpProbeReport {
        let mut report = McpProbeReport::pending();
        if should_cancel() {
            report.outcome = McpProbeOutcome::Cancelled;
            return report;
        }
        if !self.identify_mcp_probe(&mut report).await {
            return report;
        }
        if should_cancel() {
            report.outcome = McpProbeOutcome::Cancelled;
            return report;
        }
        self.finish_mcp_probe(&mut should_cancel, &mut report).await;
        report
    }

    /// Guard a fixed detached MCP read with exact identity and Gateway Off.
    ///
    /// Reads TM-D750, firmware `1.02`, and opaque type `K,2,1` exactly once,
    /// then requires a fresh `GW 0` before entering MCP. Any other identity or
    /// Gateway value refuses entry. Reads only `8..48` and `327681..327936`;
    /// no settings write, fill, or RF request is sent.
    ///
    /// An acknowledged exit leaves the old handle blocked. Close and drop its
    /// transport before any fresh CAT verification. This method never opens,
    /// closes, reopens, retries, or selects a transport. Success is
    /// [`McpProbeOutcome::AwaitingCatVerification`]; the Gateway value in the
    /// report is the pre-entry observation.
    ///
    /// # Cancellation
    ///
    /// Await the future to completion. Cancellation is checked before identity,
    /// between identity and Gateway, after Gateway, and between complete reads.
    /// An entered, synchronized session still exits; an incomplete entry, read,
    /// or ACK permits no speculative exit or other protocol command. The caller
    /// must release the handle and assess recovery before further radio access.
    pub async fn probe_mcp_gateway_off_until_exit(
        &mut self,
        mut should_cancel: impl FnMut() -> bool,
    ) -> McpGatewayOffProbeReport {
        let mut report = McpGatewayOffProbeReport {
            probe: McpProbeReport::pending(),
            gateway_mode: None,
        };
        if should_cancel() {
            report.probe.outcome = McpProbeOutcome::Cancelled;
            return report;
        }
        if !self.identify_mcp_probe(&mut report.probe).await {
            return report;
        }
        if !report.probe.identity.as_ref().is_some_and(|identity| {
            identity.model == RadioModel::TmD750
                && identity.firmware.as_str() == "1.02"
                && identity.radio_type.as_str() == "K,2,1"
        }) {
            report.probe.fail(
                McpProbeStage::Identity,
                ProtocolError::UnexpectedResponse {
                    expected: "TM-D750 / firmware 1.02 / radio type K,2,1 for the Gateway-Off probe",
                    actual: format!("{:?}", report.probe.identity),
                }
                .into(),
            );
            return report;
        }
        if should_cancel() {
            report.probe.outcome = McpProbeOutcome::Cancelled;
            return report;
        }
        match self.get_dv_gateway_mode().await {
            Ok(mode) => report.gateway_mode = Some(mode),
            Err(error) => {
                report.probe.fail(McpProbeStage::Gateway, error);
                return report;
            }
        }
        if report.gateway_mode != Some(DvGatewayMode::Off) {
            report.probe.fail(
                McpProbeStage::Gateway,
                ProtocolError::UnexpectedResponse {
                    expected: "Gateway Off before the fixed MCP probe",
                    actual: format!("{:?}", report.gateway_mode),
                }
                .into(),
            );
            return report;
        }
        if should_cancel() {
            report.probe.outcome = McpProbeOutcome::Cancelled;
            return report;
        }
        self.finish_mcp_probe(&mut should_cancel, &mut report.probe)
            .await;
        report
    }

    async fn identify_mcp_probe(&mut self, report: &mut McpProbeReport) -> bool {
        match self.identify().await {
            Ok(identity) => {
                report.identity = Some(identity);
                true
            }
            Err(error) => {
                report.fail(McpProbeStage::Identity, error);
                false
            }
        }
    }

    async fn finish_mcp_probe(
        &mut self,
        should_cancel: &mut impl FnMut() -> bool,
        report: &mut McpProbeReport,
    ) {
        let mut session = match self.enter_mcp().await {
            Ok(session) => session,
            Err(error) => {
                report.exit = McpProbeExit::RecoveryRequired;
                report.fail(McpProbeStage::Entry, error);
                return;
            }
        };
        report.entry_reply = Some(session.entry_reply().to_vec());
        report.exit = McpProbeExit::RecoveryRequired;
        for (stage, region) in [
            (McpProbeStage::GlobalRead, Region::const_new(8, 48)),
            (McpProbeStage::SlotRead, Region::const_new(327_681, 327_936)),
        ] {
            if should_cancel() {
                report.outcome = McpProbeOutcome::Cancelled;
                break;
            }
            for page in region.pages() {
                match session.read_page(page).await {
                    Ok(data) => report.segments.push(McpProbeSegment { page, data }),
                    Err(error) => {
                        report.fail(stage, error);
                        return;
                    }
                }
            }
        }
        if should_cancel() {
            report.outcome = McpProbeOutcome::Cancelled;
        }
        if let Err(error) = session.exit().await {
            report.exit = McpProbeExit::NotAcknowledged;
            report.fail(McpProbeStage::Exit, error);
            return;
        }
        report.exit = McpProbeExit::Acknowledged;
    }
}
