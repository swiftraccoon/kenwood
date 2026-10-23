//! Accessible command-line REPL for the Kenwood TH-D75 transceiver.
//!
//! Designed for screen-reader compatibility: all output is plain text,
//! one self-contained line at a time. No box-drawing, no cursor
//! repositioning or terminal escape sequences. Dynamic text uses visible
//! escapes and bounded physical lines through the shared output path.
//!
//! Also works as a scripting interface: pipe commands via stdin.
//!
//! # Modes
//!
//! The REPL has three transport states:
//!
//! - **CAT mode** (default): Direct radio control via CAT commands.
//! - **APRS mode**: Entered via `aprs start`. The radio switches to KISS
//!   TNC mode and APRS events (stations heard, messages) are announced
//!   as they arrive. Exit with `aprs stop`.
//! - **MMDVM ready**: A proved binary connection has no CAT access.
//!   Kenwood D-STAR startup is rejected before connection discovery because
//!   its modem configuration layout is unverified.

// proptest is a dev-dependency used by the library's unit tests; the bin
// target's test build links dev-deps too. Acknowledge it so
// `unused_crate_dependencies` stays silent for this compilation unit.
#[cfg(test)]
use proptest as _;

use std::future::Future;
use std::io::{IsTerminal as _, Write as _};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicI32, Ordering};

use thd75_repl::aprintln;

use aprs_is::{IgateMessaging, LoginState};
use clap::{Parser, ValueEnum};
use dstar_gateway_core::{Module, RoutingCallsign};
use kenwood_thd75::LinkDiagnosis;
use kenwood_thd75::memory::{
    MCP_D75_SCHEMA_FIRMWARE, MCP_D75_SCHEMA_FIRMWARE_IDENTITIES, is_supported_mcp_d75_schema_target,
};
use kenwood_thd75::radio::programming::DetachedMcpPageUpdate;
use kenwood_thd75::transport::EitherTransport;
use kenwood_thd75::types::{DvGatewayMode, FirmwareIdentity, PcOutputInterface, RadioModel};
use kenwood_thd75::{
    AprsClient, AprsClientConfig, AprsEvent, AprsReportTimestamp, Ax25Address, DigipeaterAliases,
    DigipeaterConfig, DirectMessageAddressee, IGateRfLocality, IGateToRfConfig, MessageText,
    ObjectReport, PositionReport, ReportState, StatusText,
};
use kenwood_thd75::{DstarGateway, PersistentMmdvm};
use kenwood_thd75::{FirmwareProfile, Radio};
use mmdvm::dstar::DstarModemConfig;

mod commands;
#[cfg(test)]
mod process_tests;
mod transport;

static PROCESS_SIGNAL_EXIT_CODE: AtomicI32 = AtomicI32::new(0);

/// Physical interface represented by an otherwise ambiguous explicit port.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum PortInterface {
    /// USB CDC control port.
    Usb,
    /// Bluetooth SPP control port.
    Bluetooth,
}

impl From<PortInterface> for PcOutputInterface {
    fn from(interface: PortInterface) -> Self {
        match interface {
            PortInterface::Usb => Self::Usb,
            PortInterface::Bluetooth => Self::Bluetooth,
        }
    }
}

/// Log verbosity level for the opt-in file sink.
///
/// The default is [`Self::Off`]: no file is created and no tracing
/// output is written. File logging is enabled only when the user
/// explicitly passes `--log-level` or `--trace`; this prevents the
/// log files from accumulating on every normal session.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
enum LogLevel {
    /// No file logging (default).
    Off,
    /// Only errors.
    Error,
    /// Warnings and errors.
    Warn,
    /// Informational messages and state transitions.
    Info,
    /// Debug events (connect flow, keepalives, stream boundaries).
    Debug,
    /// Trace events (every packet, every slow-data frame).
    Trace,
}

impl LogLevel {
    /// Render the level as the string accepted by `EnvFilter::new`,
    /// or `None` for [`Self::Off`] (in which case no file sink is
    /// attached at all).
    const fn as_filter(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Error => Some("error"),
            Self::Warn => Some("warn"),
            Self::Info => Some("info"),
            Self::Debug => Some("debug"),
            Self::Trace => Some("trace"),
        }
    }
}

/// Subcommands that bypass the interactive REPL entirely.
///
/// `check` runs the sample output self-check and exits without opening a radio.
#[derive(clap::Subcommand, Debug)]
enum Subcommand {
    /// Run the accessibility output self-check and print a report.
    ///
    /// Evaluates eight line rules on named sample strings and identifies six
    /// rules requiring other tests. Zero means those evaluated samples passed.
    /// Opens no radio connection.
    Check,
}

/// Accessible REPL for the Kenwood TH-D75 transceiver.
///
/// Screen-reader friendly: plain text output, one line at a time.
/// Also scriptable: pipe commands via stdin.
#[derive(Parser, Debug)]
#[command(version, about, color = clap::ColorChoice::Never)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "clap derive maps each `bool` flag to a discrete CLI switch (`--timestamps`, \
              `--local-time`, `--quiet`, etc.). Grouping them into a bitflags \
              enum would break the derive macro's parser generation and the --help documentation \
              format users expect."
)]
struct Cli {
    /// Optional subcommand. Defaults to interactive REPL if omitted.
    #[command(subcommand)]
    command_mode: Option<Subcommand>,

    /// Serial port path (default: auto-discover USB, then Bluetooth).
    #[arg(short, long)]
    port: Option<String>,

    /// Physical interface used by `--port` when its path is ambiguous.
    ///
    /// Auto-discovered endpoints and recognizable USB/Bluetooth paths do not
    /// need this. Windows `COM` ports and custom symlinks must specify it
    /// before selecting a transport preset for that endpoint.
    #[arg(long, value_enum, requires = "port")]
    port_interface: Option<PortInterface>,

    /// Baud rate for serial connection.
    #[arg(short, long, default_value = "115200")]
    baud: NonZeroU32,

    /// Prepend timestamps to all output lines (e.g. `[14:32:07]`).
    #[arg(short, long)]
    timestamps: bool,

    /// Display timestamps in local time instead of UTC.
    ///
    /// The offset is detected at startup by running `date +%z`. If
    /// detection fails, a warning is printed and UTC is used. Implies
    /// `--timestamps`.
    #[arg(long)]
    local_time: bool,

    /// Override the UTC offset used for timestamps, e.g. `+05:30`,
    /// `-08:00`, or `+0530`. Implies `--timestamps`.
    #[arg(long, value_parser = parse_utc_offset)]
    utc_offset: Option<time::UtcOffset>,

    /// Enable file logging at the given level (default: no file).
    ///
    /// File location (one fresh file per session, suffix is the UTC
    /// session start time at second granularity):
    /// - macOS: `~/Library/Logs/thd75-repl/thd75-repl.log.<YYYY-MM-DD-HHMMSS>`
    /// - Linux: `~/.local/state/thd75-repl/thd75-repl.log.<YYYY-MM-DD-HHMMSS>`
    /// - Windows: `%LOCALAPPDATA%\thd75-repl\logs\thd75-repl.log.<YYYY-MM-DD-HHMMSS>`
    ///
    /// Logging reserves an exclusive new file; same-second collisions receive
    /// numbered suffixes. Old files accumulate until removed. The asynchronous
    /// sink is best effort and can discard records under sustained load.
    #[arg(long, value_enum, default_value_t = LogLevel::Off)]
    log_level: LogLevel,

    /// Shorthand for `--log-level=trace`. Creates the log file and
    /// selects the most detailed tracing filter.
    #[arg(long)]
    trace: bool,

    /// Read commands from a script file (use `-` for stdin) and exit.
    ///
    /// One command per line. `#` at line start is a comment. Blank
    /// lines are skipped. `exit`/`quit` ends the script. Errors from
    /// individual commands are printed and the script continues.
    #[arg(long)]
    script: Option<std::path::PathBuf>,

    /// Use a programmed mock radio instead of real hardware.
    ///
    /// Only available when compiled with the `testing` cargo feature.
    /// Used by integration tests to drive the REPL loop without a
    /// physical radio attached. Scenario names are defined in
    /// `thd75_repl::mock_scenarios::build`.
    #[cfg(feature = "testing")]
    #[arg(long)]
    mock_radio: Option<String>,

    /// Maximum number of output lines retained by the `last` history
    /// buffer (default: 30). Older lines are evicted first. Setting
    /// this to 0 disables history recording entirely.
    #[arg(long, default_value_t = thd75_repl::HISTORY_CAPACITY_DEFAULT)]
    history_lines: usize,

    /// Skip transmit confirmation prompts.
    ///
    /// By default every transmit command (`cq`, `beacon`, `position`,
    /// `msg`, and CAT routing requests) prompts before changing transmit intent. Pass
    /// `--yes` to disable the prompt globally; required when running
    /// a script in automation and also useful for interactive sessions
    /// where the operator does not want to be asked every time.
    #[arg(long)]
    yes: bool,

    /// If the radio is found in Reflector Terminal Mode, guide an exit
    /// (prompt for the Menu 650 change) and reconnect the retained connection.
    #[arg(long)]
    exit_terminal_mode: bool,

    /// Clear the DV Gateway (Menu 650) flag via a memory write, then
    /// exit. Works over whichever port keeps CAT alive: when the
    /// radio is in Reflector Terminal Mode bound to Bluetooth (Menu
    /// 985), connect over USB so the programming handshake is routed.
    /// The radio reboots into normal control; no menu keypresses are
    /// needed on the radio.
    #[arg(long)]
    set_gateway_off: bool,

    /// Command to run after startup (for example, "status").
    ///
    /// If provided, the command is executed immediately after connecting.
    /// The REPL continues normally after the command completes.
    #[arg(trailing_var_arg = true)]
    command: Vec<String>,
}

/// Determine the on-disk directory where per-session log files
/// should be written. Returns `None` if no suitable directory can
/// be derived from the environment (extremely rare: only if
/// `home_dir()` is unset on Unix or `data_local_dir()` is unset on
/// Windows).
///
/// Locations follow platform convention:
/// - macOS: `$HOME/Library/Logs/thd75-repl`
/// - Linux: `$XDG_STATE_HOME/thd75-repl` (falls back to
///   `$HOME/.local/state/thd75-repl`)
/// - Windows: `%LOCALAPPDATA%\thd75-repl\logs`
fn log_directory() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let mut path = dirs_next::home_dir()?;
        path.push("Library");
        path.push("Logs");
        path.push("thd75-repl");
        Some(path)
    }

    #[cfg(target_os = "windows")]
    {
        let mut path = dirs_next::data_local_dir()?;
        path.push("thd75-repl");
        path.push("logs");
        Some(path)
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        // Linux / BSD: prefer $XDG_STATE_HOME, else $HOME/.local/state.
        if let Some(xdg) = std::env::var_os("XDG_STATE_HOME") {
            let mut path = std::path::PathBuf::from(xdg);
            path.push("thd75-repl");
            return Some(path);
        }
        let mut path = dirs_next::home_dir()?;
        path.push(".local");
        path.push("state");
        path.push("thd75-repl");
        Some(path)
    }
}

/// Guard returned by [`init_logging`] that must be kept alive for the
/// whole process lifetime. Dropping it terminates the background
/// flush thread for the non-blocking file sink, which would cause
/// late log lines to be silently discarded, so `main` stores it in
/// a local variable whose scope spans the entire runtime.
struct LoggingGuard {
    /// Keeps the non-blocking file sink's flush thread alive until
    /// the program exits. The inner `WorkerGuard` is intentionally
    /// unused apart from its `Drop` impl.
    _file_worker: Option<tracing_appender::non_blocking::WorkerGuard>,
}

/// Configure the global tracing subscriber, returning a
/// [`LoggingGuard`] that must be kept alive for the full process
/// lifetime.
///
/// **Default behaviour: no logging at all.** Neither a file sink nor
/// a stderr sink is attached. The interactive REPL terminal only
/// shows shared message output and no log file accumulates
/// on disk. This keeps normal sessions cheap and quiet.
///
/// Opt-in logging is controlled by CLI flags:
/// - `--log-level=X` creates a fresh per-session file at level X
/// - `--trace` is shorthand for `--log-level=trace`
///
/// For power users who want live log output on stderr, `RUST_LOG` is
/// still honoured. Setting e.g. `RUST_LOG=dstar_gateway=debug` routes
/// matching events to stderr at the requested level. `RUST_LOG` does
/// NOT enable the file sink on its own: file logging is file-flag
/// controlled, stderr logging is env-var controlled, and the two are
/// independent.
fn init_logging(cli: &Cli) -> LoggingGuard {
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::fmt;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    use tracing_subscriber::{EnvFilter, Registry};

    // Resolve the effective file level. `--trace` overrides
    // `--log-level` (shorthand for the most verbose setting).
    let file_level = if cli.trace {
        LogLevel::Trace
    } else {
        cli.log_level
    };

    // stderr layer is opt-in via `RUST_LOG` only. Default is silent.
    let stderr_layer = std::env::var("RUST_LOG").ok().map(|spec| {
        fmt::layer()
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .with_filter(EnvFilter::new(spec))
    });

    // File layer is opt-in via `--log-level` / `--trace`. Default is
    // no file created at all. When enabled, a fresh per-session file
    // is written to the platform-appropriate log directory. The
    // Filename collisions use exclusive creation and numbered suffixes.
    let mut file_layer_opt = None;
    let mut worker_guard = None;
    let mut announced_path = None;
    if let Some(level_str) = file_level.as_filter() {
        if let Some(dir) = log_directory() {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                aprintln!(
                    "Warning: could not create log directory {}: {e}. \
                     File logging disabled.",
                    dir.display()
                );
            } else {
                let session_suffix = time::OffsetDateTime::now_utc()
                    .format(time::macros::format_description!(
                        "[year]-[month]-[day]-[hour][minute][second]"
                    ))
                    .unwrap_or_else(|_| "session".to_string());
                match reserve_log_file(&dir, &session_suffix) {
                    Ok((path, file)) => {
                        let (writer, guard) = tracing_appender::non_blocking(file);
                        let layer = fmt::layer()
                            .with_writer(writer)
                            .with_ansi(false)
                            .with_filter(EnvFilter::new(level_str));
                        file_layer_opt = Some(layer);
                        worker_guard = Some(guard);
                        announced_path = Some(path);
                    }
                    Err(e) => {
                        aprintln!(
                            "Warning: could not create log file {}: {e}. \
                             File logging disabled.",
                            dir.display()
                        );
                    }
                }
            }
        } else {
            aprintln!(
                "Warning: could not determine a log directory for this \
                 platform. File logging disabled."
            );
        }
    }

    Registry::default()
        .with(stderr_layer)
        .with(file_layer_opt)
        .init();

    if let Some(path) = announced_path {
        // Per-session file: a fresh log file was just created for
        // this invocation. Print the concrete filename so users can
        // copy-paste it into bug reports.
        aprintln!(
            "Logging at {} to {}.",
            file_level.as_filter().unwrap_or("off"),
            path.display(),
        );
    }

    LoggingGuard {
        _file_worker: worker_guard,
    }
}

