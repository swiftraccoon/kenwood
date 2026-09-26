//! Captured CAT sessions over one native connection, with explicit input
//! shutdown.
//!
//! Opening uses the shared bounded retry policy. Once the session starts it
//! never retries a CAT command or reopens its transport. A failed command
//! closes the connection, and the command, input, capture, signal and close
//! failures are reported separately. MCP and D-STAR are separate workflows.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::{DvGatewayMode, Radio};
use kenwood_transport::Transport;
use kenwood_transport::bluetooth::BluetoothService;
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::input::{CommandInput, Event, InputMode, SystemInput};
use super::opening::{self, Captured, History};
use super::{Backend, Endpoint, Resolved, SystemBackend, cat};
use crate::capture::{
    Artifacts, CaptureKind, CaptureTransport, Failure, Recorder, TranscriptSummary,
};
use crate::{AppResult, Command, CommandError, CommandPolicy, LoopAction, output};

#[cfg(test)]
mod tests;

/// The command source: a prompt or script reader, or one startup command.
///
/// The startup variant never constructs or reads stdin.
enum Commands<I> {
    Prompt(I),
    Startup(Option<Command>),
}

enum Next {
    Command(Command),
    End(Termination),
}

impl<I: CommandInput> Commands<I> {
    fn mode(&self) -> Mode {
        match self {
            Self::Prompt(input) => match input.mode() {
                InputMode::Interactive => Mode::Interactive,
                InputMode::Batch => Mode::Batch,
            },
            Self::Startup(_) => Mode::Startup,
        }
    }

    async fn next(&mut self, cancelled: &AtomicBool) -> AppResult<Next> {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(Next::End(Termination::Interrupted));
        }
        let input = match self {
            Self::Prompt(input) => input,
            Self::Startup(command) => {
                return Ok(command
                    .take()
                    .map_or(Next::End(Termination::StartupComplete), Next::Command));
            }
        };
        loop {
            // Give the signal listener a turn before the next line: buffered
            // scripts and local-only commands would otherwise never yield.
            tokio::task::yield_now().await;
            if cancelled.load(Ordering::Relaxed) {
                return Ok(Next::End(Termination::Interrupted));
            }
            match input.next(cancelled).await? {
                Event::Line(line) => match parse_input_command(&line) {
                    Ok(Some(command)) => return Ok(Next::Command(command)),
                    Ok(None) => {}
                    Err(error) => {
                        if input.mode() == InputMode::Batch {
                            return Err(
                                CommandError(format!("batch command rejected: {error}")).into()
                            );
                        }
                        output::error(format_args!("Command error: {error}"));
                    }
                },
                Event::Eof => return Ok(Next::End(Termination::Eof)),
                Event::Interrupted => {
                    cancelled.store(true, Ordering::Relaxed);
                    return Ok(Next::End(Termination::Interrupted));
                }
            }
            if cancelled.load(Ordering::Relaxed) {
                return Ok(Next::End(Termination::Interrupted));
            }
        }
    }

    fn close(&mut self) -> Option<Failure> {
        match self {
            Self::Prompt(input) => input
                .close()
                .err()
                .map(|error| Failure::from_error(error.as_ref())),
            Self::Startup(_) => None,
        }
    }
}

