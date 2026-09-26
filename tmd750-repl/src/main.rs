//! Accessible TM-D750 CAT, Bluetooth D-STAR startup, and captured diagnostics.
//!
//! Output is plain text, with no cursor-oriented interface. Commands can be
//! entered interactively or supplied once on the command line for scripting.

mod capture;
mod cat;
mod connection;
mod dstar;
mod hosts;
mod logging;
mod mcp;
mod native;
mod output;
mod terminal;

use std::error::Error as StdError;
use std::fmt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;
use kenwood_tmd750::transport::{DEFAULT_BAUD, SerialCandidate, discover_serial, open_serial};
use kenwood_tmd750::{Band, DvGatewayMode, Identity, OperatingMode, Radio, SelectableMode};
use kenwood_transport::Transport;
use kenwood_transport::bluetooth::BluetoothAddress;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use connection::Connection;
use logging::LogLevel;

const PROMPT: &str = "tmd750> ";
const HELP_LINES: &[&str] = &[
    "help: Show this command list.",
    "identity | id: Read model, firmware, and radio type.",
    "status: Read identity, both band modes, and DV Gateway state.",
    "mode [a|b]: Read one band's mode; Band A is the default.",
    "mode [a|b] fm|dv|am|nfm: Select a mode; verify echo and readback.",
    "dv [a|b]: Select ordinary RF D-STAR DV, not Terminal Mode.",
    "fm [a|b] | normal [a|b]: Select FM and verify readback.",
    "gateway: Read the persistent DV Gateway state; no change.",
    "terminal: Explain manual Terminal Mode setup; no change.",
    "quit | exit: Close the radio connection and exit.",
];

type AppResult<T> = Result<T, Box<dyn StdError + Send + Sync>>;
type ConnectedRadio = (Radio<Connection>, Identity, String);

/// Accessible TM-D750 CAT, automatic Bluetooth D-STAR, and captured diagnostics.
#[derive(Debug, Parser)]
#[command(version, about, color = clap::ColorChoice::Never)]
#[command(group(clap::ArgGroup::new("native_selection")
    .args(["bluetooth", "bluetooth_address"]).multiple(true)))]
struct Cli {
    /// Use this serial endpoint instead of automatic USB/Bluetooth selection.
    #[arg(long, conflicts_with = "native_selection")]
    port: Option<String>,

    /// Use the paired Bluetooth radio even when USB is connected (macOS).
    #[arg(long)]
    bluetooth: bool,

    /// Use this exact paired Bluetooth address instead of automatic selection.
    #[arg(long, value_name = "ADDRESS")]
    bluetooth_address: Option<BluetoothAddress>,

    /// Native Bluetooth helper to run instead of the built-in one.
    #[arg(long, requires = "native_selection", value_name = "EXECUTABLE")]
    bluetooth_helper: Option<PathBuf>,

    /// USB CAT endpoint used by automatic Bluetooth dstar start.
    #[arg(long, conflicts_with = "port", value_name = "PATH")]
    control_port: Option<String>,

    /// CAT baud rate.
    #[arg(long, default_value_t = DEFAULT_BAUD, conflicts_with = "native_selection")]
    baud: u32,

    /// Prepend UTC timestamps to terminal output.
    #[arg(short, long)]
    timestamps: bool,

    /// Enable an opt-in trace-level session log.
    #[arg(long)]
    trace: bool,

    /// Enable an opt-in session log at the selected level.
    #[arg(long, value_enum, default_value_t = LogLevel::Off)]
    log_level: LogLevel,

    /// Startup command; runs once, except dstar start opens a gateway session.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

impl Cli {
    const fn requests_bluetooth(&self) -> bool {
        self.bluetooth || self.bluetooth_address.is_some()
    }

