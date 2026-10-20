//! One programming session on a selected serial endpoint, through a
//! [`ControlHost`].
//!
//! [`write_pages`] runs the order the hardware requires around a page plan:
//! on an operation-panel endpoint it first waits for [`await_settled`]; it
//! opens the endpoint once, requires the identity the plan was prepared from
//! and DV Gateway Off, enters programming mode, compare-exchanges the plan
//! with the journal called before each write, exits, closes the connection,
//! and after an acknowledged exit runs [`verify_readiness`] for that identity
//! and Gateway Off. [`backup`] runs the same order around a standard backup.
//! Nothing is retried or rolled back; a [`SessionReport`] records how far the
//! session got. The host owns every open and close.

use std::io;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};

use super::backup::McpBackupReport;
use super::probe::McpProbeExit;
use super::programming::{McpCompareExchangeReport, McpJournal, PageReplacement};
use super::readiness::{
    CloseFailure, ControlHost, ControlStage, ReadinessReport, SettleReport, await_settled,
    verify_readiness,
};
use super::{Identity, Radio};
use crate::error::Error;
use crate::transport::SerialCandidate;
use crate::types::DvGatewayMode;

#[cfg(test)]
mod tests;

/// The record a session makes durable before each page write.
pub trait WriteJournal {
    /// Record that `replacement` is about to be written; an error stops the
    /// batch before that write.
    ///
    /// # Errors
    ///
    /// Returns the failure to make the record durable.
    fn before_write(&mut self, replacement: &PageReplacement) -> io::Result<()>;
}

impl<F: FnMut(&PageReplacement) -> io::Result<()>> WriteJournal for F {
    fn before_write(&mut self, replacement: &PageReplacement) -> io::Result<()> {
        self(replacement)
    }
}

/// Whether a session entered programming mode and how it left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionExit {
    /// Programming mode was not entered.
    NotEntered,
    /// The radio acknowledged the exit.
    Acknowledged,
    /// The exit was sent and not acknowledged.
    NotAcknowledged,
    /// An exchange was left incomplete, so no exit was sent.
    RecoveryRequired,
}

/// How far one session got: every step it reached, in order.
#[derive(Debug)]
#[non_exhaustive]
pub struct SessionReport {
    /// The settle wait before the open; `NotRequired` on the main unit.
    pub settle: SettleReport,
    /// Whether the cancellation flag stopped the session before programming
    /// entry; nothing was written and `error` stays `None`.
    pub cancelled: bool,
    /// The identity the opened connection reported.
    pub identity: Option<Identity>,
    /// The Gateway mode that connection reported before entry.
    pub gateway: Option<DvGatewayMode>,
    /// Whether programming mode was entered and how it ended.
    pub exit: SessionExit,
    /// Pages a write was started for and pages read back equal.
    pub journal: McpJournal,
    /// The batch's pages when every page of it completed.
    pub compare_exchange: Option<McpCompareExchangeReport>,
    /// The failure that stopped the session.
    pub error: Option<Error>,
    /// An exit failure retained independently of the page operation's error.
    pub exit_error: Option<Error>,
    /// The close of the session connection; `None` when none opened.
    pub close: Option<Result<(), CloseFailure>>,
    /// The fresh identity and Gateway Off check after an acknowledged exit
    /// and a successful close.
    pub readiness: Option<ReadinessReport>,
}

impl SessionReport {
    fn new(settle: SettleReport) -> Self {
        Self {
            settle,
            cancelled: false,
            identity: None,
            gateway: None,
            exit: SessionExit::NotEntered,
            journal: McpJournal::default(),
            compare_exchange: None,
            error: None,
            exit_error: None,
            close: None,
            readiness: None,
        }
    }

    /// Whether the batch completed, the exit was acknowledged, the
    /// connection closed and a fresh identity with Gateway Off answered.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.error.is_none()
            && self.exit_error.is_none()
            && self.exit == SessionExit::Acknowledged
            && matches!(self.close, Some(Ok(())))
            && self
                .readiness
                .as_ref()
                .is_some_and(ReadinessReport::succeeded)
    }
}

/// A [`backup`] session's report and the captured pages.
#[derive(Debug)]
#[non_exhaustive]
pub struct BackupSession {
    /// How far the session got; `journal` and `compare_exchange` stay empty.
    pub session: SessionReport,
    /// The backup, when the connection opened; a backup that failed keeps
    /// its failure in [`McpBackupReport::outcome`], not in the session's
    /// `error`.
    pub backup: Option<McpBackupReport>,
}

impl BackupSession {
    /// Whether every standard region was captured and the session succeeded.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.session.succeeded()
            && self
                .backup
                .as_ref()
                .is_some_and(McpBackupReport::has_complete_configuration)
    }
}

