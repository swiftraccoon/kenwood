//! One captured MCP programming pass that writes a [`TerminalPlan`], recording
//! its intent through a caller-supplied [`TerminalJournal`] before each write.
//!
//! Entry reads a fresh backup, plans the requested route, and writes the
//! changed pages; restoration writes an already-derived reverse plan without a
//! second backup. Both compare every complete page against a fresh read before
//! the first write, record durable intent before each write, read each written
//! page back, and then acknowledge one MCP exit. The journal sink owns its own
//! durability; this session owns the protocol order and the safety checks.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_transport::Transport;

use super::{TerminalGatewayRoute, TerminalPlan, TerminalPlanError};
use crate::error::Error;
use crate::protocol::mcp::regions;
use crate::radio::menu::MenuFieldSnapshot;
use crate::radio::programming::{McpJournal, McpSession, PageReplacement};
use crate::radio::{Identity, Radio};
use crate::types::DvGatewayMode;

/// Which direction one programming pass writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalPhase {
    /// First entry: back up, plan the route, and write the change.
    Entry,
    /// Restoration: write the reverse plan derived from the entry plan.
    Restore,
}

/// The durable record one programming pass writes as it proceeds.
///
/// Every method must make its record durable before returning `Ok`; the
/// session treats a returned error as a failure to record and sends no write
/// that the record was meant to precede. The sink is free to synchronize a
/// separate wire transcript inside [`Self::before_write`] before recording the
/// page, so an interrupted write is always preceded by a durable intent.
pub trait TerminalJournal {
    /// Record the complete backup pages read before entry planning.
    ///
    /// Called once per [`TerminalPhase::Entry`] pass, before the plan.
    ///
    /// # Errors
    ///
    /// Returns the sink's durability failure; the session then exits without
    /// writing any page.
    fn backup(&mut self, identity: &Identity, snapshot: &MenuFieldSnapshot) -> io::Result<()>;

    /// Record the entry plan and its complete page images.
    ///
    /// Called once per [`TerminalPhase::Entry`] pass, after the backup.
    ///
    /// # Errors
    ///
    /// Returns the sink's durability failure; the session then exits without
    /// writing any page.
    fn planned(&mut self, plan: &TerminalPlan) -> io::Result<()>;

    /// Record the exact page about to be written, including both images.
    ///
    /// Called immediately before each changed page's write, never for a no-op.
    ///
    /// # Errors
    ///
    /// Returns the sink's durability failure; the page is then not written and
    /// the session stops before it.
    fn before_write(&mut self, phase: TerminalPhase, page: &PageReplacement) -> io::Result<()>;

    /// Record the pass's outcome: the exit acknowledgment and the session's
    /// possibly-written and verified pages.
    ///
    /// Called once at the end of every pass, success or failure.
    ///
    /// # Errors
    ///
    /// Returns the sink's durability failure, kept separately from any earlier
    /// operation or exit failure.
    fn checkpoint(
        &mut self,
        phase: TerminalPhase,
        acknowledged_exit: bool,
        journal: &McpJournal,
    ) -> io::Result<()>;
}

/// What one [`program_terminal`] pass writes.
#[derive(Debug, Clone, Copy)]
pub enum TerminalRequest<'a> {
    /// Back up, plan Reflector Terminal on `route`, and write the change.
    Enter {
        /// Gateway route the plan selects, including Bluetooth.
        route: TerminalGatewayRoute,
    },
    /// Write this reverse plan, derived earlier from the entry plan.
    Restore {
        /// The restoration plan; its identity and before-Gateway must match.
        plan: &'a TerminalPlan,
    },
}

impl TerminalRequest<'_> {
    const fn phase(&self) -> TerminalPhase {
        match self {
            Self::Enter { .. } => TerminalPhase::Entry,
            Self::Restore { .. } => TerminalPhase::Restore,
        }
    }
}

/// Outcome of one completed programming pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalProgramReport {
    /// The plan this pass applied: the freshly built entry plan, or the exact
    /// restoration plan supplied.
    pub plan: TerminalPlan,
    /// Whether an intent was recorded and a page write was therefore begun.
    ///
    /// True means a write frame may have reached the radio.
    pub write_started: bool,
    /// Whether the MCP exit that ends this pass was acknowledged.
    pub exit_acknowledged: bool,
}

/// First failure of one programming pass, keeping the operation, exit, and
/// checkpoint failures separate so a later one never hides the radio error.
#[derive(Debug, thiserror::Error)]
pub struct TerminalProgramError {
    /// The failed programming operation, when one failed.
    #[source]
    pub operation: Option<TerminalOperationError>,
    /// A failed MCP exit, kept even when an operation also failed.
    pub exit: Option<Error>,
    /// A failed journal checkpoint, kept even when another step also failed.
    pub checkpoint: Option<io::Error>,
}

impl TerminalProgramError {
    /// A refusal before programming entry: only the operation cause is set.
    const fn operation(operation: TerminalOperationError) -> Self {
        Self {
            operation: Some(operation),
            exit: None,
            checkpoint: None,
        }
    }
}