    fn bluetooth_request(&self) -> native::discovery::Request {
        native::discovery::Request {
            address: self.bluetooth_address.clone(),
            helper: self.bluetooth_helper.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Command {
    Help,
    Identity,
    Status,
    Mode {
        band: Band,
        set_to: Option<SelectableMode>,
    },
    Dv(Band),
    DstarStart(dstar::StartRequest),
    Fm(Band),
    Gateway,
    Terminal,
    Read(cat::Read),
    Write(cat::Write),
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CommandError(String);

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl StdError for CommandError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoopAction {
    Continue,
    Quit,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let result = run().await.and_then(|()| {
        output::check().map_err(|error| -> Box<dyn StdError + Send + Sync> { Box::new(error) })
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            output::error(format_args!("Error: {error}"));
            ExitCode::FAILURE
        }
    }
}

async fn run() -> AppResult<()> {
    let cli = Cli::parse();
    output::set_timestamps(cli.timestamps);
    let (_logging_guard, log_path) = logging::init(cli.log_level, cli.trace);
    if let Some(path) = log_path {
        output::line(format_args!("Trace log: {}.", path.display()));
    }
    if cli.baud == 0 {
        return Err(Box::new(CommandError(
            "--baud must be greater than zero".to_owned(),
        )));
    }

    // The diagnostic parsers keep the original argument boundaries. Every other
    // command joins its words and goes through one parser and one transport
    // dispatch, including a command line given as a single quoted argument.
    if !dstar::probe::matches_arguments(&cli.command)
        && cli.command.first().is_none_or(|word| word != "mcp")
    {
        let command = parse_command(&cli.command.join(" "))?;
        return run_command(&cli, command).await;
    }
    require_no_control_port(&cli)?;

    if dstar::probe::matches_arguments(&cli.command) {
        let request = match dstar::probe::parse(&cli.command) {
            Ok(request) => request,
            Err(error) if error.exit_code() == 0 => {
                output::line(format_args!("{error}"));
                return Ok(());
            }
            Err(error) => return Err(Box::new(error)),
        };
        if cli.requests_bluetooth() {
            return Err(CommandError(
                "dstar probe requires USB; native Bluetooth diagnostics use identity, status, or gateway"
                    .to_owned(),
            )
            .into());
        }
        let path = request.validate(cli.port.as_deref(), cli.baud)?;
        let endpoint = dstar::probe::select_endpoint(path, discover_serial()?)?;
        return dstar::probe::run(&endpoint, &request).await;
    }
    if cli.requests_bluetooth() {
        return run_native(&cli, None).await;
    }

    let request = match mcp::parse(&cli.command) {
        Ok(request) => request,
        Err(error) if error.exit_code() == 0 => {
            output::line(format_args!("{error}"));
            return Ok(());
        }
        Err(error) => return Err(Box::new(error)),
    };
    if let Some(result) = mcp::run_offline(&request) {
        return result;
    }
    request.validate_endpoint_selection(cli.port.is_some())?;
    let endpoint = select_probe_endpoint(cli.port.as_deref(), discover_serial()?)?;
    mcp::run(&endpoint, cli.baud, &request).await
}

fn require_no_control_port(cli: &Cli) -> Result<(), CommandError> {
    if cli.control_port.is_some() {
        Err(CommandError(
            "--control-port is only used by automatic Bluetooth dstar start".to_owned(),
        ))
    } else {
        Ok(())
    }
}

/// Dispatch the parsed command once, before selecting or opening its transport.
async fn run_command(cli: &Cli, one_shot: Option<Command>) -> AppResult<()> {
    let one_shot = match one_shot {
        Some(Command::DstarStart(request)) => return run_dstar_start(cli, request).await,
        other => other,
    };
    require_no_control_port(cli)?;
    if let Some(command) = one_shot.as_ref() {
        match command {
            Command::Help => {
                print_help();
                return Ok(());
            }
            Command::Terminal => {
                print_terminal_information();
                return Ok(());
            }
            Command::Quit => return Ok(()),
            Command::Identity
            | Command::Status
            | Command::Mode { .. }
            | Command::Dv(_)
            | Command::Fm(_)
            | Command::Gateway
            | Command::Read(_)
            | Command::Write(_) => {}
            Command::DstarStart(_) => unreachable!("D-STAR startup was dispatched above"),
        }
    }

    let selection = select_connection(cli, discover_serial)?;
    let ConnectionSelection::Serial(path) = selection else {
        return run_native(cli, one_shot).await;
    };
    let (radio, identity, path) = connect_serial(cli, path).await?;
    output::line(format_args!(
        "Connected to {} firmware {} on {} (type {}).",
        identity.model, identity.firmware, path, identity.radio_type
    ));
    run_connected(radio, one_shot).await
}

async fn run_dstar_start(cli: &Cli, request: dstar::StartRequest) -> AppResult<()> {
    let outcome = if let Some(path) = &cli.port {
        Box::pin(dstar::run(path, cli.baud, request)).await
    } else {
        if cli.baud != DEFAULT_BAUD {
            return Err(CommandError("custom --baud requires an explicit USB --port; Bluetooth has no serial baud selection".to_owned()).into());
        }
        Box::pin(dstar::run_bluetooth(
            cli.bluetooth_request(),
            cli.control_port.as_deref(),
            request,
        ))
        .await
    };
    outcome.map_err(|error| -> Box<dyn StdError + Send + Sync> { Box::new(CommandError(error)) })
}

/// Run one command over the native Bluetooth transport.
///
/// A command this transport does not support is rejected before any capture
/// file is created and before the endpoint is opened.
async fn run_native(cli: &Cli, command: Option<Command>) -> AppResult<()> {
    use native::workflow::{CatScope, Operation};

    if cli.command.first().is_some_and(|word| word == "mcp") {
        let request = match mcp::parse(&cli.command) {
            Ok(request) => request,
            Err(error) if error.exit_code() == 0 => {
                output::line(format_args!("{error}"));
                return Ok(());
            }
            Err(error) => return Err(Box::new(error)),
        };
        if let Some(result) = mcp::run_offline(&request) {
            return result;
        }
        let (operation, output_path) = match &request {
            mcp::McpCommand::Probe(request) => (Operation::FixedMcp, request.output()),
            mcp::McpCommand::Backup(request) => {
                (Operation::ConfigurationBackup, request.output())
            }
            _ => return Err(Box::new(CommandError(
                "native Bluetooth supports mcp probe and backup; live MCP settings writes and trials require USB"
                    .to_owned(),
            ))),
        };
        let endpoint = resolve_native(cli).await?;
        return native::workflow::run(&endpoint, &operation, output_path).await;
    }
    let operation = match command {
        Some(Command::Identity) => Operation::Cat(CatScope::Identity),
        Some(Command::Status) => Operation::Cat(CatScope::Status),
        Some(Command::Gateway) => Operation::Cat(CatScope::Gateway),
        Some(Command::Help) => {
            print_help();
            return Ok(());
        }
        Some(Command::Terminal) => {
            print_terminal_information();
            return Ok(());
        }
        Some(Command::Quit) => return Ok(()),
        Some(
            command @ (Command::Mode { .. }
            | Command::Dv(_)
            | Command::Fm(_)
            | Command::Read(_)
            | Command::Write(_)),
        ) => {
            let endpoint = resolve_native(cli).await?;
            return native::repl::run(&endpoint, Some(command)).await;
        }
        None if cli.command.is_empty() => {
            let endpoint = resolve_native(cli).await?;
            return native::repl::run(&endpoint, None).await;
        }
        Some(Command::DstarStart(_)) | None => {
            return Err(Box::new(CommandError(
                "this diagnostic is not supported over native Bluetooth; dstar probe requires USB"
                    .to_owned(),
            )));
        }
    };
    let endpoint = resolve_native(cli).await?;
    native::workflow::run(&endpoint, &operation, None).await
}

/// Resolve the native endpoint, opening no radio.
///
/// The discovery helper is joined before this returns, including after Ctrl-C.
async fn resolve_native(cli: &Cli) -> AppResult<native::Endpoint> {
    let cancelled = AtomicBool::new(false);
    let request = cli.bluetooth_request();
    let (selected, signal_error) = mcp::finish_on_interrupt(
        native::discovery::resolve(&request, &cancelled),
        tokio::signal::ctrl_c(),
        &cancelled,
    )
    .await;
    if let Some(error) = signal_error {
        let detail = selected.err().map_or_else(
            || "no radio connection was opened".to_owned(),
            |error| error.to_string(),
        );
        return Err(CommandError(format!(
            "Bluetooth selection signal listener failed: {error}; selection: {detail}"
        ))
        .into());
    }
    selected
}

async fn connect_serial(cli: &Cli, path: String) -> AppResult<ConnectedRadio> {
    if cli.port.is_some() {
        return match probe_path(&path, cli.baud).await {
            Ok((radio, identity)) => Ok((radio, identity, path)),
            Err(detail) => Err(Box::new(CommandError(format!(
                "could not identify a TM-D750 at {path}: {detail}"
            )))),
        };
    }
    match probe_path(&path, cli.baud).await {
        Ok((radio, identity)) => Ok((radio, identity, path)),
        Err(detail) => Err(Box::new(CommandError(format!(
            "the TM-D750 USB endpoint did not answer identity: {path}: {detail}"
        )))),
    }
}

/// Which transport automatic selection chose: an enumerated serial path, or
/// the paired Bluetooth radio.
///
/// Once chosen, a failed open is not retried on the other transport.
#[derive(Debug, PartialEq, Eq)]
enum ConnectionSelection {
    Serial(String),
    Bluetooth,
}

fn select_connection(
    cli: &Cli,
    discover: impl FnOnce() -> Result<Vec<SerialCandidate>, kenwood_transport::TransportError>,
) -> AppResult<ConnectionSelection> {
    if let Some(path) = &cli.port {
        return Ok(ConnectionSelection::Serial(path.clone()));
    }
    if cli.requests_bluetooth() {
        return Ok(ConnectionSelection::Bluetooth);
    }
    let candidates =
        deduplicate_serial_aliases(discover()?.into_iter().filter(SerialCandidate::is_tmd750));
    if candidates.is_empty() {
        if cli.baud != DEFAULT_BAUD {
            return Err(CommandError(
                "no TM-D750 USB endpoint found; custom --baud cannot be used with Bluetooth"
                    .to_owned(),
            )
            .into());
        }
        return Ok(ConnectionSelection::Bluetooth);
    }
    Ok(ConnectionSelection::Serial(
        unique_candidate(&candidates)?.path.clone(),
    ))
}

fn select_probe_endpoint(
    requested: Option<&str>,
    candidates: Vec<SerialCandidate>,
) -> Result<SerialCandidate, CommandError> {
    let known = candidates.into_iter().filter(SerialCandidate::is_tmd750);
    if let Some(path) = requested {
        return known
            .into_iter()
            .find(|candidate| candidate.path == path)
            .ok_or_else(|| {
                CommandError(format!(
                    "MCP operations require an enumerated TM-D750 USB endpoint; {path} is not recognized"
                ))
            });
    }
    let unique = deduplicate_serial_aliases(known);
    unique_candidate(&unique).cloned()
}

fn deduplicate_serial_aliases(
    candidates: impl IntoIterator<Item = SerialCandidate>,
) -> Vec<SerialCandidate> {
    let mut unique: Vec<SerialCandidate> = Vec::new();
    for candidate in candidates {
        if let Some(existing) = unique
            .iter_mut()
            .find(|existing| are_serial_aliases(existing, &candidate))
        {
            if is_macos_callout(&candidate.path) && !is_macos_callout(&existing.path) {
                *existing = candidate;
            }
        } else {
            unique.push(candidate);
        }
    }
    unique
}

fn are_serial_aliases(first: &SerialCandidate, second: &SerialCandidate) -> bool {
    if first.path == second.path {
        return true;
    }
    first.vid == second.vid
        && first.pid == second.pid
        && macos_serial_service(&first.path)
            .zip(macos_serial_service(&second.path))
            .is_some_and(|(first_service, second_service)| first_service == second_service)
}

fn macos_serial_service(path: &str) -> Option<&str> {
    path.strip_prefix("/dev/cu.")
        .or_else(|| path.strip_prefix("/dev/tty."))
}

fn is_macos_callout(path: &str) -> bool {
    path.starts_with("/dev/cu.")
}

fn unique_candidate(candidates: &[SerialCandidate]) -> Result<&SerialCandidate, CommandError> {
    match candidates {
        [] => Err(CommandError(
            "no TM-D750 USB serial endpoint found; connect the main-unit USB port or pass --port"
                .to_owned(),
        )),
        [candidate] => Ok(candidate),
        multiple => Err(CommandError(format!(
            "multiple TM-D750 USB endpoints found ({}); select one explicitly with --port",
            multiple
                .iter()
                .map(|candidate| candidate.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

async fn probe_path(path: &str, baud: u32) -> Result<(Radio<Connection>, Identity), String> {
    let transport = open_serial(path, baud).map_err(|error| error.to_string())?;
    let mut radio = Radio::new(Connection::Serial(transport));
    match radio.identify().await {
        Ok(identity) => Ok((radio, identity)),
        Err(error) => {
            let mut transport = radio.into_transport();
            let detail = error.to_string();
            match transport.close().await {
                Ok(()) => Err(detail),
                Err(close_error) => Err(format!(
                    "{detail}; closing the failed probe also failed: {close_error}"
                )),
            }
        }
    }
}

async fn run_connected(mut radio: Radio<Connection>, one_shot: Option<Command>) -> AppResult<()> {
    let session_result: AppResult<()> = if let Some(command) = one_shot {
        execute_command(&mut radio, command, CommandPolicy::Direct)
            .await
            .map(|_action| ())
    } else {
        interactive(&mut radio).await
    };

    let mut transport = radio.into_transport();
    let close_result = transport.close().await;
    session_result?;
    close_result?;
    Ok(())
}

async fn interactive<T: Transport>(radio: &mut Radio<T>) -> AppResult<()> {
    print_help();
    let mut editor = DefaultEditor::new()?;
    loop {
        let line = match editor.readline(PROMPT) {
            Ok(line) => line,
            Err(ReadlineError::Interrupted) => {
                output::line(format_args!("Interrupted; closing the radio connection."));
                return Ok(());
            }
            Err(ReadlineError::Eof) => {
                output::line(format_args!("End of input; closing the radio connection."));
                return Ok(());
            }
            Err(error) => return Err(Box::new(error)),
        };
        if !line.trim().is_empty()
            && let Err(error) = editor.add_history_entry(line.as_str())
        {
            output::error(format_args!(
                "Warning: history entry was not recorded: {error}"
            ));
        }
        let command = match parse_command(&line) {
            Ok(Some(command)) => command,
            Ok(None) => continue,
            Err(error) => {
                output::error(format_args!("Command error: {error}"));
                continue;
            }
        };
        match execute_command(radio, command, CommandPolicy::Direct).await {
            Ok(LoopAction::Continue) => {}
            Ok(LoopAction::Quit) => return Ok(()),
            Err(error) => output::error(format_args!("Radio command failed: {error}")),
        }
    }
}

/// The checks applied at each command boundary.
///
/// `Direct` adds none, keeping the existing serial wire schedule. `GatewayOff`
/// fails the command once the shared flag is set, and reads `GW` again before
/// every mode write, requiring Off.
#[derive(Clone, Copy)]
enum CommandPolicy<'a> {
    Direct,
    GatewayOff(&'a AtomicBool),
}

impl CommandPolicy<'_> {
    fn boundary(self) -> AppResult<()> {
        output::check()?;
        if let Self::GatewayOff(cancelled) = self
            && cancelled.load(Ordering::Relaxed)
        {
            return Err(
                CommandError("CAT command cancelled at a command boundary".to_owned()).into(),
            );
        }
        Ok(())
    }

    async fn admit_mode_write(self, radio: &mut Radio<impl Transport>) -> AppResult<()> {
        self.boundary()?;
        if matches!(self, Self::GatewayOff(_)) {
            let gateway = radio.get_dv_gateway_mode().await?;
            if gateway != DvGatewayMode::Off {
                return Err(CommandError(format!(
                    "native CAT mode writes require Gateway Off; observed {gateway}"
                ))
                .into());
            }
            self.boundary()?;
        }
        Ok(())
    }
}

/// Execute the same typed command vocabulary on either transport.
///
/// Cancellation is checked between complete library operations. In particular,
/// a started mode write finishes its echo and readback before the close.
async fn execute_command<T: Transport>(
    radio: &mut Radio<T>,
    command: Command,
    policy: CommandPolicy<'_>,
) -> AppResult<LoopAction> {
    policy.boundary()?;
    match command {
        Command::Help => print_help(),
        Command::Identity => {
            let identity = radio.identify().await?;
            print_identity(&identity);
        }
        Command::Status => {
            let identity = radio.identify().await?;
            policy.boundary()?;
            let band_a = radio.get_operating_mode(Band::A).await?;
            policy.boundary()?;
            let band_b = radio.get_operating_mode(Band::B).await?;
            policy.boundary()?;
            let gateway = radio.get_dv_gateway_mode().await?;
            print_identity(&identity);
            output::line(format_args!("Band A mode: {band_a}."));
            output::line(format_args!("Band B mode: {band_b}."));
            output::line(format_args!("DV Gateway state: {gateway}."));
        }
        Command::Mode { band, set_to: None } => {
            let mode = radio.get_operating_mode(band).await?;
            output::line(format_args!("Band {band} mode: {mode}."));
        }
        Command::Mode {
            band,
            set_to: Some(mode),
        } => {
            policy.admit_mode_write(radio).await?;
            radio.set_operating_mode(band, mode).await?;
            let selected = OperatingMode::from(mode);
            output::line(format_args!(
                "Band {band} mode: {selected} (write and readback verified)."
            ));
        }
        Command::Dv(band) => {
            policy.admit_mode_write(radio).await?;
            radio.enter_dstar(band).await?;
            output::line(format_args!(
                "Band {band} D-STAR DV mode: active (write and readback verified). Terminal Mode was not changed."
            ));
        }
        Command::DstarStart(_) => {
            output::error(format_args!(
                "Command error: dstar start consumes the radio connection; close this prompt and supply it as the startup command."
            ));
        }
        Command::Fm(band) => {
            policy.admit_mode_write(radio).await?;
            radio.set_operating_mode(band, SelectableMode::Fm).await?;
            output::line(format_args!(
                "Band {band} mode: FM (write and readback verified)."
            ));
        }
        Command::Gateway => {
            let gateway = radio.get_dv_gateway_mode().await?;
            output::line(format_args!("DV Gateway state: {gateway} (read only)."));
        }
        Command::Terminal => print_terminal_information(),
        Command::Read(read) => cat::execute_read(radio, read).await?,
        Command::Write(write) => {
            policy.admit_mode_write(radio).await?;
            cat::execute_write(radio, write).await?;
        }
        Command::Quit => return Ok(LoopAction::Quit),
    }
    Ok(LoopAction::Continue)
}

fn print_identity(identity: &Identity) {
    output::line(format_args!(
        "Radio: {}; firmware: {}; type: {}.",
        identity.model, identity.firmware, identity.radio_type
    ));
}

fn print_help() {
    output::line(format_args!("CAT commands (interactive or startup):"));
    for line in HELP_LINES.iter().chain(cat::HELP_LINES) {
        output::line(format_args!("{line}"));
    }
    output::line(format_args!("Startup only: dstar start CALL [REFLECTOR]"));
    output::line(format_args!(
        "Defaults to automatic Bluetooth Terminal startup, restoring over a separate USB endpoint. Explicit --port retains manual USB modem startup."
    ));
    output::line(format_args!(
        "Startup only: dstar probe --help (captured diagnostics; no modem setup)."
    ));
    output::line(format_args!(
        "Press Ctrl-C for the D-STAR prompt, then dstar stop to close it."
    ));
    output::line(format_args!(
        "Startup only: mcp probe [--output NEW_DIRECTORY], mcp reentry-probe --help, mcp backup, mcp menu --help, mcp text --help, mcp terminal --help, mcp pm1-trial --help, mcp my1-trial --help, mcp terminal-exit-trial --help"
    ));
    output::line(format_args!(
        "Probe/backup capture MCP reads and verify exit; text list/show/preview and terminal preflight inspect local backups."
    ));
    output::line(format_args!(
        "Text set changes PM1's name, PM-Off MY1 or one channel's name (--channel), with an explicit port, current backup, expected text, and --apply."
    ));
    output::line(format_args!(
        "PM1 trial performs one fixed PM1 rename and restores the original name; no other field is writable."
    ));
    output::line(format_args!(
        "Automatic Bluetooth dstar stop rewrites the Terminal and routing pages it changed, over USB; the separate Terminal exit trial is not invoked."
    ));
}

fn print_terminal_information() {
    output::line(format_args!(
        "{}",
        terminal::instructions(terminal::UsbConnection::Unknown)
    ));
}

fn parse_command(line: &str) -> Result<Option<Command>, CommandError> {
    let typed: Vec<&str> = line.split_ascii_whitespace().collect();
    if let Some(parsed) = cat::parse(&typed) {
        return match parsed.map_err(|error| CommandError(error.0))? {
            cat::Parsed::Read(read) => Ok(Some(Command::Read(read))),
            cat::Parsed::Write(write) => Ok(Some(Command::Write(write))),
        };
    }
    let lowercase: Vec<String> = typed.iter().map(|word| word.to_ascii_lowercase()).collect();
    let words: Vec<&str> = lowercase.iter().map(String::as_str).collect();
    let command = match words.as_slice() {
        [] => return Ok(None),
        ["help" | "?"] => Command::Help,
        ["identity" | "id"] => Command::Identity,
        ["status"] => Command::Status,
        ["mode"] => Command::Mode {
            band: Band::A,
            set_to: None,
        },
        ["mode", value] => match parse_band(value) {
            Ok(band) => Command::Mode { band, set_to: None },
            Err(_) => Command::Mode {
                band: Band::A,
                set_to: Some(parse_selectable_mode(value)?),
            },
        },
        ["mode", band, mode] => Command::Mode {
            band: parse_band(band)?,
            set_to: Some(parse_selectable_mode(mode)?),
        },
        ["dstar" | "d-star", "start", arguments @ ..] => {
            Command::DstarStart(dstar::StartRequest::parse(arguments).map_err(CommandError)?)
        }
        ["dstar" | "d-star", "probe", ..] => {
            return Err(CommandError(
                "dstar probe is startup-only; quit this prompt and run tmd750-repl dstar probe --help"
                    .to_owned(),
            ));
        }
        ["dstar" | "d-star", "terminal"] | ["terminal"] => Command::Terminal,
        ["dv"] => Command::Dv(Band::A),
        ["dv", band] => Command::Dv(parse_band(band)?),
        ["dstar" | "d-star", ..] => {
            return Err(CommandError(
                "dstar requires: dstar start <callsign> [reflector]; use dv [a|b] for ordinary RF DV mode"
                    .to_owned(),
            ));
        }
        ["fm" | "normal"] => Command::Fm(Band::A),
        ["fm" | "normal", band] => Command::Fm(parse_band(band)?),
        ["gateway"] => Command::Gateway,
        ["mcp", ..] => {
            return Err(CommandError(
                "MCP commands are startup-only; live operations require dedicated connections. Quit this prompt and run tmd750-repl mcp --help."
                    .to_owned(),
            ));
        }
        ["quit" | "exit"] => Command::Quit,
        [name, ..] => {
            return Err(CommandError(format!(
                "unknown or malformed command {name:?}; enter help for syntax"
            )));
        }
    };
    Ok(Some(command))
}

fn parse_band(word: &str) -> Result<Band, CommandError> {
    match word {
        "a" => Ok(Band::A),
        "b" => Ok(Band::B),
        _ => Err(CommandError(format!("band must be a or b, not {word:?}"))),
    }
}

fn parse_selectable_mode(word: &str) -> Result<SelectableMode, CommandError> {
    match word {
        "fm" => Ok(SelectableMode::Fm),
        "dv" | "dstar" | "d-star" => Ok(SelectableMode::Dv),
        "am" => Ok(SelectableMode::Am),
        "nfm" | "narrow" => Ok(SelectableMode::Nfm),
        "dr" => Err(CommandError(
            "DR is a tuning mode, not an MD value: use tuning [a|b] dr".to_owned(),
        )),
        _ => Err(CommandError(format!(
            "selectable mode must be fm, dv, am or nfm, not {word:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kenwood_transport::MockTransport;

    type TestResult = Result<(), Box<dyn StdError + Send + Sync>>;

    #[test]
    fn blank_line_is_ignored() {
        assert_eq!(parse_command("  \t "), Ok(None));
    }

    #[test]
    fn command_help_fits_on_one_line_with_a_timestamp() {
        for line in HELP_LINES.iter().chain(cat::HELP_LINES) {
            assert!(line.chars().count() + "[00:00:00] ".len() <= 80, "{line}");
        }
    }

    #[test]
    fn serial_auto_discovery_refuses_ambiguity() {
        let main = SerialCandidate {
            path: "/dev/cu.main".to_owned(),
            vid: None,
            pid: None,
        };
        let panel = SerialCandidate {
            path: "/dev/cu.panel".to_owned(),
            vid: None,
            pid: None,
        };
        assert!(unique_candidate(&[]).is_err());
        assert_eq!(unique_candidate(std::slice::from_ref(&main)), Ok(&main));
        let candidates = [main, panel];
        let ambiguous = unique_candidate(&candidates);
        assert!(
            matches!(ambiguous, Err(CommandError(ref message)) if message.contains("--port")),
            "ambiguous discovery must require an explicit endpoint: {ambiguous:?}"
        );
    }

    #[test]
    fn mcp_probe_requires_a_known_usb_identity_even_for_explicit_paths() -> TestResult {
        let known = SerialCandidate {
            path: "/dev/cu.main".to_owned(),
            vid: Some(0x2166),
            pid: Some(0x9030),
        };
        let unrelated = SerialCandidate {
            path: "/dev/cu.unrelated".to_owned(),
            vid: Some(0x2166),
            pid: Some(0x9023),
        };
        let candidates = vec![known.clone(), unrelated];
        assert_eq!(select_probe_endpoint(None, candidates.clone())?, known);
        assert_eq!(
            select_probe_endpoint(Some("/dev/cu.main"), candidates.clone())?,
            known
        );
        for path in ["/dev/cu.unrelated", "/dev/cu.not_enumerated"] {
            assert!(
                select_probe_endpoint(Some(path), candidates.clone()).is_err(),
                "explicit MCP endpoints must also have a known USB identity"
            );
        }
        assert!(
            select_probe_endpoint(
                None,
                vec![
                    known,
                    SerialCandidate {
                        path: "/dev/cu.panel".to_owned(),
                        vid: Some(0x2166),
                        pid: Some(0x9032),
                    }
                ]
            )
            .is_err(),
            "MCP probes must not guess between main and panel endpoints"
        );
        Ok(())
    }

    #[test]
    fn mcp_probe_is_rejected_inside_the_cat_prompt() {
        assert!(
            matches!(parse_command("mcp probe"), Err(CommandError(message)) if message.contains("dedicated connection")),
            "interactive MCP probes require an explicit startup-only diagnostic"
        );
    }

    #[test]
    fn macos_callout_and_dialin_paths_are_one_endpoint() {
        let aliases = [
            SerialCandidate {
                path: "/dev/tty.usbmodem101".to_owned(),
                vid: Some(0x2166),
                pid: Some(0x9030),
            },
            SerialCandidate {
                path: "/dev/cu.usbmodem101".to_owned(),
                vid: Some(0x2166),
                pid: Some(0x9030),
            },
        ];
        let deduplicated = deduplicate_serial_aliases(aliases);
        assert_eq!(deduplicated.len(), 1);
        assert_eq!(
            deduplicated
                .first()
                .map(|candidate| candidate.path.as_str()),
            Some("/dev/cu.usbmodem101")
        );

        let distinct = deduplicate_serial_aliases([
            SerialCandidate {
                path: "/dev/cu.usbmodem101".to_owned(),
                vid: Some(0x2166),
                pid: Some(0x9030),
            },
            SerialCandidate {
                path: "/dev/cu.usbmodem201".to_owned(),
                vid: Some(0x2166),
                pid: Some(0x9030),
            },
        ]);
        assert_eq!(distinct.len(), 2);
    }

    #[test]
    fn read_commands_and_aliases_parse() {
        assert_eq!(parse_command("ID"), Ok(Some(Command::Identity)));
        assert_eq!(parse_command("status"), Ok(Some(Command::Status)));
        assert_eq!(
            parse_command("mode"),
            Ok(Some(Command::Mode {
                band: Band::A,
                set_to: None,
            }))
        );
        assert_eq!(
            parse_command("mode b"),
            Ok(Some(Command::Mode {
                band: Band::B,
                set_to: None,
            }))
        );
        assert_eq!(parse_command("gateway"), Ok(Some(Command::Gateway)));
    }

    #[test]
    fn qualified_mode_writes_parse() {
        assert_eq!(
            parse_command("mode b dv"),
            Ok(Some(Command::Mode {
                band: Band::B,
                set_to: Some(SelectableMode::Dv),
            }))
        );
        assert_eq!(
            parse_command("mode fm"),
            Ok(Some(Command::Mode {
                band: Band::A,
                set_to: Some(SelectableMode::Fm),
            }))
        );
        assert_eq!(parse_command("dv"), Ok(Some(Command::Dv(Band::A))));
        assert_eq!(parse_command("DV b"), Ok(Some(Command::Dv(Band::B))));
        assert_eq!(parse_command("normal b"), Ok(Some(Command::Fm(Band::B))));
    }

    #[test]
    fn favorite_dstar_start_command_parses() -> Result<(), CommandError> {
        let request = dstar::StartRequest::parse(&["KQ4NIT", "REF030C"]).map_err(CommandError)?;
        assert_eq!(
            parse_command("dstar start KQ4NIT REF030C"),
            Ok(Some(Command::DstarStart(request)))
        );
        assert!(parse_command("dstar").is_err());
        assert!(parse_command("dstar b").is_err());
        Ok(())
    }

    #[test]
    fn favorite_startup_flags_are_distinct_from_gateway_arguments() -> TestResult {
        let cli = Cli::try_parse_from([
            "tmd750-repl",
            "--trace",
            "--timestamps",
            "dstar",
            "start",
            "KQ4NIT",
            "REF030C",
        ])?;
        assert!(cli.trace);
        assert!(cli.timestamps);
        assert_eq!(cli.command, ["dstar", "start", "KQ4NIT", "REF030C"]);
        assert_eq!(cli.baud, DEFAULT_BAUD);
        assert!(cli.port.is_none());
        assert!(!cli.bluetooth);
        assert!(cli.bluetooth_address.is_none());
        assert!(cli.control_port.is_none());
        assert!(matches!(
            parse_command(&cli.command.join(" "))?,
            Some(Command::DstarStart(_))
        ));
        Ok(())
    }

    #[test]
    fn native_endpoint_preserves_typed_address_and_helper_path() -> TestResult {
        let cli = Cli::try_parse_from([
            "tmd750-repl",
            "--bluetooth-address",
            "01:23:45:67:89:AB",
            "--bluetooth-helper",
            "/Applications/Radio Tools/Native Helper",
            "status",
        ])?;
        assert_eq!(
            cli.bluetooth_address.as_ref().map(BluetoothAddress::as_str),
            Some("01-23-45-67-89-AB")
        );
        assert_eq!(
            cli.bluetooth_helper,
            Some(PathBuf::from("/Applications/Radio Tools/Native Helper"))
        );
        assert_eq!(cli.command, ["status"]);
        assert_eq!(cli.baud, DEFAULT_BAUD);
        Ok(())
    }

    #[test]
    fn native_endpoint_rejects_serial_options_and_orphaned_helper() {
        for arguments in [
            vec!["--bluetooth-helper", "/not/a/helper"],
            vec!["--bluetooth-address", "not-an-address"],
            vec!["--bluetooth", "--port", "/not/a/port"],
            vec!["--bluetooth", "--baud", "9600"],
            vec![
                "--bluetooth-address",
                "01:23:45:67:89:AB",
                "--port",
                "/not/a/port",
            ],
            vec!["--bluetooth-address", "01:23:45:67:89:AB", "--baud", "9600"],
        ] {
            assert!(Cli::try_parse_from(std::iter::once("tmd750-repl").chain(arguments)).is_err());
        }
    }

    #[test]
    fn bluetooth_flag_needs_no_address_and_never_consumes_the_command() -> TestResult {
        for command in [
            vec![],
            vec!["status"],
            vec!["mode", "b"],
            vec!["mcp", "backup"],
        ] {
            let cli = Cli::try_parse_from(
                [
                    "tmd750-repl",
                    "--bluetooth",
                    "--bluetooth-helper",
                    "/absolute/helper",
                ]
                .into_iter()
                .chain(command.iter().copied()),
            )?;
            assert!(cli.requests_bluetooth(), "flag forces Bluetooth selection");
            assert!(cli.bluetooth_address.is_none(), "no address is required");
            assert_eq!(cli.command, command, "startup command remains intact");
            assert_eq!(
                cli.bluetooth_request().helper,
                Some(PathBuf::from("/absolute/helper")),
                "a helper override does not require an address"
            );
        }
        Ok(())
    }

    #[test]
    fn ordinary_auto_selection_prefers_usb_then_paired_bluetooth() -> TestResult {
        let cli = Cli::try_parse_from(["tmd750-repl"])?;
        let usb = SerialCandidate {
            path: "/dev/cu.radio".to_owned(),
            vid: Some(0x2166),
            pid: Some(0x9032),
        };
        assert_eq!(
            select_connection(&cli, || Ok(vec![usb.clone()]))?,
            ConnectionSelection::Serial(usb.path.clone()),
            "USB remains the first automatic choice"
        );
        assert_eq!(
            select_connection(&cli, || Ok(vec![]))?,
            ConnectionSelection::Bluetooth,
            "a paired radio can be selected without any CLI address"
        );
        let other = SerialCandidate {
            path: "/dev/cu.other".to_owned(),
            vid: None,
            pid: None,
        };
        assert_eq!(
            select_connection(&cli, || Ok(vec![other]))?,
            ConnectionSelection::Bluetooth,
            "unrelated serial devices cannot suppress Bluetooth discovery"
        );
        let second = SerialCandidate {
            path: "/dev/cu.second".to_owned(),
            ..usb
        };
        let error = select_connection(&cli, || Ok(vec![usb, second]));
        assert!(
            error.is_err(),
            "ambiguous USB selection must not choose another radio"
        );
        Ok(())
    }

    #[test]
    fn explicit_transport_selection_does_not_enumerate_other_devices() -> TestResult {
        for arguments in [
            vec!["--bluetooth"],
            vec!["--bluetooth-address", "01:23:45:67:89:AB"],
            vec!["--port", "/dev/cu.chosen"],
        ] {
            let cli = Cli::try_parse_from(std::iter::once("tmd750-repl").chain(arguments))?;
            let enumerated = std::cell::Cell::new(false);
            let selected = select_connection(&cli, || {
                enumerated.set(true);
                Ok(vec![])
            })?;
            assert!(!enumerated.get(), "explicit choices bypass USB discovery");
            assert_eq!(
                selected,
                cli.port
                    .map_or(ConnectionSelection::Bluetooth, ConnectionSelection::Serial),
                "the selected transport matches the explicit override"
            );
        }
        Ok(())
    }

    #[test]
    fn custom_serial_baud_cannot_silently_turn_into_bluetooth() -> TestResult {
        let cli = Cli::try_parse_from(["tmd750-repl", "--baud", "19200"])?;
        let error = select_connection(&cli, || Ok(vec![]));
        assert!(
            error.is_err_and(|error| error.to_string().contains("--baud")),
            "serial-only settings must not be ignored during fallback"
        );
        Ok(())
    }

    #[test]
    fn terminal_routes_are_informational() {
        assert_eq!(parse_command("terminal"), Ok(Some(Command::Terminal)));
        assert_eq!(parse_command("dstar terminal"), Ok(Some(Command::Terminal)));
        let guidance = terminal::instructions(terminal::UsbConnection::Unknown);
        assert!(
            guidance.contains("disabled")
                && guidance.contains(&format!(
                    "schema label is {}",
                    kenwood_tmd750::memory::MCP_D750_SCHEMA_FIRMWARE
                ))
                && guidance.contains("this build writes no")
                && guidance.contains("not a vendor version limit"),
            "terminal guidance must state that no Terminal or routing setting is written, without inventing a live identity or vendor version restriction"
        );
    }

    #[test]
    fn unqualified_writes_are_rejected() {
        let dr = parse_command("mode a dr");
        assert!(
            matches!(dr, Err(CommandError(ref message)) if message.contains("tuning [a|b] dr")),
            "DR is selected as a tuning mode, never as an MD write: {dr:?}"
        );
        assert_eq!(
            parse_command("mode b nfm"),
            Ok(Some(Command::Mode {
                band: Band::B,
                set_to: Some(SelectableMode::Nfm),
            }))
        );
        assert_eq!(
            parse_command("tuning b dr"),
            Ok(Some(Command::Write(cat::Write::Tuning(
                Band::B,
                kenwood_tmd750::types::TuningMode::DStarRepeater
            ))))
        );
        assert_eq!(
            parse_command("freq"),
            Ok(Some(Command::Read(cat::Read::Frequency(Band::A))))
        );
        assert!(
            parse_command("raw MD 0,1").is_err(),
            "raw CAT input must not be accepted"
        );
        assert!(
            parse_command("mode c dv").is_err(),
            "only Band A and Band B may be selected"
        );
    }

    #[tokio::test]
    async fn dv_command_uses_a_verified_mode_write() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TM-D750\r");
        mock.expect(b"FV\r", b"FV 1.02\r");
        mock.expect(b"TY\r", b"TY K,2,1\r");
        mock.expect(b"MD 1,1\r", b"MD 1,1\r");
        mock.expect(b"MD 1\r", b"MD 1,1\r");
        let mut radio = Radio::new(mock);

        let action =
            execute_command(&mut radio, Command::Dv(Band::B), CommandPolicy::Direct).await?;

        assert_eq!(action, LoopAction::Continue);
        radio.into_transport().assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn fm_command_uses_one_verified_mode_write() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TM-D750\r");
        mock.expect(b"FV\r", b"FV 1.02\r");
        mock.expect(b"TY\r", b"TY K,2,1\r");
        mock.expect(b"MD 0,0\r", b"MD 0,0\r");
        mock.expect(b"MD 0\r", b"MD 0,0\r");
        let mut radio = Radio::new(mock);

        let action =
            execute_command(&mut radio, Command::Fm(Band::A), CommandPolicy::Direct).await?;

        assert_eq!(action, LoopAction::Continue);
        radio.into_transport().assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn gateway_command_is_read_only() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"GW\r", b"GW 0\r");
        let mut radio = Radio::new(mock);

        let action = execute_command(&mut radio, Command::Gateway, CommandPolicy::Direct).await?;

        assert_eq!(action, LoopAction::Continue);
        radio.into_transport().assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn terminal_command_performs_no_radio_io() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::new(mock);

        let action = execute_command(&mut radio, Command::Terminal, CommandPolicy::Direct).await?;

        assert_eq!(action, LoopAction::Continue);
        radio.into_transport().assert_complete();
        Ok(())
    }
}