/// Write `replacements` in one programming session on `endpoint`, prepared
/// against a backup of `identity`.
///
/// `journal` is called before each changed page's write; equal pages are
/// compared and never written. Cancellation is checked before the open and
/// before entry only; once programming mode is entered the batch, the exit,
/// the close and the readiness check run to completion. Await the future to
/// completion.
pub async fn write_pages<C: ControlHost>(
    host: &mut C,
    endpoint: &SerialCandidate,
    baud: NonZeroU32,
    identity: &Identity,
    replacements: &[PageReplacement],
    journal: &mut impl WriteJournal,
    cancelled: &AtomicBool,
) -> SessionReport {
    host.stage(ControlStage::Programming);
    let settle = await_settled(host, endpoint, cancelled).await;
    let mut report = SessionReport::new(settle);
    report.cancelled = cancelled.load(Ordering::Relaxed);
    if report.cancelled || !report.settle.settled() {
        return report;
    }
    let connection = match host.open(endpoint, baud) {
        Ok(connection) => connection,
        Err(error) => {
            report.error = Some(error.into());
            return report;
        }
    };
    let mut radio = Radio::new(connection);
    if let Err(error) = program(
        &mut radio,
        identity,
        replacements,
        journal,
        cancelled,
        &mut report,
    )
    .await
    {
        report.error = Some(error);
    }
    finish(
        host,
        radio,
        endpoint,
        baud,
        identity,
        Some(DvGatewayMode::Off),
        &mut report,
    )
    .await;
    report
}

/// Run a standard backup in one programming session on `endpoint`, for a
/// radio expected to identify as `identity`.
///
/// Cancellation is checked before the open and between pages; once a
/// connection opens, its exit, close and readiness check run to completion.
/// Await the future to completion.
pub async fn backup<C: ControlHost>(
    host: &mut C,
    endpoint: &SerialCandidate,
    baud: NonZeroU32,
    identity: &Identity,
    cancelled: &AtomicBool,
) -> BackupSession {
    host.stage(ControlStage::Programming);
    let settle = await_settled(host, endpoint, cancelled).await;
    let mut report = SessionReport::new(settle);
    report.cancelled = cancelled.load(Ordering::Relaxed);
    if report.cancelled || !report.settle.settled() {
        return BackupSession {
            session: report,
            backup: None,
        };
    }
    let connection = match host.open(endpoint, baud) {
        Ok(connection) => connection,
        Err(error) => {
            report.error = Some(error.into());
            return BackupSession {
                session: report,
                backup: None,
            };
        }
    };
    let mut radio = Radio::new(connection);
    let captured = radio
        .backup_mcp_checked_until_exit(
            Some(identity),
            || cancelled.load(Ordering::Relaxed),
            |_progress| {},
        )
        .await;
    report.identity.clone_from(&captured.identity);
    report.gateway = captured.gateway_mode;
    report.exit = match captured.exit {
        McpProbeExit::Acknowledged => SessionExit::Acknowledged,
        McpProbeExit::NotAcknowledged => SessionExit::NotAcknowledged,
        McpProbeExit::NotEntered => SessionExit::NotEntered,
        McpProbeExit::RecoveryRequired => SessionExit::RecoveryRequired,
    };
    if let Some(actual) = report
        .identity
        .clone()
        .filter(|reported| reported != identity)
    {
        report.error = Some(Error::IdentityMismatch {
            expected: identity.clone(),
            actual,
        });
    }
    finish(
        host,
        radio,
        endpoint,
        baud,
        identity,
        Some(DvGatewayMode::Off),
        &mut report,
    )
    .await;
    BackupSession {
        session: report,
        backup: Some(captured),
    }
}

/// Identify, check Gateway Off, enter, compare-exchange and exit.
async fn program<T: kenwood_transport::Transport>(
    radio: &mut Radio<T>,
    identity: &Identity,
    replacements: &[PageReplacement],
    journal: &mut impl WriteJournal,
    cancelled: &AtomicBool,
    report: &mut SessionReport,
) -> Result<(), Error> {
    let actual = radio.identify().await?;
    report.identity = Some(actual.clone());
    if &actual != identity {
        return Err(Error::IdentityMismatch {
            expected: identity.clone(),
            actual,
        });
    }
    let gateway = radio.get_dv_gateway_mode().await?;
    report.gateway = Some(gateway);
    if gateway != DvGatewayMode::Off {
        return Err(Error::GatewayNotOff { observed: gateway });
    }
    if cancelled.load(Ordering::Relaxed) {
        report.cancelled = true;
        return Ok(());
    }
    report.exit = SessionExit::RecoveryRequired;
    let mut session = radio.enter_mcp().await?;
    let outcome = session
        .compare_exchange_pages(
            replacements,
            |replacement| journal.before_write(replacement),
            |_progress| {},
        )
        .await;
    report.journal = session.journal().clone();
    let exited = if session.is_ready() {
        report.exit = SessionExit::NotAcknowledged;
        session.exit().await.map(|()| {
            report.exit = SessionExit::Acknowledged;
        })
    } else {
        Ok(())
    };
    report.exit_error = exited.err();
    report.compare_exchange = Some(outcome?);
    Ok(())
}

/// Close the session connection and, after an acknowledged exit, verify a
/// fresh identity, with `gateway` read on the matching connection when given.
async fn finish<C: ControlHost>(
    host: &mut C,
    radio: Radio<C::Connection>,
    endpoint: &SerialCandidate,
    baud: NonZeroU32,
    identity: &Identity,
    gateway: Option<DvGatewayMode>,
    report: &mut SessionReport,
) {
    let closed = host.close(radio.into_transport()).await;
    let released = closed.is_ok();
    report.close = Some(closed);
    if released && report.exit == SessionExit::Acknowledged {
        host.stage(ControlStage::ReadinessAfterProgramming);
        let finish = AtomicBool::new(false);
        report.readiness =
            Some(verify_readiness(host, endpoint, baud, identity, gateway, &finish).await);
    }
}
