//! Fixed-scope, non-writing qualification of the MCP transport lifecycle.

use super::{Identity, Radio};
use crate::error::{Error, ProtocolError};
use crate::transport::Transport;
use crate::types::{Page, Region};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatVerification {
    CurrentConnection,
    Deferred,
}

/// A step in the fixed MCP qualification sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpProbeStage {
    /// Fresh CAT model, firmware, and radio-type proof before entry.
    Identity,
    /// Programming-mode entry and its exact expected response.
    Entry,
    /// The 40-byte global fragment at address 8.
    GlobalRead,
    /// The 255-byte PM-off fragment at address 327681.
    SlotRead,
    /// Programming-mode exit and its acknowledgment.
    Exit,
    /// Fresh CAT identity after exit, compared with the original identity.
    CatVerification,
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
    /// No subsequent CAT commands were sent. This includes failure to restore
    /// the transport's CAT baud rate, even if the exit byte was acknowledged.
    NotAcknowledged,
    /// The radio acknowledged exit.
    ///
    /// This alone does not prove CAT service returned; inspect the report's
    /// post-exit identity and outcome as well. The ordinary probe also restores
    /// the CAT baud rate; the detached probe leaves the old handle untouched
    /// after the ACK and requires its caller to close it.
    Acknowledged,
}

/// A successfully acknowledged fragment, without invented bytes for gaps.
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
    /// Both fragments were read, exit succeeded, and CAT identity matched.
    Complete,
    /// Both fragments were read and exit succeeded; CAT was not attempted.
    ///
    /// Returned by [`Radio::probe_mcp_until_exit`]. The caller must close this
    /// transport before explicitly qualifying a fresh connection. This outcome
    /// does not establish that CAT returned or that the radio's identity matched.
    AwaitingCatVerification,
    /// The caller requested cancellation at a complete exchange boundary.
    ///
    /// If programming mode had been entered, exit still completed. The ordinary
    /// [`Radio::probe_mcp`] also verifies CAT; [`Radio::probe_mcp_until_exit`]
    /// deliberately leaves that verification to a fresh connection. A cleanup
    /// failure is reported as [`Self::Failed`] instead.
    Cancelled,
    /// The probe stopped at a failed step, retaining earlier observations.
    Failed {
        /// Step that failed.
        stage: McpProbeStage,
        /// Original typed failure, including its error source chain.
        error: Error,
    },
}

/// Evidence from a fixed-scope MCP probe, including incomplete runs.
///
/// This is not a configuration backup and does not qualify the menu schema
/// for this firmware. Only the listed fragments were read. Raw wire capture
/// belongs in a transport wrapper so even rejected or incomplete replies can
/// be preserved before this report is returned.
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
    /// Fresh post-exit CAT identity, even when it differs from the original.
    pub cat_identity: Option<Identity>,
    /// Overall result; a completed read with failed cleanup is a failure.
    pub outcome: McpProbeOutcome,
}

impl McpProbeReport {
    const fn pending() -> Self {
        Self {
            identity: None,
            entry_reply: None,
            segments: Vec::new(),
            exit: McpProbeExit::NotEntered,
            cat_identity: None,
            outcome: McpProbeOutcome::Complete,
        }
    }

    fn fail(&mut self, stage: McpProbeStage, error: Error) {
        self.outcome = McpProbeOutcome::Failed { stage, error };
    }
}

impl<T: Transport> Radio<T> {
    /// Qualify MCP entry, two fixed reads, exit, and return to CAT.
    ///
    /// Reads exactly `8..48` and `327681..327936`, using the official transfer
    /// fragment boundaries. Sends control commands, read requests, and ACKs;
    /// never sends memory-write or fill commands. No RF transmission is requested.
    /// Every failure returns the evidence already collected in the report.
    /// No settings layout compatibility is inferred from a successful probe.
    ///
    /// # Cancellation
    ///
    /// Await this future to completion. Use `should_cancel` to stop between
    /// complete exchanges: an entered, synchronized session is exited and CAT
    /// is verified before returning. Dropping the future cannot perform async
    /// cleanup and may leave the radio in programming mode. After an incomplete
    /// exchange, the probe sends no speculative exit or CAT bytes; restore the
    /// radio to normal operation before opening a fresh connection.
    pub async fn probe_mcp(&mut self, mut should_cancel: impl FnMut() -> bool) -> McpProbeReport {
        self.probe_mcp_with_verification(&mut should_cancel, CatVerification::CurrentConnection)
            .await
    }

