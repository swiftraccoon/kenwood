//! One captured MCP session: read the backup pages, compare and write the
//! planned pages, then exit.

use std::fs::File;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::memory::TerminalGatewayRoute;
use kenwood_tmd750::protocol::mcp::regions;
use kenwood_tmd750::radio::programming::{McpSession, PageReplacement};
use kenwood_tmd750::radio::terminal::TerminalPlan;
use kenwood_tmd750::{DvGatewayMode, Identity, MenuFieldSnapshot, Radio};
use kenwood_transport::{Transport, TransportError};

use crate::capture::CaptureTransport;
use crate::{AppResult, CommandError};

use super::journal::{Journal, Phase};

pub(super) struct Borrowed<'a, T>(pub(super) &'a mut T);

impl<T: Transport> Transport for Borrowed<'_, T> {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.0.write(bytes).await
    }
    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        self.0.read(bytes).await
    }
    async fn close(&mut self) -> Result<(), TransportError> {
        self.0.close().await
    }
    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        self.0.set_baud_rate(baud)
    }
}

pub(super) fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "D-STAR startup cancelled",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn validate_identity(identity: &Identity) -> AppResult<()> {
    if identity.model != kenwood_tmd750::RadioModel::TmD750
        || identity.firmware.as_str() != "1.02"
        || identity.radio_type.as_str() != "K,2,1"
    {
        return Err(CommandError(
            "automatic Terminal startup requires matching TM-D750 / 1.02 / K,2,1 identities"
                .to_owned(),
        )
        .into());
    }
    Ok(())
}

async fn identify_radio(
    radio: &mut Radio<impl Transport>,
    expected: Option<&Identity>,
) -> AppResult<(Identity, DvGatewayMode)> {
    let identity = radio.identify().await?;
    validate_identity(&identity)?;
    if expected.is_some_and(|expected| *expected != identity) {
        return Err(CommandError(
            "fresh CAT identity differs from the retained startup identity".to_owned(),
        )
        .into());
    }
    let gateway = radio.get_dv_gateway_mode().await?;
    if !matches!(gateway, DvGatewayMode::Off | DvGatewayMode::Terminal) {
        return Err(CommandError(format!(
            "automatic Terminal startup refuses Gateway {gateway}"
        ))
        .into());
    }
    Ok((identity, gateway))
}

async fn snapshot<T: Transport>(
    session: &mut McpSession<'_, T>,
    cancelled: &AtomicBool,
) -> AppResult<MenuFieldSnapshot> {
    let mut pages = Vec::new();
    for page in regions::menu_regions()
        .into_iter()
        .flat_map(kenwood_tmd750::Region::pages)
    {
        check_cancelled(cancelled)?;
        pages.push((page, session.read_page(page).await?));
    }
    Ok(MenuFieldSnapshot::from_pages(pages)?)
}

/// Compare and write the planned pages in one MCP session, then exit it.
///
/// Each page is compared against its captured image and read back in full
/// before the exit. Cancellation stops the session only until the first
/// journal record is synced; after that the page transaction runs to its end.
pub(super) async fn apply<T: Transport>(
    transport: &mut CaptureTransport<T, File>,
    expected: &Identity,
    gateway: DvGatewayMode,
    restore: Option<&TerminalPlan>,
    journal: &mut Journal,
    cancelled: &AtomicBool,
) -> AppResult<TerminalPlan> {
    transport.synchronize()?;
    check_cancelled(cancelled)?;
    if restore.is_some_and(|plan| plan.identity() != expected || plan.before() != gateway) {
        return Err(CommandError(
            "restoration plan identity or Gateway differs from the requested restoration"
                .to_owned(),
        )
        .into());
    }
    let raw = transport.synchronization_handle()?;
    let mut radio = Radio::new(Borrowed(transport));
    let (_, observed) = identify_radio(&mut radio, Some(expected)).await?;
    if observed != gateway {
        return Err(
            CommandError("fresh Gateway state changed before programming".to_owned()).into(),
        );
    }
    check_cancelled(cancelled)?;
    let mut session = radio.enter_mcp().await?;
    let phase = if restore.is_some() {
        Phase::Restore
    } else {
        Phase::Entry
    };
    let outcome = async {
        let plan = if let Some(plan) = restore {
            plan.clone()
        } else {
            let snapshot = snapshot(&mut session, cancelled).await?;
            journal.backup(expected, &snapshot)?;
            let plan =
                TerminalPlan::for_route(expected, &snapshot, TerminalGatewayRoute::Bluetooth)?;
            if plan.before() != gateway {
                return Err(
                    CommandError("captured Gateway differs from fresh CAT".to_owned()).into(),
                );
            }
            journal.planned(&plan)?;
            plan
        };
        check_cancelled(cancelled)?;
        let mut admitted = false;
        let _compared = session
            .compare_exchange_terminal(
                &plan,
                |page| {
                    admit_write(phase, page, journal, cancelled, &mut admitted, || {
                        raw.sync_all()
                    })
                },
                |_| {},
            )
            .await?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(plan)
    }
    .await;
    let pages = session.journal().clone();
    let exit: AppResult<()> = if session.is_ready() {
        session.exit().await.map_err(Into::into)
    } else {
        Err(
            CommandError("MCP framing is uncertain; no speculative exit was sent".to_owned())
                .into(),
        )
    };
    let checkpoint = journal.checkpoint(phase, exit.is_ok(), &pages);
    finish(outcome, exit, checkpoint)
}

/// Sync the wire transcript and record this page in the journal before it is
/// written. Cancellation is checked only before the first such record.
fn admit_write(
    phase: Phase,
    page: &PageReplacement,
    journal: &mut Journal,
    cancelled: &AtomicBool,
    admitted: &mut bool,
    synchronize_wire: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    if !*admitted {
        check_cancelled(cancelled)?;
    }
    synchronize_wire()?;
    journal.before_write(phase, page)?;
    *admitted = true;
    Ok(())
}

/// The operation, exit and journal-checkpoint failures of one `apply`, kept
/// separately so a journal failure never hides the radio error.
#[derive(Debug)]
struct ApplyFailure {
    operation: Option<Box<dyn std::error::Error + Send + Sync>>,
    exit: Option<Box<dyn std::error::Error + Send + Sync>>,
    checkpoint: Option<io::Error>,
}

impl std::fmt::Display for ApplyFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Terminal programming incomplete")?;
        for (stage, failure) in [
            ("operation", self.operation.as_deref()),
            ("exit", self.exit.as_deref()),
        ] {
            if let Some(error) = failure {
                write!(formatter, "; {stage}: {error}")?;
            }
        }
        if let Some(error) = &self.checkpoint {
            write!(formatter, "; checkpoint: {error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ApplyFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.operation
            .as_deref()
            .or(self.exit.as_deref())
            .map(|error| -> &dyn std::error::Error { error })
            .or_else(|| {
                self.checkpoint
                    .as_ref()
                    .map(|error| -> &dyn std::error::Error { error })
            })
    }
}

fn finish(
    outcome: AppResult<TerminalPlan>,
    exit: AppResult<()>,
    checkpoint: io::Result<()>,
) -> AppResult<TerminalPlan> {
    match (outcome, exit, checkpoint) {
        (Ok(plan), Ok(()), Ok(())) => Ok(plan),
        (outcome, exit, checkpoint) => Err(ApplyFailure {
            operation: outcome.err(),
            exit: exit.err(),
            checkpoint: checkpoint.err(),
        }
        .into()),
    }
}

#[cfg(test)]
pub(super) mod tests;
