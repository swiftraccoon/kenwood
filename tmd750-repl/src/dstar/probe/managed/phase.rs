//! One phase: compare and write pages, exit MCP, wait for the endpoint to
//! re-enumerate, then reread the pages over a fresh CAT connection.

use std::fs::File;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::radio::terminal::{TerminalPlan, TerminalTarget};
use kenwood_tmd750::transport::{DEFAULT_BAUD, SerialCandidate};
use kenwood_tmd750::{DvGatewayMode, Radio};
use kenwood_transport::Transport;

use crate::capture::{CaptureTransport, Event, Recorder};
use crate::mcp::reconnect::{Backend, endpoint_is_unambiguous, verify_readiness};
use crate::{AppResult, CommandError};

use super::super::{Borrowed, Limits, select_endpoint};
use super::evidence::{ConnectionResult, Exit, FailureStage, PhaseResult, Problem};
use super::{JournalEvent, Phase, record_journal};

pub(super) struct Captures {
    pub(super) exchange: Recorder<File>,
    pub(super) readiness: Recorder<File>,
    pub(super) verification: Recorder<File>,
}

#[derive(Clone, Copy)]
enum Operation<'a> {
    Compare(&'a TerminalPlan),
    Verify(&'a TerminalPlan),
}

impl<'a> Operation<'a> {
    const fn plan(self) -> &'a TerminalPlan {
        match self {
            Self::Compare(plan) | Self::Verify(plan) => plan,
        }
    }

    const fn expected_gateway(self) -> DvGatewayMode {
        match self {
            Self::Compare(plan) => plan.before(),
            Self::Verify(plan) => match plan.target() {
                TerminalTarget::Off => DvGatewayMode::Off,
                TerminalTarget::ReflectorTerminal => DvGatewayMode::Terminal,
            },
        }
    }
}

pub(super) fn check_cancellation(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "cancelled before write intent",
        ))
    } else {
        Ok(())
    }
}

struct Context<'a> {
    endpoint: &'a SerialCandidate,
    operation: Operation<'a>,
    phase: Phase,
    cancelled: &'a AtomicBool,
    limits: Limits,
}

/// Check identity and Gateway, then run the planned page exchange.
///
/// Cancellation is checked only up to the first journal record; once one page
/// write has been recorded, the exchange and its `E` exit run to completion.
async fn operate(
    radio: &mut Radio<impl Transport>,
    context: &Context<'_>,
    raw: &File,
    journal: &mut Recorder<File>,
    result: &mut ConnectionResult,
) -> AppResult<()> {
    check_cancellation(context.cancelled)?;
    let identity = radio.identify().await?;
    result.identity = Some((&identity).into());
    if &identity != context.operation.plan().identity() {
        return Err(
            CommandError("control identity differs from the completed backup".to_owned()).into(),
        );
    }
    check_cancellation(context.cancelled)?;
    let gateway = radio.get_dv_gateway_mode().await?;
    result.gateway = Some(gateway.into());
    if gateway != context.operation.expected_gateway() {
        return Err(CommandError(
            "fresh control Gateway differs from the planned state".to_owned(),
        )
        .into());
    }
    let Operation::Compare(plan) = context.operation else {
        return Ok(());
    };
    check_cancellation(context.cancelled)?;
    result.exit = Exit::Uncertain;
    let mut session = radio.enter_mcp().await?;
    let outcome = session
        .compare_exchange_terminal(
            plan,
            |page| {
                if !result.intent_recorded {
                    check_cancellation(context.cancelled)?;
                }
                if let Err(error) = raw.sync_all() {
                    result
                        .problems
                        .push(Problem::new(FailureStage::Capture, &error));
                    return Err(error);
                }
                record_journal(
                    journal,
                    JournalEvent::BeforeWrite {
                        phase: context.phase,
                        page: page.into(),
                    },
                )?;
                result.intent_recorded = true;
                Ok(())
            },
            |_progress| {},
        )
        .await;
    result.possible = session
        .journal()
        .possibly_written
        .iter()
        .copied()
        .map(Into::into)
        .collect();
    result.verified = session
        .journal()
        .verified
        .iter()
        .copied()
        .map(Into::into)
        .collect();
    if session.is_ready() {
        match session.exit().await {
            Ok(()) => result.exit = Exit::Acknowledged,
            Err(error) => result
                .problems
                .push(Problem::new(FailureStage::Exit, &error)),
        }
    }
    result.compared = outcome?
        .compared_pages
        .into_iter()
        .map(Into::into)
        .collect();
    Ok(())
}