fn reserve_log_file(
    directory: &std::path::Path,
    session_suffix: &str,
) -> std::io::Result<(std::path::PathBuf, std::fs::File)> {
    for collision in 0..1024 {
        let name = if collision == 0 {
            format!("thd75-repl.log.{session_suffix}")
        } else {
            format!("thd75-repl.log.{session_suffix}.{collision}")
        };
        let path = directory.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "all 1024 per-second session log names are in use",
    ))
}

/// Open a real hardware transport (USB serial or macOS Bluetooth)
/// and return it alongside a fresh tokio runtime.
///
/// Discovery runs with the runtime's reactor active so serial descriptor
/// registration has a current Tokio I/O driver. The runtime is returned with
/// the connection so the caller keeps that driver alive.
fn open_real_transport(
    cli_port: Option<&str>,
    cli_baud: NonZeroU32,
    port_interface: Option<PcOutputInterface>,
) -> AppResult<(transport::OpenedTransport, tokio::runtime::Runtime)> {
    let rt = tokio::runtime::Runtime::new()?;
    let opened = {
        let _reactor = rt.enter();
        transport::discover_and_open(cli_port, cli_baud, port_interface)?
    };
    Ok((opened, rt))
}

/// Parse a UTC offset string like `+05:30`, `-08:00`, `+0530`, or
/// `+5`. Returns a checked fixed offset, positive for east of UTC.
///
/// Accepted forms:
/// - `+HH:MM`, `-HH:MM`
/// - `+HHMM`, `-HHMM`
/// - `+H`, `-H` (hours only, no minutes)
/// - Leading sign is optional for positive offsets.
fn parse_utc_offset(s: &str) -> Result<time::UtcOffset, OffsetError> {
    if s.is_empty() {
        return Err(OffsetError::Syntax);
    }
    // Reject non-ASCII up front: the slicing below is byte-indexed,
    // and a multi-byte character straddling a slice boundary would
    // panic instead of reaching the graceful invalid-offset error.
    if !s.is_ascii() {
        return Err(OffsetError::Syntax);
    }
    let bytes = s.as_bytes();
    let (sign, rest) = match bytes.first() {
        Some(b'+') => (1i32, s.get(1..).unwrap_or("")),
        Some(b'-') => (-1i32, s.get(1..).unwrap_or("")),
        _ => (1i32, s),
    };
    if rest.is_empty() {
        return Err(OffsetError::Syntax);
    }
    let (h_str, m_str) = if let Some((h, m)) = rest.split_once(':') {
        (h, m)
    } else if rest.len() >= 3 {
        let split = rest.len() - 2;
        (
            rest.get(..split).unwrap_or_default(),
            rest.get(split..).unwrap_or_default(),
        )
    } else {
        (rest, "0")
    };
    let h: i32 = h_str.parse().map_err(OffsetError::Number)?;
    let m: i32 = m_str.parse().map_err(OffsetError::Number)?;
    if !(0..=14).contains(&h) {
        return Err(OffsetError::Hours(h));
    }
    if !(0..=59).contains(&m) {
        return Err(OffsetError::Minutes(m));
    }
    time::UtcOffset::from_whole_seconds(sign * (h * 3600 + m * 60)).map_err(OffsetError::Range)
}

#[derive(Debug)]
enum OffsetError {
    Syntax,
    Number(std::num::ParseIntError),
    Hours(i32),
    Minutes(i32),
    Range(time::error::ComponentRange),
}

impl std::fmt::Display for OffsetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Syntax => f.write_str("use a signed UTC offset such as +05:30 or -0800"),
            Self::Number(error) => write!(f, "invalid UTC offset number: {error}"),
            Self::Hours(value) => write!(f, "UTC offset hours {value} are outside 0 through 14"),
            Self::Minutes(value) => {
                write!(f, "UTC offset minutes {value} are outside 0 through 59")
            }
            Self::Range(error) => std::fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for OffsetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Number(error) => Some(error),
            Self::Range(error) => Some(error),
            Self::Syntax | Self::Hours(_) | Self::Minutes(_) => None,
        }
    }
}

fn main() -> std::process::ExitCode {
    let result = run_main();
    let signal_exit_code = PROCESS_SIGNAL_EXIT_CODE.load(Ordering::Acquire);
    match result {
        Ok(()) if signal_exit_code == 0 => std::process::ExitCode::SUCCESS,
        Ok(()) => std::process::ExitCode::from(u8::try_from(signal_exit_code).unwrap_or(1)),
        Err(e) => {
            let mut stderr = std::io::stderr().lock();
            let display = e.to_string();
            let display = display.strip_prefix("error: ").unwrap_or(&display);
            for line in thd75_repl::render_message(&format!("Error: {display}"), None) {
                if writeln!(stderr, "{line}").is_err() {
                    break;
                }
            }
            if signal_exit_code == 0 {
                std::process::ExitCode::FAILURE
            } else {
                std::process::ExitCode::from(u8::try_from(signal_exit_code).unwrap_or(1))
            }
        }
    }
}

/// Apply process-global output and confirmation options before startup output.
fn configure_process_options(cli: &Cli) -> bool {
    thd75_repl::set_history_capacity(cli.history_lines);

    if cli.yes {
        thd75_repl::confirm::set_required(false);
    }
    // Script mode also covers piped stdin: a confirmation prompt cannot be
    // answered there without consuming the next queued command.
    let in_script_mode = cli.script.is_some();
    if in_script_mode || !std::io::stdin().is_terminal() {
        thd75_repl::confirm::set_script_mode(true);
    }

    let mut local_offset_error = None;
    let timestamp_mode = if cli.local_time || cli.utc_offset.is_some() {
        let offset = cli.utc_offset.unwrap_or_else(|| {
            time::UtcOffset::current_local_offset().unwrap_or_else(|error| {
                local_offset_error = Some(error);
                time::UtcOffset::UTC
            })
        });
        thd75_repl::TimestampMode::Offset(offset)
    } else if cli.timestamps {
        thd75_repl::TimestampMode::Utc
    } else {
        thd75_repl::TimestampMode::Disabled
    };
    thd75_repl::set_timestamp_mode(timestamp_mode);
    if let Some(error) = local_offset_error {
        aprintln!("Warning: local UTC offset unavailable: {error}. Using UTC.");
    }

    in_script_mode
}

fn run_main() -> AppResult<()> {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) if error.exit_code() == 0 => {
            thd75_repl::emit_output(error.to_string().trim_end_matches('\n'));
            return finish_output(Ok(()));
        }
        Err(error) => return Err(error.into()),
    };

    let in_script_mode = configure_process_options(&cli);

    // Subcommands bypass the REPL loop entirely. `check` runs a
    // hardware-free accessibility self-check and exits.
    if matches!(&cli.command_mode, Some(Subcommand::Check)) {
        let status = thd75_repl::check::run();
        let result = if status == 0 {
            Ok(())
        } else {
            Err("sample output failed the evaluated accessibility rules".into())
        };
        return finish_output(result);
    }

    let initial_command = (!cli.command.is_empty()).then(|| cli.command.join(" "));
    let script = cli
        .script
        .as_deref()
        .map(thd75_repl::script::Script::from_path)
        .transpose()?;
    if let Some(command) = initial_command.as_deref() {
        preflight_startup_command(command)?;
    }
    if let Some(script) = &script {
        for command in script.commands() {
            preflight_startup_command(command)?;
        }
    }

    // Configure logging before anything else so the file captures
    // the full startup sequence. The guard returned here must live
    // until the program exits; dropping it would terminate the
    // background flush thread and silently drop late log lines.
    let _logging_guard = init_logging(&cli);

    aprintln!(
        "{}",
        thd75_repl::output::startup_banner(env!("CARGO_PKG_VERSION"))
    );

    // Create the Tokio runtime before transport discovery because the serial
    // path registers a mio descriptor while it opens. Native macOS Bluetooth
    // is isolated in a helper process whose main thread owns IOBluetooth and
    // its CFRunLoop, so the host runtime has no thread-affinity requirement.
    //
    // When compiled with the `testing` feature and `--mock-radio
    // <scenario>` is passed, short-circuit the real transport
    // discovery entirely and construct a programmed `MockTransport`
    // instead.
    #[cfg(feature = "testing")]
    let (opened, rt) = if let Some(ref scenario) = cli.mock_radio {
        let mock = thd75_repl::mock_scenarios::build(scenario).ok_or_else(|| {
            format!(
                "Unknown mock scenario: {scenario}. Known: simple, empty, startup, \
                 mmdvm, mmdvm_takeover, aprs."
            )
        })?;
        let rt = tokio::runtime::Runtime::new()?;
        (
            transport::OpenedTransport {
                label: format!("mock:{scenario}"),
                transport: EitherTransport::Mock(mock),
                endpoint_interface: transport::EndpointInterface::Known(PcOutputInterface::Usb),
            },
            rt,
        )
    } else {
        open_real_transport(
            cli.port.as_deref(),
            cli.baud,
            cli.port_interface.map(PcOutputInterface::from),
        )?
    };
    #[cfg(not(feature = "testing"))]
    let (opened, rt) = open_real_transport(
        cli.port.as_deref(),
        cli.baud,
        cli.port_interface.map(PcOutputInterface::from),
    )?;

    let transport::OpenedTransport {
        label,
        transport,
        endpoint_interface,
    } = opened;
    tracing::debug!(?endpoint_interface, "selected physical interface");

    aprintln!("{}", thd75_repl::output::connected_via(&label));

    // `--set-gateway-off`: clear the DV Gateway flag and exit, without
    // entering the interactive loop. Runs over whatever port answered
    // (use USB when Bluetooth is busy in terminal mode).
    if cli.set_gateway_off {
        let local = tokio::task::LocalSet::new();
        return finish_output(local.block_on(&rt, run_set_gateway_off(transport)));
    }

    // Keep the REPL's existing local owner scope. The MMDVM pump itself is
    // runtime-spawned, and Bluetooth's CFRunLoop lives entirely in the private
    // transport helper.
    let local = tokio::task::LocalSet::new();
    let result = local.block_on(
        &rt,
        run_repl(
            transport,
            ReplOptions {
                initial_command,
                script,
                script_mode: in_script_mode,
                exit_terminal_mode: cli.exit_terminal_mode,
            },
        ),
    );
    finish_output(result)
}

/// Read one line with rustyline without blocking the async radio tasks.
async fn read_line_async(
    mut editor: rustyline::DefaultEditor,
    prompt: &'static str,
) -> Result<
    (
        rustyline::DefaultEditor,
        Result<Option<String>, rustyline::error::ReadlineError>,
    ),
    tokio::task::JoinError,
> {
    tokio::task::spawn_blocking(move || {
        let line = read_line_blocking(&mut editor, prompt);
        (editor, line)
    })
    .await
}

/// Read one line from the user via rustyline. EOF and Ctrl-C request shutdown;
/// other input failures retain their typed cause. Run this blocking function through
/// [`read_line_async`].
fn read_line_blocking(
    rl: &mut rustyline::DefaultEditor,
    prompt: &str,
) -> Result<Option<String>, rustyline::error::ReadlineError> {
    match rl.readline(prompt) {
        Ok(line) => Ok(Some(line)),
        Err(
            rustyline::error::ReadlineError::Interrupted | rustyline::error::ReadlineError::Eof,
        ) => Ok(None),
        Err(error) => Err(error),
    }
}

type AppResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug)]
struct CleanupFailure {
    operation: Box<dyn std::error::Error + Send + Sync>,
    cleanup: Box<dyn std::error::Error + Send + Sync>,
}

impl std::fmt::Display for CleanupFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}; cleanup also failed: {}",
            self.operation, self.cleanup
        )
    }
}

impl std::error::Error for CleanupFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.operation.as_ref())
    }
}

fn finish_with_cleanup<T>(operation: AppResult<T>, cleanup: AppResult<()>) -> AppResult<T> {
    match (operation, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(Box::new(CleanupFailure { operation, cleanup })),
    }
}

fn finish_output(operation: AppResult<()>) -> AppResult<()> {
    let output = thd75_repl::take_output_error().map_or(Ok(()), |error| Err(error.into()));
    finish_with_cleanup(operation, output)
}

/// Close the selected connection, retaining both mode-exit and close failures.
async fn shutdown_state(state: ReplState) -> AppResult<()> {
    match state {
        ReplState::Cat(radio) | ReplState::Mmdvm(radio) => {
            radio.disconnect().await.map_err(Into::into)
        }
        ReplState::Aprs(client) => match client.stop().await {
            Ok(desynced) => desynced
                .into_radio_unproven()
                .disconnect()
                .await
                .map_err(Into::into),
            Err((client, error)) => {
                let cleanup = (*client).into_radio_recovery_required().disconnect().await;
                finish_with_cleanup(Err(error.into()), cleanup.map_err(Into::into))
            }
        },
    }
}