fn parse_input_command(line: &str) -> Result<Option<Command>, CommandError> {
    match crate::parse_command(line)? {
        Some(Command::DstarStart(_)) => Err(CommandError(
            "dstar start is startup-only; exit this CAT session and supply it as the startup command"
                .to_owned(),
        )),
        command => Ok(command),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Termination {
    StartupComplete,
    Eof,
    Quit,
    Interrupted,
    Failed,
}

/// How commands reach this session: a terminal prompt, a redirected batch, or
/// one command supplied on the command line.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    Interactive,
    Batch,
    Startup,
}

impl Mode {
    fn completed(self, termination: Termination, cancelled: bool) -> bool {
        match self {
            Self::Interactive => match termination {
                Termination::Eof | Termination::Quit => !cancelled,
                Termination::Interrupted => true,
                Termination::StartupComplete | Termination::Failed => false,
            },
            Self::Batch => {
                matches!(termination, Termination::Eof | Termination::Quit) && !cancelled
            }
            Self::Startup => termination == Termination::StartupComplete && !cancelled,
        }
    }
}

#[derive(Debug, Serialize)]
struct SessionEvidence {
    identity: Option<cat::IdentityEvidence>,
    gateway_before: Option<u8>,
    commands_completed: usize,
    termination: Termination,
    operation_error: Option<Failure>,
    input_error: Option<Failure>,
    close_error: Option<Failure>,
    capture_error: Option<Failure>,
    transcript: TranscriptSummary,
}

impl SessionEvidence {
    const fn new(transcript: TranscriptSummary) -> Self {
        Self {
            identity: None,
            gateway_before: None,
            commands_completed: 0,
            termination: Termination::Failed,
            operation_error: None,
            input_error: None,
            close_error: None,
            capture_error: None,
            transcript,
        }
    }

    fn succeeded(&self) -> bool {
        self.identity.is_some()
            && self.gateway_before == Some(DvGatewayMode::Off.into())
            && self.termination != Termination::Failed
            && self.operation_error.is_none()
            && self.input_error.is_none()
            && self.close_error.is_none()
            && self.capture_error.is_none()
            && self.transcript.complete
    }
}

async fn admit(
    radio: &mut Radio<impl Transport>,
    cancelled: &AtomicBool,
    evidence: &mut SessionEvidence,
    check_output: &(impl Fn() -> AppResult<()> + Sync),
) -> AppResult<()> {
    check_output()?;
    let policy = CommandPolicy::GatewayOff(cancelled);
    policy.boundary()?;
    let identity = radio.identify().await?;
    crate::print_identity(&identity);
    evidence.identity = Some(cat::IdentityEvidence(identity));
    check_output()?;
    policy.boundary()?;
    let gateway = radio.get_dv_gateway_mode().await?;
    evidence.gateway_before = Some(gateway.into());
    if gateway != DvGatewayMode::Off {
        return Err(CommandError(format!(
            "native CAT sessions require Gateway Off; observed {gateway}. No mode change was attempted"
        ))
        .into());
    }
    policy.boundary()?;
    check_output()
}

async fn execute(
    radio: &mut Radio<impl Transport>,
    commands: &mut Commands<impl CommandInput>,
    cancelled: &AtomicBool,
    evidence: &mut SessionEvidence,
    check_output: &(impl Fn() -> AppResult<()> + Sync),
) -> AppResult<()> {
    admit(radio, cancelled, evidence, check_output).await?;
    if matches!(commands.mode(), Mode::Interactive) {
        crate::print_help();
    }
    loop {
        check_output()?;
        let next = match commands.next(cancelled).await {
            Ok(next) => next,
            Err(error) => {
                evidence.input_error = Some(Failure::from_error(error.as_ref()));
                return Ok(());
            }
        };
        let command = match next {
            Next::Command(command) => command,
            Next::End(termination) => {
                evidence.termination = termination;
                return Ok(());
            }
        };
        let action =
            crate::execute_command(radio, command, CommandPolicy::GatewayOff(cancelled)).await?;
        check_output()?;
        evidence.commands_completed += 1;
        if action == LoopAction::Quit {
            evidence.termination = Termination::Quit;
            return Ok(());
        }
    }
}

/// Run the command loop, closing the connection once in every outcome.
async fn session(
    transport: CaptureTransport<impl Transport, File>,
    commands: &mut Commands<impl CommandInput>,
    cancelled: &AtomicBool,
) -> SessionEvidence {
    session_with_output(transport, commands, cancelled, &|| {
        output::check().map_err(Into::into)
    })
    .await
}

async fn session_with_output(
    mut transport: CaptureTransport<impl Transport, File>,
    commands: &mut Commands<impl CommandInput>,
    cancelled: &AtomicBool,
    check_output: &(impl Fn() -> AppResult<()> + Sync),
) -> SessionEvidence {
    let mut evidence = SessionEvidence::new(transport.transcript_summary());
    let ready = transport.synchronize();
    let mut radio = cat::wrap(transport);
    match ready {
        Ok(()) => {
            if let Err(error) =
                execute(&mut radio, commands, cancelled, &mut evidence, check_output).await
            {
                evidence.operation_error = Some(Failure::from_error(error.as_ref()));
            }
        }
        Err(error) => evidence.capture_error = Some(Failure::from_error(&error)),
    }
    let mut transport = radio.into_transport();
    evidence.close_error = super::close(&mut transport).await;
    let mut recorder = transport.into_recorder();
    if let Err(error) = recorder.synchronize() {
        evidence.capture_error = Some(Failure::from_error(&error));
    }
    evidence.transcript = recorder.summary();
    evidence
}

#[derive(Debug, Serialize)]
struct Outcome {
    opening: History,
    endpoint: Option<Resolved>,
    session: Option<SessionEvidence>,
}

async fn run_selected(
    backend: &mut impl Backend,
    endpoint: &Endpoint,
    recorder: Recorder<File>,
    commands: &mut Commands<impl CommandInput>,
    cancelled: &AtomicBool,
) -> Outcome {
    let selected = opening::open_selected(
        backend,
        endpoint,
        BluetoothService::SerialPort,
        recorder,
        cancelled,
    )
    .await;
    let mut outcome = Outcome {
        opening: selected.history,
        endpoint: None,
        session: None,
    };
    if let Some(Captured {
        transport,
        resolved,
        channel: _,
    }) = selected.opened
    {
        output::line(format_args!(
            "Native Bluetooth: {}, RFCOMM channel {}.",
            resolved.address, resolved.rfcomm_channel
        ));
        outcome.endpoint = Some(resolved);
        outcome.session = Some(session(transport, commands, cancelled).await);
    }
    outcome
}

#[derive(Serialize)]
struct Report<'a> {
    format_version: u8,
    operation: &'static str,
    mode: Mode,
    transport: &'static str,
    requested_address: &'a str,
    helper_executable: Option<&'a Path>,
    started_at_utc: String,
    finished_at_utc: String,
    outcome: Outcome,
    input_close_error: Option<Failure>,
    output_error: Option<Failure>,
    signal_error: Option<Failure>,
    cancelled: bool,
}