impl std::fmt::Display for TerminalProgramError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Terminal programming incomplete")?;
        if let Some(error) = &self.operation {
            write!(formatter, "; operation: {error}")?;
        }
        if let Some(error) = &self.exit {
            write!(formatter, "; exit: {error}")?;
        }
        if let Some(error) = &self.checkpoint {
            write!(formatter, "; checkpoint: {error}")?;
        }
        Ok(())
    }
}

/// Typed cause of a failed programming operation, before exit or checkpoint.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TerminalOperationError {
    /// Cancellation was honored before any write intent was recorded.
    #[error("Terminal programming cancelled before any write")]
    Cancelled,
    /// The fresh identity is not the exact qualified target, or differs from
    /// the expected identity.
    #[error("fresh identity {actual} is not the expected qualified target")]
    Identity {
        /// The identity the connection reported.
        actual: Identity,
    },
    /// The fresh Gateway state is neither Off nor Terminal, or differs from the
    /// state observed before this pass.
    #[error("fresh Gateway state {actual} differs from the expected {expected}")]
    Gateway {
        /// The state required for this pass.
        expected: DvGatewayMode,
        /// The state the connection reported.
        actual: DvGatewayMode,
    },
    /// The restoration plan's identity or before-Gateway does not match this
    /// pass's expected identity and Gateway.
    #[error("restoration plan does not match the requested identity or Gateway")]
    RestorationMismatch,
    /// The captured Gateway differs from the fresh CAT Gateway during entry.
    #[error("captured Gateway differs from the fresh CAT Gateway")]
    CapturedGatewayMismatch,
    /// A CAT, MCP, transport, or timeout failure.
    #[error(transparent)]
    Io(#[from] Error),
    /// A plan could not be built or validated.
    #[error(transparent)]
    Plan(#[from] TerminalPlanError),
    /// The journal sink could not record the backup or plan before any write.
    #[error("Terminal journal record failed: {0}")]
    Journal(#[source] io::Error),
}

/// Run one MCP programming pass for `request`, recording through `journal`.
///
/// The radio is identified fresh and must be the exact qualified target and
/// equal to `expected`; its Gateway must equal `gateway` and be Off or
/// Terminal. Entry reads the canonical backup pages, records them, plans the
/// route, records the plan, then compares and writes; restoration writes the
/// supplied reverse plan without a backup. Each changed page's intent is
/// recorded before its write. After the batch, one MCP exit is acknowledged
/// when the session is ready, and the checkpoint is recorded. Cancellation
/// stops the pass only until the first intent is recorded; after that the
/// bounded page transaction and its exit run to completion.
///
/// # Errors
///
/// Returns [`TerminalProgramError`] with the first operation failure and any
/// separate exit or checkpoint failure. An interrupted write leaves the
/// journal's recorded intent and possibly-written pages for recovery. The
/// error is boxed because its typed causes carry the crate's large [`Error`].
pub async fn program_terminal<T: Transport>(
    radio: &mut Radio<T>,
    expected: &Identity,
    gateway: DvGatewayMode,
    request: TerminalRequest<'_>,
    journal: &mut impl TerminalJournal,
    cancelled: &AtomicBool,
) -> Result<TerminalProgramReport, Box<TerminalProgramError>> {
    let phase = request.phase();
    // Refusals before programming entry return the operation cause alone; the
    // checkpoint records only passes that entered MCP.
    if let TerminalRequest::Restore { plan } = request
        && (plan.identity() != expected || plan.before() != gateway)
    {
        return Err(Box::new(TerminalProgramError::operation(
            TerminalOperationError::RestorationMismatch,
        )));
    }
    if let Err(error) = check_cancelled(cancelled) {
        return Err(Box::new(TerminalProgramError::operation(error)));
    }
    if let Err(error) = identify_and_check(radio, expected, gateway).await {
        return Err(Box::new(TerminalProgramError::operation(error)));
    }
    if let Err(error) = check_cancelled(cancelled) {
        return Err(Box::new(TerminalProgramError::operation(error)));
    }
    let mut session = match radio.enter_mcp().await {
        Ok(session) => session,
        Err(error) => {
            return Err(Box::new(TerminalProgramError::operation(
                TerminalOperationError::Io(error),
            )));
        }
    };
    let mut write_started = false;
    let operation = run_pages(
        &mut session,
        expected,
        gateway,
        request,
        journal,
        cancelled,
        &mut write_started,
    )
    .await;
    let mcp_journal = session.journal().clone();
    let exit = if session.is_ready() {
        session.exit().await
    } else {
        Err(Error::from(crate::error::McpError::RecoveryRequired))
    };
    let checkpoint = journal.checkpoint(phase, exit.is_ok(), &mcp_journal);
    finish(operation, write_started, exit, checkpoint)
}

/// Build the plan and write its pages inside an open MCP session.
async fn run_pages<T: Transport>(
    session: &mut McpSession<'_, T>,
    expected: &Identity,
    gateway: DvGatewayMode,
    request: TerminalRequest<'_>,
    journal: &mut impl TerminalJournal,
    cancelled: &AtomicBool,
    write_started: &mut bool,
) -> Result<TerminalPlan, TerminalOperationError> {
    let phase = request.phase();
    let plan = prepare_plan(session, expected, gateway, request, journal, cancelled).await?;
    let _report = session
        .compare_exchange_terminal(
            &plan,
            |page| record_before_write(journal, phase, page, write_started, cancelled),
            |_progress| {},
        )
        .await
        .map_err(operation_from_plan_error)?;
    Ok(plan)
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<(), TerminalOperationError> {
    if cancelled.load(Ordering::Relaxed) {
        Err(TerminalOperationError::Cancelled)
    } else {
        Ok(())
    }
}

/// Build the plan for this pass: fresh backup and route for entry, or the
/// supplied reverse plan for restoration.
async fn prepare_plan<T: Transport>(
    session: &mut McpSession<'_, T>,
    expected: &Identity,
    gateway: DvGatewayMode,
    request: TerminalRequest<'_>,
    journal: &mut impl TerminalJournal,
    cancelled: &AtomicBool,
) -> Result<TerminalPlan, TerminalOperationError> {
    match request {
        TerminalRequest::Restore { plan } => Ok(plan.clone()),
        TerminalRequest::Enter { route } => {
            let snapshot = read_backup(session, cancelled).await?;
            journal
                .backup(expected, &snapshot)
                .map_err(TerminalOperationError::Journal)?;
            let plan = TerminalPlan::for_route(expected, &snapshot, route)?;
            if plan.before() != gateway {
                return Err(TerminalOperationError::CapturedGatewayMismatch);
            }
            journal
                .planned(&plan)
                .map_err(TerminalOperationError::Journal)?;
            Ok(plan)
        }
    }
}

/// Read every canonical planning page, checking cancellation between pages.
async fn read_backup<T: Transport>(
    session: &mut McpSession<'_, T>,
    cancelled: &AtomicBool,
) -> Result<MenuFieldSnapshot, TerminalOperationError> {
    let mut pages = Vec::new();
    for region in regions::menu_regions() {
        for page in region.pages() {
            if cancelled.load(Ordering::Relaxed) {
                return Err(TerminalOperationError::Cancelled);
            }
            let bytes = session.read_page(page).await?;
            pages.push((page, bytes));
        }
    }
    Ok(MenuFieldSnapshot::from_pages(pages).map_err(Error::from)?)
}

/// Identify the radio fresh and require the exact qualified expected target
/// and the expected Gateway.
async fn identify_and_check<T: Transport>(
    radio: &mut Radio<T>,
    expected: &Identity,
    gateway: DvGatewayMode,
) -> Result<(), TerminalOperationError> {
    let identity = radio.identify().await?;
    if !identity.is_qualified_write_target() || identity != *expected {
        return Err(TerminalOperationError::Identity { actual: identity });
    }
    let observed = radio.get_dv_gateway_mode().await?;
    if !matches!(observed, DvGatewayMode::Off | DvGatewayMode::Terminal) || observed != gateway {
        return Err(TerminalOperationError::Gateway {
            expected: gateway,
            actual: observed,
        });
    }
    Ok(())
}

/// Record one page's intent and mark that a write was begun.
///
/// Cancellation is honored only before the first intent; once a write may have
/// reached the radio, the bounded transaction runs to its end.
fn record_before_write(
    journal: &mut impl TerminalJournal,
    phase: TerminalPhase,
    page: &PageReplacement,
    write_started: &mut bool,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    if !*write_started && cancelled.load(Ordering::Relaxed) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Terminal programming cancelled before the first write",
        ));
    }
    journal.before_write(phase, page)?;
    *write_started = true;
    Ok(())
}

/// Map the plan-engine error back to an operation cause.
fn operation_from_plan_error(error: TerminalPlanError) -> TerminalOperationError {
    match error {
        TerminalPlanError::Operation(error) => TerminalOperationError::Io(error),
        other => TerminalOperationError::Plan(other),
    }
}

/// Combine the operation, exit and checkpoint results into one report or error.
fn finish(
    operation: Result<TerminalPlan, TerminalOperationError>,
    write_started: bool,
    exit: Result<(), Error>,
    checkpoint: io::Result<()>,
) -> Result<TerminalProgramReport, Box<TerminalProgramError>> {
    match (operation, exit, checkpoint) {
        (Ok(plan), Ok(()), Ok(())) => Ok(TerminalProgramReport {
            plan,
            write_started,
            exit_acknowledged: true,
        }),
        (operation, exit, checkpoint) => Err(Box::new(TerminalProgramError {
            operation: operation.err(),
            exit: exit.err(),
            checkpoint: checkpoint.err(),
        })),
    }
}

#[cfg(test)]
mod tests;