/// The three transport-ownership states of the REPL.
enum ReplState {
    /// Normal CAT control: radio is directly accessible. Boxed to
    /// keep this variant's size near its siblings'
    /// (`clippy::large_enum_variant`).
    Cat(Box<Radio<EitherTransport>>),
    /// Complete MMDVM version framing has been proved on the link. CAT
    /// commands remain blocked; the menu's persistent gateway mode is unknown.
    Mmdvm(Box<Radio<EitherTransport>>),
    /// APRS/KISS mode: radio consumed by `AprsClient`.
    Aprs(Box<AprsClient<EitherTransport>>),
}

/// Catchable process signals routed to the one operation that currently owns
/// a partially applied persistent radio change.
#[derive(Copy, Clone, Debug)]
enum ProcessSignal {
    Interrupt,
    Terminate,
    Hangup,
}

impl ProcessSignal {
    const fn description(self) -> &'static str {
        match self {
            Self::Interrupt => "interrupt",
            Self::Terminate => "termination",
            Self::Hangup => "hangup",
        }
    }

    const fn exit_code(self) -> i32 {
        match self {
            Self::Interrupt => 130,
            Self::Terminate => 143,
            Self::Hangup => 129,
        }
    }
}

/// Process-lifetime signal owner with a scoped route for critical MCP work.
///
/// Tokio retains ownership of an OS signal after the first listener is
/// installed. Keeping these listeners alive for the whole REPL prevents a
/// successful terminal-mode transition from leaving SIGTERM or SIGHUP
/// swallowed by a dropped temporary listener.
#[derive(Debug)]
struct ProcessSignalRouter {
    critical_slot: CriticalSignalSlot,
    interrupt_installed: std::sync::atomic::AtomicBool,
}

type CriticalSignalSlot =
    std::sync::Arc<std::sync::Mutex<Option<tokio::sync::mpsc::Sender<ProcessSignal>>>>;

#[derive(Debug)]
enum SignalError {
    LockPoisoned,
    OperationActive,
    Install(std::io::Error),
    RouterStopped,
    Received(ProcessSignal),
}

impl std::fmt::Display for SignalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LockPoisoned => f.write_str("process signal router lock is poisoned"),
            Self::OperationActive => f.write_str("another critical radio operation is active"),
            Self::Install(error) => write!(f, "could not install interrupt listener: {error}"),
            Self::RouterStopped => f.write_str("process signal router stopped"),
            Self::Received(signal) => write!(f, "{} signal received", signal.description()),
        }
    }
}

impl std::error::Error for SignalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Install(error) => Some(error),
            Self::LockPoisoned
            | Self::OperationActive
            | Self::RouterStopped
            | Self::Received(_) => None,
        }
    }
}

impl ProcessSignalRouter {
    fn install() -> std::io::Result<Self> {
        let critical_slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        install_process_signal_task(critical_slot.clone())?;
        Ok(Self {
            critical_slot,
            interrupt_installed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    #[cfg(test)]
    fn disconnected_for_test() -> Self {
        Self {
            critical_slot: std::sync::Arc::new(std::sync::Mutex::new(None)),
            // Unit tests inject their interruption futures directly; no OS
            // signal task should be installed into the shared test process.
            interrupt_installed: std::sync::atomic::AtomicBool::new(true),
        }
    }

    fn begin_critical(&self) -> Result<CriticalSignalGuard, SignalError> {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let mut slot = self
            .critical_slot
            .lock()
            .map_err(|_| SignalError::LockPoisoned)?;
        if slot.is_some() {
            return Err(SignalError::OperationActive);
        }
        *slot = Some(sender.clone());
        drop(slot);

        if self
            .interrupt_installed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            && let Err(error) = install_process_interrupt_task(self.critical_slot.clone())
        {
            self.interrupt_installed.store(false, Ordering::Release);
            if let Ok(mut slot) = self.critical_slot.lock()
                && slot
                    .as_ref()
                    .is_some_and(|active| active.same_channel(&sender))
            {
                drop(slot.take());
            }
            return Err(SignalError::Install(error));
        }
        Ok(CriticalSignalGuard {
            receiver,
            critical_slot: self.critical_slot.clone(),
            armed: true,
        })
    }
}

#[derive(Debug)]
struct CriticalSignalGuard {
    receiver: tokio::sync::mpsc::Receiver<ProcessSignal>,
    critical_slot: CriticalSignalSlot,
    armed: bool,
}

impl CriticalSignalGuard {
    async fn recv(&mut self) -> Result<ProcessSignal, SignalError> {
        self.receiver.recv().await.ok_or(SignalError::RouterStopped)
    }

    /// Atomically unregister this critical operation, then check for a signal
    /// that raced with its final successful poll. Clearing the slot while
    /// holding the same mutex used by the router ensures a later TERM/HUP
    /// takes the normal process-exit path instead of entering a dead receiver.
    fn finish(mut self) -> Result<(), SignalError> {
        self.unregister()?;
        self.armed = false;
        match self.receiver.try_recv() {
            Ok(signal) => Err(SignalError::Received(signal)),
            Err(
                tokio::sync::mpsc::error::TryRecvError::Empty
                | tokio::sync::mpsc::error::TryRecvError::Disconnected,
            ) => Ok(()),
        }
    }

    fn unregister(&self) -> Result<(), SignalError> {
        drop(
            self.critical_slot
                .lock()
                .map_err(|_| SignalError::LockPoisoned)?
                .take(),
        );
        Ok(())
    }
}

impl Drop for CriticalSignalGuard {
    fn drop(&mut self) {
        if self.armed {
            drop(self.unregister());
        }
    }
}

fn route_process_signal(signal: ProcessSignal, critical_slot: &CriticalSignalSlot) {
    let delivered = critical_slot
        .lock()
        .ok()
        .and_then(|slot| {
            slot.as_ref().map(|sender| {
                let delivered = matches!(
                    sender.try_send(signal),
                    Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Full(_))
                );
                if delivered {
                    let _previous = PROCESS_SIGNAL_EXIT_CODE.compare_exchange(
                        0,
                        signal.exit_code(),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                }
                delivered
            })
        })
        .unwrap_or(false);
    if delivered {
        return;
    }

    if !matches!(signal, ProcessSignal::Interrupt) {
        std::process::exit(signal.exit_code());
    }
}

#[cfg(unix)]
fn install_process_signal_task(critical_slot: CriticalSignalSlot) -> std::io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    drop(tokio::spawn(async move {
        loop {
            let observed = tokio::select! {
                _ = terminate.recv() => ProcessSignal::Terminate,
                _ = hangup.recv() => ProcessSignal::Hangup,
            };
            route_process_signal(observed, &critical_slot);
        }
    }));
    Ok(())
}

#[cfg(windows)]
fn install_process_signal_task(critical_slot: CriticalSignalSlot) -> std::io::Result<()> {
    use tokio::signal::windows;

    let mut ctrl_close = windows::ctrl_close()?;
    let mut ctrl_logoff = windows::ctrl_logoff()?;
    let mut ctrl_shutdown = windows::ctrl_shutdown()?;
    drop(tokio::spawn(async move {
        loop {
            let observed = tokio::select! {
                _ = ctrl_close.recv() => ProcessSignal::Terminate,
                _ = ctrl_logoff.recv() => ProcessSignal::Hangup,
                _ = ctrl_shutdown.recv() => ProcessSignal::Terminate,
            };
            route_process_signal(observed, &critical_slot);
        }
    }));
    Ok(())
}

#[cfg(not(any(unix, windows)))]
const fn install_process_signal_task(_critical_slot: CriticalSignalSlot) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn install_process_interrupt_task(critical_slot: CriticalSignalSlot) -> std::io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut interrupt = signal(SignalKind::interrupt())?;
    drop(tokio::spawn(async move {
        loop {
            let _received = interrupt.recv().await;
            route_process_signal(ProcessSignal::Interrupt, &critical_slot);
        }
    }));
    Ok(())
}

#[cfg(windows)]
fn install_process_interrupt_task(critical_slot: CriticalSignalSlot) -> std::io::Result<()> {
    use tokio::signal::windows;

    let mut ctrl_c = windows::ctrl_c()?;
    let mut ctrl_break = windows::ctrl_break()?;
    drop(tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = ctrl_c.recv() => {}
                _ = ctrl_break.recv() => {}
            }
            route_process_signal(ProcessSignal::Interrupt, &critical_slot);
        }
    }));
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn install_process_interrupt_task(critical_slot: CriticalSignalSlot) -> std::io::Result<()> {
    drop(tokio::spawn(async move {
        loop {
            if let Err(error) = tokio::signal::ctrl_c().await {
                tracing::error!(%error, "process interrupt listener stopped");
                return;
            }
            route_process_signal(ProcessSignal::Interrupt, &critical_slot);
        }
    }));
    Ok(())
}

/// Main REPL loop options for CAT, MMDVM-ready and APRS states.
/// Each state owns the radio transport exclusively.
#[derive(Debug)]
struct ReplOptions {
    initial_command: Option<String>,
    script: Option<thd75_repl::script::Script>,
    script_mode: bool,
    exit_terminal_mode: bool,
}

