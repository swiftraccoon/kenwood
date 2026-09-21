//! Fixed configuration-region reads, retained page by page.
//!
//! Every operation here reads: none sends a memory-write, fill, or RF command.

use super::qualification::{McpProbeExit, McpProbeSegment};
use super::{Identity, Progress, Radio};
use crate::error::{Error, ProtocolError};
use crate::protocol::mcp::{ENTER_RESPONSE, regions};
use crate::types::{DvGatewayMode, Page, RadioModel, Region};
use kenwood_transport::Transport;

/// A step in the fixed configuration backup sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpBackupStage {
    /// Fresh CAT model, firmware, and radio-type proof before entry.
    Identity,
    /// The guarded backup's fresh Gateway-Off observation before entry.
    Gateway,
    /// Programming-mode entry and its exact expected response.
    Entry,
    /// One configuration page and its complete acknowledgment exchange.
    Read {
        /// Exact page whose exchange failed.
        page: Page,
    },
    /// Programming-mode exit and its acknowledgment.
    Exit,
}

/// Completion, cooperative cancellation, or the first failed backup step.
#[derive(Debug)]
pub enum McpBackupOutcome {
    /// Every configuration page and exit ACK were captured; CAT is unverified.
    ///
    /// The caller closes and drops this transport before verifying identity on
    /// a fresh connection.
    AwaitingCatVerification,
    /// Cancellation was requested at a complete exchange boundary.
    ///
    /// An entered session was exited successfully. A cleanup failure is
    /// reported as [`Self::Failed`] instead. No post-exit CAT is sent.
    Cancelled,
    /// The backup stopped, retaining every earlier acknowledged page.
    Failed {
        /// Step whose complete success could not be established.
        stage: McpBackupStage,
        /// Original typed failure, including its error source chain.
        error: Error,
    },
}

/// The pages read from the official configuration regions, without the bitmap.
///
/// Each segment contains one fully acknowledged page; unread gaps are absent,
/// not zero-filled. This is a sparse page set, not a complete memory image or a
/// `.d750` file. Wrap the transport to preserve raw wire traffic, including
/// partial or rejected replies that never become acknowledged segments.
#[derive(Debug)]
pub struct McpBackupReport {
    /// Identity proven immediately before programming-mode entry, if reached.
    pub identity: Option<Identity>,
    /// Fresh Gateway state, present only for the Gateway-Off guarded backup.
    /// [`Radio::backup_mcp_until_exit`] leaves it absent.
    pub gateway_mode: Option<DvGatewayMode>,
    /// Accepted programming-entry reply without its carriage return.
    pub entry_reply: Option<Vec<u8>>,
    /// Fully acknowledged pages, in the official configuration-transfer order.
    pub segments: Vec<McpProbeSegment>,
    /// Exit disposition, independent of how many pages were captured.
    pub exit: McpProbeExit,
    /// Overall transfer result; failed cleanup makes the operation fail.
    pub outcome: McpBackupOutcome,
}

impl McpBackupReport {
    const fn pending() -> Self {
        Self {
            identity: None,
            gateway_mode: None,
            entry_reply: None,
            segments: Vec::new(),
            exit: McpProbeExit::NotEntered,
            outcome: McpBackupOutcome::AwaitingCatVerification,
        }
    }

    fn fail(&mut self, stage: McpBackupStage, error: Error) {
        self.outcome = McpBackupOutcome::Failed { stage, error };
    }

    /// Whether this report contains the exact completed configuration read.
    ///
    /// Requires the non-cancelled outcome, identity, exact entry reply, exit
    /// ACK, and every expected page in order with the exact payload length.
    /// Extra, duplicate, reordered, missing, or truncated pages are rejected.
    /// It inspects this report only: it reads nothing and says nothing about the
    /// connection's state afterwards.
    #[must_use]
    pub fn has_complete_configuration(&self) -> bool {
        if !matches!(self.outcome, McpBackupOutcome::AwaitingCatVerification)
            || self.identity.is_none()
            || self.entry_reply.as_deref() != Some(ENTER_RESPONSE)
            || self.exit != McpProbeExit::Acknowledged
        {
            return false;
        }
        let pages = configuration_pages();
        self.segments.len() == pages.len()
            && self
                .segments
                .iter()
                .zip(pages)
                .all(|(segment, page)| segment.page == page && segment.data.len() == page.len())
    }
}

fn configuration_pages() -> Vec<Page> {
    regions::menu_regions()
        .into_iter()
        .flat_map(Region::pages)
        .collect()
}

