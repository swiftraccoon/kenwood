//! Fixed configuration-region reads with page-granular evidence preservation.

use super::qualification::{McpProbeExit, McpProbeSegment};
use super::{Identity, Progress, Radio};
use crate::error::Error;
use crate::protocol::mcp::{ENTER_RESPONSE, regions};
use crate::transport::Transport;
use crate::types::{Page, Region};

/// A step in the fixed configuration backup sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpBackupStage {
    /// Fresh CAT model, firmware, and radio-type proof before entry.
    Identity,
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
    /// The caller must close and drop this transport before any explicitly
    /// selected fresh-connection identity verification.
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

/// Evidence from reading the official configuration regions without the bitmap.
///
/// Each segment contains one fully acknowledged page. Unread gaps are absent,
/// not invented as zero bytes. This is not a complete memory-image dump or a
/// validated `.d750` file, and it does not establish settings-schema compatibility.
/// A transport wrapper should preserve raw wire traffic, including partial or
/// rejected replies that cannot be included as acknowledged segments.
#[derive(Debug)]
pub struct McpBackupReport {
    /// Identity proven immediately before programming-mode entry, if reached.
    pub identity: Option<Identity>,
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
    /// This does not verify transport capture completeness, return to CAT,
    /// physical-unit continuity, or firmware compatibility with a schema.
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
    /// Read every official configuration region, then acknowledge MCP exit.
    ///
    /// Reads the fixed global regions followed by the six Programmable-Memory
    /// slots, with the official page boundaries. The startup bitmap and unnamed
    /// region group are excluded. Sends identity queries, programming entry,
    /// read requests, protocol ACKs, and exit; never sends memory-write, fill,
    /// or RF-transmit commands. Reading does not qualify a firmware schema.
    ///
    /// `progress` receives the completed and total page counts after each
    /// fully acknowledged page. Every return preserves all such pages. A read
    /// failure records its exact page and sends no speculative exit or CAT.
    ///
    /// After the exit ACK, no baud change or CAT command touches the old handle.
    /// Further protocol access remains blocked. The caller must close and drop
    /// the transport before any explicitly selected fresh-connection verification.
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
        if should_cancel() {
            report.outcome = McpBackupOutcome::Cancelled;
            return report;
        }
        match self.identify().await {
            Ok(identity) => report.identity = Some(identity),
            Err(error) => {
                report.fail(McpBackupStage::Identity, error);
                return report;
            }
        }
        if should_cancel() {
            report.outcome = McpBackupOutcome::Cancelled;
            return report;
        }
        let mut session = match self.enter_mcp().await {
            Ok(session) => session,
            Err(error) => {
                report.exit = McpProbeExit::RecoveryRequired;
                report.fail(McpBackupStage::Entry, error);
                return report;
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
                    return report;
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
        if let Err(error) = session.exit_detached().await {
            report.exit = McpProbeExit::NotAcknowledged;
            report.fail(McpBackupStage::Exit, error);
            return report;
        }
        report.exit = McpProbeExit::Acknowledged;
        report
    }
}