#[expect(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    reason = "The dispatch loop retains exclusive ownership through CAT/APRS transitions \
              and handles command input, recovery and explicit shutdown in the same state match."
)]
async fn run_repl(transport: EitherTransport, options: ReplOptions) -> AppResult<()> {
    let ReplOptions {
        initial_command,
        script,
        script_mode: in_script_mode,
        exit_terminal_mode,
    } = options;

    let mut radio = Radio::new(transport);

    // Install one process-lifetime signal owner before any radio I/O. Scoped
    // MCP transitions subscribe to it; outside those transitions, TERM/HUP
    // retain their normal process-termination behavior.
    let _process_signals = match ProcessSignalRouter::install() {
        Ok(signals) => signals,
        Err(error) => {
            return finish_with_cleanup(
                Err(error.into()),
                radio.disconnect().await.map_err(Into::into),
            );
        }
    };

    // Preserve an already-ready CAT link with a read-only identity proof. If
    // the bounded packet-mode fallback still cannot prove CAT, retain this
    // exact transport owner long enough to distinguish persistent MMDVM from
    // an unresponsive link.
    let cat_ready = match radio.prepare_cat_or_retain_for_diagnosis().await {
        Ok(()) => true,
        Err(error) => {
            tracing::debug!(%error, "CAT preparation failed; checking for a persistent binary owner");
            false
        }
    };

    let cat_startup = if cat_ready {
        match radio.identify().await {
            Ok(info) => match radio.get_firmware_version().await {
                Ok(firmware) => Some((info, firmware)),
                Err(error) => {
                    tracing::debug!(%error, "firmware qualification failed after CAT preparation; checking for binary takeover");
                    None
                }
            },
            Err(error) => {
                tracing::debug!(%error, "CAT identity disappeared after preparation; checking for binary takeover");
                None
            }
        }
    } else {
        None
    };

    let mut state = if let Some((info, firmware)) = cat_startup {
        if let Err(error) = radio.get_serial_information().await {
            return finish_with_cleanup(
                Err(error.into()),
                radio.disconnect().await.map_err(Into::into),
            );
        }
        aprintln!(
            "{}",
            thd75_repl::output::startup_identified(info.model, &firmware,)
        );
        aprintln!("{}", thd75_repl::output::type_help_hint());
        ReplState::Cat(Box::new(radio))
    } else {
        match radio.probe_silent_link().await {
            LinkDiagnosis::ReconnectRequired => {
                return finish_with_cleanup(
                    Err(StartupDiagnosis(LinkDiagnosis::ReconnectRequired).into()),
                    radio.disconnect().await.map_err(Into::into),
                );
            }
            LinkDiagnosis::MmdvmMode if exit_terminal_mode => {
                // CAT is offline on this link, so neither model nor
                // firmware can be qualified before an offset-based MCP
                // write. Keep this path read-only and guide the operator.
                // Fully automated exit remains available through
                // `--set-gateway-off` on the other, CAT-capable interface,
                // where ID/FV are proved before touching flash.
                aprintln!(
                    "This gateway link cannot prove the radio model and firmware required \
                         for a safe automated memory write."
                );
                let RestoredCat {
                    radio,
                    identity,
                    firmware,
                } = guide_exit_terminal_mode(radio).await?;
                aprintln!(
                    "{}",
                    thd75_repl::output::startup_identified(identity.model, &firmware,)
                );
                aprintln!("{}", thd75_repl::output::type_help_hint());
                ReplState::Cat(Box::new(radio))
            }
            LinkDiagnosis::MmdvmMode => {
                // Store the binary-proved owner in its dedicated state so
                // CAT dispatch is unreachable. MMDVM framing proves the
                // DV Gateway wire protocol, but cannot distinguish
                // Reflector Terminal from Access Point mode.
                aprintln!("Radio is in DV Gateway/MMDVM mode.");
                aprintln!("CAT commands like freq, mode, and status do not work in this mode.");
                aprintln!(
                    "Kenwood D-STAR initialization is unavailable: no verified wire profile."
                );
                aprintln!("Set Menu No. 650 to Off, then restart or use --exit-terminal-mode.");
                ReplState::Mmdvm(Box::new(radio))
            }
            LinkDiagnosis::Unresponsive => {
                return finish_with_cleanup(
                    Err(StartupDiagnosis(LinkDiagnosis::Unresponsive).into()),
                    radio.disconnect().await.map_err(Into::into),
                );
            }
        }
    };

    // Rustyline performs a blocking terminal read. Move the editor through
    // `spawn_blocking` so the async runtime remains free to drive radio tasks.
    let mut rl = Some(match rustyline::DefaultEditor::new() {
        Ok(editor) => editor,
        Err(error) => return finish_with_cleanup(Err(error.into()), shutdown_state(state).await),
    });
    let mut pending_command = initial_command;
    let mut script_commands: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    if let Some(s) = script {
        for cmd in s.into_commands() {
            script_commands.push_back(cmd);
        }
    }

    loop {
        let prompt = match &state {
            ReplState::Cat(_) | ReplState::Mmdvm(_) => "d75> ",
            ReplState::Aprs(_) => "aprs> ",
        };

        // Command source priority: initial command, then script queue,
        // then interactive input. When in script mode and the queue
        // drains, return None so the main loop exits cleanly instead
        // of falling into the rustyline prompt. The explicit if-ladder keeps
        // that priority visible.
        let line = if let Some(cmd) = pending_command.take() {
            aprintln!("{prompt}{cmd}");
            Some(cmd)
        } else if let Some(cmd) = script_commands.pop_front() {
            aprintln!("{prompt}{cmd}");
            Some(cmd)
        } else if in_script_mode {
            None
        } else {
            let editor = rl.take().ok_or("interactive editor ownership was lost")?;
            let (editor, line) = match read_line_async(editor, prompt).await {
                Ok(result) => result,
                Err(error) => {
                    return finish_with_cleanup(Err(error.into()), shutdown_state(state).await);
                }
            };
            rl = Some(editor);
            match line {
                Ok(line) => line,
                Err(error) => {
                    return finish_with_cleanup(Err(error.into()), shutdown_state(state).await);
                }
            }
        };
        let Some(line) = line else {
            shutdown_state(state).await?;
            aprintln!("{}", thd75_repl::output::goodbye());
            return Ok(());
        };

        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }
        if let Some(editor) = rl.as_mut() {
            drop(editor.add_history_entry(&line));
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        let Some(first_word) = parts.first() else {
            continue;
        };

        let cmd = first_word.to_lowercase();

        // Global commands available in any mode.
        match cmd.as_str() {
            "help" | "?" => {
                if let Some(sub) = parts.get(1) {
                    if *sub == "all" {
                        for cmd in thd75_repl::help_text::ALL_COMMANDS {
                            if let Some(text) = thd75_repl::help_text::for_command(cmd) {
                                aprintln!("{text}");
                                aprintln!();
                            }
                        }
                    } else if let Some(text) = thd75_repl::help_text::for_command(sub) {
                        aprintln!("{text}");
                    } else {
                        aprintln!(
                            "{}",
                            thd75_repl::output::error(format_args!(
                                "help for {sub:?} not found. Type help for a list of commands."
                            ))
                        );
                    }
                } else {
                    let text = match &state {
                        ReplState::Cat(_) => thd75_repl::help_text::CAT_MODE_HELP,
                        ReplState::Mmdvm(_) => thd75_repl::help_text::MMDVM_MODE_HELP,
                        ReplState::Aprs(_) => thd75_repl::help_text::APRS_MODE_HELP,
                    };
                    aprintln!("{text}");
                }
                continue;
            }
            "quit" | "exit" | "q" => {
                shutdown_state(state).await?;
                aprintln!("{}", thd75_repl::output::goodbye());
                return Ok(());
            }
            "last" | "repeat" => {
                let count = if let Some(arg) = parts.get(1) {
                    if *arg == "all" {
                        // Everything the buffer holds. `last_lines`
                        // clamps to the stored line count, so this
                        // respects whatever `--history-lines` set
                        // rather than assuming the default capacity.
                        usize::MAX
                    } else if let Ok(n) = arg.parse::<usize>() {
                        n
                    } else {
                        aprintln!(
                            "{}",
                            thd75_repl::output::error(format_args!(
                                "invalid count {arg:?}. Use a number or \"all\"."
                            ))
                        );
                        continue;
                    }
                } else {
                    1
                };
                let lines = thd75_repl::last_lines(count);
                if lines.is_empty() {
                    aprintln!("No previous output available.");
                } else {
                    thd75_repl::replay_output(&lines);
                }
                continue;
            }
            "verbose" => {
                let new_value = match parts.get(1).map(|s| s.to_lowercase()).as_deref() {
                    Some("on" | "true" | "1") => true,
                    Some("off" | "false" | "0") => false,
                    Some(other) => {
                        aprintln!(
                            "{}",
                            thd75_repl::output::error(format_args!(
                                "unknown verbose argument {other:?}. Use on or off."
                            ))
                        );
                        continue;
                    }
                    None => !thd75_repl::is_verbose(),
                };
                thd75_repl::set_verbose(new_value);
                aprintln!("Verbose output: {}", if new_value { "on" } else { "off" });
                continue;
            }
            "quiet" => {
                thd75_repl::set_verbose(false);
                aprintln!("Verbose output: off");
                continue;
            }
            "confirm" => {
                let new_value = match parts.get(1).map(|s| s.to_lowercase()).as_deref() {
                    Some("on" | "true" | "1") => true,
                    Some("off" | "false" | "0") => false,
                    Some(other) => {
                        aprintln!(
                            "{}",
                            thd75_repl::output::error(format_args!(
                                "unknown confirm argument {other:?}. Use on or off."
                            ))
                        );
                        continue;
                    }
                    None => !thd75_repl::confirm::is_required(),
                };
                thd75_repl::confirm::set_required(new_value);
                aprintln!(
                    "Transmit confirmation: {}",
                    if new_value { "on" } else { "off" }
                );
                continue;
            }
            "check" => {
                // Hardware-free accessibility self-check, also
                // available as the `check` CLI subcommand. The exit
                // code is meaningless mid-session, so discard it.
                let _ = thd75_repl::check::run();
                continue;
            }
            "dstar" => {
                let result = if parts.get(1) == Some(&"start") {
                    preflight_dstar_start(parts.get(2..).unwrap_or(&[]))
                } else {
                    Err(DstarStartError::Usage)
                };
                if let Err(error) = result {
                    aprintln!("Error: {error}");
                }
                continue;
            }
            _ => {}
        }

        // An MMDVM-ready owner has positive binary framing proof, not CAT
        // ownership. Keep ordinary radio and APRS commands away from it.
        if matches!(&state, ReplState::Mmdvm(_)) && cmd != "dstar" {
            aprintln!("{}", LinkDiagnosis::MmdvmMode.guidance());
            aprintln!("Restart with --exit-terminal-mode to return to CAT control.");
            aprintln!(
                "D-STAR startup is unavailable for the unverified Kenwood configuration profile."
            );
            continue;
        }

        // Mode-specific dispatch.
        state = match state {
            ReplState::Cat(mut radio) => {
                if let Err(error) = dispatch_cat(&mut radio, &cmd, &parts).await {
                    return finish_with_cleanup(
                        Err(error),
                        shutdown_state(ReplState::Cat(radio)).await,
                    );
                }

                // Ordinary CAT failures are rendered by their handlers. Check
                // the model's frame state before the next command and recover
                // any ambiguous boundary.
                if radio.cat_recovery_required() {
                    aprintln!("CAT response boundary is ambiguous; recovering the radio link.");
                    if let Err(error) = radio.recover_cat().await {
                        aprintln!(
                            "{}",
                            thd75_repl::output::error(format_args!("CAT recovery failed: {error}"))
                        );
                        let cleanup = radio.disconnect().await.map_err(Into::into);
                        return finish_with_cleanup(Err(error.into()), cleanup);
                    }
                    aprintln!("CAT link recovered.");
                }

                // Check for mode transitions.
                if cmd == "aprs" && parts.get(1).is_some_and(|s| *s == "start") {
                    match enter_aprs(*radio, parts.get(2..).unwrap_or(&[])).await {
                        Ok(next_state) => next_state,
                        Err(failure) => {
                            let (radio_back, error) = *failure;
                            aprintln!(
                                "{}",
                                thd75_repl::output::error(format_args!(
                                    "entering APRS mode: {error}"
                                ))
                            );
                            match radio_back {
                                Some(radio) => ReplState::Cat(Box::new(radio)),
                                None => return Err(error),
                            }
                        }
                    }
                } else {
                    ReplState::Cat(radio)
                }
            }

            ReplState::Mmdvm(radio) => ReplState::Mmdvm(radio),

            ReplState::Aprs(mut client) => {
                if cmd == "aprs" && parts.get(1).is_some_and(|s| *s == "stop") {
                    match client.stop().await {
                        Ok(desynced) => match desynced.restore().await {
                            Ok(radio) => {
                                aprintln!("APRS mode stopped. Returned to CAT mode.");
                                ReplState::Cat(Box::new(radio))
                            }
                            Err(failure) => {
                                let (desynced, error) = *failure;
                                aprintln!(
                                    "{}",
                                    thd75_repl::output::error(format_args!(
                                        "KISS exit succeeded, but CAT restoration failed: \
                                             {error}"
                                    ))
                                );
                                let cleanup = desynced
                                    .into_radio_unproven()
                                    .disconnect()
                                    .await
                                    .map_err(Into::into);
                                return finish_with_cleanup(Err(error.into()), cleanup);
                            }
                        },
                        Err((client, e)) => {
                            aprintln!(
                                "{}",
                                thd75_repl::output::error(format_args!("stopping APRS: {e}"))
                            );
                            let cleanup = (*client)
                                .into_radio_recovery_required()
                                .disconnect()
                                .await
                                .map_err(Into::into);
                            return finish_with_cleanup(Err(e.into()), cleanup);
                        }
                    }
                } else {
                    if let Err(error) = dispatch_aprs(&mut client, &cmd, &parts).await {
                        let cleanup = shutdown_state(ReplState::Aprs(client)).await;
                        return finish_with_cleanup(Err(error), cleanup);
                    }
                    ReplState::Aprs(client)
                }
            }
        };
    }
}

// ---------------------------------------------------------------------------
// CAT mode dispatch
// ---------------------------------------------------------------------------