/// Open the endpoint, run `operate` on it, then close it once.
///
/// Open, protocol, capture and close failures are each appended to the
/// returned result's problems; the close is attempted in every path.
async fn connection(
    backend: &mut impl Backend,
    context: Context<'_>,
    mut recorder: Recorder<File>,
    journal: &mut Recorder<File>,
) -> ConnectionResult {
    let mut result = ConnectionResult::new(recorder.summary());
    let opened = prepare_open(backend, &context, &mut recorder);
    match opened {
        Err(error) => result
            .problems
            .push(Problem::new(FailureStage::Admission, error.as_ref())),
        Ok((connection, raw)) => {
            let mut transport = CaptureTransport::required(connection, recorder);
            match transport.synchronize() {
                Ok(()) => {
                    let mut radio = Radio::new(Borrowed(&mut transport));
                    radio.set_timeout(context.limits.cat_io_step);
                    if let Err(error) =
                        operate(&mut radio, &context, &raw, journal, &mut result).await
                    {
                        result
                            .problems
                            .push(Problem::new(FailureStage::Operation, error.as_ref()));
                    }
                }
                Err(error) => result
                    .problems
                    .push(Problem::new(FailureStage::Capture, &error)),
            }
            match tokio::time::timeout(context.limits.close, transport.close()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => result
                    .problems
                    .push(Problem::new(FailureStage::Close, &error)),
                Err(error) => result
                    .problems
                    .push(Problem::new(FailureStage::Close, &error)),
            }
            recorder = transport.into_recorder();
        }
    }
    if let Err(error) = recorder.synchronize() {
        result
            .problems
            .push(Problem::new(FailureStage::Capture, &error));
    }
    result.transcript = recorder.summary();
    result
}

fn prepare_open<B: Backend>(
    backend: &mut B,
    context: &Context<'_>,
    recorder: &mut Recorder<File>,
) -> AppResult<(B::Connection, File)> {
    check_cancellation(context.cancelled)?;
    let candidates = backend.enumerate()?;
    let selected = select_endpoint(&context.endpoint.path, candidates.clone())?;
    if selected != *context.endpoint || !endpoint_is_unambiguous(&selected, &candidates) {
        return Err(CommandError(
            "control endpoint metadata changed or became ambiguous".to_owned(),
        )
        .into());
    }
    let raw = recorder.synchronization_handle()?;
    recorder.record(Event::OpenRequested {
        path: &selected.path,
        baud: DEFAULT_BAUD,
    });
    recorder.synchronize()?;
    match backend.open(&selected, DEFAULT_BAUD) {
        Ok(connection) => {
            recorder.record(Event::OpenCompleted);
            Ok((connection, raw))
        }
        Err(error) => {
            recorder.record(Event::OpenFailed {
                error: crate::capture::Failure::from_error(&error),
            });
            Err(error.into())
        }
    }
}

pub(super) struct Request<'a> {
    pub(super) endpoint: &'a SerialCandidate,
    pub(super) plan: &'a TerminalPlan,
    pub(super) phase: Phase,
    pub(super) cancelled: &'a AtomicBool,
    pub(super) limits: Limits,
}

pub(super) async fn run(
    backend: &mut impl Backend,
    request: Request<'_>,
    captures: Captures,
    journal: &mut Recorder<File>,
) -> PhaseResult {
    let exchange = connection(
        backend,
        Context {
            endpoint: request.endpoint,
            operation: Operation::Compare(request.plan),
            phase: request.phase,
            cancelled: request.cancelled,
            limits: request.limits,
        },
        captures.exchange,
        journal,
    )
    .await;
    let mut result = PhaseResult {
        exchange,
        readiness: None,
        verification: None,
    };
    if let Err(error) = record_journal(
        journal,
        JournalEvent::ExchangeFinished {
            phase: request.phase,
            evidence: &result.exchange,
        },
    ) {
        result
            .exchange
            .problems
            .push(Problem::new(FailureStage::Journal, &error));
    }
    if !result.exchange.clean_release()
        || result.exchange.exit != Exit::Acknowledged
        || (!result.exchange.succeeded() && !result.exchange.intent_recorded)
    {
        return result;
    }
    // A journal record was written and synced before the first page write, so
    // the readback runs even under Ctrl-C: swap in a never-set cancellation
    // flag. A capture error still stops further traffic regardless.
    let finish_required = AtomicBool::new(false);
    let cancelled = if result.exchange.intent_recorded {
        &finish_required
    } else {
        request.cancelled
    };
    let readiness = verify_readiness(
        backend,
        request.endpoint,
        DEFAULT_BAUD,
        request.plan.identity(),
        captures.readiness,
        cancelled,
    )
    .await;
    let ready = readiness.succeeded();
    result.readiness = Some(readiness);
    if ready {
        result.verification = Some(
            connection(
                backend,
                Context {
                    endpoint: request.endpoint,
                    operation: Operation::Verify(request.plan),
                    phase: request.phase,
                    cancelled,
                    limits: request.limits,
                },
                captures.verification,
                journal,
            )
            .await,
        );
    }
    result
}