impl Report<'_> {
    fn succeeded(&self) -> bool {
        self.outcome.opening.succeeded()
            && self.outcome.session.as_ref().is_some_and(|session| {
                session.succeeded() && self.mode.completed(session.termination, self.cancelled)
            })
            && self.input_close_error.is_none()
            && self.output_error.is_none()
            && self.signal_error.is_none()
    }

    fn publish(&self, file: &mut File) -> io::Result<()> {
        serde_json::to_writer_pretty(&mut *file, self)?;
        file.write_all(b"\n")?;
        file.flush()?;
        file.sync_all()
    }

    fn print_errors(&self) {
        for attempt in &self.outcome.opening.attempts {
            if let Some(error) = &attempt.error {
                output::error(format_args!(
                    "Native opening attempt {}: {error}",
                    attempt.number
                ));
            }
            if let Some(error) = &attempt.interruption {
                output::error(format_args!("Native opening interrupted: {error}"));
            }
        }
        let session_errors = self
            .outcome
            .session
            .as_ref()
            .into_iter()
            .flat_map(|session| {
                [
                    ("CAT operation", &session.operation_error),
                    ("Command input", &session.input_error),
                    ("Radio close", &session.close_error),
                    ("Session capture", &session.capture_error),
                ]
            });
        for (phase, error) in [
            ("Opening retry", &self.outcome.opening.retry_error),
            ("Opening capture", &self.outcome.opening.capture_error),
            ("Input close", &self.input_close_error),
            ("Command output", &self.output_error),
            ("Signal handler", &self.signal_error),
        ]
        .into_iter()
        .chain(session_errors)
        {
            if let Some(error) = error {
                output::error(format_args!("{phase}: {error}"));
            }
        }
    }
}

fn output_failure() -> Option<Failure> {
    output::check()
        .err()
        .as_ref()
        .map(|error| Failure::from_error(error.as_ref()))
}

/// Open a captured interactive shell or execute one typed CAT startup command.
pub(crate) async fn run(endpoint: &Endpoint, command: Option<Command>) -> AppResult<()> {
    if !cfg!(target_os = "macos") {
        return Err(CommandError(
            "native Bluetooth is currently available only on macOS".to_owned(),
        )
        .into());
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let artifacts = Artifacts::create(CaptureKind::NativeBluetooth, None, Arc::clone(&cancelled))?;
    File::open(&artifacts.directory)?.sync_all()?;
    File::open(
        artifacts
            .directory
            .parent()
            .unwrap_or_else(|| Path::new(".")),
    )?
    .sync_all()?;
    let started_at_utc = OffsetDateTime::now_utc().format(&Rfc3339)?;
    // Every fallible setup step runs before input is acquired and before the
    // native connection is opened.
    let mut commands = match command {
        Some(command) => Commands::Startup(Some(command)),
        None => Commands::Prompt(SystemInput::new()?),
    };
    let mode = commands.mode();
    let Artifacts {
        directory,
        report: mut report_file,
        transcript,
    } = artifacts;
    output::line(format_args!(
        "Native Bluetooth capture: {}. CAT failures close the session; commands are not retried.",
        directory.display()
    ));
    if output::check().is_err() {
        cancelled.store(true, Ordering::Relaxed);
    }
    let (outcome, signal_error) = crate::mcp::finish_on_interrupt(
        run_selected(
            &mut SystemBackend,
            endpoint,
            transcript,
            &mut commands,
            &cancelled,
        ),
        tokio::signal::ctrl_c(),
        &cancelled,
    )
    .await;
    let input_close_error = commands.close();
    let mut report = Report {
        format_version: 1,
        operation: "cat_session",
        mode,
        transport: "native_bluetooth",
        requested_address: endpoint.address.as_str(),
        helper_executable: endpoint.helper.as_deref(),
        started_at_utc,
        finished_at_utc: OffsetDateTime::now_utc().format(&Rfc3339)?,
        outcome,
        input_close_error,
        output_error: output_failure(),
        signal_error,
        cancelled: cancelled.load(Ordering::Relaxed),
    };
    report.print_errors();
    output::line(format_args!(
        "Native CAT report: {}.",
        directory.join("report.json").display()
    ));
    // Record output failures from the lines printed above before the report is
    // written and synchronized.
    report.output_error = output_failure();
    report.publish(&mut report_file).map_err(|error| {
        CommandError(format!(
            "native CAT report publication failed: {error}; retain {}",
            directory.display()
        ))
    })?;
    if report.succeeded() {
        Ok(())
    } else {
        Err(CommandError(format!(
            "native CAT session incomplete; retain {}",
            directory.display()
        ))
        .into())
    }
}