/// Dispatch a command in CAT mode to the appropriate handler function.
#[expect(
    clippy::cognitive_complexity,
    reason = "Command dispatch table for every CAT-mode REPL command. The flat `match` over \
              string aliases is the idiomatic shape for a dispatcher; any refactor would move \
              the dispatch into a per-command map without reducing real complexity."
)]
async fn dispatch_cat(
    radio: &mut Radio<EitherTransport>,
    cmd: &str,
    parts: &[&str],
) -> AppResult<()> {
    let args = parts.get(1..).unwrap_or(&[]);
    match cmd {
        "id" | "identify" => commands::identify(radio).await,
        "freq" | "frequency" => commands::frequency(radio, args).await,
        "mode" => commands::set_operating_mode(radio, args).await,
        "squelch" | "sq" => commands::squelch(radio, args).await,
        "power" | "pwr" => commands::set_power(radio, args).await,
        "tnc" => commands::tnc_mode(radio, args).await,
        "beaconmode" => commands::beacon_mode(radio, args).await?,
        "battery" | "bat" => commands::battery(radio).await,
        "lock" => commands::lock(radio, args),
        "dualband" | "dual" => commands::dual_band(radio, args).await,
        "bluetooth" | "bt" => commands::bluetooth(radio, args).await,
        "vox" => commands::vox(radio, args).await,
        "fm" => commands::fm_radio(radio, args).await,
        "attenuator" | "att" => commands::attenuator(radio, args).await,
        "step" => commands::step_size(radio, args).await,
        "up" => commands::step_up(radio, args).await,
        "down" => commands::step_down(radio, args).await,
        "channel" | "ch" => commands::channel(radio, args).await,
        "channels" => commands::channels(radio, args).await,
        "tune" => commands::tune(radio, args).await,
        "recall" => commands::recall(radio, args).await,
        "meter" | "smeter" => commands::smeter(radio, args).await,
        "vfo" => commands::vfo(radio, args).await,
        "clock" | "time" => commands::clock(radio).await,
        "gps" => commands::gps(radio, args).await,
        "urcall" | "ur" => commands::urcall(radio, args).await,
        "cq" => commands::cq(radio).await?,
        "reflector" | "ref" => commands::reflector(radio, args).await,
        "unreflector" | "unref" | "unlink" => commands::unreflector(radio).await,
        "status" => commands::status(radio).await,
        "band" => commands::band(radio, args).await,
        "ifout" | "usbout" => commands::ifout(radio, args).await,
        "aprs" => {
            if parts.get(1).is_some_and(|s| *s == "start") {
                // Handled by caller after dispatch.
            } else {
                aprintln!("Usage: aprs start <callsign> <a or b> [ssid] [digi]");
                aprintln!("Enters APRS KISS mode. Type aprs stop to exit.");
                aprintln!("The required band selects the TNC data band, not packet speed.");
                aprintln!("Add digi to enable the WIDE1-1 fill-in digipeater.");
            }
        }
        other => aprintln!("Unknown command: {other}. Type help for a list of commands."),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// APRS mode
// ---------------------------------------------------------------------------

/// Select APRS KISS mode, retaining CAT when transmit confirmation is declined.
///
/// Input-validation errors return the untouched radio. Once KISS entry is
/// attempted, this function proves CAT synchronization before returning a
/// radio; if that proof fails, it disconnects and returns no owner.
/// Confirmation I/O failures close the connection and retain any close failure.
async fn enter_aprs(
    radio: Radio<EitherTransport>,
    args: &[&str],
) -> Result<
    ReplState,
    Box<(
        Option<Radio<EitherTransport>>,
        Box<dyn std::error::Error + Send + Sync>,
    )>,
> {
    let parsed = match thd75_repl::aprs_args::StartArgs::parse(args) {
        Ok(p) => p,
        Err(e) => return Err(Box::new((Some(radio), e.into()))),
    };
    match commands::confirm_transmit() {
        Ok(true) => {}
        Ok(false) => return Ok(ReplState::Cat(Box::new(radio))),
        Err(error) => {
            let operation: Box<dyn std::error::Error + Send + Sync> = error.into();
            let failure = match radio.disconnect().await {
                Ok(()) => operation,
                Err(error) => Box::new(CleanupFailure {
                    operation,
                    cleanup: error.into(),
                }),
            };
            return Err(Box::new((None, failure)));
        }
    }
    let data_band = parsed.data_band();
    let digipeater = parsed.digipeater();
    let address = parsed.into_address();
    aprintln!("Leaving normal radio control. Entering APRS mode as {address}.");
    let builder = match AprsClientConfig::builder(address.clone(), data_band) {
        Ok(builder) => builder,
        Err(error) => return Err(Box::new((Some(radio), error.into()))),
    };
    let config = if digipeater {
        let wide1 = match Ax25Address::new("WIDE1", 1) {
            Ok(alias) => alias,
            Err(e) => {
                return Err(Box::new((Some(radio), e.into())));
            }
        };
        let digi_cfg = DigipeaterConfig::new(
            address,
            DigipeaterAliases {
                uidigipeat: vec![wide1],
                uiflood: None,
                uitrace: None,
            },
        );
        builder.digipeater(digi_cfg).build()
    } else {
        builder.build()
    };

    match AprsClient::start(radio, config).await {
        Ok(client) => {
            aprintln!("{}", thd75_repl::output::aprs_mode_active());
            if digipeater {
                aprintln!("Digipeater enabled: WIDE1-1 fill-in.");
            }
            print_aprs_command_list();
            Ok(ReplState::Aprs(Box::new(client)))
        }
        Err(failure) => {
            let (radio, error) = *failure;
            Err(Box::new(
                recover_cat_after_failed_mode_entry(radio, error.into()).await,
            ))
        }
    }
}

/// Prove CAT synchronization after a mode-entry attempt returned its owner.
///
/// A failed proof consumes the still-untrusted connection by disconnecting it,
/// so callers cannot accidentally resume ordinary CAT commands on that stream.
async fn recover_cat_after_failed_mode_entry(
    mut radio: Radio<EitherTransport>,
    entry_error: Box<dyn std::error::Error + Send + Sync>,
) -> (
    Option<Radio<EitherTransport>>,
    Box<dyn std::error::Error + Send + Sync>,
) {
    match radio.restore_cat_after_mode_exit().await {
        Ok(()) => (Some(radio), entry_error),
        Err(recovery_error) => {
            let recovery: Box<dyn std::error::Error + Send + Sync> = Box::new(CleanupFailure {
                operation: entry_error,
                cleanup: recovery_error.into(),
            });
            let failure = match radio.disconnect().await {
                Ok(()) => recovery,
                Err(error) => Box::new(CleanupFailure {
                    operation: recovery,
                    cleanup: error.into(),
                }),
            };
            (None, failure)
        }
    }
}

/// Print the APRS-mode command list, wrapped across two lines so each
/// stays within the accessibility line-length limit (80 characters).
/// Shared by the mode-entry banner and the unrecognized-command hint.
fn print_aprs_command_list() {
    aprintln!("Commands: monitor, msg, position, compressed, mice, object,");
    aprintln!("status, motion, beacon, stations, igate, aprs stop");
}

/// Dispatch a command in APRS mode.
#[expect(
    clippy::too_many_lines,
    reason = "Command dispatch for APRS mode covering listen, monitor, msg, position, beacon, \
              stations, igate, and the aprs-stop transition. Each arm is a small handler; the \
              sum is long but the structure is a flat dispatch table, so splitting helpers would \
              only fragment a single command table across files."
)]
async fn dispatch_aprs(
    client: &mut AprsClient<EitherTransport>,
    cmd: &str,
    parts: &[&str],
) -> AppResult<()> {
    match cmd {
        "listen" | "poll" => match client.next_event().await {
            Ok(Some(event)) => print_aprs_event(&event),
            Ok(None) => aprintln!("No APRS activity."),
            Err(e) => return Err(e.into()),
        },
        "monitor" => {
            aprintln!("Monitoring APRS. Press Ctrl-C to stop.");
            run_aprs_monitor(client).await?;
        }
        "msg" | "message" => {
            if parts.len() < 3 {
                aprintln!("Usage: msg <callsign> <message text>");
                return Ok(());
            }
            let Some(&addressee) = parts.get(1) else {
                return Ok(());
            };
            let text = parts.get(2..).unwrap_or(&[]).join(" ");
            let addressee = match DirectMessageAddressee::new(addressee) {
                Ok(addressee) => addressee,
                Err(error) => {
                    aprintln!("Error: invalid message addressee: {error}.");
                    return Ok(());
                }
            };
            let text = match MessageText::new(&text) {
                Ok(text) => text,
                Err(error) => {
                    aprintln!("Error: invalid message text: {error}.");
                    return Ok(());
                }
            };
            if !commands::confirm_transmit()? {
                return Ok(());
            }
            match client.queue_message(&addressee, &text) {
                Ok(msg_id) => aprintln!("Message queued to {addressee}: {text} (ID: {msg_id})"),
                Err(e) => aprintln!(
                    "{}",
                    thd75_repl::output::error(format_args!("sending message: {e}"))
                ),
            }
        }
        "beacon" => {
            if !commands::confirm_transmit()? {
                return Ok(());
            }
            let text = match StatusText::new("REPL beacon") {
                Ok(text) => text,
                Err(error) => {
                    aprintln!("Error: built-in beacon text is invalid: {error}.");
                    return Ok(());
                }
            };
            match client.send_status(&text).await {
                Ok(()) => aprintln!("Status beacon sent."),
                Err(e) => return Err(e.into()),
            }
        }
        "position" | "pos" => {
            let args =
                match thd75_repl::aprs_args::PositionArgs::parse(parts.get(1..).unwrap_or(&[])) {
                    Ok(a) => a,
                    Err(e) => {
                        aprintln!("Error: {e}");
                        return Ok(());
                    }
                };
            if !commands::confirm_transmit()? {
                return Ok(());
            }
            let (latitude, longitude, comment) =
                (args.pos.latitude, args.pos.longitude, args.comment);
            match client.beacon_position(latitude, longitude, &comment).await {
                Ok(()) => aprintln!(
                    "Position beacon sent: {:.4}, {:.4}{}.",
                    latitude.as_degrees(),
                    longitude.as_degrees(),
                    if comment.as_str().is_empty() {
                        String::new()
                    } else {
                        format!(" ({comment})")
                    }
                ),
                Err(e) => return Err(e.into()),
            }
        }
        "compressed" => {
            let args = match thd75_repl::aprs_args::CompressedPositionArgs::parse(
                parts.get(1..).unwrap_or(&[]),
            ) {
                Ok(a) => a,
                Err(e) => {
                    aprintln!("Error: {e}");
                    return Ok(());
                }
            };
            if !commands::confirm_transmit()? {
                return Ok(());
            }
            match client
                .beacon_position_compressed(args.pos.latitude, args.pos.longitude, &args.comment)
                .await
            {
                Ok(()) => aprintln!(
                    "Compressed position beacon sent: {:.4}, {:.4}.",
                    args.pos.latitude.as_degrees(),
                    args.pos.longitude.as_degrees()
                ),
                Err(e) => return Err(e.into()),
            }
        }
        "mice" => {
            let args = match thd75_repl::aprs_args::MiceArgs::parse(parts.get(1..).unwrap_or(&[])) {
                Ok(a) => a,
                Err(e) => {
                    aprintln!("Error: {e}");
                    return Ok(());
                }
            };
            if !commands::confirm_transmit()? {
                return Ok(());
            }
            match client
                .beacon_position_mice(
                    args.pos.latitude,
                    args.pos.longitude,
                    args.speed,
                    args.course,
                    &args.status_text,
                )
                .await
            {
                Ok(()) => aprintln!(
                    "Mic-E beacon sent: {:.4}, {:.4}, {} knots, course {}.",
                    args.pos.latitude.as_degrees(),
                    args.pos.longitude.as_degrees(),
                    args.speed.as_knots(),
                    args.course.as_degrees()
                ),
                Err(e) => return Err(e.into()),
            }
        }
        "object" => {
            let args = match thd75_repl::aprs_args::ObjectArgs::parse(parts.get(1..).unwrap_or(&[]))
            {
                Ok(a) => a,
                Err(e) => {
                    aprintln!("Error: {e}");
                    return Ok(());
                }
            };
            let now_utc = time::OffsetDateTime::now_utc();
            let timestamp = match AprsReportTimestamp::day_hour_minute_utc(
                now_utc.day(),
                now_utc.hour(),
                now_utc.minute(),
            ) {
                Ok(timestamp) => timestamp,
                Err(error) => {
                    aprintln!("Error: current UTC time cannot be encoded for APRS: {error}");
                    return Ok(());
                }
            };
            if !commands::confirm_transmit()? {
                return Ok(());
            }
            let report = ObjectReport::new(
                args.name,
                ReportState::Live,
                timestamp,
                PositionReport::new(
                    args.pos.latitude,
                    args.pos.longitude,
                    client.config().symbol(),
                    args.comment,
                ),
            );
            match client.send_object(&report).await {
                Ok(()) => aprintln!(
                    "Object {} sent at {:.4}, {:.4}.",
                    report.name(),
                    args.pos.latitude.as_degrees(),
                    args.pos.longitude.as_degrees()
                ),
                Err(e) => return Err(e.into()),
            }
        }
        "status" => {
            if parts.len() < 2 {
                aprintln!("Usage: status <text>");
                aprintln!("Example: status QRV on 144.390");
                return Ok(());
            }
            let text = parts.get(1..).unwrap_or(&[]).join(" ");
            let text = match StatusText::new(&text) {
                Ok(text) => text,
                Err(error) => {
                    aprintln!("Error: invalid status text: {error}.");
                    return Ok(());
                }
            };
            if !commands::confirm_transmit()? {
                return Ok(());
            }
            match client.send_status(&text).await {
                Ok(()) => aprintln!("Status sent: {text}"),
                Err(e) => return Err(e.into()),
            }
        }
        "motion" => {
            let args = match thd75_repl::aprs_args::MotionArgs::parse(parts.get(1..).unwrap_or(&[]))
            {
                Ok(a) => a,
                Err(e) => {
                    aprintln!("Error: {e}");
                    return Ok(());
                }
            };
            if !commands::confirm_transmit()? {
                return Ok(());
            }
            match client
                .update_motion(
                    args.speed,
                    args.heading,
                    args.pos.latitude,
                    args.pos.longitude,
                )
                .await
            {
                Ok(true) => aprintln!("Motion updated. SmartBeaconing transmitted a beacon."),
                Ok(false) => aprintln!("Motion updated. No beacon due yet."),
                Err(e) => return Err(e.into()),
            }
        }
        "stations" | "heard" => {
            let stations = client.stations().recent();
            if stations.is_empty() {
                aprintln!("No stations heard yet.");
            } else {
                for entry in stations.iter().take(20) {
                    let elapsed = commands::fmt_elapsed(entry.last_heard().elapsed());
                    let position = entry.position().map(|p| thd75_repl::aprs_args::LatLon {
                        latitude: p.latitude(),
                        longitude: p.longitude(),
                    });
                    aprintln!(
                        "{}",
                        thd75_repl::output::aprs_station_entry(
                            entry.address(),
                            position,
                            entry.packet_count(),
                            &elapsed,
                        )
                    );
                }
                aprintln!("{}", thd75_repl::output::stations_summary(stations.len()));
            }
        }
        "igate" => {
            if parts.len() < 6 {
                aprintln!(
                    "Usage: igate <filter> <max-repeated-hops> <receiver-rf-minutes> \
                     <sender-rf-minutes> <receiver-internet-minutes>"
                );
                aprintln!("Connects to APRS-IS and bridges RF to internet.");
                aprintln!("Example: igate r/35.30/-82.46/100 2 60 30 30");
                aprintln!("Receive stations within 100km of the given lat/lon.");
                aprintln!("Press Ctrl-C to disconnect.");
                return Ok(());
            }
            let policy_args = parts.get(parts.len().saturating_sub(4)..).unwrap_or(&[]);
            let policy = match parse_igate_policy(policy_args) {
                Ok(policy) => policy,
                Err(error) => {
                    aprintln!("Invalid IGate policy: {error}");
                    return Ok(());
                }
            };
            let filter_end = parts.len().saturating_sub(4);
            let filter = parts.get(1..filter_end).unwrap_or(&[]).join(" ");
            // Starting the bridge authorizes an open-ended sequence of
            // Internet-to-RF transmissions, so require the same explicit RF
            // confirmation as every other transmitting REPL command.
            if !commands::confirm_transmit()? {
                return Ok(());
            }
            client.configure_igate_to_rf(policy);
            run_igate(client, &filter).await?;
        }
        _ => {
            aprintln!("APRS command not recognized: {cmd}.");
            print_aprs_command_list();
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IgateArgument {
    RepeatedHops,
    ReceiverRfMinutes,
    SenderRfMinutes,
    ReceiverInternetMinutes,
}

impl std::fmt::Display for IgateArgument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::RepeatedHops => "maximum repeated hops",
            Self::ReceiverRfMinutes => "receiver RF minutes",
            Self::SenderRfMinutes => "sender RF minutes",
            Self::ReceiverInternetMinutes => "receiver Internet minutes",
        })
    }
}

#[derive(Debug)]
enum IgatePolicyError {
    Missing(IgateArgument),
    Number {
        argument: IgateArgument,
        source: std::num::ParseIntError,
    },
    Overflow(IgateArgument),
    Validation(kenwood_thd75::error::ValidationError),
}

impl std::fmt::Display for IgatePolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(argument) => write!(f, "missing {argument}"),
            Self::Number { argument, source } => write!(f, "invalid {argument}: {source}"),
            Self::Overflow(argument) => write!(f, "{argument} exceeds the representable seconds"),
            Self::Validation(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for IgatePolicyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Number { source, .. } => Some(source),
            Self::Validation(error) => Some(error),
            Self::Missing(_) | Self::Overflow(_) => None,
        }
    }
}

