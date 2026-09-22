//! Library-owned Reflector Terminal startup and restoration over two
//! caller-supplied hosts: a serial [`ControlHost`] for CAT and MCP, and a
//! [`ModemHost`] for the modem link.
//!
//! [`TerminalLifecycle::prepare`] observes the control endpoint, opens the
//! modem link, writes the Terminal and routing pages, acquires MMDVM framing,
//! and hands back the proved modem connection plus a [`TerminalRecovery`] that
//! rewrites exactly the pages it changed. When the stored Gateway is Off the
//! pages are written over the modem link itself; when a Terminal route is
//! already active the modem link may already carry MMDVM, so the pages are
//! written over the independent control endpoint instead and the modem link is
//! never sent CAT. The caller owns endpoint selection, the journal sink's
//! durability, the modem host's open and reopen, and the decision to restore.
//!
//! This composes [`observe_control`], [`verify_readiness`],
//! [`program_terminal`] and [`acquire_modem`]; it enters no mode on its own
//! beyond those, sends no RF command, and never reopens a serial endpoint
//! automatically.

use std::sync::atomic::AtomicBool;

use super::session::{
    TerminalJournal, TerminalProgramError, TerminalProgramReport, TerminalRequest, program_terminal,
};
use super::transition::{
    ModemHost, ModemOpenFailure, ProvenModem, TransitionAttempt, TransitionError, acquire_modem,
};
use super::{TerminalGatewayRoute, TerminalPlan, TerminalPlanError, TerminalTarget};
use crate::radio::readiness::{
    CloseFailure, ControlHost, ControlStage, Expectation, ObservationReport, ReadinessReport,
    observe_control, verify_readiness,
};
use crate::radio::{Identity, Radio};
use crate::transport::SerialCandidate;
use crate::types::DvGatewayMode;

/// Whether the original Gateway settings still need to be rewritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestorationState {
    /// No page was changed, so nothing is owed.
    NotRequired,
    /// A change may have been written and has not yet been reversed.
    Owed,
    /// The reverse plan was written and verified on a fresh connection.
    Verified,
    /// Restoration was owed but cannot proceed, for example because the modem
    /// connection was not confirmed closed.
    Blocked,
}