impl<T: Transport> Radio<T> {
    /// Read the standard configuration only after exact identity and Gateway Off.
    ///
    /// Requires `TM-D750 / 1.02 / K,2,1` and a fresh `GW 0` on this connection.
    /// Uses the same read and detached-exit engine as
    /// [`Self::backup_mcp_until_exit`], with the same cancellation boundaries.
    /// It never sends memory-write, RF, recovery, or post-exit CAT commands.
    ///
    /// Await completion; dropping an active exchange cannot preserve its report
    /// or establish a safe protocol boundary for subsequent access.
    pub async fn backup_mcp_gateway_off_until_exit(
        &mut self,
        mut should_cancel: impl FnMut() -> bool,
        mut progress: impl FnMut(Progress),
    ) -> McpBackupReport {
        let mut report = McpBackupReport::pending();
        if !self.identify_backup(&mut should_cancel, &mut report).await {
            return report;
        }
        if !report.identity.as_ref().is_some_and(|identity| {
            identity.model == RadioModel::TmD750
                && identity.firmware.as_str() == "1.02"
                && identity.radio_type.as_str() == "K,2,1"
        }) {
            report.fail(
                McpBackupStage::Identity,
                ProtocolError::UnexpectedResponse {
                    expected: "TM-D750 / firmware 1.02 / radio type K,2,1 for the Gateway-Off backup",
                    actual: format!("{:?}", report.identity),
                }
                .into(),
            );
            return report;
        }
        if should_cancel() {
            report.outcome = McpBackupOutcome::Cancelled;
            return report;
        }
        match self.get_dv_gateway_mode().await {
            Ok(mode) => report.gateway_mode = Some(mode),
            Err(error) => {
                report.fail(McpBackupStage::Gateway, error);
                return report;
            }
        }
        if report.gateway_mode != Some(DvGatewayMode::Off) {
            report.fail(
                McpBackupStage::Gateway,
                ProtocolError::UnexpectedResponse {
                    expected: "Gateway Off before the standard configuration backup",
                    actual: format!("{:?}", report.gateway_mode),
                }
                .into(),
            );
            return report;
        }
        self.finish_backup(&mut should_cancel, &mut progress, &mut report)
            .await;
        report
    }

    /// Read every official configuration region, then acknowledge MCP exit.
    ///
    /// Reads the fixed global regions followed by the six Programmable-Memory
    /// slots, with the official page boundaries. The startup bitmap and unnamed
    /// region group are excluded. Sends identity queries, programming entry,
    /// read requests, protocol ACKs, and exit; never sends memory-write, fill,
    /// or RF-transmit commands.
    ///
    /// `progress` receives the completed and total page counts after each
    /// fully acknowledged page. Every return preserves all such pages. A read
    /// failure records its exact page and sends no speculative exit or CAT.
    ///
    /// After the exit ACK, no baud change or CAT command touches the old handle.
    /// Further protocol access remains blocked. The caller closes and drops the
    /// transport before verifying identity on a fresh connection.
    /// This method never closes, reopens, enumerates, or selects a transport.
    ///
    /// # Cancellation
    ///
    /// Await this future to completion. `should_cancel` is checked before
    /// identity, before entry, at every page boundary, and before exit. Once
    /// cancellation is observed, an entered synchronized session is exited
    /// without another page read. Dropping the future cannot perform async
    /// cleanup or return the accumulated report; interrupted wire exchanges
    /// leave the protocol state uncertain and block further traffic. Restore
    /// normal radio operation before opening another connection in that case.
    pub async fn backup_mcp_until_exit(
        &mut self,
        mut should_cancel: impl FnMut() -> bool,
        mut progress: impl FnMut(Progress),
    ) -> McpBackupReport {
        let mut report = McpBackupReport::pending();
        if self.identify_backup(&mut should_cancel, &mut report).await {
            self.finish_backup(&mut should_cancel, &mut progress, &mut report)
                .await;
        }
        report
    }

    async fn identify_backup(
        &mut self,
        should_cancel: &mut impl FnMut() -> bool,
        report: &mut McpBackupReport,
    ) -> bool {
        if should_cancel() {
            report.outcome = McpBackupOutcome::Cancelled;
            return false;
        }
        match self.identify().await {
            Ok(identity) => {
                report.identity = Some(identity);
                true
            }
            Err(error) => {
                report.fail(McpBackupStage::Identity, error);
                false
            }
        }
    }

    async fn finish_backup(
        &mut self,
        should_cancel: &mut impl FnMut() -> bool,
        progress: &mut impl FnMut(Progress),
        report: &mut McpBackupReport,
    ) {
        if should_cancel() {
            report.outcome = McpBackupOutcome::Cancelled;
            return;
        }
        let mut session = match self.enter_mcp().await {
            Ok(session) => session,
            Err(error) => {
                report.exit = McpProbeExit::RecoveryRequired;
                report.fail(McpBackupStage::Entry, error);
                return;
            }
        };
        report.entry_reply = Some(session.entry_reply().to_vec());
        report.exit = McpProbeExit::RecoveryRequired;
        let pages = configuration_pages();
        let total = pages.len();
        for page in pages {
            if should_cancel() {
                report.outcome = McpBackupOutcome::Cancelled;
                break;
            }
            match session.read_page(page).await {
                Ok(data) => report.segments.push(McpProbeSegment { page, data }),
                Err(error) => {
                    report.fail(McpBackupStage::Read { page }, error);
                    return;
                }
            }
            progress(Progress {
                done: report.segments.len(),
                total,
            });
        }
        if should_cancel() {
            report.outcome = McpBackupOutcome::Cancelled;
        }
        if let Err(error) = session.exit().await {
            report.exit = McpProbeExit::NotAcknowledged;
            report.fail(McpBackupStage::Exit, error);
            return;
        }
        report.exit = McpProbeExit::Acknowledged;
    }
}