fn parse_igate_policy(args: &[&str]) -> Result<IGateToRfConfig, IgatePolicyError> {
    let hops = args
        .first()
        .ok_or(IgatePolicyError::Missing(IgateArgument::RepeatedHops))?
        .parse::<u8>()
        .map_err(|source| IgatePolicyError::Number {
            argument: IgateArgument::RepeatedHops,
            source,
        })?;
    let minutes =
        |index: usize, argument: IgateArgument| -> Result<std::time::Duration, IgatePolicyError> {
            let minutes = args
                .get(index)
                .ok_or(IgatePolicyError::Missing(argument))?
                .parse::<u64>()
                .map_err(|source| IgatePolicyError::Number { argument, source })?;
            let seconds = minutes
                .checked_mul(60)
                .ok_or(IgatePolicyError::Overflow(argument))?;
            Ok(std::time::Duration::from_secs(seconds))
        };
    IGateToRfConfig::new(
        IGateRfLocality::new(hops).map_err(IgatePolicyError::Validation)?,
        minutes(1, IgateArgument::ReceiverRfMinutes)?,
        minutes(2, IgateArgument::SenderRfMinutes)?,
        minutes(3, IgateArgument::ReceiverInternetMinutes)?,
    )
    .map_err(IgatePolicyError::Validation)
}

/// APRS-IS login callsign (`CALL` or `CALL-SSID`) for the client's
/// station identity, or `None` when APRS was started receive-only.
fn igate_login_call(client: &AprsClient<EitherTransport>) -> Option<String> {
    let station = client.config().source()?;
    let callsign = station.callsign.as_str();
    let ssid = station.ssid.get();
    Some(if ssid > 0 {
        format!("{callsign}-{ssid}")
    } else {
        callsign.to_owned()
    })
}

/// Run the `IGate` bridge: APRS-IS ↔ RF.
///
/// Connects to the default APRS-IS server, forwards received RF packets
/// to IS, and gates appropriate IS packets back to RF. Runs until the
/// user presses Ctrl-C or the connection is lost.
async fn run_igate(client: &mut AprsClient<EitherTransport>, filter: &str) -> AppResult<()> {
    use kenwood_thd75::{AprsIsClient, AprsIsEvent};

    let Some(login_call) = igate_login_call(client) else {
        return Err(IgateFailure::NoStationIdentity.into());
    };

    aprintln!("Connecting to APRS-IS as {login_call}.");
    let is_config = igate_configuration(&login_call, filter)?;
    let mut is_client = AprsIsClient::connect(is_config).await?;
    let mut observed_login = LoginState::Pending;

    aprintln!("{}", thd75_repl::output::aprs_is_connected());

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    let operation: AppResult<()> = loop {
        tokio::select! {
            signal = &mut ctrl_c => {
                aprintln!("IGate stopping.");
                break signal.map_err(Into::into);
            }
            // Poll APRS-IS for incoming packets.
            is_result = is_client.next_event() => {
                match is_result {
                    Ok(AprsIsEvent::Packet(pkt)) => {
                        // `pkt.line` is the lossy UTF-8 view intended only for
                        // display. Gating uses `pkt.raw` so validation and
                        // third-party wrapping operate on the exact wire bytes.
                        aprintln!("{}", thd75_repl::output::aprs_is_incoming(&pkt.line()));
                        // Gate to RF if appropriate. The helper checks
                        // whether the packet should be forwarded per
                        // IGate rules (station heard on RF recently, etc).
                        if let Err(error) = client.gate_from_is(pkt.raw(), std::time::Instant::now()).await {
                            break Err(error.into());
                        }
                    }
                    Ok(AprsIsEvent::Comment(line)) => {
                        tracing::debug!("APRS-IS comment: {line}");
                    }
                    Ok(AprsIsEvent::LoggedIn { server }) => {
                        observed_login = is_client.login_state();
                        match server {
                            Some(s) => aprintln!("APRS-IS login {observed_login} (server {s})."),
                            None => aprintln!("APRS-IS login {observed_login}."),
                        }
                    }
                    Ok(AprsIsEvent::LoginRejected { reason }) => {
                        break Err(IgateFailure::LoginRejected { reason }.into());
                    }
                    Ok(AprsIsEvent::Disconnected) => {
                        break Err(IgateFailure::Disconnected.into());
                    }
                    Err(e) => {
                        break Err(e.into());
                    }
                }
            }
            // Poll RF for incoming packets.
            rf_result = client.next_event() => {
                match rf_result {
                    Ok(maybe_event) => {
                        if let Some(event) = &maybe_event {
                            print_aprs_event(event);
                        }
                        // Gate EVERY packet heard on RF to APRS-IS, not only
                        // the ones that surface as `RawPacket`. Typed events
                        // (PositionReceived, StationHeard, WeatherReceived,
                        // ...) carry the same underlying frame, which
                        // `take_last_rf_packet` exposes for this cycle; it is
                        // `Some` only when this cycle actually received a
                        // frame off the air.
                        if let Some(pkt) = client.take_last_rf_packet()
                            && observed_login == LoginState::Verified
                            && is_client.login_state() == LoginState::Verified
                            && let Err(error) = upload_rf_packet(client, &mut is_client, &pkt).await {
                            break Err(error);
                        }
                    }
                    Err(e) => {
                        break Err(e.into());
                    }
                }
            }
        }

        // Send keepalive if interval elapsed.
        if let Err(e) = is_client.maybe_send_keepalive().await {
            break Err(e.into());
        }
    };
    finish_with_cleanup(operation, is_client.shutdown().await.map_err(Into::into))
}

#[derive(Debug)]
enum IgateFailure {
    NoStationIdentity,
    LoginRejected { reason: aprs_is::LoginRejection },
    Disconnected,
}

impl std::fmt::Display for IgateFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoStationIdentity => f.write_str("the IGate requires a station identity"),
            Self::LoginRejected { reason } => write!(f, "APRS-IS login rejected: {reason}"),
            Self::Disconnected => f.write_str("APRS-IS disconnected"),
        }
    }
}

impl std::error::Error for IgateFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::LoginRejected { reason } => Some(reason),
            Self::NoStationIdentity | Self::Disconnected => None,
        }
    }
}

fn igate_configuration(
    login: &str,
    filter: &str,
) -> Result<kenwood_thd75::AprsIsConfig, kenwood_thd75::AprsIsError> {
    kenwood_thd75::AprsIsConfig::new(login)?
        .with_software("kenwood-thd75", env!("CARGO_PKG_VERSION"))?
        .with_raw_filter(filter)
}

async fn upload_rf_packet(
    client: &AprsClient<EitherTransport>,
    network: &mut kenwood_thd75::AprsIsClient,
    packet: &kenwood_thd75::Ax25Packet,
) -> AppResult<()> {
    match client.format_packet_for_aprs_is(packet, IgateMessaging::Bidirectional) {
        Ok(line) => network.send_uplink_line(&line).await?,
        Err(kenwood_thd75::IGateFormatError::GatingRefused(reason)) => {
            tracing::debug!(%reason, "RF packet is not eligible for APRS-IS");
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Continuous APRS monitoring loop. Polls for events and prints them.
/// Exits on Ctrl-C.
async fn run_aprs_monitor(client: &mut AprsClient<EitherTransport>) -> AppResult<()> {
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            signal = &mut ctrl_c => {
                signal?;
                aprintln!("Monitor stopped. Type monitor to resume, or help for commands.");
                break;
            }
            result = client.next_event() => {
                match result {
                    Ok(Some(event)) => print_aprs_event(&event),
                    Ok(None) => {}
                    Err(e) => {
                        return Err(e.into());
                    }
                }
            }
        }
    }
    Ok(())
}

/// Format and print an APRS event as screen-reader-friendly text.
fn print_aprs_event(event: &AprsEvent) {
    match event {
        AprsEvent::StationHeard(entry) => {
            aprintln!(
                "{}",
                thd75_repl::output::aprs_station_heard(entry.address())
            );
        }
        AprsEvent::MessageReceived(msg) => {
            aprintln!("{}", thd75_repl::output::aprs_message_received(msg));
        }
        AprsEvent::MessageDelivered(id) => {
            aprintln!("{}", thd75_repl::output::aprs_message_delivered(id));
        }
        AprsEvent::MessageRejected(id) => {
            aprintln!("{}", thd75_repl::output::aprs_message_rejected(id));
        }
        AprsEvent::MessageExpired(id) => {
            aprintln!("{}", thd75_repl::output::aprs_message_expired(id));
        }
        AprsEvent::PositionReceived { source, position } => {
            aprintln!(
                "{}",
                thd75_repl::output::aprs_position(
                    source,
                    thd75_repl::aprs_args::LatLon {
                        latitude: position.latitude(),
                        longitude: position.longitude(),
                    }
                )
            );
        }
        AprsEvent::WeatherReceived { source, .. } => {
            aprintln!("{}", thd75_repl::output::aprs_weather(source));
        }
        AprsEvent::PacketDigipeated { source } => {
            if !thd75_repl::is_verbose() {
                return;
            }
            aprintln!("{}", thd75_repl::output::aprs_digipeated(source));
        }
        AprsEvent::DigipeaterCapacityExceeded { queue } => {
            aprintln!("Warning: digipeater {queue} capacity reached; packet was not relayed.");
        }
        AprsEvent::QueryResponded { to } => {
            if !thd75_repl::is_verbose() {
                return;
            }
            aprintln!("{}", thd75_repl::output::aprs_query_responded(to));
        }
        AprsEvent::RawPacket(pkt) => {
            if !thd75_repl::is_verbose() {
                return;
            }
            aprintln!(
                "{}",
                thd75_repl::output::aprs_raw_packet(&pkt.header.source)
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Persistent binary-mode exit
// ---------------------------------------------------------------------------

/// Guide the operator out of Reflector Terminal Mode and reconnect.
///
/// Prints the link guidance, waits for the operator to switch Menu 650
/// off, then reopens the retained transport and re-proves CAT identity/firmware.
/// EOF, input failure or failed reconnect stops this operation without discovery.
async fn guide_exit_terminal_mode<T: kenwood_transport::Transport>(
    radio: Radio<T>,
) -> AppResult<RestoredCat<T>> {
    aprintln!("{}", LinkDiagnosis::MmdvmMode.guidance());
    aprintln!("Set Menu 650 to Off on the radio, then press Enter.");
    let input = thd75_repl::take_output_error()
        .map_or_else(|| read_guidance_response(&mut std::io::stdin().lock()), Err);
    finish_terminal_exit(radio, input).await
}

/// Reopened connection and the identity read from that connection.
struct RestoredCat<T: kenwood_transport::Transport> {
    radio: Radio<T>,
    identity: kenwood_thd75::radio::RadioInfo,
    firmware: FirmwareIdentity,
}

#[derive(Debug)]
struct StartupDiagnosis(LinkDiagnosis);

impl std::fmt::Display for StartupDiagnosis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.guidance())
    }
}

impl std::error::Error for StartupDiagnosis {}

/// Reconnect the retained connection only after a complete input response.
async fn finish_terminal_exit<T: kenwood_transport::Transport>(
    mut radio: Radio<T>,
    response: std::io::Result<()>,
) -> AppResult<RestoredCat<T>> {
    if let Err(error) = response {
        return finish_with_cleanup(
            Err(error.into()),
            radio.disconnect().await.map_err(Into::into),
        );
    }
    aprintln!("Reconnecting to the selected radio.");
    let reconnect: AppResult<_> = async {
        radio.reconnect().await?;
        let identity = radio.identify().await?;
        let firmware = radio.get_firmware_version().await?;
        let _serial = radio.get_serial_information().await?;
        Ok((identity, firmware))
    }
    .await;
    let (identity, firmware) = match reconnect {
        Ok(identity) => identity,
        Err(error) => {
            return finish_with_cleanup(Err(error), radio.disconnect().await.map_err(Into::into));
        }
    };
    aprintln!("Radio restored to CAT control mode.");
    Ok(RestoredCat {
        radio,
        identity,
        firmware,
    })
}

fn read_guidance_response(reader: &mut impl std::io::BufRead) -> std::io::Result<()> {
    let mut line = String::new();
    let mut bounded = std::io::Read::take(reader, 65);
    if std::io::BufRead::read_line(&mut bounded, &mut line)? == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "input ended before the radio-mode change; no reconnect was attempted",
        ));
    }
    if line.len() > 64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "radio-mode response exceeds 64 bytes; no reconnect was attempted",
        ));
    }
    Ok(())
}