/// First failure of one [`TerminalLifecycle::prepare`] call.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LifecycleError {
    /// The control endpoint did not answer a complete identity and Gateway
    /// preflight.
    #[error("the control endpoint did not complete its identity and Gateway preflight")]
    ControlPreflight,
    /// The observed identity is not the exact qualified target.
    #[error("control endpoint identity {actual} is not the qualified TM-D750 target")]
    UnsupportedTarget {
        /// The identity the control endpoint reported.
        actual: Identity,
    },
    /// The observed Gateway state is neither Off nor a Terminal route.
    #[error("control endpoint Gateway state {actual} is neither Off nor Terminal")]
    UnsupportedGateway {
        /// The Gateway state the control endpoint reported.
        actual: DvGatewayMode,
    },
    /// The modem link could not be opened.
    #[error("modem link open failed: {0}")]
    ModemOpen(#[source] ModemOpenFailure),
    /// The control endpoint could not be opened for active-route programming.
    #[error("control endpoint open failed for active-route programming")]
    ControlOpen,
    /// The Terminal pages could not be written.
    #[error(transparent)]
    Program(#[from] Box<TerminalProgramError>),
    /// A reverse plan could not be derived from the entry plan.
    #[error(transparent)]
    RestorationPlan(#[from] TerminalPlanError),
    /// The control endpoint was not confirmed closed after active-route
    /// programming.
    #[error("the control endpoint was not confirmed closed after programming: {0}")]
    ControlClose(#[source] CloseFailure),
    /// MMDVM framing was not proved within the acquisition window.
    #[error("modem framing was not proved: {0}")]
    Transition(#[source] TransitionError),
}

/// Reflector Terminal startup over a [`ControlHost`] and a [`ModemHost`].
///
/// One value performs one startup. It holds the control endpoint and baud, the
/// Gateway route the modem link uses, and the modem-acquisition window.
#[derive(Debug, Clone)]
pub struct TerminalLifecycle {
    control_endpoint: SerialCandidate,
    baud: u32,
    route: TerminalGatewayRoute,
}

impl TerminalLifecycle {
    /// Configure startup over `control_endpoint` at `baud`, with the modem
    /// link on `route`.
    ///
    /// `route` must name the interface the [`ModemHost`] actually opens; this
    /// workflow uses [`TerminalGatewayRoute::Bluetooth`].
    #[must_use]
    pub const fn new(
        control_endpoint: SerialCandidate,
        baud: u32,
        route: TerminalGatewayRoute,
    ) -> Self {
        Self {
            control_endpoint,
            baud,
            route,
        }
    }

    /// The control endpoint restoration reuses.
    #[must_use]
    pub const fn control_endpoint(&self) -> &SerialCandidate {
        &self.control_endpoint
    }

    /// Observe the control endpoint, program the Terminal pages, and acquire
    /// MMDVM framing.
    ///
    /// On success the [`TerminalStartup`] carries the proved modem connection
    /// and a [`TerminalRecovery`] owing the reverse plan. On failure it carries
    /// the same recovery, so the caller can still restore. The sub-reports are
    /// populated whether or not the step after them ran, for the caller to
    /// record. This performs no restoration itself and never reopens a serial
    /// endpoint automatically.
    pub async fn prepare<C, M, J>(
        &self,
        control: &mut C,
        modem: &mut M,
        journal: &mut J,
        cancelled: &AtomicBool,
    ) -> TerminalStartup<M::Connection>
    where
        C: ControlHost,
        M: ModemHost,
        J: TerminalJournal,
    {
        let mut startup = TerminalStartup::new();
        control.stage(ControlStage::Preflight);
        let preflight = observe_control(
            control,
            &self.control_endpoint,
            self.baud,
            Expectation::default(),
            cancelled,
        )
        .await;
        let observed = preflight
            .observed()
            .map(|(identity, gateway)| (identity.clone(), gateway));
        startup.preflight = Some(preflight);
        let Some((identity, gateway)) = observed else {
            return startup.fail(LifecycleError::ControlPreflight);
        };
        if !identity.is_qualified_write_target() {
            return startup.fail(LifecycleError::UnsupportedTarget { actual: identity });
        }
        if !matches!(gateway, DvGatewayMode::Off | DvGatewayMode::Terminal) {
            return startup.fail(LifecycleError::UnsupportedGateway { actual: gateway });
        }
        let modem_connection = match modem.open(cancelled).await {
            Ok(connection) => connection,
            Err(error) => return startup.fail(LifecycleError::ModemOpen(error)),
        };
        let (modem_connection, plan) = match self
            .program(
                control,
                modem_connection,
                &identity,
                gateway,
                journal,
                cancelled,
                &mut startup,
            )
            .await
        {
            Ok(prepared) => prepared,
            Err((connection, error)) => {
                startup.cleanup_error = modem.retire(connection).await.err();
                return startup.fail(error);
            }
        };
        startup.recovery = TerminalRecovery {
            control_endpoint: self.control_endpoint.clone(),
            baud: self.baud,
            owed: None,
            state: RestorationState::NotRequired,
        };
        if plan.replacements().iter().any(|page| !page.is_noop()) {
            match plan.restoration() {
                Ok(reverse) => {
                    startup.recovery.owed = Some(OwedRestoration {
                        identity,
                        plan: reverse,
                    });
                    startup.recovery.state = RestorationState::Owed;
                }
                Err(error) => {
                    startup.cleanup_error = modem.retire(modem_connection).await.err();
                    return startup.fail(LifecycleError::RestorationPlan(error));
                }
            }
        }
        let transition = acquire_modem(modem, Some(modem_connection), cancelled).await;
        startup.transition_attempts = transition.attempts;
        startup.cleanup_error = startup.cleanup_error.or(transition.cleanup_error);
        match transition.proof {
            Some(proof) => {
                startup.proof = Some(proof);
                startup
            }
            None => startup.fail(LifecycleError::Transition(
                transition.error.unwrap_or(TransitionError::WindowExpired),
            )),
        }
    }

    /// Write the Terminal pages, over the modem link when Gateway is Off or the
    /// control endpoint when a Terminal route is already active.
    #[expect(
        clippy::too_many_arguments,
        reason = "one programming pass threads both hosts, the observed target, the sink, \
                  cancellation and the startup report; a bundle would only relocate the wiring"
    )]
    async fn program<C, MC, J>(
        &self,
        control: &mut C,
        modem_connection: MC,
        identity: &Identity,
        gateway: DvGatewayMode,
        journal: &mut J,
        cancelled: &AtomicBool,
        startup: &mut TerminalStartup<MC>,
    ) -> Result<(MC, TerminalPlan), (MC, LifecycleError)>
    where
        C: ControlHost,
        MC: kenwood_transport::Transport,
        J: TerminalJournal,
    {
        let request = TerminalRequest::Enter { route: self.route };
        if gateway == DvGatewayMode::Off {
            // The stored route is not the modem link yet, so CAT and MCP run on
            // the modem link itself.
            let mut radio = Radio::new(modem_connection);
            let result =
                program_terminal(&mut radio, identity, gateway, request, journal, cancelled).await;
            let connection = radio.into_transport();
            match result {
                Ok(report) => {
                    let plan = report.plan.clone();
                    startup.entry = Some(report);
                    Ok((connection, plan))
                }
                Err(error) => Err((connection, LifecycleError::Program(error))),
            }
        } else {
            // A Terminal route is active; the modem link may already carry
            // MMDVM, so program over the independent control endpoint and leave
            // the modem link untouched.
            control.stage(ControlStage::ActiveEntry);
            let control_connection = match control.open(&self.control_endpoint, self.baud) {
                Ok(connection) => connection,
                Err(_error) => return Err((modem_connection, LifecycleError::ControlOpen)),
            };
            let mut radio = Radio::new(control_connection);
            let result =
                program_terminal(&mut radio, identity, gateway, request, journal, cancelled).await;
            let closed = control.close(radio.into_transport()).await;
            match (result, closed) {
                (Ok(report), Ok(())) => {
                    let plan = report.plan.clone();
                    startup.entry = Some(report);
                    Ok((modem_connection, plan))
                }
                (Ok(report), Err(error)) => {
                    startup.entry = Some(report);
                    Err((modem_connection, LifecycleError::ControlClose(error)))
                }
                (Err(error), _) => Err((modem_connection, LifecycleError::Program(error))),
            }
        }
    }
}

/// The result of one [`TerminalLifecycle::prepare`] call.
///
/// `proof` is present only on success; `recovery` is always present so the
/// caller can restore after a failure too. The sub-reports and attempt records
/// let the caller serialize the run.
#[derive(Debug)]
pub struct TerminalStartup<T> {
    /// The proved modem connection, present only on success.
    pub proof: Option<ProvenModem<T>>,
    /// The reverse-plan owner; check [`TerminalRecovery::state`] for what is
    /// owed.
    pub recovery: TerminalRecovery,
    /// First failure, when startup did not complete.
    pub error: Option<LifecycleError>,
    /// The control-endpoint preflight observation.
    pub preflight: Option<ObservationReport>,
    /// The Terminal programming pass, when it ran.
    pub entry: Option<TerminalProgramReport>,
    /// Every MMDVM probe and reopen attempt.
    pub transition_attempts: Vec<TransitionAttempt>,
    /// A connection the startup held that was not confirmed closed.
    pub cleanup_error: Option<CloseFailure>,
}

impl<T> TerminalStartup<T> {
    const fn new() -> Self {
        Self {
            proof: None,
            recovery: TerminalRecovery::none(),
            error: None,
            preflight: None,
            entry: None,
            transition_attempts: Vec::new(),
            cleanup_error: None,
        }
    }

    fn fail(mut self, error: LifecycleError) -> Self {
        if self.recovery.state == RestorationState::Owed && self.cleanup_error.is_some() {
            self.recovery.state = RestorationState::Blocked;
        }
        self.error = Some(error);
        self
    }

    /// Whether the modem was proved and no cleanup failure was recorded.
    #[must_use]
    pub const fn succeeded(&self) -> bool {
        self.proof.is_some() && self.error.is_none()
    }
}

/// Owns the reverse plan and rewrites exactly the pages startup changed.
///
/// It outlives the modem session, so restoration is still possible after the
/// runtime fails. [`Self::finish`] restores over the control endpoint.
#[derive(Debug, Clone)]
pub struct TerminalRecovery {
    control_endpoint: SerialCandidate,
    baud: u32,
    owed: Option<OwedRestoration>,
    state: RestorationState,
}

/// The identity and reverse plan owed once startup changed a page.
#[derive(Debug, Clone)]
struct OwedRestoration {
    identity: Identity,
    plan: TerminalPlan,
}

impl TerminalRecovery {
    const fn none() -> Self {
        Self {
            control_endpoint: SerialCandidate {
                path: String::new(),
                vid: None,
                pid: None,
            },
            baud: 0,
            owed: None,
            state: RestorationState::NotRequired,
        }
    }

    /// A recovery that owes nothing, for a startup path that changed no page.
    ///
    /// [`Self::finish`] returns [`RestorationState::Verified`] without any
    /// traffic. `control_endpoint` and `baud` are retained only so a report can
    /// name them.
    #[must_use]
    pub const fn nothing_owed(control_endpoint: SerialCandidate, baud: u32) -> Self {
        Self {
            control_endpoint,
            baud,
            owed: None,
            state: RestorationState::NotRequired,
        }
    }

    /// What the original Gateway settings still owe.
    #[must_use]
    pub const fn state(&self) -> RestorationState {
        self.state
    }

    /// The reverse plan, present once startup changed a page.
    #[must_use]
    pub fn restoration(&self) -> Option<&TerminalPlan> {
        self.owed.as_ref().map(|owed| &owed.plan)
    }

    /// Rewrite and verify the original Gateway settings over the control
    /// endpoint.
    ///
    /// When nothing was owed the report is [`RestorationState::Verified`]
    /// without any traffic, whatever `released` is. Otherwise `released` must be
    /// true (the modem connection was closed and dropped); when it is false the
    /// reverse write is skipped and the report is [`RestorationState::Blocked`].
    /// A restoration waits for the control endpoint's CAT to return, rewrites
    /// the reverse plan, and verifies the identity and Gateway on a fresh
    /// connection.
    pub async fn finish<C, J>(
        &mut self,
        control: &mut C,
        journal: &mut J,
        released: bool,
        cancelled: &AtomicBool,
    ) -> RestorationReport
    where
        C: ControlHost,
        J: TerminalJournal,
    {
        let mut report = RestorationReport::new();
        let Some(owed) = self.owed.clone() else {
            self.state = RestorationState::Verified;
            report.state = self.state;
            return report;
        };
        if !released {
            self.state = RestorationState::Blocked;
            report.state = self.state;
            report.error = Some(RestorationError::ModemNotReleased);
            return report;
        }
        self.run_restore(
            control,
            journal,
            &owed.identity,
            &owed.plan,
            cancelled,
            &mut report,
        )
        .await;
        report
    }

    async fn run_restore<C, J>(
        &mut self,
        control: &mut C,
        journal: &mut J,
        identity: &Identity,
        plan: &TerminalPlan,
        cancelled: &AtomicBool,
        report: &mut RestorationReport,
    ) where
        C: ControlHost,
        J: TerminalJournal,
    {
        control.stage(ControlStage::ReadinessBeforeRestore);
        let before = verify_readiness(
            control,
            &self.control_endpoint,
            self.baud,
            identity,
            cancelled,
        )
        .await;
        let ready = before.succeeded();
        report.readiness_before = Some(before);
        if !ready {
            report.state = RestorationState::Owed;
            report.error = Some(RestorationError::ControlNotReady);
            self.state = RestorationState::Owed;
            return;
        }
        control.stage(ControlStage::Restore);
        let control_connection = match control.open(&self.control_endpoint, self.baud) {
            Ok(connection) => connection,
            Err(_error) => {
                report.state = RestorationState::Owed;
                report.error = Some(RestorationError::ControlOpen);
                self.state = RestorationState::Owed;
                return;
            }
        };
        let mut radio = Radio::new(control_connection);
        let result = program_terminal(
            &mut radio,
            identity,
            DvGatewayMode::Terminal,
            TerminalRequest::Restore { plan },
            journal,
            cancelled,
        )
        .await;
        let closed = control.close(radio.into_transport()).await;
        report.restore = Some(match &result {
            Ok(_report) => RestoreOutcome::Written,
            Err(_error) => RestoreOutcome::Failed,
        });
        if result.is_err() || closed.is_err() {
            report.state = RestorationState::Owed;
            report.error = Some(if result.is_err() {
                RestorationError::RestoreWrite
            } else {
                RestorationError::ControlClose
            });
            self.state = RestorationState::Owed;
            return;
        }
        self.verify_restored(control, identity, plan, cancelled, report)
            .await;
    }

    async fn verify_restored<C: ControlHost>(
        &mut self,
        control: &mut C,
        identity: &Identity,
        plan: &TerminalPlan,
        cancelled: &AtomicBool,
        report: &mut RestorationReport,
    ) {
        control.stage(ControlStage::ReadinessAfterRestore);
        let after = verify_readiness(
            control,
            &self.control_endpoint,
            self.baud,
            identity,
            cancelled,
        )
        .await;
        let ready = after.succeeded();
        report.readiness_after = Some(after);
        if !ready {
            report.state = RestorationState::Owed;
            report.error = Some(RestorationError::ControlNotReady);
            self.state = RestorationState::Owed;
            return;
        }
        let expected_gateway = match plan.target() {
            TerminalTarget::Off => DvGatewayMode::Off,
            TerminalTarget::ReflectorTerminal => DvGatewayMode::Terminal,
        };
        control.stage(ControlStage::Verification);
        let verification = observe_control(
            control,
            &self.control_endpoint,
            self.baud,
            Expectation {
                identity: Some(identity),
                gateway: Some(expected_gateway),
            },
            cancelled,
        )
        .await;
        let verified = verification.succeeded();
        report.verification = Some(verification);
        if verified {
            self.state = RestorationState::Verified;
        } else {
            self.state = RestorationState::Owed;
            report.error = Some(RestorationError::GatewayNotVerified);
        }
        report.state = self.state;
    }
}

/// Which restoration step failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RestorationError {
    /// The modem connection was not confirmed closed, so no traffic was sent.
    #[error("the modem connection was not confirmed closed; no restoration was sent")]
    ModemNotReleased,
    /// The control endpoint did not regain CAT readiness.
    #[error("the control endpoint did not regain CAT readiness")]
    ControlNotReady,
    /// The control endpoint could not be opened for restoration.
    #[error("the control endpoint could not be opened for restoration")]
    ControlOpen,
    /// The reverse plan could not be written.
    #[error("the reverse plan could not be written")]
    RestoreWrite,
    /// The control endpoint was not confirmed closed after restoration.
    #[error("the control endpoint was not confirmed closed after restoration")]
    ControlClose,
    /// The restored Gateway state did not verify on a fresh connection.
    #[error("the restored Gateway state did not verify on a fresh connection")]
    GatewayNotVerified,
}

/// Whether the reverse plan reached the radio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreOutcome {
    /// The reverse plan was written and read back.
    Written,
    /// The reverse plan write failed; the journal holds any recorded intent.
    Failed,
}

/// The record of one [`TerminalRecovery::finish`] call.
#[derive(Debug)]
pub struct RestorationReport {
    /// Final restoration state.
    pub state: RestorationState,
    /// First failure, when restoration did not verify.
    pub error: Option<RestorationError>,
    /// Readiness before the reverse write.
    pub readiness_before: Option<ReadinessReport>,
    /// Whether the reverse plan reached the radio.
    pub restore: Option<RestoreOutcome>,
    /// Readiness after the reverse write.
    pub readiness_after: Option<ReadinessReport>,
    /// Identity and Gateway verification after restoration.
    pub verification: Option<ObservationReport>,
}

impl RestorationReport {
    const fn new() -> Self {
        Self {
            state: RestorationState::NotRequired,
            error: None,
            readiness_before: None,
            restore: None,
            readiness_after: None,
            verification: None,
        }
    }

    /// Whether restoration verified, or nothing was owed.
    #[must_use]
    pub const fn verified(&self) -> bool {
        matches!(self.state, RestorationState::Verified)
    }
}

#[cfg(test)]
mod tests;