    /// Read the two fixed MCP fragments and acknowledge exit without sending CAT.
    ///
    /// The official program closes its transfer handle after the exit ACK.
    /// The first main-unit USB bench run likewise re-enumerated after that ACK,
    /// invalidating the old handle. This method leaves handle release and any
    /// explicitly selected fresh-connection identity proof to the caller.
    /// It does not close, reopen, enumerate, or select a transport itself.
    /// After the ACK it performs no baud-rate change and keeps further protocol
    /// access blocked on this handle, which the caller must close and drop.
    ///
    /// Success returns [`McpProbeOutcome::AwaitingCatVerification`], never
    /// [`McpProbeOutcome::Complete`]. The report preserves the original identity,
    /// both acknowledged fragments, and the exit disposition. No settings schema
    /// is qualified and no memory-write or fill commands are sent.
    ///
    /// # Cancellation
    ///
    /// Await the future to completion and request cancellation through the
    /// callback. A synchronized session is exited before returning; no CAT is
    /// sent on this connection, even during cleanup. Close the transport before
    /// any subsequent operation. After an incomplete exchange, restore normal
    /// radio operation before opening another connection.
    pub async fn probe_mcp_until_exit(
        &mut self,
        mut should_cancel: impl FnMut() -> bool,
    ) -> McpProbeReport {
        self.probe_mcp_with_verification(&mut should_cancel, CatVerification::Deferred)
            .await
    }

    async fn probe_mcp_with_verification(
        &mut self,
        should_cancel: &mut impl FnMut() -> bool,
        verification: CatVerification,
    ) -> McpProbeReport {
        let mut report = McpProbeReport::pending();
        if should_cancel() {
            report.outcome = McpProbeOutcome::Cancelled;
            return report;
        }
        match self.identify().await {
            Ok(identity) => report.identity = Some(identity),
            Err(error) => {
                report.fail(McpProbeStage::Identity, error);
                return report;
            }
        }
        if should_cancel() {
            report.outcome = McpProbeOutcome::Cancelled;
            return report;
        }
        let mut session = match self.enter_mcp().await {
            Ok(session) => session,
            Err(error) => {
                report.exit = McpProbeExit::RecoveryRequired;
                report.fail(McpProbeStage::Entry, error);
                return report;
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
                        return report;
                    }
                }
            }
        }
        if should_cancel() {
            report.outcome = McpProbeOutcome::Cancelled;
        }
        let exit = match verification {
            CatVerification::CurrentConnection => session.exit().await,
            CatVerification::Deferred => session.exit_detached().await,
        };
        if let Err(error) = exit {
            report.exit = McpProbeExit::NotAcknowledged;
            report.fail(McpProbeStage::Exit, error);
            return report;
        }
        report.exit = McpProbeExit::Acknowledged;
        match verification {
            CatVerification::CurrentConnection => self.verify_probe_cat(&mut report).await,
            CatVerification::Deferred => {
                if matches!(report.outcome, McpProbeOutcome::Complete) {
                    report.outcome = McpProbeOutcome::AwaitingCatVerification;
                }
            }
        }
        report
    }

    async fn verify_probe_cat(&mut self, report: &mut McpProbeReport) {
        match self.identify().await {
            Ok(identity) => {
                report.cat_identity = Some(identity);
                if report.cat_identity != report.identity {
                    report.fail(
                        McpProbeStage::CatVerification,
                        ProtocolError::UnexpectedResponse {
                            expected: "unchanged CAT model, firmware, and radio type after MCP exit",
                            actual: format!("{:?}", report.cat_identity),
                        }
                        .into(),
                    );
                }
            }
            Err(error) => report.fail(McpProbeStage::CatVerification, error),
        }
    }
}