/// Clear the DV Gateway flag over the connected port, then return.
///
/// Connects a [`Radio`], writes the gateway-mode byte to `Off`, and
/// reports the outcome. Used by `--set-gateway-off` to leave Reflector
/// Terminal Mode with no radio keypresses: run it over USB while the
/// radio's terminal mode is bound to Bluetooth (Menu 985), where the
/// programming handshake is still routed. The radio reboots into
/// normal control after the write, so the link is expected to drop.
///
/// # Errors
///
/// Retains typed identity, firmware, MCP and connection-close failures. A
/// handshake timeout leaves the cause unverified; it is not a routing proof.
async fn run_set_gateway_off<T: kenwood_transport::Transport>(transport: T) -> AppResult<()> {
    // This path is selected specifically because CAT is already alive
    // on the gateway's unused interface. Do not use `connect_with_tnc_exit`
    // here: its recovery preamble writes TN 0,0 and would change the
    // very state we need to inspect before the MCP write.
    let mut radio = Radio::new(transport);
    let target = match inspect_gateway_target(&mut radio).await {
        Ok(target) => target,
        Err(error) => {
            let cleanup = radio.disconnect().await.map_err(Into::into);
            return finish_with_cleanup(Err(error), cleanup);
        }
    };
    let GatewayTarget { firmware, mode } = target;
    if let Some(gateway) = mode {
        aprintln!("Verified TH-D75 firmware {firmware}; Menu 650 is {gateway}.");
        if gateway == DvGatewayMode::Off {
            radio.disconnect().await?;
            aprintln!("DV Gateway is already off; no memory write or reboot was needed.");
            return Ok(());
        }
    } else {
        aprintln!(
            "Verified TH-D75 firmware {firmware}; checking Menu 650 through its verified MCP byte."
        );
    }

    aprintln!("Clearing Menu 650 via its firmware-verified MCP byte.");
    let process_signals = match ProcessSignalRouter::install() {
        Ok(signals) => signals,
        Err(error) => {
            let cleanup = radio.disconnect().await.map_err(Into::into);
            return finish_with_cleanup(Err(error.into()), cleanup);
        }
    };
    let mut critical_signals = match process_signals.begin_critical() {
        Ok(signals) => signals,
        Err(error) => {
            let cleanup = radio.disconnect().await.map_err(Into::into);
            return finish_with_cleanup(Err(error.into()), cleanup);
        }
    };
    let operation = clear_gateway_mode_with_interrupt(&mut radio, critical_signals.recv()).await;
    let late_signal = critical_signals.finish().err();

    match operation {
        InterruptibleMcpOperation::Completed(Ok(update)) if late_signal.is_none() => {
            finish_gateway_clear(radio, update).await
        }
        InterruptibleMcpOperation::Completed(Ok(update)) => {
            let signal = late_signal.ok_or(SignalError::RouterStopped)?;
            let cleanup = finish_gateway_clear(radio, update).await;
            finish_with_cleanup(Err(signal.into()), cleanup)
        }
        InterruptibleMcpOperation::Completed(Err(error)) => {
            gateway_clear_failure(radio, error, late_signal).await
        }
        InterruptibleMcpOperation::Interrupted { signal, completion } => match completion {
            Ok(update) => {
                let signal = signal.map_or_else(|error| error, SignalError::Received);
                let cleanup = finish_gateway_clear(radio, update).await;
                finish_with_cleanup(Err(signal.into()), cleanup)
            }
            Err(error) => {
                let interruption = signal.map_or_else(|error| error, SignalError::Received);
                gateway_clear_failure(radio, error, Some(interruption)).await
            }
        },
    }
}

#[derive(Debug)]
struct GatewayTarget {
    firmware: FirmwareIdentity,
    mode: Option<DvGatewayMode>,
}

/// Read only the firmware-supported CAT queries before MCP entry.
async fn inspect_gateway_target<T: kenwood_transport::Transport>(
    radio: &mut Radio<T>,
) -> AppResult<GatewayTarget> {
    let identity = radio.identify().await?;
    let firmware = radio.get_firmware_version().await?;
    validate_gateway_mcp_target(identity.model, &firmware)?;
    let _serial = radio.get_serial_information().await?;
    let mode = if FirmwareProfile::from_identity(&firmware).supports_bare_gateway() {
        Some(radio.read_gateway().await?)
    } else {
        None
    };
    Ok(GatewayTarget { firmware, mode })
}

async fn finish_gateway_clear<T: kenwood_transport::Transport>(
    radio: Radio<T>,
    update: DetachedMcpPageUpdate,
) -> AppResult<()> {
    match update {
        DetachedMcpPageUpdate::UnchangedCatReady => {
            radio.disconnect().await?;
            aprintln!("DV Gateway is already off; no memory write or reboot was needed.");
        }
        DetachedMcpPageUpdate::ChangedRadioRebooting => {
            radio.disconnect().await?;
            aprintln!(
                "DV Gateway flag clear was read-back verified. The radio is rebooting into normal \
                 control mode; reconnect for CAT or APRS once it finishes."
            );
        }
    }
    Ok(())
}

async fn gateway_clear_failure<T: kenwood_transport::Transport>(
    radio: Radio<T>,
    error: kenwood_thd75::Error,
    interruption: Option<SignalError>,
) -> AppResult<()> {
    let requires_recovery = error.requires_recovery() || radio.cat_recovery_required();
    let close_error = radio.disconnect().await.err();
    Err(Box::new(GatewayClearFailure {
        operation: error,
        close_error,
        interruption,
        requires_recovery,
    }))
}

#[derive(Debug)]
struct GatewayClearFailure {
    operation: kenwood_thd75::Error,
    close_error: Option<kenwood_thd75::Error>,
    interruption: Option<SignalError>,
    requires_recovery: bool,
}

impl std::fmt::Display for GatewayClearFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(signal) = &self.interruption {
            write!(f, "{signal}; ")?;
        }
        write!(f, "Menu 650 update failed: {}.", self.operation)?;
        if let Some(error) = &self.close_error {
            write!(f, " Connection close also failed: {error}.")?;
        }
        if self.requires_recovery {
            f.write_str(" The binary boundary requires recovery; send no further commands before a full radio power cycle.")
        } else {
            f.write_str(" Reconnect through the CAT-capable interface before another operation.")
        }
    }
}

impl std::error::Error for GatewayClearFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.operation)
    }
}

/// Completed MCP result or an interruption together with its completed result.
///
/// An interruption keeps the bounded model operation alive until it returns;
/// callers retain both the signal and the operation's partial-progress error.
#[derive(Debug)]
enum InterruptibleMcpOperation {
    Completed(Result<DetachedMcpPageUpdate, kenwood_thd75::Error>),
    Interrupted {
        signal: Result<ProcessSignal, SignalError>,
        completion: Result<DetachedMcpPageUpdate, kenwood_thd75::Error>,
    },
}

/// Clear Menu 650 while avoiding a write and detached reboot when its byte is
/// already zero.
async fn clear_gateway_mode_with_interrupt<T: kenwood_transport::Transport, I>(
    radio: &mut Radio<T>,
    interrupt: I,
) -> InterruptibleMcpOperation
where
    I: Future<Output = Result<ProcessSignal, SignalError>>,
{
    // The schema target was proved by this flow's own identity reads; the
    // library owns the registry-pinned Menu 650 offset.
    let operation = radio.disable_dv_gateway_detached_unverified();
    tokio::pin!(operation);
    tokio::pin!(interrupt);
    tokio::select! {
        biased;
        result = &mut operation => InterruptibleMcpOperation::Completed(result),
        signal = &mut interrupt => {
            // Latch the signal but do not drop a binary exchange at an
            // arbitrary byte boundary. The bounded library operation owns
            // cleanup and its exact result remains authoritative.
            let completion = operation.await;
            InterruptibleMcpOperation::Interrupted { signal, completion }
        },
    }
}

fn validate_gateway_mcp_target(
    model: RadioModel,
    firmware: &FirmwareIdentity,
) -> Result<(), GatewayTargetError> {
    if is_supported_mcp_d75_schema_target(model, firmware) {
        return Ok(());
    }
    Err(GatewayTargetError {
        model,
        firmware: firmware.clone(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GatewayTargetError {
    model: RadioModel,
    firmware: FirmwareIdentity,
}

impl std::fmt::Display for GatewayTargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refusing the Menu 650 update for {} firmware {}: the MCP schema supports \
            vendor firmware {MCP_D75_SCHEMA_FIRMWARE}; accepted exact CAT FV identities are \
            {MCP_D75_SCHEMA_FIRMWARE_IDENTITIES:?}",
            self.model, self.firmware
        )
    }
}

impl std::error::Error for GatewayTargetError {}

/// Validate all D-STAR startup input before any radio operation can occur.
fn parse_dstar_start(args: &[&str]) -> Result<(), DstarStartError> {
    let Some(raw_callsign) = args.first() else {
        return Err(DstarStartError::Usage);
    };
    if args.len() > 2 {
        return Err(DstarStartError::Usage);
    }

    // Callsigns are upper-case on D-STAR headers and DPlus authentication.
    let callsign = raw_callsign.to_ascii_uppercase();
    let modem_config = DstarModemConfig::new(&callsign).map_err(DstarStartError::Identity)?;
    let _routing =
        RoutingCallsign::try_from(modem_config.callsign()).map_err(DstarStartError::Routing)?;
    let _link = args
        .get(1)
        .map(|reflector| reflector.to_ascii_uppercase())
        .as_deref()
        .map(parse_link_arg)
        .transpose()?;

    Ok(())
}

/// Reject an unsupported Kenwood modem profile before discovery or radio I/O.
fn preflight_dstar_start(args: &[&str]) -> Result<(), DstarStartError> {
    parse_dstar_start(args)?;
    let _configuration = DstarGateway::<EitherTransport, PersistentMmdvm>::wire_configuration()
        .map_err(DstarStartError::Configuration)?;
    Err(DstarStartError::RuntimeUnavailable)
}

/// Check known unsupported startup operations before opening a connection.
fn preflight_startup_command(command: &str) -> Result<(), DstarStartError> {
    let parts: Vec<_> = command.split_whitespace().collect();
    if parts
        .first()
        .is_some_and(|word| word.eq_ignore_ascii_case("dstar"))
        && parts
            .get(1)
            .is_some_and(|word| word.eq_ignore_ascii_case("start"))
    {
        preflight_dstar_start(parts.get(2..).unwrap_or(&[]))?;
    }
    Ok(())
}

#[derive(Debug)]
enum DstarStartError {
    Usage,
    Identity(mmdvm::dstar::DstarConfigError),
    Routing(dstar_gateway_core::TypeError),
    ReflectorSyntax,
    ReflectorName(dstar_gateway_core::hosts::HostNameError),
    Configuration(mmdvm::ConfigurationError),
    RuntimeUnavailable,
}

impl std::fmt::Display for DstarStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage => f.write_str("usage: dstar start <callsign> [reflector]"),
            Self::Identity(error) => write!(f, "invalid station callsign: {error}"),
            Self::Routing(error) => write!(f, "invalid routing identity or module: {error}"),
            Self::ReflectorSyntax => f.write_str("invalid reflector; use REF030C or B:REF030C"),
            Self::ReflectorName(error) => write!(f, "invalid reflector name: {error}"),
            Self::Configuration(error) => write!(f, "D-STAR startup unavailable: {error}"),
            Self::RuntimeUnavailable => f.write_str("this binary has no Kenwood D-STAR runtime"),
        }
    }
}

impl std::error::Error for DstarStartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Identity(error) => Some(error),
            Self::Routing(error) => Some(error),
            Self::ReflectorName(error) => Some(error),
            Self::Configuration(error) => Some(error),
            Self::Usage | Self::ReflectorSyntax | Self::RuntimeUnavailable => None,
        }
    }
}

/// Validate reflector routing identity and optional local-module prefix.
fn parse_link_arg(s: &str) -> Result<(), DstarStartError> {
    let (local_prefix, refl_str) = if let Some((left, right)) = s.split_once(':') {
        (Some(left), right)
    } else {
        (None, s)
    };
    let name_end = refl_str
        .len()
        .checked_sub(1)
        .ok_or(DstarStartError::ReflectorSyntax)?;
    let name = refl_str
        .get(..name_end)
        .filter(|name| !name.is_empty())
        .ok_or(DstarStartError::ReflectorSyntax)?;
    let module = refl_str
        .get(name_end..)
        .and_then(|value| value.chars().next())
        .ok_or(DstarStartError::ReflectorSyntax)?;
    let _catalog_name =
        dstar_gateway_core::hosts::HostName::new(name).map_err(DstarStartError::ReflectorName)?;
    let _routing = RoutingCallsign::new(name).map_err(DstarStartError::Routing)?;
    let _reflector_module = Module::try_from_char(module).map_err(DstarStartError::Routing)?;
    if let Some(prefix) = local_prefix {
        if prefix.len() != 1 {
            return Err(DstarStartError::ReflectorSyntax);
        }
        let module = prefix
            .chars()
            .next()
            .ok_or(DstarStartError::ReflectorSyntax)?;
        let _local_module = Module::try_from_char(module).map_err(DstarStartError::Routing)?;
    }
    Ok(())
}

#[cfg(test)]
mod offset_tests {
    use super::parse_utc_offset;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn parses_plus_hhmm_colon() -> TestResult {
        assert_eq!(
            parse_utc_offset("+05:30")?.whole_seconds(),
            5 * 3600 + 30 * 60
        );
        Ok(())
    }

    #[test]
    fn parses_minus_hhmm_colon() -> TestResult {
        assert_eq!(parse_utc_offset("-08:00")?.whole_seconds(), -8 * 3600);
        Ok(())
    }

    #[test]
    fn parses_plus_hhmm_no_colon() -> TestResult {
        assert_eq!(
            parse_utc_offset("+0530")?.whole_seconds(),
            5 * 3600 + 30 * 60
        );
        Ok(())
    }

    #[test]
    fn parses_plus_h() -> TestResult {
        assert_eq!(parse_utc_offset("+5")?.whole_seconds(), 5 * 3600);
        Ok(())
    }

    #[test]
    fn parses_no_sign_positive() -> TestResult {
        assert_eq!(parse_utc_offset("03:00")?.whole_seconds(), 3 * 3600);
        Ok(())
    }

    #[test]
    fn rejects_out_of_range_hours() {
        assert!(parse_utc_offset("+15:00").is_err());
    }

    #[test]
    fn rejects_out_of_range_minutes() {
        assert!(parse_utc_offset("+02:60").is_err());
    }

    #[test]
    fn rejects_empty() {
        assert!(parse_utc_offset("").is_err());
    }

    #[test]
    fn parses_zero_offset() -> TestResult {
        assert_eq!(parse_utc_offset("+00:00")?.whole_seconds(), 0);
        assert_eq!(parse_utc_offset("-00:00")?.whole_seconds(), 0);
        Ok(())
    }

    #[test]
    fn rejects_non_ascii_without_panicking() {
        // A multi-byte character where the HHMM split lands mid-char
        // must be a plain error, never a byte-slice panic.
        assert!(parse_utc_offset("\u{e9}5").is_err());
        assert!(parse_utc_offset("+\u{e9}\u{e9}").is_err());
    }
}

#[cfg(test)]
mod dstar_start_tests {
    use super::parse_dstar_start;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn startup_uses_one_validated_identity_for_modem_and_network() -> TestResult {
        parse_dstar_start(&["n0call", "ref030c"])?;
        parse_dstar_start(&["n0call", "b:ref030c"])?;
        Ok(())
    }

    #[test]
    fn startup_rejects_invalid_identity_before_preparing_the_radio() {
        for callsign in [
            "",
            "        ",
            "N0,CALL",
            "N0\x0DCALL",
            "N0\nCALL",
            "NØCALL",
            "N0CALL   ",
        ] {
            assert!(
                parse_dstar_start(&[callsign]).is_err(),
                "invalid station identity accepted: {callsign:?}"
            );
        }
    }

    #[test]
    fn startup_rejects_bad_arguments_and_reflector() {
        for arguments in [
            &[][..],
            &["N0CALL", "REF030C", "extra"][..],
            &["N0CALL", "REF030?"][..],
        ] {
            assert!(
                parse_dstar_start(arguments).is_err(),
                "invalid startup arguments accepted: {arguments:?}"
            );
        }
    }

    #[test]
    fn identity_error_retains_the_typed_validation_failure() -> TestResult {
        let error = parse_dstar_start(&["N0\x0DCALL"])
            .err()
            .ok_or("control byte must fail identity validation")?;
        assert!(
            matches!(
                error,
                super::DstarStartError::Identity(mmdvm::dstar::DstarConfigError::InvalidByte {
                    byte: 13,
                    ..
                })
            ),
            "{error:?}"
        );
        Ok(())
    }
}

#[cfg(test)]
mod gateway_off_tests {
    use super::{
        InterruptibleMcpOperation, PROCESS_SIGNAL_EXIT_CODE, ProcessSignal, ProcessSignalRouter,
        clear_gateway_mode_with_interrupt, route_process_signal, run_set_gateway_off,
        validate_gateway_mcp_target,
    };

    /// Wire pins for Menu 650 scripts below. Production offsets
    /// are registry-pinned inside `kenwood-thd75::radio::terminal_mode`.
    /// Page containing Menu 650.
    const GATEWAY_MODE_PAGE: u16 = 0x1C;
    /// Byte index of the gateway mode value within its page.
    const GATEWAY_MODE_BYTE: usize = 0xA0;
    use kenwood_thd75::memory::MCP_D75_SCHEMA_FIRMWARE_IDENTITIES;
    use kenwood_thd75::protocol::programming;
    use kenwood_thd75::transport::EitherTransport;
    use kenwood_thd75::{FirmwareIdentity, Radio, RadioModel};
    use kenwood_transport::MockTransport;
    use std::sync::atomic::Ordering;

    type TestResult = super::AppResult<()>;
    static SIGNAL_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[derive(Debug)]
    struct RecordedMock {
        inner: MockTransport,
        writes: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    }

    impl kenwood_transport::Transport for RecordedMock {
        async fn write(&mut self, bytes: &[u8]) -> Result<(), kenwood_transport::TransportError> {
            self.writes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(bytes.to_vec());
            self.inner.write(bytes).await
        }

        async fn read(
            &mut self,
            buffer: &mut [u8],
        ) -> Result<usize, kenwood_transport::TransportError> {
            self.inner.read(buffer).await
        }

        async fn close(&mut self) -> Result<(), kenwood_transport::TransportError> {
            self.inner.assert_complete();
            self.inner.close().await
        }
    }

    fn page_response(page: u16, data: &[u8; programming::PAGE_SIZE]) -> Vec<u8> {
        let [page_hi, page_lo] = page.to_be_bytes();
        let mut response = Vec::with_capacity(programming::W_RESPONSE_SIZE);
        response.extend_from_slice(&[b'W', page_hi, page_lo, 0, 0]);
        response.extend_from_slice(data);
        response
    }

    #[tokio::test]
    async fn critical_signal_guard_detects_a_signal_racing_with_finish() -> TestResult {
        let _test_lock = SIGNAL_TEST_LOCK.lock().await;
        PROCESS_SIGNAL_EXIT_CODE.store(0, Ordering::Release);
        let signals = ProcessSignalRouter::disconnected_for_test();
        let guard = signals.begin_critical()?;
        route_process_signal(ProcessSignal::Interrupt, &signals.critical_slot);
        let finish_error = guard
            .finish()
            .err()
            .ok_or("queued critical signal was lost during guard finish")?;
        let exit_code = PROCESS_SIGNAL_EXIT_CODE.swap(0, Ordering::AcqRel);
        assert!(finish_error.to_string().contains("interrupt signal"));
        assert_eq!(exit_code, 130);
        Ok(())
    }

    #[tokio::test]
    async fn repeated_critical_signals_coalesce_and_retain_the_first_exit_status() -> TestResult {
        let _test_lock = SIGNAL_TEST_LOCK.lock().await;
        PROCESS_SIGNAL_EXIT_CODE.store(0, Ordering::Release);
        let signals = ProcessSignalRouter::disconnected_for_test();
        let guard = signals.begin_critical()?;
        route_process_signal(ProcessSignal::Interrupt, &signals.critical_slot);
        for _ in 0..1024 {
            route_process_signal(ProcessSignal::Terminate, &signals.critical_slot);
        }
        assert_eq!(
            guard.receiver.len(),
            1,
            "repeated signals must use bounded storage"
        );
        let finish = guard.finish();
        let exit_code = PROCESS_SIGNAL_EXIT_CODE.swap(0, Ordering::AcqRel);
        assert!(
            matches!(
                finish,
                Err(super::SignalError::Received(ProcessSignal::Interrupt))
            ),
            "{finish:?}"
        );
        assert_eq!(
            exit_code, 130,
            "later signals must not overwrite the first one"
        );
        Ok(())
    }

    #[test]
    fn gateway_target_uses_exact_central_firmware_identities() -> TestResult {
        for firmware in MCP_D75_SCHEMA_FIRMWARE_IDENTITIES {
            let firmware = FirmwareIdentity::new(firmware)?;
            validate_gateway_mcp_target(RadioModel::ThD75, &firmware)?;
        }
        for firmware in ["1.03.001", "1.04", "1.03.0"] {
            let identity = FirmwareIdentity::new(firmware)?;
            let error = validate_gateway_mcp_target(RadioModel::ThD75, &identity)
                .err()
                .ok_or("unsupported gateway firmware unexpectedly accepted")?;
            assert!(
                error.to_string().contains("1.03")
                    && error.to_string().contains("1.03.000")
                    && error.to_string().contains(firmware),
                "gateway refusal lost accepted or actual identity: {error}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn refuses_wrong_model_before_firmware_query() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\x0D", b"ID TH-D74\x0D");

        let result = run_set_gateway_off(EitherTransport::Mock(mock)).await;
        let error = match result {
            Ok(()) => return Err("wrong-model gateway write unexpectedly succeeded".into()),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(
            message.contains("expected exact TH-D75") && message.contains("TH-D74"),
            "wrong-model refusal lost its safety context: {message}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn refuses_wrong_firmware_before_gateway_query() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");
        mock.expect(b"FV\x0D", b"FV 1.04\x0D");

        let result = run_set_gateway_off(EitherTransport::Mock(mock)).await;
        let error = match result {
            Ok(()) => return Err("wrong-firmware gateway write unexpectedly succeeded".into()),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(
            message.contains("vendor firmware 1.03")
                && message.contains("1.03.000")
                && message.contains("1.04"),
            "wrong-firmware refusal lost its safety context: {message}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn already_off_disconnects_without_memory_write() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");
        mock.expect(b"FV\x0D", b"FV 1.03\x0D");
        mock.expect(b"AE\r", b"AE SYN00001,K01\r");
        mock.expect(b"GW\x0D", b"GW 0\x0D");

        run_set_gateway_off(EitherTransport::Mock(mock)).await?;
        Ok(())
    }

    #[tokio::test]
    async fn active_gateway_is_cleared_with_verified_memory_write() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");
        mock.expect(b"FV\x0D", b"FV 1.03.000\x0D");
        mock.expect(b"AE\r", b"AE SYN00001,K01\r");
        mock.expect(b"GW\x0D", b"GW 1\x0D");
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);

        let mut original = [0x5A; programming::PAGE_SIZE];
        original[GATEWAY_MODE_BYTE] = 1;
        let read_command =
            programming::build_read_command(programming::McpPage::new(GATEWAY_MODE_PAGE)?);
        let original_response = page_response(GATEWAY_MODE_PAGE, &original);
        mock.expect(&read_command, &original_response);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let mut modified = original;
        modified[GATEWAY_MODE_BYTE] = 0;
        let write_command = programming::build_write_command(
            programming::WritableMcpPage::new(GATEWAY_MODE_PAGE)?,
            &modified,
        );
        mock.expect(&write_command, &[programming::ACK]);
        let modified_response = page_response(GATEWAY_MODE_PAGE, &modified);
        mock.expect(&read_command, &modified_response);
        mock.expect(&[programming::ACK], &[programming::ACK]);
        mock.expect(&[programming::EXIT], &[programming::ACK]);

        let writes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        run_set_gateway_off(RecordedMock {
            inner: mock,
            writes: writes.clone(),
        })
        .await?;
        let observed = writes.lock().map_err(|_| "write record lock poisoned")?;
        assert!(
            observed.contains(&write_command),
            "the verified page write was skipped"
        );
        assert_eq!(
            observed.last().map(Vec::as_slice),
            Some(&[programming::EXIT][..]),
            "the successful operation must complete its binary exit"
        );
        drop(observed);
        Ok(())
    }

    #[tokio::test]
    async fn azimuth_already_off_uses_mcp_read_without_gw_or_page_write() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");
        mock.expect(b"FV\x0D", b"FV 1.03.AZM\x0D");
        mock.expect(b"AE\r", b"AE SYN00001,K01\r");
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);

        let mut original = [0x5A; programming::PAGE_SIZE];
        original[GATEWAY_MODE_BYTE] = 0;
        let read_command =
            programming::build_read_command(programming::McpPage::new(GATEWAY_MODE_PAGE)?);
        mock.expect(&read_command, page_response(GATEWAY_MODE_PAGE, &original));
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // An unchanged page takes the normal MCP exit/reconnect path. There
        // is deliberately no page write and no GW exchange in this script.
        mock.expect(&[programming::EXIT], &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");
        mock.expect(b"AE\r", b"AE SYN00001,K01\r");

        run_set_gateway_off(EitherTransport::Mock(mock)).await?;
        Ok(())
    }

    #[tokio::test]
    async fn azimuth_active_gateway_is_cleared_without_gw_query() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");
        mock.expect(b"FV\x0D", b"FV 1.03.AZM\x0D");
        mock.expect(b"AE\r", b"AE SYN00001,K01\r");
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);

        let mut original = [0x5A; programming::PAGE_SIZE];
        original[GATEWAY_MODE_BYTE] = 1;
        let read_command =
            programming::build_read_command(programming::McpPage::new(GATEWAY_MODE_PAGE)?);
        mock.expect(&read_command, page_response(GATEWAY_MODE_PAGE, &original));
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let mut modified = original;
        modified[GATEWAY_MODE_BYTE] = 0;
        let write_command = programming::build_write_command(
            programming::WritableMcpPage::new(GATEWAY_MODE_PAGE)?,
            &modified,
        );
        mock.expect(&write_command, &[programming::ACK]);
        mock.expect(&read_command, page_response(GATEWAY_MODE_PAGE, &modified));
        mock.expect(&[programming::ACK], &[programming::ACK]);
        mock.expect(&[programming::EXIT], &[programming::ACK]);

        run_set_gateway_off(EitherTransport::Mock(mock)).await?;
        Ok(())
    }

    #[tokio::test]
    async fn interrupted_gateway_write_reports_ambiguous_partial_progress() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);

        let mut original = [0x5A; programming::PAGE_SIZE];
        original[GATEWAY_MODE_BYTE] = 1;
        let read_command =
            programming::build_read_command(programming::McpPage::new(GATEWAY_MODE_PAGE)?);
        mock.expect(&read_command, page_response(GATEWAY_MODE_PAGE, &original));
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Let the write ACK complete, then hang its verification read so
        // termination lands after the Menu 650 byte may have changed.
        let mut modified = original;
        modified[GATEWAY_MODE_BYTE] = 0;
        let write_command = programming::build_write_command(
            programming::WritableMcpPage::new(GATEWAY_MODE_PAGE)?,
            &modified,
        );
        mock.expect(&write_command, &[programming::ACK]);
        mock.expect_hang(&read_command);

        let mut radio = Radio::new(EitherTransport::Mock(mock));
        let interrupt = async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            Ok(ProcessSignal::Interrupt)
        };
        let InterruptibleMcpOperation::Interrupted {
            signal: Ok(ProcessSignal::Interrupt),
            completion: Err(error),
        } = clear_gateway_mode_with_interrupt(&mut radio, interrupt).await
        else {
            return Err("interrupted gateway write lost its signal or failure outcome".into());
        };
        assert!(
            error.requires_recovery()
                && error.to_string().contains("possibly written pages: [")
                && error.to_string().contains("verified pages: []")
                && error.to_string().contains("wire boundary is ambiguous"),
            "interruption outcome lost partial-write or ambiguous-boundary state: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn interrupted_gateway_read_with_unproved_boundary_requires_recovery() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);

        let read_command =
            programming::build_read_command(programming::McpPage::new(GATEWAY_MODE_PAGE)?);
        mock.expect_hang(&read_command);

        let mut radio = Radio::new(EitherTransport::Mock(mock));
        let interrupt = async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            Ok(ProcessSignal::Interrupt)
        };
        let InterruptibleMcpOperation::Interrupted {
            signal: Ok(ProcessSignal::Interrupt),
            completion: Err(error),
        } = clear_gateway_mode_with_interrupt(&mut radio, interrupt).await
        else {
            return Err("interrupted terminal read lost its signal or failure outcome".into());
        };
        assert!(
            error.requires_recovery()
                && error.to_string().contains("possibly written pages: []")
                && error.to_string().contains("wire boundary is ambiguous"),
            "unproved-boundary outcome was incomplete: {error}"
        );
        Ok(())
    }
}
