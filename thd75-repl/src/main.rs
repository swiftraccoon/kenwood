//! Accessible command-line REPL for the Kenwood TH-D75 transceiver.
//!
//! Designed for screen-reader compatibility: all output is plain text,
//! one self-contained line at a time. No box-drawing, no cursor
//! repositioning, no escape sequences beyond basic color (which screen
//! readers ignore).
//!
//! Also works as a scripting interface: pipe commands via stdin.
//!
//! # Modes
//!
//! The REPL operates in one of three modes:
//!
//! - **CAT mode** (default): Direct radio control via CAT commands.
//! - **APRS mode**: Entered via `aprs start`. The radio switches to KISS
//!   TNC mode and APRS events (stations heard, messages) are announced
//!   as they arrive. Exit with `aprs stop`.
//! - **D-STAR mode**: Entered via `dstar start`. The radio switches to
//!   MMDVM mode and D-STAR events (voice, text messages, stations heard)
//!   are announced. Exit with `dstar stop`.

mod commands;
mod transport;

// proptest is a dev-dependency used by the library's unit tests; the bin
// target's test build links dev-deps too. Acknowledge it so
// `unused_crate_dependencies` stays silent for this compilation unit.
#[cfg(test)]
use proptest as _;

use std::future::Future;
use std::io::IsTerminal as _;
use std::net::ToSocketAddrs;
use std::sync::atomic::{AtomicI32, Ordering};

use thd75_repl::aprintln;

use clap::{Parser, ValueEnum};
use dstar_gateway_core::slowdata::{
    SlowDataTextCollector, SlowDataTextMessage, encode_text_message,
};
use kenwood_thd75::LinkDiagnosis;
use kenwood_thd75::memory::{
    MCP_D75_SCHEMA_FIRMWARE, MCP_D75_SCHEMA_FIRMWARE_IDENTITIES, is_supported_mcp_d75_schema_target,
};
use kenwood_thd75::radio::programming::DetachedMcpPageUpdate;
use kenwood_thd75::transport::EitherTransport;
use kenwood_thd75::types::{
    DstarCallsign, DvGatewayMode, FirmwareIdentity, PcOutputInterface, RadioModel, TncDataBand,
};
use kenwood_thd75::{
    AprsClient, AprsClientConfig, AprsEvent, AprsReportTimestamp, Ax25Address, DigipeaterConfig,
    IGateRfLocality, IGateToRfConfig, MessageAddressee, MessageText, StatusText,
};
use kenwood_thd75::{DstarEvent, DstarGateway, DstarGatewayConfig, PersistentMmdvm};
use kenwood_thd75::{FirmwareProfile, Radio};

use dstar_gateway::auth::AuthClient;
use dstar_gateway::tokio_shell::{AnyAsyncSession, AnyEvent, AsyncSession, ShellError};
use dstar_gateway_core::header::DstarHeader;
use dstar_gateway_core::hosts::HostFile;
use dstar_gateway_core::session::client::{
    Connected, Connecting, DExtra, DPlus, Dcs, DisconnectReason, Session,
};
use dstar_gateway_core::types::ProtocolKind;
use dstar_gateway_core::voice::VoiceFrame;
use dstar_gateway_core::{Callsign, Module, StreamId, Suffix};

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
/// rotating log file from accumulating hundreds of megabytes on
/// every normal session (D-STAR voice at trace level is ~1 MB/s).
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
/// `check` runs the accessibility output self-check and exits.
/// Any future non-interactive operations (status dump, send-one,
/// etc.) belong as sibling variants here.
#[derive(clap::Subcommand, Debug)]
enum Subcommand {
    /// Run the accessibility output self-check and print a report.
    ///
    /// Exercises every user-facing formatter, runs the accessibility
    /// lint on each result, and prints a rule-by-rule report. Exits
    /// 0 if every rule passes, 1 otherwise. Does not connect to a
    /// radio and is safe to run unattended.
    Check,
}

/// Accessible REPL for the Kenwood TH-D75 transceiver.
///
/// Screen-reader friendly: plain text output, one line at a time.
/// Also scriptable: pipe commands via stdin.
#[derive(Parser, Debug)]
#[command(version, about)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "clap derive maps each `bool` flag to a discrete CLI switch (`--timestamps`, \
              `--local-time`, `--quiet`, `--script-strict`, etc.). Grouping them into a bitflags \
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
    /// before a CAT session can enable persistent D-STAR gateway mode.
    #[arg(long, value_enum, requires = "port")]
    port_interface: Option<PortInterface>,

    /// Baud rate for serial connection.
    #[arg(short, long, default_value_t = 115_200)]
    baud: u32,

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
    #[arg(long)]
    utc_offset: Option<String>,

    /// Enable file logging at the given level (default: no file).
    ///
    /// File location (one fresh file per session, suffix is the UTC
    /// session start time at second granularity):
    /// - macOS: `~/Library/Logs/thd75-repl/thd75-repl.log.<YYYY-MM-DD-HHMMSS>`
    /// - Linux: `~/.local/state/thd75-repl/thd75-repl.log.<YYYY-MM-DD-HHMMSS>`
    /// - Windows: `%LOCALAPPDATA%\thd75-repl\logs\thd75-repl.log.<YYYY-MM-DD-HHMMSS>`
    ///
    /// Every `thd75-repl` invocation creates its own log file; old
    /// files accumulate until you clean them up manually. **File
    /// logging is opt-in** because trace-level capture during D-STAR
    /// voice flow generates large files fast (~1 MB/s of trace output
    /// per active reflector link). Pass `--log-level=trace` or
    /// `--trace` when you want to capture a bug report; leave it off
    /// for normal operation.
    #[arg(long, value_enum, default_value_t = LogLevel::Off)]
    log_level: LogLevel,

    /// Shorthand for `--log-level=trace`. Creates the log file and
    /// captures every packet, every frame, every state transition.
    #[arg(long)]
    trace: bool,

    /// Read commands from a script file (use `-` for stdin) and exit.
    ///
    /// One command per line. `#` at line start is a comment. Blank
    /// lines are skipped. `exit`/`quit` ends the script. Errors from
    /// individual commands are printed and the script continues.
    #[arg(long)]
    script: Option<std::path::PathBuf>,

    /// When running a script, halt on the first command error.
    ///
    /// Reserved for future strict-mode enforcement. Currently
    /// accepted and recorded but not yet enforced by the dispatcher.
    #[arg(long)]
    script_strict: bool,

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
    /// `msg`, `echo`, `link`) prompts before keying the radio. Pass
    /// `--yes` to disable the prompt globally; required when running
    /// a script in automation and also useful for interactive sessions
    /// where the operator does not want to be asked every time.
    #[arg(long)]
    yes: bool,

    /// If the radio is found in Reflector Terminal Mode, guide an exit
    /// (prompt for the Menu 650 change) and reconnect, instead of
    /// offering D-STAR mode.
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

    /// Command to run on startup (e.g. "dstar start KQ4NIT REF030C").
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
/// shows `println!` / `aprintln!` output and no log file accumulates
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
    // filename embeds the UTC session start time at second
    // granularity so sequential invocations don't overwrite each
    // other and can be sorted chronologically by `ls`.
    let mut file_layer_opt = None;
    let mut worker_guard = None;
    let mut announced_path = None;
    if let Some(level_str) = file_level.as_filter() {
        if let Some(dir) = log_directory() {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!(
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
                let file_name = format!("thd75-repl.log.{session_suffix}");
                let path = dir.join(&file_name);
                match std::fs::File::create(&path) {
                    Ok(file) => {
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
                        eprintln!(
                            "Warning: could not create log file {}: {e}. \
                             File logging disabled.",
                            path.display()
                        );
                    }
                }
            }
        } else {
            eprintln!(
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
        println!(
            "Logging at {} to {}.",
            file_level.as_filter().unwrap_or("off"),
            path.display(),
        );
    }

    LoggingGuard {
        _file_worker: worker_guard,
    }
}

/// Open a real hardware transport (USB serial or macOS Bluetooth)
/// and return it alongside a fresh tokio runtime.
///
/// The runtime is created first and discovery runs under its reactor
/// context: the Bluetooth path is synchronous and ignores it, but the
/// serial path registers a `tokio::io::AsyncFd`, and `tokio-serial`
/// PANICS (rather than erroring) when no reactor exists. Discovering
/// before creating the runtime therefore crashes whenever Bluetooth
/// is unavailable and a serial port is present; an error-driven
/// fallback branch never gets the chance to run.
/// Factored out of `main` so the mock + real transport branches stay
/// symmetric: both return the same `(opened transport, runtime)` shape.
fn open_real_transport(
    cli_port: Option<&str>,
    cli_baud: u32,
    port_interface: Option<PcOutputInterface>,
) -> Result<(transport::OpenedTransport, tokio::runtime::Runtime), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    let opened = {
        let _reactor = rt.enter();
        transport::discover_and_open(cli_port, cli_baud, port_interface)?
    };
    Ok((opened, rt))
}

/// Parse a UTC offset string like `+05:30`, `-08:00`, `+0530`, or
/// `+5`. Returns the offset in seconds, positive for east of UTC.
///
/// Accepted forms:
/// - `+HH:MM`, `-HH:MM`
/// - `+HHMM`, `-HHMM`
/// - `+H`, `-H` (hours only, no minutes)
/// - Leading sign is optional for positive offsets.
fn parse_utc_offset(s: &str) -> Result<i32, String> {
    if s.is_empty() {
        return Err("empty".to_string());
    }
    // Reject non-ASCII up front: the slicing below is byte-indexed,
    // and a multi-byte character straddling a slice boundary would
    // panic instead of reaching the graceful invalid-offset error.
    if !s.is_ascii() {
        return Err("contains non-ASCII characters".to_string());
    }
    let bytes = s.as_bytes();
    let (sign, rest) = match bytes.first() {
        Some(b'+') => (1i32, s.get(1..).unwrap_or("")),
        Some(b'-') => (-1i32, s.get(1..).unwrap_or("")),
        _ => (1i32, s),
    };
    let rest = rest.trim_start_matches(':');
    if rest.is_empty() {
        return Err("missing hours".to_string());
    }
    let (h_str, m_str) = if let Some((h, m)) = rest.split_once(':') {
        (h, m)
    } else if rest.len() >= 3 {
        (&rest[..rest.len() - 2], &rest[rest.len() - 2..])
    } else {
        (rest, "0")
    };
    let h: i32 = h_str
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    let m: i32 = m_str
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    if !(0..=14).contains(&h) {
        return Err("hours out of range (0 to 14)".to_string());
    }
    if !(0..=59).contains(&m) {
        return Err("minutes out of range (0 to 59)".to_string());
    }
    Ok(sign * (h * 3600 + m * 60))
}

/// Detect the local UTC offset by running `date +%z` on Unix.
///
/// Returns `None` on platforms where this is not supported (Windows)
/// or when the detection command fails.
#[cfg(unix)]
fn detect_utc_offset_seconds() -> Option<i32> {
    let out = std::process::Command::new("date")
        .arg("+%z")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    parse_utc_offset(s.trim()).ok()
}

/// No reliable non-subprocess way to read the local UTC offset on
/// non-Unix targets; callers fall back to UTC.
#[cfg(not(unix))]
const fn detect_utc_offset_seconds() -> Option<i32> {
    None
}

fn main() -> std::process::ExitCode {
    // Render fatal errors with `Display`, not the runtime's `Debug`
    // formatting: multi-line guidance must reach the operator as real
    // newlines, never as literal `\n` escapes (which screen readers
    // announce as noise).
    let result = run_main();
    let signal_exit_code = PROCESS_SIGNAL_EXIT_CODE.load(Ordering::Acquire);
    match result {
        Ok(()) if signal_exit_code == 0 => std::process::ExitCode::SUCCESS,
        Ok(()) => std::process::ExitCode::from(u8::try_from(signal_exit_code).unwrap_or(1)),
        Err(e) => {
            eprintln!("Error: {e}");
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

    if cli.timestamps {
        thd75_repl::TIMESTAMPS.store(true, Ordering::Relaxed);
    }
    if cli.local_time || cli.utc_offset.is_some() {
        thd75_repl::TIMESTAMPS.store(true, Ordering::Relaxed);
        let offset_secs = cli.utc_offset.as_ref().map_or_else(
            || {
                detect_utc_offset_seconds().unwrap_or_else(|| {
                    println!("Warning: could not detect local time zone. Using UTC.");
                    0
                })
            },
            |spec| {
                parse_utc_offset(spec).unwrap_or_else(|error| {
                    println!("Warning: invalid --utc-offset {spec:?}: {error}. Using UTC.");
                    0
                })
            },
        );
        thd75_repl::UTC_OFFSET_SECS.store(offset_secs, Ordering::Relaxed);
    }

    in_script_mode
}

fn run_main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Subcommands bypass the REPL loop entirely. `check` runs a
    // hardware-free accessibility self-check and exits.
    if matches!(&cli.command_mode, Some(Subcommand::Check)) {
        std::process::exit(thd75_repl::check::run());
    }

    let in_script_mode = configure_process_options(&cli);

    // Configure logging before anything else so the file captures
    // the full startup sequence. The guard returned here must live
    // until the program exits; dropping it would terminate the
    // background flush thread and silently drop late log lines.
    let _logging_guard = init_logging(&cli);

    println!(
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
                "Unknown mock scenario: {scenario}. Known: simple, empty, mmdvm, \
                 mmdvm_takeover, mmdvm_dstar, mmdvm_dstar_idle, aprs."
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

    println!("{}", thd75_repl::output::connected_via(&label));

    // `--set-gateway-off`: clear the DV Gateway flag and exit, without
    // entering the interactive loop. Runs over whatever port answered
    // (use USB when Bluetooth is busy in terminal mode).
    if cli.set_gateway_off {
        let local = tokio::task::LocalSet::new();
        return local.block_on(&rt, run_set_gateway_off(transport));
    }

    let initial_command = if cli.command.is_empty() {
        None
    } else {
        Some(cli.command.join(" "))
    };

    // Load script if --script was provided.
    let script = cli.script.as_ref().map(|path| {
        thd75_repl::script::Script::from_path(path).unwrap_or_else(|e| {
            eprintln!("Error: could not read script {}: {e}", path.display());
            std::process::exit(1)
        })
    });
    let script_strict = cli.script_strict;

    // Keep the REPL's existing local owner scope. The MMDVM pump itself is
    // runtime-spawned, and Bluetooth's CFRunLoop lives entirely in the private
    // transport helper.
    let local = tokio::task::LocalSet::new();
    local.block_on(
        &rt,
        run_repl(
            transport,
            cli.port.clone(),
            cli.baud,
            initial_command,
            script,
            script_strict,
            in_script_mode,
            cli.exit_terminal_mode,
            endpoint_interface,
            cli.port_interface.map(PcOutputInterface::from),
        ),
    )?;

    Ok(())
}

/// Read one line with rustyline without blocking the async radio tasks.
async fn read_line_async(
    mut editor: rustyline::DefaultEditor,
    prompt: &'static str,
) -> Result<(rustyline::DefaultEditor, Option<String>), tokio::task::JoinError> {
    tokio::task::spawn_blocking(move || {
        let line = read_line_blocking(&mut editor, prompt);
        (editor, line)
    })
    .await
}

/// Read one line from the user via rustyline. Returns `None` on EOF, Ctrl-C,
/// or input error. This blocking function must run through
/// [`read_line_async`].
fn read_line_blocking(rl: &mut rustyline::DefaultEditor, prompt: &str) -> Option<String> {
    match rl.readline(prompt) {
        Ok(line) => Some(line),
        Err(
            rustyline::error::ReadlineError::Interrupted | rustyline::error::ReadlineError::Eof,
        ) => None,
        Err(e) => {
            println!("Error: input failure: {e}");
            None
        }
    }
}

/// The four transport-ownership states of the REPL.
enum ReplState {
    /// Normal CAT control: radio is directly accessible. Boxed to
    /// keep this variant's size near its siblings'
    /// (`clippy::large_enum_variant`).
    Cat(Box<Radio<EitherTransport>>),
    /// Complete MMDVM version framing has been proved on the link, but the
    /// radio has not yet been consumed by a D-STAR gateway session. This
    /// proves the wire protocol, not which persistent DV Gateway mode the
    /// operator selected in Menu 650. CAT commands are invalid here.
    Mmdvm(Box<Radio<EitherTransport>>),
    /// APRS/KISS mode: radio consumed by `AprsClient`.
    Aprs(Box<AprsClient<EitherTransport>>),
    /// D-STAR gateway/MMDVM mode: radio consumed by `DstarGateway`.
    Dstar(Box<DstarSession>),
}

/// An inactive radio link that can either accept CAT commands or is already
/// proved to speak persistent MMDVM framing.
enum DstarEntryRadio {
    Cat(Radio<EitherTransport>),
    Mmdvm(Radio<EitherTransport>),
}

impl DstarEntryRadio {
    fn into_repl_state(self) -> ReplState {
        match self {
            Self::Cat(radio) => ReplState::Cat(Box::new(radio)),
            Self::Mmdvm(radio) => ReplState::Mmdvm(Box::new(radio)),
        }
    }
}

/// Why the interactive owner loop ended without an orderly quit/EOF return.
enum ReplLoopExit {
    /// A transport or protocol failure consumed the only radio owner.
    RadioConnectionLost,
    /// The command handler already printed the complete terminal error.
    ErrorAlreadyReported,
}

/// Active D-STAR session holding both the radio MMDVM gateway and
/// the reflector UDP client.
struct DstarSession {
    /// Radio-side MMDVM gateway.
    gateway: DstarGateway<EitherTransport, PersistentMmdvm>,
    /// Reflector-side UDP session wrapper (runtime-dispatched across
    /// all three supported protocols).
    reflector: Option<ReflectorSession>,
    /// Station callsign (validated once at session construction).
    callsign: Callsign,
    /// TX stream ID for radio-to-reflector relay (`None` = not transmitting).
    tx_stream_id: Option<StreamId>,
    /// TX sequence counter (0-20 cycle).
    tx_seq: u8,
    /// Local module letter (what we present to the reflector as our
    /// originating module). Cross-module linking uses this to differ
    /// from `reflector_module`.
    local_module: Module,
    /// Reflector module letter we are linked to.
    reflector_module: Module,
    /// Reflector callsign (e.g. `REF030  `, `XLX307  `, `DCS001  `).
    ///
    /// Used by the radio-to-reflector relay to build the outbound
    /// `rpt2` field. The D-STAR convention for a hotspot relaying
    /// voice to a reflector is:
    /// - `rpt1` = operator's own callsign + local module letter
    /// - `rpt2` = reflector's callsign + reflector module letter
    ///
    /// Both end in the same A-E module letter (NEVER a literal `G`)
    /// because xlxd's `cdplusprotocol.cpp:209` rejects inbound
    /// packets whose `rpt1` module byte is not a valid module; see
    /// [`build_reflector_header`] for details.
    reflector_callsign: Callsign,
    /// Current RX stream ID from reflector (`None` = no active stream).
    rx_stream_id: Option<StreamId>,
    /// Echo test unit: records TX frames and schedules their playback
    /// (the `echo` REPL command arms it; URCALL `"       E"` triggers
    /// it per ircDDBGateway convention).
    echo: dstar_gateway_core::echo::EchoUnit,
    /// Slow data decoder for incoming reflector voice frames.
    /// Decodes text messages embedded in the slow data bytes.
    rx_slow_data: SlowDataTextCollector,
    /// Latch: the slow-data text message already announced for the
    /// current stream. D-STAR radios re-transmit one fixed 20-char
    /// message continuously across the whole voice stream (so late
    /// joiners can see it); the message cannot legitimately change
    /// mid-transmission, so any differing re-assembly is RF bit
    /// corruption (the slow-data channel has no error correction).
    /// Announce the first complete assembly per stream and suppress
    /// everything after it, matching the reference collector
    /// behavior in `ircDDBGateway/Common/TextCollector.cpp`. Cleared
    /// on `VoiceStart` for a new stream and on `VoiceEnd`.
    rx_last_slow_text: Option<SlowDataTextMessage>,
    /// Outgoing slow data text message to embed in TX voice frames.
    /// Set via the `text` command. Cleared after one transmission.
    tx_text: Option<String>,
    /// Pre-encoded slow data payloads for the current TX text.
    tx_slow_data: Vec<[u8; 3]>,
    /// Index into `tx_slow_data` for the next frame to send.
    tx_slow_data_idx: usize,
    /// Count of `VoiceFrame` events received for the current RX
    /// stream. Reset to 0 on each fresh `VoiceStart`; read at
    /// `VoiceEnd` to report the transmission length alongside the
    /// elapsed duration. Helps distinguish real voice from dead-key
    /// carriers (which produce only a handful of frames).
    rx_frame_count: u32,
    /// Wall-clock timestamp of the first `VoiceStart` for the current
    /// RX stream (`None` when no stream is active). Used to report
    /// the elapsed duration at `VoiceEnd`.
    rx_stream_start: Option<std::time::Instant>,
    /// Open file handle for DVSI-origin AMBE capture.
    ///
    /// When set (via `AMBE_CAPTURE=/path/to/file.ambe` env var at
    /// startup), every 9-byte AMBE frame the radio's DVSI chip
    /// produces during a TX session is appended to the file. The
    /// result is a byte-for-byte record of what the real hardware
    /// encoder emits for a specific known input, used as a golden
    /// vector against which the Rust encoder's output can be
    /// compared.
    ///
    /// Captures both radio→reflector frames (user speaking into the
    /// mic during D-STAR TX) and the raw DVSI TX output regardless
    /// of whether a reflector is linked, so the operator can do a
    /// pure PTT-and-speak capture without setting up a reflector.
    ambe_capture: Option<std::fs::File>,
    /// Last voice frame received from the reflector for the current
    /// inbound stream. Used as the repeat-template by
    /// [`emit_silence_pad_if_needed`] to keep the modem's D-STAR
    /// FIFO fed while the reflector itself pauses delivery
    /// (observed: gaps of 400-600 ms on REF030 C mid-stream). Without
    /// padding, those gaps drain the modem buffer and the radio
    /// briefly drops carrier, producing audible cuts. Cleared on
    /// every `VoiceStart` and `VoiceEnd` so padding never leaks
    /// across streams.
    last_rx_voice_frame: Option<VoiceFrame>,
    /// Wall-clock timestamp of the most recent relay to the radio
    /// (real or padded frame). Drives the pacing decision in
    /// [`emit_silence_pad_if_needed`]: if no frame has been sent
    /// for longer than [`PAD_INITIAL_THRESHOLD`], the pad timer emits a
    /// copy of the last known frame to cover the gap.
    last_relay_at: Option<std::time::Instant>,
    /// Consecutive padding frames emitted since the last real voice
    /// frame. Capped at [`PAD_FRAMES_MAX`] so a fully dead reflector
    /// doesn't keep the modem fed with repeated audio indefinitely.
    pad_frames_emitted: u32,
    /// Set after the radio-side MMDVM drain hits a fatal transport
    /// error, so the failure is announced to the operator exactly
    /// once instead of on every 100 ms poll cycle. Cleared only by
    /// leaving D-STAR mode (the session is torn down).
    radio_link_lost: bool,
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
    std::sync::Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<ProcessSignal>>>>;

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

    fn begin_critical(&self) -> Result<CriticalSignalGuard, String> {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut slot = self
            .critical_slot
            .lock()
            .map_err(|_| "process signal router lock is poisoned".to_owned())?;
        if slot.is_some() {
            return Err("another critical radio operation is already active".to_owned());
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
            return Err(format!("could not install interrupt listener: {error}"));
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
    receiver: tokio::sync::mpsc::UnboundedReceiver<ProcessSignal>,
    critical_slot: CriticalSignalSlot,
    armed: bool,
}

impl CriticalSignalGuard {
    async fn recv(&mut self) -> Result<ProcessSignal, String> {
        self.receiver
            .recv()
            .await
            .ok_or_else(|| "process signal router stopped".to_owned())
    }

    /// Atomically unregister this critical operation, then check for a signal
    /// that raced with its final successful poll. Clearing the slot while
    /// holding the same mutex used by the router ensures a later TERM/HUP
    /// takes the normal process-exit path instead of entering a dead receiver.
    fn finish(mut self) -> Result<(), String> {
        self.unregister()?;
        self.armed = false;
        match self.receiver.try_recv() {
            Ok(signal) => Err(format!("{} signal received", signal.description())),
            Err(
                tokio::sync::mpsc::error::TryRecvError::Empty
                | tokio::sync::mpsc::error::TryRecvError::Disconnected,
            ) => Ok(()),
        }
    }

    fn unregister(&self) -> Result<(), String> {
        drop(
            self.critical_slot
                .lock()
                .map_err(|_| "process signal router lock is poisoned".to_owned())?
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
        .and_then(|slot| slot.as_ref().map(|sender| sender.send(signal).is_ok()))
        .unwrap_or(false);
    if delivered {
        PROCESS_SIGNAL_EXIT_CODE.store(signal.exit_code(), Ordering::Release);
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

impl std::fmt::Debug for DstarSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DstarSession")
            .field("callsign", &self.callsign)
            .field("reflector_connected", &self.reflector.is_some())
            .finish_non_exhaustive()
    }
}

/// Protocol-erased reflector session; events arrive as the equally
/// erased [`AnyEvent`], which the runtime handlers
/// (`relay_reflector_to_radio`, `trace_reflector_event`,
/// `print_reflector_event`) consume directly.
type ReflectorSession = AnyAsyncSession;

/// Erased session event; alias so call sites read at the same level
/// of abstraction as [`ReflectorSession`].
type RuntimeEvent = AnyEvent;

/// Close the reflector side without allowing a stalled UDP task to block
/// recovery of the radio-side terminal mode.
async fn disconnect_reflector(session: &mut ReflectorSession) {
    match session.disconnect().await {
        Ok(()) => {
            aprintln!("Disconnected from reflector.");
        }
        Err(ShellError::DisconnectUnacknowledged) => {
            aprintln!(
                "The reflector did not acknowledge the disconnect; the local reflector session \
                 closed after its protocol timeout."
            );
        }
        Err(ShellError::DisconnectedBeforeUnlink { reason }) => match reason {
            DisconnectReason::KeepaliveInactivity => {
                aprintln!("The reflector link had already closed after inactivity.");
            }
            DisconnectReason::Rejected => {
                aprintln!("The reflector link was already closed after rejection.");
            }
            _ => {
                aprintln!("The reflector session had already closed: {reason:?}.");
            }
        },
        Err(error) => {
            println!(
                "{}",
                thd75_repl::output::error(format_args!(
                    "reflector disconnect did not complete: {error}. Continuing radio recovery"
                ))
            );
        }
    }
}

/// Main REPL loop. Manages CAT, MMDVM-ready, APRS, and D-STAR states.
/// Each state owns the radio transport exclusively.
#[expect(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "`run_repl` is the single state-machine owner for CAT/APRS/D-STAR modes: it fetches \
              input from the appropriate queue (pending command, script queue, or rustyline), \
              dispatches to the active mode, handles mode transitions, and unwinds on quit. \
              Extracting the dispatch arms would require threading every mode's session \
              type through helpers and would not meaningfully reduce the function's inherent \
              complexity: the branching is the algorithm. The argument list carries the CLI \
              flags the state machine needs; bundling them into a struct would only move the \
              same fields behind one more indirection."
)]
async fn run_repl(
    transport: EitherTransport,
    cli_port: Option<String>,
    cli_baud: u32,
    initial_command: Option<String>,
    script: Option<thd75_repl::script::Script>,
    script_strict: bool,
    in_script_mode: bool,
    exit_terminal_mode: bool,
    mut endpoint_interface: transport::EndpointInterface,
    port_interface_override: Option<PcOutputInterface>,
) -> Result<(), Box<dyn std::error::Error>> {
    // `script_strict` is reserved for a future enhancement that halts
    // the REPL loop on the first command error. Silencing the
    // unused-var lint keeps the signature stable now so later tasks
    // don't have to reshape the calls.
    let _ = script_strict;

    // Install one process-lifetime signal owner before any radio I/O. Scoped
    // MCP transitions subscribe to it; outside those transitions, TERM/HUP
    // retain their normal process-termination behavior.
    let process_signals = ProcessSignalRouter::install()?;

    // Preserve an already-ready CAT link with a read-only identity proof. If
    // the bounded packet-mode fallback still cannot prove CAT, retain this
    // exact transport owner long enough to distinguish persistent MMDVM from
    // an unresponsive link.
    let mut radio = Radio::new(transport);
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
        println!(
            "{}",
            thd75_repl::output::startup_identified(info.model, &firmware,)
        );
        println!("{}", thd75_repl::output::type_help_hint());
        ReplState::Cat(Box::new(radio))
    } else {
        match radio.probe_silent_link().await {
            LinkDiagnosis::ReconnectRequired => {
                return Err(LinkDiagnosis::ReconnectRequired.guidance().into());
            }
            LinkDiagnosis::MmdvmMode if exit_terminal_mode => {
                // CAT is offline on this link, so neither model nor
                // firmware can be qualified before an offset-based MCP
                // write. Keep this path read-only and guide the operator.
                // Fully automated exit remains available through
                // `--set-gateway-off` on the other, CAT-capable interface,
                // where ID/FV are proved before touching flash.
                println!(
                    "This gateway link cannot prove the radio model and firmware required \
                         for a safe automated memory write."
                );
                let (restored_radio, restored_interface) = guide_exit_terminal_mode(
                    radio,
                    cli_port.as_deref(),
                    cli_baud,
                    port_interface_override,
                )
                .await?;
                radio = restored_radio;
                endpoint_interface = restored_interface;
                let identity = radio.identify().await?;
                let firmware = radio.get_firmware_version().await?;
                println!(
                    "{}",
                    thd75_repl::output::startup_identified(identity.model, &firmware,)
                );
                println!("{}", thd75_repl::output::type_help_hint());
                ReplState::Cat(Box::new(radio))
            }
            LinkDiagnosis::MmdvmMode => {
                // Store the binary-proved owner in its dedicated state so
                // CAT dispatch is unreachable. MMDVM framing proves the
                // DV Gateway wire protocol, but cannot distinguish
                // Reflector Terminal from Access Point mode.
                println!("Radio is in DV Gateway/MMDVM mode.");
                println!("CAT commands like freq, mode, and status do not work in this mode.");
                println!("To use D-STAR now: dstar start <callsign> [reflector]");
                println!("  Example: dstar start W1AW REF030C");
                println!("To restore normal radio control: set Menu No. 650 (DV Gateway) to Off,");
                println!("  then restart, or relaunch with --exit-terminal-mode.");
                ReplState::Mmdvm(Box::new(radio))
            }
            LinkDiagnosis::Unresponsive => {
                return Err(LinkDiagnosis::Unresponsive.guidance().into());
            }
        }
    };

    // Rustyline performs a blocking terminal read. Move the editor through
    // `spawn_blocking` so the async runtime remains free to drive radio tasks.
    let mut rl = Some(rustyline::DefaultEditor::new()?);
    let mut pending_command = initial_command;
    let mut script_commands: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    if let Some(s) = script {
        for cmd in s.commands {
            script_commands.push_back(cmd);
        }
    }

    let loop_exit = loop {
        let prompt = match &state {
            ReplState::Cat(_) | ReplState::Mmdvm(_) => "d75> ",
            ReplState::Aprs(_) => "aprs> ",
            ReplState::Dstar(_) => "dstar> ",
        };

        // Command source priority: initial command, then script queue,
        // then interactive input. When in script mode and the queue
        // drains, return None so the main loop exits cleanly instead
        // of falling into the rustyline prompt. The explicit if-ladder keeps
        // that priority visible.
        let line = if let Some(cmd) = pending_command.take() {
            println!("{prompt}{cmd}");
            Some(cmd)
        } else if let Some(cmd) = script_commands.pop_front() {
            println!("{prompt}{cmd}");
            Some(cmd)
        } else if in_script_mode {
            None
        } else {
            let editor = rl.take().ok_or("interactive editor ownership was lost")?;
            let (editor, line) = read_line_async(editor, prompt).await?;
            rl = Some(editor);
            line
        };
        let Some(line) = line else {
            // EOF or Ctrl-C: disconnect cleanly.
            let radio = match state {
                ReplState::Cat(r) | ReplState::Mmdvm(r) => Some(*r),
                ReplState::Aprs(c) => Some(
                    c.stop()
                        .await
                        .map_err(|(_client, error)| {
                            format!("stopping APRS at end of input: {error}")
                        })?
                        // Disconnecting next, so CAT re-proof is moot.
                        .into_radio_unproven(),
                ),
                ReplState::Dstar(mut s) => {
                    if let Some(mut reflector) = s.reflector.take() {
                        disconnect_reflector(&mut reflector).await;
                    }
                    let radio = s.gateway.stop().await;
                    // Mirror the guidance the interactive quit path
                    // gives via `exit_dstar`: the radio stays in
                    // DV Gateway/MMDVM mode until the operator changes Menu
                    // 650, and end-of-input gives us no way to walk them
                    // through a reconnect.
                    println!("The radio is still in DV Gateway/MMDVM mode.");
                    println!("Set Menu 650 (DV Gateway) to Off to restore normal operation.");
                    Some(radio.map_err(|error| {
                        format!("stopping D-STAR gateway at end of input: {error}")
                    })?)
                }
            };
            if let Some(r) = radio {
                drop(r.disconnect().await);
            }
            println!("{}", thd75_repl::output::goodbye());
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
                                println!("{text}");
                                println!();
                            }
                        }
                    } else if let Some(text) = thd75_repl::help_text::for_command(sub) {
                        println!("{text}");
                    } else {
                        println!(
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
                        ReplState::Dstar(_) => thd75_repl::help_text::DSTAR_MODE_HELP,
                    };
                    println!("{text}");
                }
                continue;
            }
            "quit" | "exit" | "q" => {
                // Recover the radio from any active mode, then disconnect
                // to release the RFCOMM channel cleanly.
                let radio = match state {
                    ReplState::Aprs(client) => {
                        println!("Exiting APRS mode.");
                        // Disconnecting next, so CAT re-proof is moot.
                        client
                            .stop()
                            .await
                            .ok()
                            .map(kenwood_thd75::DesyncedRadio::into_radio_unproven)
                    }
                    ReplState::Dstar(mut session) => {
                        println!("Exiting D-STAR mode and restoring normal radio mode.");
                        if let Some(mut reflector) = session.reflector.take() {
                            disconnect_reflector(&mut reflector).await;
                        }
                        match exit_dstar(
                            session.gateway,
                            cli_port.as_deref(),
                            cli_baud,
                            port_interface_override,
                        )
                        .await
                        {
                            Ok((r, _)) => Some(r),
                            Err(e) => {
                                println!(
                                    "{}",
                                    thd75_repl::output::error(format_args!(
                                        "restoring radio mode: {e}"
                                    ))
                                );
                                None
                            }
                        }
                    }
                    ReplState::Cat(r) | ReplState::Mmdvm(r) => Some(*r),
                };
                if let Some(r) = radio {
                    drop(r.disconnect().await);
                }
                println!("{}", thd75_repl::output::goodbye());
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
                        println!(
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
                    println!("No previous output available.");
                } else {
                    for line in lines {
                        println!("{line}");
                    }
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
                thd75_repl::VERBOSE.store(new_value, Ordering::Relaxed);
                aprintln!("Verbose output: {}", if new_value { "on" } else { "off" });
                continue;
            }
            "quiet" => {
                thd75_repl::VERBOSE.store(false, Ordering::Relaxed);
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
            _ => {}
        }

        // An MMDVM-ready owner has positive binary framing proof, not CAT
        // ownership. Keep ordinary radio and APRS commands away from it; only
        // D-STAR startup may consume this state.
        if matches!(&state, ReplState::Mmdvm(_)) && cmd != "dstar" {
            println!("{}", LinkDiagnosis::MmdvmMode.guidance());
            println!("Then restart this program, or relaunch with --exit-terminal-mode.");
            println!("Or type dstar start <callsign> [reflector] to use D-STAR now.");
            continue;
        }

        // Mode-specific dispatch.
        state = match state {
            ReplState::Cat(mut radio) => {
                dispatch_cat(&mut radio, &cmd, &parts).await;

                // Command handlers intentionally own their user-facing error
                // messages, so the dispatcher cannot infer safety from their
                // return values. Ask the radio directly whether the exchange
                // left an ambiguous frame boundary and recover before the next
                // prompt.
                if radio.cat_recovery_required() {
                    aprintln!("CAT response boundary is ambiguous; recovering the radio link.");
                    if let Err(error) = radio.recover_cat().await {
                        println!(
                            "{}",
                            thd75_repl::output::error(format_args!("CAT recovery failed: {error}"))
                        );
                        drop(radio.disconnect().await);
                        break ReplLoopExit::RadioConnectionLost;
                    }
                    aprintln!("CAT link recovered.");
                }

                // Check for mode transitions.
                if cmd == "aprs" && parts.get(1).is_some_and(|s| *s == "start") {
                    match enter_aprs(*radio, parts.get(2..).unwrap_or(&[])).await {
                        Ok(client) => ReplState::Aprs(Box::new(client)),
                        Err((Some(radio_back), e)) => {
                            println!(
                                "{}",
                                thd75_repl::output::error(format_args!("entering APRS mode: {e}"))
                            );
                            ReplState::Cat(Box::new(radio_back))
                        }
                        Err((None, e)) => {
                            println!(
                                "{}",
                                thd75_repl::output::error(format_args!("entering APRS mode: {e}"))
                            );
                            break ReplLoopExit::RadioConnectionLost;
                        }
                    }
                } else if cmd == "dstar" && parts.get(1).is_some_and(|s| *s == "start") {
                    match start_dstar_repl(
                        DstarEntryRadio::Cat(*radio),
                        parts.get(2..).unwrap_or(&[]),
                        &process_signals,
                        endpoint_interface,
                    )
                    .await
                    {
                        Some(next_state) => next_state,
                        None => break ReplLoopExit::ErrorAlreadyReported,
                    }
                } else {
                    ReplState::Cat(radio)
                }
            }

            ReplState::Mmdvm(radio) => {
                if cmd == "dstar" && parts.get(1).is_some_and(|s| *s == "start") {
                    match start_dstar_repl(
                        DstarEntryRadio::Mmdvm(*radio),
                        parts.get(2..).unwrap_or(&[]),
                        &process_signals,
                        endpoint_interface,
                    )
                    .await
                    {
                        Some(next_state) => next_state,
                        None => break ReplLoopExit::ErrorAlreadyReported,
                    }
                } else {
                    println!("Usage: dstar start <callsign> [reflector]");
                    println!("  Enters D-STAR gateway mode. Optionally connects to a reflector.");
                    println!("  Example: dstar start W1AW REF030C");
                    ReplState::Mmdvm(radio)
                }
            }

            ReplState::Aprs(mut client) => {
                if cmd == "aprs" && parts.get(1).is_some_and(|s| *s == "stop") {
                    match client.stop().await {
                        Ok(desynced) => match desynced.restore().await {
                            Ok(radio) => {
                                aprintln!("APRS mode stopped. Returned to CAT mode.");
                                ReplState::Cat(Box::new(radio))
                            }
                            Err((desynced, error)) => {
                                println!(
                                    "{}",
                                    thd75_repl::output::error(format_args!(
                                        "KISS exit succeeded, but CAT restoration failed: \
                                             {error}"
                                    ))
                                );
                                drop(desynced.into_radio_unproven().disconnect().await);
                                break ReplLoopExit::RadioConnectionLost;
                            }
                        },
                        Err((_client, e)) => {
                            println!(
                                "{}",
                                thd75_repl::output::error(format_args!("stopping APRS: {e}"))
                            );
                            break ReplLoopExit::RadioConnectionLost;
                        }
                    }
                } else {
                    dispatch_aprs(&mut client, &cmd, &parts).await;
                    ReplState::Aprs(client)
                }
            }

            ReplState::Dstar(mut session) => {
                if cmd == "dstar" && parts.get(1).is_some_and(|s| *s == "stop") {
                    if let Some(mut reflector) = session.reflector.take() {
                        disconnect_reflector(&mut reflector).await;
                    }
                    match exit_dstar(
                        session.gateway,
                        cli_port.as_deref(),
                        cli_baud,
                        port_interface_override,
                    )
                    .await
                    {
                        Ok((radio, reopened_interface)) => {
                            endpoint_interface = reopened_interface;
                            // `exit_dstar` reconnected and verified CAT
                            // control, so this owner is no longer terminal.
                            aprintln!("D-STAR mode stopped. Returned to normal radio control.");
                            ReplState::Cat(Box::new(radio))
                        }
                        Err(e) => {
                            println!(
                                "{}",
                                thd75_repl::output::error(format_args!("exiting D-STAR mode: {e}"))
                            );
                            break ReplLoopExit::RadioConnectionLost;
                        }
                    }
                } else if cmd == "monitor" {
                    // Enter continuous monitoring mode with background
                    // keepalives and voice relay. Blocks until user
                    // presses Ctrl-C or types "stop".
                    println!("Entering monitor mode. Press Ctrl-C to return to prompt.");
                    run_dstar_monitor(&mut session).await;
                    ReplState::Dstar(session)
                } else {
                    dispatch_dstar(&mut session, &cmd, &parts).await;
                    ReplState::Dstar(session)
                }
            }
        };
    };

    match loop_exit {
        ReplLoopExit::RadioConnectionLost => {
            // Surface unreported ownership loss as a real error so scripts and
            // automation see a non-zero exit code; `main` supplies `Error:`.
            Err("radio connection lost. Please close and reopen the program.".into())
        }
        ReplLoopExit::ErrorAlreadyReported => Ok(()),
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
async fn dispatch_cat(radio: &mut Radio<EitherTransport>, cmd: &str, parts: &[&str]) {
    let args = parts.get(1..).unwrap_or(&[]);
    match cmd {
        "id" | "identify" => commands::identify(radio).await,
        "freq" | "frequency" => commands::frequency(radio, args).await,
        "mode" => commands::set_operating_mode(radio, args).await,
        "squelch" | "sq" => commands::squelch(radio, args).await,
        "power" | "pwr" => commands::set_power(radio, args).await,
        "tnc" => commands::tnc_mode(radio, args).await,
        "beaconmode" => commands::beacon_mode(radio, args).await,
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
        "cq" => commands::cq(radio).await,
        "reflector" | "ref" => commands::reflector(radio, args).await,
        "unreflector" | "unref" | "unlink" => commands::unreflector(radio).await,
        "status" => commands::status(radio).await,
        "band" => commands::band(radio, args).await,
        "ifout" | "usbout" => commands::ifout(radio, args).await,
        "aprs" => {
            if parts.get(1).is_some_and(|s| *s == "start") {
                // Handled by caller after dispatch.
            } else {
                println!("Usage: aprs start <callsign> <a or b> [ssid] [digi]");
                println!("  Enters APRS KISS mode. Type aprs stop to exit.");
                println!("  The required band selects the TNC data band, not packet speed.");
                println!("  Add digi to enable the WIDE1-1 fill-in digipeater.");
            }
        }
        "dstar" => {
            if parts.get(1).is_some_and(|s| *s == "start") {
                // Handled by caller after dispatch.
            } else {
                println!("Usage: dstar start <callsign> [reflector]");
                println!("  Enters D-STAR gateway mode. Optionally connects to a reflector.");
                println!("  Example: dstar start W1AW REF030C");
            }
        }
        other => println!("Unknown command: {other}. Type help for a list of commands."),
    }
}

// ---------------------------------------------------------------------------
// APRS mode
// ---------------------------------------------------------------------------

/// Enter APRS KISS mode. On success, returns an `AprsClient`.
///
/// Input-validation errors return the untouched radio. Once KISS entry is
/// attempted, this function proves CAT synchronization before returning a
/// radio; if that proof fails, it disconnects and returns no owner.
async fn enter_aprs(
    radio: Radio<EitherTransport>,
    args: &[&str],
) -> Result<AprsClient<EitherTransport>, (Option<Radio<EitherTransport>>, String)> {
    let parsed = match thd75_repl::aprs_args::parse_start(args) {
        Ok(p) => p,
        Err(e) => return Err((Some(radio), e)),
    };
    println!(
        "Leaving normal radio control. Entering APRS mode as {}-{}.",
        parsed.callsign, parsed.ssid
    );

    let config = if parsed.digi {
        let addr = match Ax25Address::new(&parsed.callsign, parsed.ssid) {
            Ok(a) => a,
            Err(e) => {
                return Err((Some(radio), format!("invalid callsign for digipeater: {e}")));
            }
        };
        let wide1 = match Ax25Address::new("WIDE1", 1) {
            Ok(alias) => alias,
            Err(e) => {
                return Err((
                    Some(radio),
                    format!("invalid fill-in digipeater alias: {e}"),
                ));
            }
        };
        let digi_cfg = DigipeaterConfig::new(addr.clone(), vec![wide1], None, None);
        match AprsClientConfig::builder(addr, parsed.data_band) {
            Ok(builder) => builder.digipeater(digi_cfg).build(),
            Err(e) => return Err((Some(radio), format!("{e}"))),
        }
    } else {
        match AprsClientConfig::try_builder(&parsed.callsign, parsed.ssid, parsed.data_band) {
            Ok(builder) => builder.build(),
            Err(e) => return Err((Some(radio), format!("{e}"))),
        }
    };

    match AprsClient::start(radio, config).await {
        Ok(client) => {
            println!("{}", thd75_repl::output::aprs_mode_active());
            if parsed.digi {
                println!("Digipeater enabled: WIDE1-1 fill-in.");
            }
            print_aprs_command_list();
            Ok(client)
        }
        Err((radio, e)) => Err(recover_cat_after_failed_mode_entry(radio, e.to_string()).await),
    }
}

/// Prove CAT synchronization after a mode-entry attempt returned its owner.
///
/// A failed proof consumes the still-untrusted connection by disconnecting it,
/// so callers cannot accidentally resume ordinary CAT commands on that stream.
async fn recover_cat_after_failed_mode_entry(
    mut radio: Radio<EitherTransport>,
    entry_error: String,
) -> (Option<Radio<EitherTransport>>, String) {
    match radio.restore_cat_after_mode_exit().await {
        Ok(()) => (Some(radio), entry_error),
        Err(recovery_error) => {
            drop(radio.disconnect().await);
            (
                None,
                format!(
                    "{entry_error}; CAT restoration after the failed mode entry also failed: \
                     {recovery_error}"
                ),
            )
        }
    }
}

/// Print the APRS-mode command list, wrapped across two lines so each
/// stays within the accessibility line-length limit (80 characters).
/// Shared by the mode-entry banner and the unrecognized-command hint.
fn print_aprs_command_list() {
    println!("Commands: monitor, msg, position, compressed, mice, object,");
    println!("  status, motion, beacon, stations, igate, aprs stop");
}

/// Dispatch a command in APRS mode.
#[expect(
    clippy::too_many_lines,
    reason = "Command dispatch for APRS mode covering listen, monitor, msg, position, beacon, \
              stations, igate, and the aprs-stop transition. Each arm is a small handler; the \
              sum is long but the structure is a flat dispatch table, so splitting helpers would \
              only fragment a single command table across files."
)]
async fn dispatch_aprs(client: &mut AprsClient<EitherTransport>, cmd: &str, parts: &[&str]) {
    match cmd {
        "listen" | "poll" => match client.next_event().await {
            Ok(Some(event)) => print_aprs_event(&event),
            Ok(None) => println!("No APRS activity."),
            Err(e) => println!("Error: {e}"),
        },
        "monitor" => {
            println!("Monitoring APRS. Press Ctrl-C to stop.");
            run_aprs_monitor(client).await;
        }
        "msg" | "message" => {
            if parts.len() < 3 {
                println!("Usage: msg <callsign> <message text>");
                return;
            }
            let Some(&addressee) = parts.get(1) else {
                return;
            };
            let text = parts.get(2..).unwrap_or(&[]).join(" ");
            let addressee = match MessageAddressee::new(addressee) {
                Ok(addressee) => addressee,
                Err(error) => {
                    println!("Error: invalid message addressee: {error}.");
                    return;
                }
            };
            let text = match MessageText::new(&text) {
                Ok(text) => text,
                Err(error) => {
                    println!("Error: invalid message text: {error}.");
                    return;
                }
            };
            if !thd75_repl::confirm::tx_confirm() {
                return;
            }
            match client.send_message(&addressee, &text).await {
                Ok(msg_id) => println!("Message queued to {addressee}: {text} (ID: {msg_id})"),
                Err(e) => println!(
                    "{}",
                    thd75_repl::output::error(format_args!("sending message: {e}"))
                ),
            }
        }
        "beacon" => {
            if !thd75_repl::confirm::tx_confirm() {
                return;
            }
            let text = match StatusText::new("REPL beacon") {
                Ok(text) => text,
                Err(error) => {
                    println!("Error: built-in beacon text is invalid: {error}.");
                    return;
                }
            };
            match client.send_status(&text).await {
                Ok(()) => println!("Status beacon sent."),
                Err(e) => println!(
                    "{}",
                    thd75_repl::output::error(format_args!("sending beacon: {e}"))
                ),
            }
        }
        "position" | "pos" => {
            let args = match thd75_repl::aprs_args::parse_position(parts.get(1..).unwrap_or(&[])) {
                Ok(a) => a,
                Err(e) => {
                    println!("Error: {e}");
                    return;
                }
            };
            if !thd75_repl::confirm::tx_confirm() {
                return;
            }
            let (latitude, longitude, comment) =
                (args.pos.latitude, args.pos.longitude, args.comment);
            match client.beacon_position(latitude, longitude, &comment).await {
                Ok(()) => println!(
                    "Position beacon sent: {:.4}, {:.4}{}.",
                    latitude.as_degrees(),
                    longitude.as_degrees(),
                    if comment.as_str().is_empty() {
                        String::new()
                    } else {
                        format!(" ({comment})")
                    }
                ),
                Err(e) => println!(
                    "{}",
                    thd75_repl::output::error(format_args!("sending position: {e}"))
                ),
            }
        }
        "compressed" => {
            let args = match thd75_repl::aprs_args::parse_compressed_position(
                parts.get(1..).unwrap_or(&[]),
            ) {
                Ok(a) => a,
                Err(e) => {
                    println!("Error: {e}");
                    return;
                }
            };
            if !thd75_repl::confirm::tx_confirm() {
                return;
            }
            match client
                .beacon_position_compressed(args.pos.latitude, args.pos.longitude, &args.comment)
                .await
            {
                Ok(()) => println!(
                    "Compressed position beacon sent: {:.4}, {:.4}.",
                    args.pos.latitude.as_degrees(),
                    args.pos.longitude.as_degrees()
                ),
                Err(e) => println!(
                    "{}",
                    thd75_repl::output::error(format_args!("sending compressed position: {e}"))
                ),
            }
        }
        "mice" => {
            let args = match thd75_repl::aprs_args::parse_mice(parts.get(1..).unwrap_or(&[])) {
                Ok(a) => a,
                Err(e) => {
                    println!("Error: {e}");
                    return;
                }
            };
            if !thd75_repl::confirm::tx_confirm() {
                return;
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
                Ok(()) => println!(
                    "Mic-E beacon sent: {:.4}, {:.4}, {} knots, course {}.",
                    args.pos.latitude.as_degrees(),
                    args.pos.longitude.as_degrees(),
                    args.speed.as_knots(),
                    args.course.as_degrees()
                ),
                Err(e) => println!(
                    "{}",
                    thd75_repl::output::error(format_args!("sending Mic-E beacon: {e}"))
                ),
            }
        }
        "object" => {
            let args = match thd75_repl::aprs_args::parse_object(parts.get(1..).unwrap_or(&[])) {
                Ok(a) => a,
                Err(e) => {
                    println!("Error: {e}");
                    return;
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
                    println!("Error: current UTC time cannot be encoded for APRS: {error}");
                    return;
                }
            };
            if !thd75_repl::confirm::tx_confirm() {
                return;
            }
            match client
                .send_object(
                    &args.name,
                    true,
                    timestamp,
                    args.pos.latitude,
                    args.pos.longitude,
                    &args.comment,
                )
                .await
            {
                Ok(()) => println!(
                    "Object {} sent at {:.4}, {:.4}.",
                    args.name,
                    args.pos.latitude.as_degrees(),
                    args.pos.longitude.as_degrees()
                ),
                Err(e) => println!(
                    "{}",
                    thd75_repl::output::error(format_args!("sending object: {e}"))
                ),
            }
        }
        "status" => {
            if parts.len() < 2 {
                println!("Usage: status <text>");
                println!("  Example: status QRV on 144.390");
                return;
            }
            let text = parts.get(1..).unwrap_or(&[]).join(" ");
            let text = match StatusText::new(&text) {
                Ok(text) => text,
                Err(error) => {
                    println!("Error: invalid status text: {error}.");
                    return;
                }
            };
            if !thd75_repl::confirm::tx_confirm() {
                return;
            }
            match client.send_status(&text).await {
                Ok(()) => println!("Status sent: {text}"),
                Err(e) => println!(
                    "{}",
                    thd75_repl::output::error(format_args!("sending status: {e}"))
                ),
            }
        }
        "motion" => {
            let args = match thd75_repl::aprs_args::parse_motion(parts.get(1..).unwrap_or(&[])) {
                Ok(a) => a,
                Err(e) => {
                    println!("Error: {e}");
                    return;
                }
            };
            if !thd75_repl::confirm::tx_confirm() {
                return;
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
                Ok(true) => println!("Motion updated. SmartBeaconing transmitted a beacon."),
                Ok(false) => println!("Motion updated. No beacon due yet."),
                Err(e) => println!(
                    "{}",
                    thd75_repl::output::error(format_args!("updating motion: {e}"))
                ),
            }
        }
        "stations" | "heard" => {
            let stations = client.stations().recent();
            if stations.is_empty() {
                println!("No stations heard yet.");
            } else {
                for entry in stations.iter().take(20) {
                    let elapsed = commands::fmt_elapsed(entry.last_heard.elapsed());
                    let position = entry.position.as_ref().map(|p| (p.latitude, p.longitude));
                    println!(
                        "{}",
                        thd75_repl::output::aprs_station_entry(
                            &entry.callsign,
                            position,
                            entry.packet_count,
                            &elapsed,
                        )
                    );
                }
                println!("{}", thd75_repl::output::stations_summary(stations.len()));
            }
        }
        "igate" => {
            if parts.len() < 6 {
                println!(
                    "Usage: igate <filter> <max-repeated-hops> <receiver-rf-minutes> \
                     <sender-rf-minutes> <receiver-internet-minutes>"
                );
                println!("  Connects to APRS-IS and bridges RF to internet.");
                println!("  Example: igate r/35.30/-82.46/100 2 60 30 30");
                println!("  (receive stations within 100km of the given lat/lon)");
                println!("  Press Ctrl-C to disconnect.");
                return;
            }
            let policy_args = parts.get(parts.len().saturating_sub(4)..).unwrap_or(&[]);
            let parse_minutes = |index: usize, name: &str| -> Result<std::time::Duration, String> {
                let raw = policy_args
                    .get(index)
                    .ok_or_else(|| format!("missing {name}"))?;
                let minutes = raw
                    .parse::<u64>()
                    .map_err(|_| format!("{name} must be a whole number of minutes"))?;
                let seconds = minutes
                    .checked_mul(60)
                    .ok_or_else(|| format!("{name} is too large"))?;
                Ok(std::time::Duration::from_secs(seconds))
            };
            let policy = match (|| {
                let maximum_repeated_hops = policy_args
                    .first()
                    .ok_or_else(|| String::from("missing maximum repeated hops"))?
                    .parse::<u8>()
                    .map_err(|_| String::from("maximum repeated hops must be 0-8"))?;
                IGateToRfConfig::new(
                    IGateRfLocality::new(maximum_repeated_hops)
                        .map_err(|error| error.to_string())?,
                    parse_minutes(1, "receiver RF period")?,
                    parse_minutes(2, "sender RF period")?,
                    parse_minutes(3, "receiver Internet period")?,
                )
                .map_err(|error| error.to_string())
            })() {
                Ok(policy) => policy,
                Err(error) => {
                    println!("Invalid IGate policy: {error}");
                    return;
                }
            };
            let filter_end = parts.len().saturating_sub(4);
            let filter = parts.get(1..filter_end).unwrap_or(&[]).join(" ");
            // Starting the bridge authorizes an open-ended sequence of
            // Internet-to-RF transmissions, so require the same explicit RF
            // confirmation as every other transmitting REPL command.
            if !thd75_repl::confirm::tx_confirm() {
                return;
            }
            client.configure_igate_to_rf(policy);
            run_igate(client, &filter).await;
        }
        _ => {
            println!("APRS command not recognized: {cmd}.");
            print_aprs_command_list();
        }
    }
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
async fn run_igate(client: &mut AprsClient<EitherTransport>, filter: &str) {
    use kenwood_thd75::{AprsIsClient, AprsIsConfig, AprsIsEvent, IGateFormatError};

    let Some(login_call) = igate_login_call(client) else {
        aprintln!("Error: the IGate needs a station identity; APRS was started receive-only");
        return;
    };

    println!("Connecting to APRS-IS as {login_call}.");
    let mut is_config = AprsIsConfig::new(&login_call);
    // Preserve the pre-extraction software_name identity when running under
    // the thd75 stack. The aprs-is crate defaults to "aprs-is" so callers
    // from inside kenwood-thd75 must override explicitly.
    is_config.software_name = String::from("kenwood-thd75");
    filter.clone_into(&mut is_config.filter);
    let is_passcode = is_config.passcode;

    let mut is_client = match AprsIsClient::connect(is_config).await {
        Ok(c) => c,
        Err(e) => {
            println!("Error: APRS-IS connect failed: {e}");
            return;
        }
    };

    println!("{}", thd75_repl::output::aprs_is_connected());

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            _ = &mut ctrl_c => {
                println!("IGate stopping.");
                drop(is_client.shutdown().await);
                break;
            }
            // Poll APRS-IS for incoming packets.
            is_result = is_client.next_event() => {
                match is_result {
                    Ok(AprsIsEvent::Packet(pkt)) => {
                        // `pkt.line` is the lossy UTF-8 view intended only for
                        // display. Gating uses `pkt.raw` so validation and
                        // third-party wrapping operate on the exact wire bytes.
                        aprintln!("{}", thd75_repl::output::aprs_is_incoming(&pkt.line));
                        // Gate to RF if appropriate. The helper checks
                        // whether the packet should be forwarded per
                        // IGate rules (station heard on RF recently, etc).
                        gate_aprs_is_packet_to_rf(client, &pkt.raw).await;
                    }
                    Ok(AprsIsEvent::Comment(line)) => {
                        tracing::debug!("APRS-IS comment: {line}");
                    }
                    Ok(AprsIsEvent::LoggedIn { server }) => {
                        match server {
                            Some(s) => println!("APRS-IS login verified (server {s})."),
                            None => println!("APRS-IS login verified."),
                        }
                    }
                    Ok(AprsIsEvent::LoginRejected { reason }) => {
                        println!("APRS-IS login rejected: {reason}");
                        break;
                    }
                    Ok(AprsIsEvent::Disconnected) => {
                        println!("APRS-IS disconnected. Stopping IGate.");
                        break;
                    }
                    Err(e) => {
                        println!("Error: APRS-IS: {e}");
                        break;
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
                        if let Some(pkt) = client.take_last_rf_packet() {
                            match client.format_packet_for_aprs_is(&pkt, is_passcode) {
                                Ok(is_line) => {
                                    if let Err(error) = is_client.send_uplink_line(&is_line).await {
                                        println!("Error: gate to IS: {error}");
                                    }
                                }
                                Err(IGateFormatError::GatingRefused(reason)) => {
                                    tracing::debug!(%reason, "RF packet is not eligible for APRS-IS");
                                }
                                Err(IGateFormatError::InvalidUplinkLine(error)) => {
                                    println!("Error: unsafe APRS-IS uplink rejected: {error}");
                                }
                                Err(error) => {
                                    println!("Error: APRS-IS uplink formatting failed: {error}");
                                }
                            }
                        }
                    }
                    Err(e) => {
                        println!("Error: RF: {e}");
                    }
                }
            }
        }

        // Send keepalive if interval elapsed.
        if let Err(e) = is_client.maybe_send_keepalive().await {
            println!("Error: APRS-IS keepalive: {e}");
            break;
        }
    }
}

/// Continuous APRS monitoring loop. Polls for events and prints them.
/// Exits on Ctrl-C.
async fn run_aprs_monitor(client: &mut AprsClient<EitherTransport>) {
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            _ = &mut ctrl_c => {
                println!("Monitor stopped. Type monitor to resume, or help for commands.");
                break;
            }
            result = client.next_event() => {
                match result {
                    Ok(Some(event)) => print_aprs_event(&event),
                    Ok(None) => {}
                    Err(e) => {
                        println!("Error: {e}");
                        break;
                    }
                }
            }
        }
    }
}

/// Format and print an APRS event as screen-reader-friendly text.
fn print_aprs_event(event: &AprsEvent) {
    match event {
        AprsEvent::StationHeard(entry) => {
            aprintln!(
                "{}",
                thd75_repl::output::aprs_station_heard(&entry.callsign)
            );
        }
        AprsEvent::MessageReceived(msg) => {
            aprintln!(
                "{}",
                thd75_repl::output::aprs_message_received(&msg.addressee, &msg.text)
            );
        }
        AprsEvent::MessageDelivered(id) => {
            aprintln!(
                "{}",
                thd75_repl::output::aprs_message_delivered(id.as_str())
            );
        }
        AprsEvent::MessageRejected(id) => {
            aprintln!("{}", thd75_repl::output::aprs_message_rejected(id.as_str()));
        }
        AprsEvent::MessageExpired(id) => {
            aprintln!("{}", thd75_repl::output::aprs_message_expired(id.as_str()));
        }
        AprsEvent::PositionReceived { source, position } => {
            aprintln!(
                "{}",
                thd75_repl::output::aprs_position(source, position.latitude, position.longitude)
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
                thd75_repl::output::aprs_raw_packet(&pkt.source.to_string())
            );
        }
    }
}

async fn gate_aprs_is_packet_to_rf(client: &mut AprsClient<EitherTransport>, packet: &[u8]) {
    if let Err(error) = client.gate_from_is(packet, std::time::Instant::now()).await {
        println!("Error: gate to RF: {error}");
    }
}

// ---------------------------------------------------------------------------
// D-STAR mode
// ---------------------------------------------------------------------------

/// Guide the operator out of Reflector Terminal Mode and reconnect.
///
/// Prints the link guidance, waits for the operator to switch Menu 650
/// off, then reconnects and re-checks. Loops until CAT control is
/// restored; returns an error only if the radio cannot be reopened.
async fn guide_exit_terminal_mode(
    mut radio: Radio<EitherTransport>,
    cli_port: Option<&str>,
    cli_baud: u32,
    port_interface_override: Option<PcOutputInterface>,
) -> Result<(Radio<EitherTransport>, transport::EndpointInterface), String> {
    loop {
        println!("{}", LinkDiagnosis::MmdvmMode.guidance());
        println!();
        println!("Set Menu 650 to Off on the radio, then press Enter when done (Ctrl-C to quit).");
        let mut line = String::new();
        drop(std::io::stdin().read_line(&mut line));

        drop(radio.disconnect().await);
        println!("Reconnecting...");
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;

        let opened = transport::discover_and_open(cli_port, cli_baud, port_interface_override)
            .map_err(|e| format!("Reconnect failed: {e}"))?;
        let reopened_interface = opened.endpoint_interface;
        radio = Radio::connect_with_tnc_exit(opened.transport)
            .await
            .map_err(|e| format!("Connect failed: {e}"))?;

        if radio.identify().await.is_ok() {
            println!("Radio restored to CAT control mode.");
            return Ok((radio, reopened_interface));
        }
        println!("The radio is still in DV Gateway/MMDVM mode. Try again.");
    }
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
/// Returns an error string if the connection or the memory write
/// fails. The notable case is a handshake timeout, which means this
/// port is the one dedicated to the gateway; try the other interface.
async fn run_set_gateway_off(transport: EitherTransport) -> Result<(), Box<dyn std::error::Error>> {
    // This path is selected specifically because CAT is already alive
    // on the gateway's unused interface. Do not use `connect_with_tnc_exit`
    // here: its recovery preamble writes TN 0,0 and would change the
    // very state we need to inspect before the MCP write.
    let mut radio = Radio::new(transport);
    let identity = radio.identify().await?;
    let firmware = radio.get_firmware_version().await?;
    validate_gateway_mcp_target(identity.model, &firmware)?;
    let profile = FirmwareProfile::from_identity(&firmware);
    if profile.supports_bare_gateway() {
        let gateway = radio.read_gateway().await?;
        println!("Verified TH-D75 firmware {firmware}; Menu 650 is {gateway}.");
        if gateway == DvGatewayMode::Off {
            radio.disconnect().await?;
            println!("DV Gateway is already off; no memory write or reboot was needed.");
            return Ok(());
        }
    } else {
        println!(
            "Verified TH-D75 firmware {firmware}; checking Menu 650 through its verified MCP byte."
        );
    }

    println!("Clearing Menu 650 via its firmware-verified MCP byte.");
    println!("Attempting automated exit: clearing the DV Gateway flag via memory write.");
    let process_signals = ProcessSignalRouter::install()?;
    let mut critical_signals = process_signals.begin_critical()?;
    let operation = clear_gateway_mode_with_interrupt(&mut radio, critical_signals.recv()).await;
    let late_signal = critical_signals.finish().err();

    match operation {
        InterruptibleMcpOperation::Completed(Ok(update)) if late_signal.is_none() => {
            finish_gateway_clear(radio, update).await
        }
        InterruptibleMcpOperation::Completed(Ok(update)) => {
            finish_gateway_clear(radio, update).await?;
            println!(
                "{} after the verified Menu 650 operation completed. The reported radio outcome \
                 above is authoritative.",
                late_signal.unwrap_or_else(|| "process signal received".to_owned())
            );
            Ok(())
        }
        InterruptibleMcpOperation::Completed(Err(error)) => {
            gateway_clear_failure(radio, error, late_signal.as_deref()).await
        }
        InterruptibleMcpOperation::Interrupted { signal, completion } => match completion {
            Ok(update) => {
                finish_gateway_clear(radio, update).await?;
                println!(
                    "{}. The signal was held until the verified Menu 650 operation completed; \
                     the reported radio outcome above is authoritative.",
                    describe_mcp_interrupt(signal)
                );
                Ok(())
            }
            Err(error) => {
                let interruption = describe_mcp_interrupt(signal);
                gateway_clear_failure(radio, error, Some(&interruption)).await
            }
        },
    }
}

async fn finish_gateway_clear(
    radio: Radio<EitherTransport>,
    update: DetachedMcpPageUpdate,
) -> Result<(), Box<dyn std::error::Error>> {
    match update {
        DetachedMcpPageUpdate::UnchangedCatReady => {
            radio.disconnect().await?;
            println!("DV Gateway is already off; no memory write or reboot was needed.");
        }
        DetachedMcpPageUpdate::ChangedRadioRebooting => {
            drop(radio.disconnect().await);
            println!(
                "DV Gateway flag clear was read-back verified. The radio is rebooting into normal \
                 control mode; reconnect for CAT or APRS once it finishes."
            );
        }
    }
    Ok(())
}

async fn gateway_clear_failure(
    radio: Radio<EitherTransport>,
    error: kenwood_thd75::Error,
    interruption: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let requires_recovery = error.requires_recovery() || radio.cat_recovery_required();
    drop(radio.disconnect().await);
    let interruption = interruption.map_or_else(String::new, |reason| format!("{reason}; "));
    let guidance = if requires_recovery {
        "The binary boundary is not safe for another command. Do not retry either memory update; \
         fully power-cycle the radio first."
    } else {
        "No write was started. This may be the gateway-owned interface; reconnect through the \
         other interface (USB when Menu 985 is Bluetooth) before retrying."
    };
    Err(format!(
        "{interruption}the Menu 650 operation completed with an error: {error}\n{guidance}"
    )
    .into())
}

/// Leave Reflector Terminal Mode through the non-gateway control interface.
///
/// The DV Gateway flag lives in radio flash and the gateway-owned interface
/// does not route the MCP programming handshake (`0M PROGRAM`). This operation
/// therefore requires the other interface selected by Menu 985, such as USB
/// when Bluetooth owns the gateway. On success the flag is cleared (verified
/// by read-back inside the session) and the radio reboots into normal control;
/// the current link drops with the reboot. Once boot completes, the caller may
/// open a new transport normally.
///
#[derive(Debug)]
enum InterruptibleMcpOperation {
    Completed(Result<DetachedMcpPageUpdate, kenwood_thd75::Error>),
    Interrupted {
        signal: Result<ProcessSignal, String>,
        completion: Result<DetachedMcpPageUpdate, kenwood_thd75::Error>,
    },
}

fn describe_mcp_interrupt(signal: Result<ProcessSignal, String>) -> String {
    signal.map_or_else(
        |error| format!("MCP signal listener failed: {error}"),
        |signal| format!("{} signal received", signal.description()),
    )
}

/// Clear Menu 650 while avoiding a write and detached reboot when its byte is
/// already zero.
async fn clear_gateway_mode_with_interrupt<I>(
    radio: &mut Radio<EitherTransport>,
    interrupt: I,
) -> InterruptibleMcpOperation
where
    I: Future<Output = Result<ProcessSignal, String>>,
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

/// Bind Menu 985 to the active endpoint and enable Menu 650 while treating a
/// catchable process termination as an interrupted MCP transaction that must
/// be explicitly recovered.
///
/// Returns a named outcome distinguishing a detached reboot from an unchanged
/// page whose CAT connection was restored without a flash write.
async fn write_terminal_mode_with_interrupt<I>(
    radio: &mut Radio<EitherTransport>,
    pc_interface: PcOutputInterface,
    interrupt: I,
) -> InterruptibleMcpOperation
where
    I: Future<Output = Result<ProcessSignal, String>>,
{
    // The caller proved the schema target with its own identity reads; the
    // library owns both registry-pinned Menu 985 / Menu 650 offsets.
    let operation = radio.set_reflector_terminal_mode_detached_unverified(pc_interface);
    tokio::pin!(operation);
    tokio::pin!(interrupt);
    tokio::select! {
        biased;
        result = &mut operation => InterruptibleMcpOperation::Completed(result),
        signal = &mut interrupt => {
            // Latch termination, then let the bounded two-page operation
            // finish its current exchange and cleanup. Dropping it here could
            // turn a raw exit byte into page payload.
            let completion = operation.await;
            InterruptibleMcpOperation::Interrupted { signal, completion }
        },
    }
}

const TERMINAL_MODE_TRANSITION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

#[cfg(not(test))]
const TERMINAL_MODE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

#[cfg(test)]
const TERMINAL_MODE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1);

#[derive(Debug)]
enum TerminalTransitionOutcome {
    Ready,
    Retry,
    Interrupted(Result<ProcessSignal, String>),
    TimedOut,
}

fn validate_gateway_mcp_target(
    model: RadioModel,
    firmware: &FirmwareIdentity,
) -> Result<(), String> {
    if is_supported_mcp_d75_schema_target(model, firmware) {
        return Ok(());
    }
    Err(format!(
        "refusing the Menu 985 / Menu 650 update: MCP offsets 0x1093 and 0x1CA0 are qualified \
         only for vendor firmware {MCP_D75_SCHEMA_FIRMWARE}; accepted exact CAT FV identities are \
         {MCP_D75_SCHEMA_FIRMWARE_IDENTITIES:?}, got {firmware}"
    ))
}

/// Ensure the radio is in Reflector Terminal Mode, returning it once
/// its link speaks MMDVM.
///
/// The input is a normal CAT link; callers that already own positive MMDVM
/// framing proof bypass this function. The CAT link is qualified directly
/// without injecting a raw MMDVM probe into its line parser. Reflector Terminal
/// Mode is enabled via an MCP write, which reboots the radio. The same transport
/// object retains its physical identity across that reset and is reopened
/// between bounded MMDVM probes until terminal mode answers. Keeping the
/// transition in-process avoids rediscovery selecting another interface or
/// another radio.
#[expect(
    clippy::too_many_lines,
    reason = "This is one persistent radio-state transition: target qualification, verified MCP \
              mutation, signal-safe ambiguous-write recovery, reboot polling, and fail-closed \
              cleanup must remain visibly ordered around the same owned Radio and signal guard."
)]
async fn ensure_terminal_mode(
    mut radio: Radio<EitherTransport>,
    process_signals: &ProcessSignalRouter,
    pc_interface: PcOutputInterface,
) -> Result<Radio<EitherTransport>, (Option<Radio<EitherTransport>>, String)> {
    println!("Checking if radio is already in D-STAR gateway mode.");
    // The offset below is qualified only for the TH-D75 1.03 memory schema.
    // Unlike the CAT-off exit path, CAT is available here, so prove both
    // values before entering MCP or touching flash.
    let identity = match radio.identify().await {
        Ok(identity) => identity,
        Err(error) => {
            return Err((
                Some(radio),
                format!("refusing terminal-mode memory write: could not identify radio: {error}"),
            ));
        }
    };
    let firmware = match radio.get_firmware_version().await {
        Ok(firmware) => firmware,
        Err(error) => {
            return Err((
                Some(radio),
                format!(
                    "refusing terminal-mode memory write: could not read firmware version: {error}"
                ),
            ));
        }
    };
    if let Err(error) = validate_gateway_mcp_target(identity.model, &firmware) {
        return Err((Some(radio), error));
    }

    // Enabling Reflector Terminal Mode via an MCP write reboots the radio,
    // drops this connection, and eventually changes the interface protocol.
    println!(
        "Verified {} firmware {firmware}; routing DV Gateway to {pc_interface} and enabling \
         Reflector Terminal Mode in one verified memory update.",
        identity.model
    );
    let mut critical_signals = match process_signals.begin_critical() {
        Ok(guard) => guard,
        Err(error) => return Err((Some(radio), error)),
    };
    // Detached: this write reboots the radio out of CAT mode, so the normal
    // exit-path CAT reconnect would race a deliberate protocol transition.
    // The identity-preserving diagnostic reopen below owns recovery.
    let interrupt = critical_signals.recv();
    let page_update =
        match write_terminal_mode_with_interrupt(&mut radio, pc_interface, interrupt).await {
            InterruptibleMcpOperation::Completed(Ok(update)) => update,
            InterruptibleMcpOperation::Completed(Err(error)) => {
                // Once the page operation starts, its structured failure may
                // report possible or verified writes. Never hand the handle
                // back for CAT: firmware may still switch to MMDVM later.
                drop(radio.disconnect().await);
                let router_context = critical_signals
                    .finish()
                    .err()
                    .map_or_else(String::new, |finish_error| format!("; {finish_error}"));
                return Err((
                    None,
                    format!(
                        "terminal-mode update completed with an error: {error}{router_context}. \
                         The connection was closed instead of resuming CAT. Follow the nested \
                         recovery guidance and use the reported possible/verified page lists; do \
                         not repeat the memory update blindly."
                    ),
                ));
            }
            InterruptibleMcpOperation::Interrupted { signal, completion } => {
                drop(radio.disconnect().await);
                let signal = describe_mcp_interrupt(signal);
                let router_context = critical_signals
                    .finish()
                    .err()
                    .map_or_else(String::new, |error| format!("; {error}"));
                let completion = match completion {
                    Ok(DetachedMcpPageUpdate::ChangedRadioRebooting) => {
                        "Menu 985 / Menu 650 changes were read-back verified and the radio is \
                         rebooting"
                            .to_owned()
                    }
                    Ok(DetachedMcpPageUpdate::UnchangedCatReady) => {
                        "Menu 985 / Menu 650 already matched and MCP cleanup completed its radio \
                         reset"
                            .to_owned()
                    }
                    Err(error) => format!(
                        "the bounded update completed with an error: {error}; use its \
                         possible/verified page lists and recovery guidance"
                    ),
                };
                return Err((
                    None,
                    format!(
                        "{signal}{router_context}; {completion}. Terminal-mode transition proof \
                         was stopped and the connection was closed."
                    ),
                ));
            }
        };

    match page_update {
        DetachedMcpPageUpdate::ChangedRadioRebooting => {
            println!(
                "Menu 985 / Menu 650 update verified; the radio is rebooting into terminal mode \
                 on {pc_interface}."
            );
        }
        DetachedMcpPageUpdate::UnchangedCatReady => {
            // Even an unchanged MCP operation resets and re-enumerates the radio
            // on exit. The CAT identity proof performed by normal MCP cleanup can
            // be only the early boot window before the terminal application takes
            // over, so it is not safe to hand this handle back as ordinary CAT.
            println!(
                "Menu 985 already selected {pc_interface} and Menu 650 was already Reflector \
                 Terminal. MCP exit reset the radio; waiting on this same interface."
            );
        }
    }

    // Terminal mode engages slowly: the radio first boots ordinary firmware
    // and may answer CAT for tens of seconds before the gateway application
    // takes over. Probe, then reopen this exact transport identity after every
    // negative result. A raw probe that lands in the temporary CAT window is
    // discarded with that reopen instead of contaminating a later ID command.
    println!("Waiting for MMDVM on {pc_interface}.");
    let deadline = tokio::time::Instant::now() + TERMINAL_MODE_TRANSITION_TIMEOUT;
    loop {
        let outcome = {
            let transition_step = async {
                tokio::time::sleep(TERMINAL_MODE_POLL_INTERVAL).await;
                if radio.probe_silent_link().await == LinkDiagnosis::MmdvmMode {
                    return true;
                }
                println!("Not yet. Reopening the same transport before the next probe.");
                if let Err(error) = radio.reopen_for_link_diagnosis().await {
                    tracing::debug!(%error, "terminal-mode transport is not ready to reopen");
                }
                false
            };
            tokio::pin!(transition_step);
            tokio::select! {
                biased;
                signal = critical_signals.recv() => {
                    TerminalTransitionOutcome::Interrupted(signal)
                }
                () = tokio::time::sleep_until(deadline) => TerminalTransitionOutcome::TimedOut,
                ready = &mut transition_step => {
                    if ready {
                        TerminalTransitionOutcome::Ready
                    } else {
                        TerminalTransitionOutcome::Retry
                    }
                }
            }
        };

        match outcome {
            TerminalTransitionOutcome::Ready => {
                if let Err(reason) = critical_signals.finish() {
                    drop(radio.disconnect().await);
                    return Err((
                        None,
                        format!(
                            "Terminal-mode transition stopped because {reason}. The connection \
                             was closed after binary-mode proof."
                        ),
                    ));
                }
                println!("Radio is in Reflector Terminal Mode.");
                return Ok(radio);
            }
            TerminalTransitionOutcome::Retry => {}
            TerminalTransitionOutcome::Interrupted(signal) => {
                let reason = signal.map_or_else(
                    |error| error,
                    |signal| format!("{} signal received", signal.description()),
                );
                drop(radio.disconnect().await);
                let additional_signal = critical_signals
                    .finish()
                    .err()
                    .map_or_else(String::new, |error| format!("; {error}"));
                return Err((
                    None,
                    format!(
                        "Terminal-mode transition stopped because {reason}{additional_signal}. \
                         The connection was closed; wait for the radio to finish booting and do \
                         not repeat the Menu 985 / Menu 650 update blindly."
                    ),
                ));
            }
            TerminalTransitionOutcome::TimedOut => break,
        }
    }

    drop(radio.disconnect().await);
    if let Err(reason) = critical_signals.finish() {
        return Err((
            None,
            format!(
                "Terminal-mode transition stopped because {reason}. The connection was closed; \
                 wait for the radio to finish booting."
            ),
        ));
    }
    Err((
        None,
        "Menu 985 and Menu 650 were read-back verified, but the selected link did not start \
         answering MMDVM probes during the transition window.\n\
         Not retrying the memory update, because another attempt would only reboot the radio \
         again."
            .into(),
    ))
}

/// Consume an inactive radio owner and enter the D-STAR REPL state.
async fn start_dstar_repl(
    entry_radio: DstarEntryRadio,
    args: &[&str],
    process_signals: &ProcessSignalRouter,
    endpoint_interface: transport::EndpointInterface,
) -> Option<ReplState> {
    if args.is_empty() {
        println!("Error: callsign required. Usage: dstar start <callsign> [reflector]");
        println!("Example: dstar start W1AW XRF030C");
        return Some(entry_radio.into_repl_state());
    }

    match enter_dstar(entry_radio, args, process_signals, endpoint_interface).await {
        Ok(mut session) => {
            if session.reflector.is_some() {
                println!("Monitoring. Press Ctrl-C to return to prompt.");
                run_dstar_monitor(&mut session).await;
            }
            Some(ReplState::Dstar(Box::new(session)))
        }
        Err((Some(entry_radio), error)) => {
            println!(
                "{}",
                thd75_repl::output::error(format_args!("entering D-STAR mode: {error}"))
            );
            Some(entry_radio.into_repl_state())
        }
        Err((None, error)) => {
            println!(
                "{}",
                thd75_repl::output::error(format_args!("entering D-STAR mode: {error}"))
            );
            None
        }
    }
}

struct DstarStartRequest {
    callsign: String,
    gateway_callsign: DstarCallsign,
    network_callsign: Callsign,
    link: Option<LinkArg>,
}

/// Validate all D-STAR startup input before any radio operation can occur.
fn parse_dstar_start(args: &[&str]) -> Result<DstarStartRequest, String> {
    let Some(raw_callsign) = args.first() else {
        return Err("callsign required".to_string());
    };
    if args.len() > 2 {
        return Err("too many arguments; expected dstar start <callsign> [reflector]".to_string());
    }

    // Callsigns are upper-case on D-STAR headers and DPlus authentication.
    let callsign = raw_callsign.to_ascii_uppercase();
    let gateway_callsign = DstarCallsign::new(&callsign)
        .map_err(|error| format!("Invalid station callsign {callsign}: {error}"))?;
    let network_callsign = Callsign::try_from_str(gateway_callsign.as_str())
        .map_err(|error| format!("Invalid network callsign {callsign}: {error}"))?;
    let link = args
        .get(1)
        .map(|reflector| reflector.to_ascii_uppercase())
        .as_deref()
        .map(parse_link_arg)
        .transpose()?;

    Ok(DstarStartRequest {
        callsign,
        gateway_callsign,
        network_callsign,
        link,
    })
}

/// Produce a positively proved MMDVM owner for D-STAR startup.
///
/// A CAT owner is qualified and transitioned into Reflector Terminal Mode via
/// [`ensure_terminal_mode`]. An MMDVM owner already carries positive complete
/// framing proof and bypasses every CAT and MCP operation.
///
/// On a recoverable error, returns the radio with its protocol state attached.
async fn prepare_dstar_radio(
    entry_radio: DstarEntryRadio,
    process_signals: &ProcessSignalRouter,
    endpoint_interface: transport::EndpointInterface,
) -> Result<Radio<EitherTransport>, (Option<DstarEntryRadio>, String)> {
    match entry_radio {
        DstarEntryRadio::Mmdvm(radio) => {
            println!("Checking if radio is already in D-STAR gateway mode.");
            println!("The selected link already speaks MMDVM.");
            Ok(radio)
        }
        DstarEntryRadio::Cat(radio) => {
            let pc_interface = match endpoint_interface {
                transport::EndpointInterface::Known(interface) => interface,
                transport::EndpointInterface::UnspecifiedExplicitPort => {
                    return Err((
                        Some(DstarEntryRadio::Cat(radio)),
                        "the explicit serial port does not identify whether it is USB or \
                         Bluetooth. Relaunch with --port-interface usb or \
                         --port-interface bluetooth before enabling persistent D-STAR mode"
                            .to_owned(),
                    ));
                }
            };
            match ensure_terminal_mode(radio, process_signals, pc_interface).await {
                Ok(radio) => Ok(radio),
                Err((Some(radio), error)) => {
                    let (radio, error) = recover_cat_after_failed_mode_entry(radio, error).await;
                    Err((radio.map(DstarEntryRadio::Cat), error))
                }
                Err((None, error)) => Err((None, error)),
            }
        }
    }
}

/// Validate the request, acquire an MMDVM owner, and start D-STAR gateway mode.
///
/// Gateway-init errors return an MMDVM owner; validation and CAT-side
/// preflight errors preserve the caller's original state when safe.
async fn enter_dstar(
    entry_radio: DstarEntryRadio,
    args: &[&str],
    process_signals: &ProcessSignalRouter,
    endpoint_interface: transport::EndpointInterface,
) -> Result<DstarSession, (Option<DstarEntryRadio>, String)> {
    let request = match parse_dstar_start(args) {
        Ok(request) => request,
        Err(error) => return Err((Some(entry_radio), error)),
    };
    let DstarStartRequest {
        callsign,
        gateway_callsign,
        network_callsign: callsign_typed,
        link: link_arg,
    } = request;

    let radio = prepare_dstar_radio(entry_radio, process_signals, endpoint_interface).await?;

    // Radio is now in MMDVM mode. Start the gateway.
    println!("Starting D-STAR gateway as {callsign}.");

    // Preserve the TH-D75 AZM transient-gateway wire choice `TN 3,1`.
    let config = DstarGatewayConfig::new(gateway_callsign, TncDataBand::B);
    let gateway = match DstarGateway::start_gateway_mode(radio, config).await {
        Ok(gw) => gw,
        Err((radio, error)) => {
            return Err((
                radio.map(DstarEntryRadio::Mmdvm),
                format!("Gateway init failed: {error}"),
            ));
        }
    };
    println!("MMDVM modem initialized.");

    // Connect to reflector if specified.
    let (reflector, link_arg) = if let Some(arg) = link_arg {
        match connect_reflector(callsign_typed, &arg).await {
            Ok(client) => (Some(client), Some(arg)),
            Err(e) => {
                println!("Error: could not connect to reflector: {e}");
                println!("Gateway active without reflector. Use link command to connect later.");
                (None, None)
            }
        }
    } else {
        println!("No reflector specified. Use link command to connect.");
        (None, None)
    };

    println!("D-STAR gateway active. Type dstar stop to exit.");
    println!("Commands: monitor, link, unlink, echo, text, heard, status, dstar stop");
    let default_module = Module::C;
    let (local_module, reflector_module) = link_arg
        .as_ref()
        .map_or((default_module, default_module), |arg| {
            (arg.local_module, arg.reflector_module)
        });
    // Parse the reflector name into a Callsign. This is used in the
    // radio-to-reflector relay path to build the outbound rpt2 field
    // per ircDDBGateway convention (see build_reflector_header). If
    // no reflector was specified on the command line, we fall back
    // to the station callsign as a placeholder; the relay path only
    // cares once link has actually connected.
    let reflector_callsign = link_arg.as_ref().map_or(callsign_typed, |arg| {
        Callsign::try_from_str(&arg.reflector_name).unwrap_or(callsign_typed)
    });
    Ok(DstarSession {
        gateway,
        reflector,
        callsign: callsign_typed,
        tx_stream_id: None,
        tx_seq: 0,
        local_module,
        reflector_module,
        reflector_callsign,
        rx_stream_id: None,
        echo: dstar_gateway_core::echo::EchoUnit::new(),
        rx_slow_data: SlowDataTextCollector::new(),
        rx_last_slow_text: None,
        tx_text: None,
        tx_slow_data: Vec::new(),
        tx_slow_data_idx: 0,
        rx_frame_count: 0,
        rx_stream_start: None,
        ambe_capture: open_ambe_capture_from_env(),
        last_rx_voice_frame: None,
        last_relay_at: None,
        pad_frames_emitted: 0,
        radio_link_lost: false,
    })
}

/// If `AMBE_CAPTURE=/path/to/file.ambe` is set, open that file for
/// append. Every `DstarEvent::VoiceData` the radio emits during TX
/// will write its 9-byte AMBE to this file, producing a ground-truth
/// record of DVSI-encoder output for whatever the operator speaks
/// into the microphone.
fn open_ambe_capture_from_env() -> Option<std::fs::File> {
    let path = std::env::var_os("AMBE_CAPTURE")?;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(f) => {
            println!(
                "AMBE capture enabled: writing DVSI-origin voice frames to {}",
                std::path::Path::new(&path).display()
            );
            Some(f)
        }
        Err(e) => {
            println!(
                "Warning: AMBE_CAPTURE={} could not be opened: {e}",
                std::path::Path::new(&path).display()
            );
            None
        }
    }
}

/// Parse a reflector string like `"XRF030C"` into (name, module).
fn parse_reflector_arg(s: &str) -> Option<(String, char)> {
    if s.len() < 4 {
        return None;
    }
    let module = s.chars().last()?;
    if !module.is_ascii_uppercase() {
        return None;
    }
    let name = &s[..s.len() - 1];
    Some((name.to_uppercase(), module))
}

/// Parsed form of the `link` command argument.
///
/// Supports two forms:
/// - `XRF030C`: link to `XRF030` module `C`, with our local module
///   matching the reflector module (`C`).
/// - `B:XRF030C`: link to `XRF030` module `C`, but present our local
///   module as `B` for cross-module routing.
struct LinkArg {
    reflector_name: String,
    reflector_module: Module,
    local_module: Module,
}

fn parse_link_arg(s: &str) -> Result<LinkArg, String> {
    let (local_prefix, refl_str) = if let Some((left, right)) = s.split_once(':') {
        (Some(left), right)
    } else {
        (None, s)
    };
    let (reflector_name, refl_mod_char) = parse_reflector_arg(refl_str)
        .ok_or_else(|| format!("Invalid reflector format: {refl_str}. Expected e.g. XRF030C"))?;
    let reflector_module = Module::try_from_char(refl_mod_char)
        .map_err(|e| format!("Invalid reflector module letter: {e}"))?;
    let local_module = if let Some(prefix) = local_prefix {
        if prefix.len() != 1 {
            return Err(format!(
                "Local module prefix must be a single letter, got {prefix:?}"
            ));
        }
        let c = prefix.chars().next().unwrap_or_default();
        Module::try_from_char(c).map_err(|e| format!("Invalid local module letter: {e}"))?
    } else {
        reflector_module
    };
    Ok(LinkArg {
        reflector_name,
        reflector_module,
        local_module,
    })
}

/// Pi-Star host file URLs.
const HOST_FILES: &[(&str, &str, u16)] = &[
    (
        "DExtra_Hosts.txt",
        "https://www.pistar.uk/downloads/DExtra_Hosts.txt",
        30001,
    ),
    (
        "DPlus_Hosts.txt",
        "https://www.pistar.uk/downloads/DPlus_Hosts.txt",
        20001,
    ),
    (
        "DCS_Hosts.txt",
        "https://www.pistar.uk/downloads/DCS_Hosts.txt",
        30051,
    ),
];

/// Get the host files config directory (platform-appropriate).
///
/// - macOS: `~/Library/Application Support/thd75-repl/`
/// - Linux: `~/.config/thd75-repl/`
/// - Windows: `%APPDATA%/thd75-repl/`
fn host_files_dir() -> std::path::PathBuf {
    dirs_next::config_dir()
        .unwrap_or_else(|| dirs_next::home_dir().unwrap_or_default().join(".config"))
        .join("thd75-repl")
}

/// Download Pi-Star host files if not present.
async fn ensure_host_files() {
    let dir = host_files_dir();
    let all_present = HOST_FILES
        .iter()
        .all(|(name, _, _)| dir.join(name).exists());
    if all_present {
        return;
    }

    println!("D-STAR reflector host files are not installed.");
    println!("Download them from pistar.uk? (y/n)");

    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return;
    }
    if !input.trim().eq_ignore_ascii_case("y") {
        println!("Skipped. Place host files in {} manually.", dir.display());
        return;
    }

    println!("Downloading host files.");
    drop(std::fs::create_dir_all(&dir));

    for (name, url, _) in HOST_FILES {
        let path = dir.join(name);
        if path.exists() {
            continue;
        }
        // Use a simple TCP GET since we don't have an HTTP client dep.
        // Shell out to curl which is available on macOS/Linux.
        // `-f` makes curl exit non-zero on HTTP errors (404, 500);
        // without it the error page body would be written to the
        // hosts file and reported as a successful download.
        match tokio::process::Command::new("curl")
            .args(["-fsSL", "-o"])
            .arg(&path)
            .arg(url)
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                println!("Downloaded {name}.");
            }
            Ok(output) => {
                // `-o` may have created a partial or empty file
                // before the failure; remove it so the presence
                // check above retries the download next time
                // instead of treating the stub as installed.
                drop(std::fs::remove_file(&path));
                println!(
                    "Error: failed to download {name}: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            Err(e) => {
                println!("Error: could not run curl to download {name}: {e}");
            }
        }
    }
}

/// Filename for the user's local/vanity reflector host list. Seeded
/// on first run with the POLARIS local test reflector and parsed as
/// `DExtra` (port 30001). Add your own reflectors here manually.
const LOCAL_HOSTS_FILE: &str = "Local_Hosts.txt";

/// Initial content written to `Local_Hosts.txt` if missing. Documents
/// the format and seeds the POLARIS test reflector so `link POLARIS`
/// works out of the box against a locally-running `polaris` server.
const LOCAL_HOSTS_SEED: &str = "\
# Local / vanity D-STAR reflectors. Each line: NAME ADDRESS [PORT]
# Entries here are treated as DExtra (default port 30001).
# POLARIS is the local test reflector (`cargo run -p dstar-gateway-server --bin polaris`).
POLARIS\t127.0.0.1
";

/// Load Pi-Star host files from `~/.config/thd75-repl/`.
///
/// Also seeds and loads `Local_Hosts.txt` (`DExtra` port) on first
/// use so vanity callsigns like POLARIS resolve to the local test
/// server without any manual setup.
fn load_host_files() -> HostFile {
    let mut hosts = HostFile::new();
    let dir = host_files_dir();

    for (name, _, port) in HOST_FILES {
        let path = dir.join(name);
        if let Ok(content) = std::fs::read_to_string(&path) {
            hosts.parse(&content, *port);
        }
    }

    // Seed + load the local/vanity host file.
    let local_path = dir.join(LOCAL_HOSTS_FILE);
    if !local_path.exists() {
        drop(std::fs::create_dir_all(&dir));
        if let Err(e) = std::fs::write(&local_path, LOCAL_HOSTS_SEED) {
            tracing::warn!(error = %e, "failed to seed Local_Hosts.txt");
        }
    }
    if let Ok(content) = std::fs::read_to_string(&local_path) {
        hosts.parse(&content, 30001);
    }

    hosts
}

/// Connect deadline handed to the shared handshake pump.
const REFLECTOR_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Bind an ephemeral local UDP socket for a new reflector session.
async fn bind_reflector_socket() -> Result<std::sync::Arc<tokio::net::UdpSocket>, String> {
    tokio::net::UdpSocket::bind("0.0.0.0:0")
        .await
        .map(std::sync::Arc::new)
        .map_err(|e| format!("UDP bind failed: {e}"))
}

/// Drive a `Session<P, Connecting>` through the reflector handshake via
/// the shared pump in `dstar_gateway::tokio_shell::drive_connecting`,
/// flattening its typed [`ConnectError`](dstar_gateway::tokio_shell::ConnectError)
/// into this module's `String` error convention.
///
/// Shared between [`connect_dextra`], [`connect_dplus`], and
/// [`connect_dcs`]; the handshake packet count differs across
/// protocols but the pump is identical.
async fn drive_handshake_to_connected<P>(
    session: Session<P, Connecting>,
    socket: &tokio::net::UdpSocket,
) -> Result<Session<P, Connected>, String>
where
    P: dstar_gateway_core::session::client::Protocol,
{
    dstar_gateway::tokio_shell::drive_connecting(session, socket, REFLECTOR_CONNECT_TIMEOUT)
        .await
        .map_err(|error| error.to_string())
}

/// Build and drive a full `DExtra` connect handshake.
async fn connect_dextra(
    callsign: Callsign,
    peer: std::net::SocketAddr,
    local_module: Module,
    reflector_module: Module,
    reflector_callsign: Callsign,
) -> Result<AsyncSession<DExtra>, String> {
    tracing::info!(
        target: "thd75_repl::reflector",
        protocol = "DExtra",
        %callsign,
        %local_module,
        %reflector_module,
        peer = %peer,
        "connecting to reflector"
    );
    let socket = bind_reflector_socket().await?;

    let configured = Session::<DExtra, _>::builder()
        .callsign(callsign)
        .local_module(local_module)
        .reflector_module(reflector_module)
        .reflector_callsign(reflector_callsign)
        .peer(peer)
        .build();

    let connecting = configured
        .connect(std::time::Instant::now())
        .map_err(|f| format!("enqueue LINK failed: {}", f.error))?;

    let connected = drive_handshake_to_connected(connecting, &socket).await?;
    Ok(AsyncSession::spawn(connected, socket))
}

/// Build, authenticate, and drive a full `DPlus` (REF) connect handshake.
///
/// Performs the mandatory TCP auth step via [`AuthClient`] first, then
/// attaches the returned host list to the sans-io session to satisfy
/// the `Authenticated` typestate. If the TCP auth fails, the function
/// falls back to an empty host list so the caller can still attempt
/// the UDP handshake (matching the legacy best-effort behavior).
async fn connect_dplus(
    callsign: Callsign,
    peer: std::net::SocketAddr,
    local_module: Module,
    reflector_module: Module,
    reflector_callsign: Callsign,
) -> Result<AsyncSession<DPlus>, String> {
    tracing::info!(
        target: "thd75_repl::reflector",
        protocol = "DPlus",
        %callsign,
        %local_module,
        %reflector_module,
        peer = %peer,
        "connecting to reflector"
    );
    println!("Authenticating with D-STAR gateway server.");
    let hosts = match AuthClient::new().authenticate(callsign).await {
        Ok(h) => {
            println!("Authentication successful.");
            h
        }
        Err(e) => {
            println!(
                "Warning: authentication failed: {e}. \
                 Trying to connect anyway (previous auth may still be valid)."
            );
            dstar_gateway_core::codec::dplus::HostList::new()
        }
    };

    let socket = bind_reflector_socket().await?;

    let configured = Session::<DPlus, _>::builder()
        .callsign(callsign)
        .local_module(local_module)
        .reflector_module(reflector_module)
        .reflector_callsign(reflector_callsign)
        .peer(peer)
        .build();

    let authenticated = configured
        .authenticate(hosts)
        .map_err(|f| format!("attach host list failed: {}", f.error))?;

    let connecting = authenticated
        .connect(std::time::Instant::now())
        .map_err(|f| format!("enqueue LINK1 failed: {}", f.error))?;

    let connected = drive_handshake_to_connected(connecting, &socket).await?;
    Ok(AsyncSession::spawn(connected, socket))
}

/// Build and drive a full `DCS` connect handshake.
async fn connect_dcs(
    callsign: Callsign,
    peer: std::net::SocketAddr,
    local_module: Module,
    reflector_module: Module,
    reflector_callsign: Callsign,
) -> Result<AsyncSession<Dcs>, String> {
    tracing::info!(
        target: "thd75_repl::reflector",
        protocol = "DCS",
        %callsign,
        %local_module,
        %reflector_module,
        peer = %peer,
        "connecting to reflector"
    );
    let socket = bind_reflector_socket().await?;

    let configured = Session::<Dcs, _>::builder()
        .callsign(callsign)
        .local_module(local_module)
        .reflector_module(reflector_module)
        .reflector_callsign(reflector_callsign)
        .peer(peer)
        .build();

    let connecting = configured
        .connect(std::time::Instant::now())
        .map_err(|f| format!("enqueue CONNECT failed: {}", f.error))?;

    let connected = drive_handshake_to_connected(connecting, &socket).await?;
    Ok(AsyncSession::spawn(connected, socket))
}

/// Re-establish the reflector link in place using the session's
/// remembered link parameters.
///
/// Triggered automatically when a `Disconnected(KeepaliveInactivity)`
/// event arrives; those mean we lost contact with the reflector for
/// 30 s but neither side intentionally closed the link, so a fresh
/// `LINK1` handshake usually restores service. On success
/// `session.reflector` is replaced with the newly-spawned client; on
/// failure it is left as `None` so the user can retry manually with
/// `link <reflector>`.
async fn try_reconnect_reflector(session: &mut DstarSession) {
    let link = LinkArg {
        reflector_name: render_gateway_callsign(session.reflector_callsign),
        reflector_module: session.reflector_module,
        local_module: session.local_module,
    };
    aprintln!(
        "Auto-reconnecting to {} module {}.",
        link.reflector_name,
        link.reflector_module
    );
    match connect_reflector(session.callsign, &link).await {
        Ok(client) => {
            session.reflector = Some(client);
        }
        Err(e) => {
            aprintln!(
                "{}",
                thd75_repl::output::error(format_args!("auto-reconnect failed: {e}"))
            );
        }
    }
}

/// Connect to a reflector using a parsed [`LinkArg`].
///
/// The caller validates the reflector string into `LinkArg` first so
/// this function takes already-typed parameters and can focus on
/// protocol selection, host lookup, and driving the sans-io
/// typestate session through the handshake.
async fn connect_reflector(callsign: Callsign, link: &LinkArg) -> Result<ReflectorSession, String> {
    let ref_name = &link.reflector_name;

    // Ensure host files exist, downloading if needed.
    ensure_host_files().await;
    let hosts = load_host_files();
    let entry = hosts.lookup(ref_name).ok_or_else(|| {
        format!(
            "Reflector {ref_name} not found in host files. \
             Download Pi-Star files or add {ref_name} to Local_Hosts.txt in \
             ~/.config/thd75-repl/."
        )
    })?;

    // Prefix-based protocol detection covers the stock Pi-Star host
    // files (REF/XRF/XLX/DCS). Vanity entries in Local_Hosts.txt fall
    // through to port-based inference; any other port defaults to
    // DExtra because Local_Hosts.txt is parsed with that port.
    let protocol = ProtocolKind::from_reflector_prefix(ref_name)
        .or_else(|| ProtocolKind::from_port(entry.port))
        .unwrap_or(ProtocolKind::DExtra);

    let addr = format!("{}:{}", entry.address, entry.port)
        .to_socket_addrs()
        .map_err(|e| format!("Address resolution failed for {}: {e}", entry.address))?
        .next()
        .ok_or_else(|| format!("No address found for {}", entry.address))?;

    println!(
        "Connecting to {ref_name} module {} (local {}) at {addr}.",
        link.reflector_module, link.local_module
    );
    println!("Waiting for reflector acknowledgement.");

    // Parse the reflector name into a Callsign so the sans-io
    // session can embed it in DCS wire packets (the DCS client's
    // LINK / UNLINK / POLL packets each carry the target reflector
    // callsign, and the default `DCS001  ` fallback would make us
    // invisible to any other DCS reflector). DPlus and DExtra
    // don't carry it on the wire but we pass it along anyway as
    // metadata.
    let reflector_callsign = Callsign::try_from_str(ref_name)
        .map_err(|e| format!("Reflector name {ref_name:?} is not a valid callsign: {e}"))?;

    let session = match protocol {
        ProtocolKind::DPlus => connect_dplus(
            callsign,
            addr,
            link.local_module,
            link.reflector_module,
            reflector_callsign,
        )
        .await
        .map(ReflectorSession::DPlus)?,
        ProtocolKind::DExtra => connect_dextra(
            callsign,
            addr,
            link.local_module,
            link.reflector_module,
            reflector_callsign,
        )
        .await
        .map(ReflectorSession::DExtra)?,
        ProtocolKind::Dcs => connect_dcs(
            callsign,
            addr,
            link.local_module,
            link.reflector_module,
            reflector_callsign,
        )
        .await
        .map(ReflectorSession::Dcs)?,
        // `ProtocolKind` is `#[non_exhaustive]`; any future variants
        // would need their own `connect_*` helper above. Until then,
        // only the three classic protocols are reachable here.
        _ => return Err(format!("Unsupported reflector protocol: {protocol:?}")),
    };

    aprintln!("Connected to {ref_name} module {}.", link.reflector_module);
    Ok(session)
}

/// Stop D-STAR gateway traffic, then wait for the operator to turn Menu 650
/// off before reconnecting and proving CAT mode.
async fn exit_dstar(
    gw: DstarGateway<EitherTransport, PersistentMmdvm>,
    cli_port: Option<&str>,
    cli_baud: u32,
    port_interface_override: Option<PcOutputInterface>,
) -> Result<(Radio<EitherTransport>, transport::EndpointInterface), String> {
    // Stop the gateway without sending the transient `TN 0,0` exit. Menu 650
    // keeps this link in its persistent DV Gateway/MMDVM mode.
    println!("Stopping D-STAR gateway.");
    let radio = gw
        .stop()
        .await
        .map_err(|e| format!("Gateway stop failed: {e}"))?;

    // Disconnect BT to release the RFCOMM channel.
    drop(radio.disconnect().await);

    // The radio is still in DV Gateway/MMDVM mode. Its gateway-owned link
    // does not route MCP entry, and this session does not own a qualified
    // transport on the other interface. The operator must change Menu 650 on
    // the front panel before this same-link reconnect can prove CAT.
    println!("D-STAR gateway stopped.");
    println!("Please set Menu 650 (DV Gateway) to Off on the radio.");
    println!("Press Enter when done.");

    // Blocking stdin read. Note: once any monitor loop has awaited
    // `tokio::signal::ctrl_c()`, tokio owns the process SIGINT
    // handler for good; Ctrl-C no longer terminates the process, so
    // Enter is the only way past this prompt. Don't advertise Ctrl-C
    // here.
    let mut input = String::new();
    drop(std::io::stdin().read_line(&mut input));

    // Reconnect: should be in CAT mode now if user changed Menu 650.
    println!("Reconnecting.");
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let opened = transport::discover_and_open(cli_port, cli_baud, port_interface_override)
        .map_err(|e| format!("Reconnect failed: {e}"))?;

    let reopened_interface = opened.endpoint_interface;
    let mut radio = Radio::connect_with_tnc_exit(opened.transport)
        .await
        .map_err(|e| format!("Connect failed: {e}"))?;

    // Verify we're back in CAT mode.
    if radio.identify().await.is_ok() {
        println!("Radio restored to normal mode.");
        Ok((radio, reopened_interface))
    } else {
        println!("Error: radio is still in DV Gateway/MMDVM mode.");
        println!("Please set Menu 650 to Off and restart the REPL.");
        Err("Radio still in MMDVM mode".into())
    }
}

/// Dispatch a command in D-STAR mode.
///
/// Run continuous D-STAR monitoring with background keepalives and
/// voice relay. Polls both radio MMDVM and reflector UDP in a loop
/// until the user sends a SIGINT (Ctrl-C).
async fn run_dstar_monitor(session: &mut DstarSession) {
    // Set short timeout for radio polling so it doesn't block.
    session
        .gateway
        .set_event_timeout(std::time::Duration::from_millis(10));

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            _ = &mut ctrl_c => {
                println!("Monitor stopped. Type monitor to resume, or help for commands.");
                // Restore default timeout.
                session.gateway.set_event_timeout(std::time::Duration::from_millis(500));
                break;
            }
            () = dstar_poll_cycle(session) => {}
        }
    }
}

/// D-STAR voice frame interval: 50 fps, one frame per 20 ms.
///
/// Matches the rate the modem's internal AMBE decoder consumes
/// frames at. Used as:
/// 1. The tick interval for the silence-padding timer in
///    [`dstar_poll_cycle`].
/// 2. The cadence check for *subsequent* pads inside a single
///    silence gap (see [`emit_silence_pad_if_needed`]). Once we've
///    decided a gap is real and started padding, each additional
///    pad fires on modem consumption rhythm so the FIFO stays
///    fed without over-production.
const PAD_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// How long the reflector must be silent before we decide a gap
/// is real and start padding.
///
/// The modem's D-STAR FIFO averages ~100-140 ms of buffered audio
/// per the `dstar_space_before` telemetry (mean 120/125 slots
/// free, i.e. 5-7 slots used = 100-140 ms). Any gap shorter than
/// the buffer depth gets absorbed natively by the modem without
/// an audible cut, so padding short gaps is pure over-production:
/// every synthetic frame becomes permanent latency the modem
/// plays out at 50 fps. A prior 30 ms threshold fired on routine
/// 20-50 ms inter-frame jitter and accumulated 6+ seconds of
/// audio delay over a 57 s stream.
///
/// 100 ms lets the FIFO absorb all normal jitter and only kicks
/// in on genuine reflector-side silences where the FIFO would
/// otherwise drain to zero.
const PAD_INITIAL_THRESHOLD: std::time::Duration = std::time::Duration::from_millis(100);

/// Cap on consecutive padding frames per silence gap.
///
/// 30 frames × 20 ms = 600 ms of padding per gap. Past that, the
/// reflector has been silent long enough that continuing to stuff
/// stale audio is counter-productive: the FIFO reaches steady
/// state at full, the BT/radio buffers accumulate, and by the time
/// real frames arrive we've added seconds of latency that never
/// drain. Better to let the modem underrun cleanly and the radio
/// squelch, which gives the operator an accurate signal that the
/// reflector is having real trouble.
const PAD_FRAMES_MAX: u32 = 30;

/// Emit one padding frame if the reflector has gone silent mid-stream.
///
/// No-ops when no stream is active, when the pad-template frame is
/// absent (stream hasn't produced its first voice frame yet), when
/// the last relay was too recent to count as a gap, or when the
/// per-gap cap has already been reached. On match, sends a copy of
/// the last received voice frame through the same unpaced relay
/// path the real frames take, then updates the relay timestamp so
/// the next tick re-evaluates against the moment of padding (not
/// the moment of the last real frame). This keeps us emitting at
/// a steady 20 ms cadence until either a real frame arrives or the
/// cap hits.
async fn emit_silence_pad_if_needed(session: &mut DstarSession) {
    if session.rx_stream_id.is_none() {
        return;
    }
    let Some(last_at) = session.last_relay_at else {
        return;
    };
    if session.pad_frames_emitted >= PAD_FRAMES_MAX {
        return;
    }
    let Some(pad_frame) = session.last_rx_voice_frame else {
        return;
    };

    // Two-phase threshold: wait until the modem's FIFO would
    // actually be in trouble before we *start* padding, then once
    // we're in pad mode keep up with the 50 fps consumption rate
    // so the FIFO stays stable rather than draining. Re-checking
    // `last_relay_at.elapsed()` against `PAD_INTERVAL` on every
    // subsequent pad gives an effective 20 ms cadence that matches
    // the modem's consumption exactly: no over-production, no
    // under-production.
    let threshold = if session.pad_frames_emitted == 0 {
        PAD_INITIAL_THRESHOLD
    } else {
        PAD_INTERVAL
    };
    if last_at.elapsed() < threshold {
        return;
    }

    let pad_no = session.pad_frames_emitted;
    if pad_no == 0 {
        // First pad of this gap: log at debug so operators can
        // correlate audible smoothing events with reflector
        // silence in the trace log without drowning the stream
        // in per-pad-frame noise.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "Silence-pad interval is measured in milliseconds of radio dead-air. The \
                      value will be tens of milliseconds in practice (well under u64::MAX), so \
                      the u128→u64 truncation can never lose bits."
        )]
        let elapsed_ms = last_at.elapsed().as_millis() as u64;
        tracing::debug!(
            target: "thd75_repl::reflector",
            elapsed_ms,
            "reflector silence: emitting silence pad to keep modem fed"
        );
    }
    tracing::trace!(
        target: "thd75_repl::hang_hunt",
        pad_no,
        "emit_silence_pad: awaiting send_voice_unpaced"
    );
    if let Err(e) = session.gateway.send_voice_unpaced(&pad_frame).await {
        tracing::warn!(
            target: "thd75_repl::reflector",
            error = %e,
            "silence pad send failed; giving up this gap"
        );
        session.pad_frames_emitted = PAD_FRAMES_MAX;
        return;
    }
    session.last_relay_at = Some(std::time::Instant::now());
    session.pad_frames_emitted = pad_no.saturating_add(1);
    tracing::trace!(
        target: "thd75_repl::hang_hunt",
        pad_no = pad_no + 1,
        "emit_silence_pad: sent"
    );
}

/// Maximum number of reflector events processed inline per poll cycle.
///
/// Caps the inline processing loop at roughly one D-STAR superframe
/// (21 voice frames plus a handful of control events = 24) so the
/// outer `select!` in [`run_dstar_monitor`] can yield to `ctrl_c`,
/// radio polling, and the rest of the cycle even while voice is
/// flowing continuously at the 20 ms D-STAR frame cadence.
///
/// Without this cap the inline loop never breaks during an active
/// voice burst: `DPlus`'s internal `recv_from` has a 100 ms timeout
/// and reflectors send voice frames every 20 ms, so `poll()` always
/// returns `Ok(Some(_))` well before the recv timeout fires.
const MAX_EVENTS_PER_CYCLE: usize = 24;

/// One cycle of the D-STAR poll loop: poll reflector (keepalive + rx),
/// poll radio (mmdvm events), relay between them.
///
/// **Inline processing:** each reflector event is relayed to the
/// radio immediately after it is pulled from the socket, instead of
/// the previous drain-into-Vec-then-process-Vec pattern. The old
/// two-phase version blocked the relay for the duration of the drain
/// phase (typically 30-80 ms waiting for `poll()` to return `None`
/// at the end of a superframe), during which the 20 already-drained
/// voice frames sat in the Vec instead of going to the radio. The
/// MMDVM modem's small voice buffer underran during those gaps,
/// dropping the receive popup. Observed live on REF030 C with 85 ms
/// gaps between BT write bursts mid-stream; confirmed in the trace
/// log at `kenwood_thd75::transport::bluetooth::inner: BT write`
/// timestamps. With inline processing, each frame is handed off to
/// the paced `send_voice` immediately, so the modem sees a steady
/// 20 ms cadence with no >20 ms gaps inside a stream.
async fn dstar_poll_cycle(session: &mut DstarSession) {
    // Matches the legacy `ReflectorClient::poll` 100 ms inner recv
    // timeout, which gives the reflector session task a short window to
    // deliver a frame before we yield control back to the outer
    // `select!` for radio polling and ctrl_c.
    const EVENT_POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);

    // Local pad_tick: lives for the duration of this cycle only.
    // The previous design put the tick on the outer `run_dstar_monitor`
    // select!, which caused tokio to cancel `dstar_poll_cycle` every
    // time the tick won the race. Cancelling in the middle of
    // `relay_reflector_to_radio.await` dropped 48 of 2634 in-flight
    // relays in a 60 s trace and repeatedly short-circuited the radio
    // drain below, the same mmdvm event-channel backpressure that
    // produced the original deadlock. Making the tick a branch of
    // this cycle's inner select! keeps cancellation scoped to a
    // single iteration and the radio drain is reached reliably.
    let mut pad_tick = tokio::time::interval(PAD_INTERVAL);
    pad_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    for _ in 0..MAX_EVENTS_PER_CYCLE {
        // Race "next reflector event" vs "20 ms pad tick", scoping
        // the `session.reflector.as_mut()` borrow so it doesn't
        // conflict with `emit_silence_pad_if_needed(session)` which
        // needs the full session.
        let mut event_opt: Option<RuntimeEvent> = None;
        let mut pad_fired = false;
        let mut reflector_closed = false;
        {
            let Some(client) = session.reflector.as_mut() else {
                break;
            };
            tokio::select! {
                biased;
                r = tokio::time::timeout(EVENT_POLL_TIMEOUT, client.next_event()) => {
                    match r {
                        Ok(Some(e)) => event_opt = Some(e),
                        Ok(None) => reflector_closed = true,
                        Err(_) => { /* 100 ms elapsed, no event */ }
                    }
                }
                _ = pad_tick.tick() => {
                    pad_fired = true;
                }
            }
        }
        if reflector_closed {
            session.reflector = None;
            break;
        }
        if let Some(event) = event_opt {
            trace_reflector_event(&event);
            // Only print VoiceStart for new streams (avoid duplicate
            // announcements on superframe-boundary header refreshes
            // that the parser's stream tracker did not suppress).
            if let RuntimeEvent::VoiceStart { stream_id, .. } = &event {
                if session.rx_stream_id != Some(*stream_id) {
                    print_reflector_event(&event, session);
                }
            } else {
                print_reflector_event(&event, session);
            }
            relay_reflector_to_radio(session, &event).await;

            // Auto-reconnect on keepalive inactivity. The other disconnect
            // reasons (`Rejected`, `UnlinkAcked`, `DisconnectTimeout`)
            // reflect deliberate or terminal closures; never retry those
            // automatically.
            if matches!(
                event,
                RuntimeEvent::Disconnected {
                    reason: DisconnectReason::KeepaliveInactivity
                }
            ) {
                session.reflector = None;
                try_reconnect_reflector(session).await;
                break;
            }
        } else if pad_fired {
            emit_silence_pad_if_needed(session).await;
        } else {
            // client.next_event timed out with no event and no pad
            // tick this iteration. Nothing useful is happening, so
            // exit and let the outer loop re-enter (which also gives
            // ctrl_c a chance to fire).
            break;
        }
    }

    // Poll radio: drain MMDVM events until the queue empties.
    //
    // The radio emits voice-data events at the D-STAR frame rate
    // (~50 fps) while keyed.  A prior revision handled at most ONE
    // event per outer cycle, which capped the radio→reflector
    // forwarding rate at 10 fps (one event per 100 ms cycle) and
    // produced the "voice in bursts with ~80 ms gaps" symptom the
    // user reported on their POLARIS test reflector.  Draining
    // all immediately-available events per cycle lets us keep pace
    // with the modem; the `MAX_EVENTS_PER_CYCLE` cap prevents a
    // runaway radio (huge pending queue) from starving the outer
    // select loop on reflector input or Ctrl-C.
    //
    // Drop the per-event timeout to 5 ms for the drain so "nothing
    // pending" exits fast: the first call reuses the modem's
    // already-buffered events with zero wait, subsequent calls
    // wait briefly in case a 20 ms-paced frame arrives mid-drain,
    // and the loop exits as soon as the queue runs dry.
    let saved_timeout = session.gateway.event_timeout();
    session
        .gateway
        .set_event_timeout(std::time::Duration::from_millis(5));
    for _ in 0..MAX_EVENTS_PER_CYCLE {
        let event = match session.gateway.next_event().await {
            Ok(Some(event)) => event,
            // No MMDVM event within the timeout: queue is dry.
            Ok(None) => break,
            // `Err` is a fatal transport failure, never a timeout
            // (see `DstarGateway::next_event`). Announce it once
            // instead of silently treating a dead radio link like
            // an empty queue while the monitor keeps running.
            Err(e) => {
                if !session.radio_link_lost {
                    session.radio_link_lost = true;
                    aprintln!(
                        "{}",
                        thd75_repl::output::error(format_args!("radio link failed: {e}"))
                    );
                }
                break;
            }
        };
        trace_dstar_event(&event);
        print_dstar_event(&event);
        // Tap DVSI-origin voice bytes into the golden-vector capture
        // file BEFORE any further processing, so the bytes we record
        // are exactly what the chip emitted (untouched by our
        // slow-data rewrite or sequence-number bookkeeping in
        // `relay_radio_to_reflector`).
        if let (Some(file), DstarEvent::VoiceData(frame)) = (session.ambe_capture.as_mut(), &event)
        {
            use std::io::Write;
            if let Err(e) = file.write_all(&frame.ambe) {
                println!("Warning: AMBE capture write failed: {e}");
            }
        }
        relay_radio_to_reflector(session, &event).await;
    }
    session.gateway.set_event_timeout(saved_timeout);

    // Drive echo playback state machine.
    echo_playback_tick(session).await;
}

/// `listen` polls both the radio MMDVM and reflector UDP, relaying
/// voice frames between them.
#[expect(
    clippy::too_many_lines,
    reason = "D-STAR command dispatch covering listen, link/unlink, echo, text, heard, status, \
              and the dstar-stop transition. Each arm is a small handler; the sum is long but \
              the structure is a flat dispatch table. Splitting helpers would only fragment a \
              single command table across files."
)]
async fn dispatch_dstar(session: &mut DstarSession, cmd: &str, parts: &[&str]) {
    match cmd {
        "listen" | "poll" => {
            // One cycle of poll: radio + reflector, relay voice.
            dstar_poll_cycle(session).await;
        }
        "link" => {
            if parts.len() < 2 {
                println!("Usage: link <reflector>");
                println!("Example: link XRF030C");
                println!(
                    "Example: link B:XRF030C (present local module B when \
                     cross-linking to reflector module C)"
                );
                return;
            }
            if session.reflector.is_some() {
                println!("Already linked. Run unlink before connecting to another reflector.");
                return;
            }
            let Some(&link_arg) = parts.get(1) else {
                return;
            };
            let link = match parse_link_arg(link_arg) {
                Ok(l) => l,
                Err(e) => {
                    println!("Error: {e}");
                    return;
                }
            };
            if !thd75_repl::confirm::tx_confirm() {
                return;
            }
            match connect_reflector(session.callsign, &link).await {
                Ok(client) => {
                    session.local_module = link.local_module;
                    session.reflector_module = link.reflector_module;
                    // Update the stored reflector callsign too, since
                    // the relay path uses it to build the outbound rpt2
                    // field (see build_reflector_header).
                    if let Ok(cs) = Callsign::try_from_str(&link.reflector_name) {
                        session.reflector_callsign = cs;
                    }
                    session.reflector = Some(client);
                    println!("Monitoring. Press Ctrl-C to return to prompt.");
                    run_dstar_monitor(session).await;
                }
                Err(e) => println!("Error: {e}"),
            }
        }
        "unlink" => {
            if let Some(mut reflector) = session.reflector.take() {
                disconnect_reflector(&mut reflector).await;
            } else {
                println!("Not connected to a reflector.");
            }
        }
        "heard" => {
            let list = session.gateway.last_heard();
            if list.is_empty() {
                println!("No stations heard yet.");
            } else {
                for entry in list {
                    println!(
                        "Station {}: destination {}, heard {} ago.",
                        render_gateway_callsign(entry.callsign),
                        render_gateway_callsign(entry.destination),
                        commands::fmt_elapsed(entry.timestamp.elapsed())
                    );
                }
                println!("{}", thd75_repl::output::stations_summary(list.len()));
            }
        }
        "status" => {
            match session.gateway.poll_status().await {
                Ok(status) => println!(
                    "Modem status: D-STAR buffer {}, transmit {}.",
                    status.dstar_space,
                    if status.tx() { "active" } else { "idle" }
                ),
                Err(e) => println!("Error: {e}"),
            }
            if session.reflector.is_some() {
                // The new typestate API only exposes a session handle
                // once the sans-io core has reached `Connected`, so
                // any `Some(_)` here means we're operational. The
                // transient `Connecting` / `Disconnecting` states run
                // inside the per-protocol handshake helpers before
                // `AsyncSession::spawn` is called and don't surface
                // here.
                println!("Reflector connection: connected.");
            } else {
                println!("Reflector: not connected.");
            }
        }
        "echo" => {
            if !thd75_repl::confirm::tx_confirm() {
                return;
            }
            session.echo.arm();
            println!(
                "Echo test: transmit now. Your audio will be recorded \
                 and played back."
            );
            run_echo_monitor(session).await;
        }
        "text" | "msg" | "message" => {
            if parts.len() < 2 {
                if let Some(ref text) = session.tx_text {
                    println!("Current outgoing text: {text}");
                    println!("This text will be sent with your next transmission.");
                } else {
                    println!("No outgoing text set.");
                }
                println!("Usage: text <message up to 20 printable ASCII bytes>");
                println!("  Sets text to embed in your next voice transmission.");
                println!("  text clear: Remove the outgoing text.");
                return;
            }
            let Some(&first_arg) = parts.get(1) else {
                return;
            };
            if first_arg == "clear" || first_arg == "off" || first_arg == "none" {
                session.tx_text = None;
                session.tx_slow_data.clear();
                session.tx_slow_data_idx = 0;
                println!("Outgoing text cleared.");
            } else {
                let text = parts.get(1..).unwrap_or(&[]).join(" ");
                let message = match SlowDataTextMessage::try_from_text(&text) {
                    Ok(message) => message,
                    Err(error) => {
                        println!("Outgoing text rejected: {error}");
                        return;
                    }
                };
                session.tx_slow_data = encode_text_message(message).to_vec();
                session.tx_slow_data_idx = 0;
                println!(
                    "Outgoing text set: \"{text}\". \
                     Will be embedded in your next transmission."
                );
                session.tx_text = Some(text);
            }
        }
        _ => println!(
            "D-STAR command not recognized: {cmd}. \
             Commands: monitor, link, unlink, echo, text, heard, status, dstar stop"
        ),
    }
}

/// Render exact malformed wire bytes instead of substituting Unicode text.
fn render_invalid_wire_bytes(bytes: &[u8], error: dstar_gateway_core::WireTextError) -> String {
    let hexadecimal = bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!("<invalid {hexadecimal}: {error}>")
}

fn render_gateway_callsign(callsign: impl Into<kenwood_thd75::ObservedDstarCallsign>) -> String {
    let callsign = callsign.into();
    callsign.text().map_or_else(
        |error| render_invalid_wire_bytes(callsign.as_bytes(), error),
        str::to_owned,
    )
}

fn render_gateway_suffix(suffix: Suffix) -> String {
    suffix.text().map_or_else(
        |error| render_invalid_wire_bytes(suffix.as_bytes(), error),
        str::to_owned,
    )
}

fn render_slow_data_text(message: &SlowDataTextMessage) -> String {
    message.text().map_or_else(
        |error| render_invalid_wire_bytes(message.as_bytes(), error),
        str::to_owned,
    )
}

/// Finish the echo recording and announce its length.
fn finish_echo_recording(session: &mut DstarSession) {
    if let Some(count) = session.echo.on_voice_end(std::time::Instant::now()) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "The echo buffer is capped at 3000 frames, which fits exactly in \
                      f64's 52-bit mantissa, so the usize→f64 conversion never loses precision."
        )]
        let secs = count as f64 / 50.0;
        println!("Echo test: recorded {secs:.1} seconds of audio. Playing back.");
    }
}

/// Build the outbound reflector [`DstarHeader`] from the
/// kenwood-thd75 header emitted by the radio.
///
/// The TH-D75 in Reflector Terminal Mode emits its TX header with
/// `rpt1` / `rpt2` both set to the literal string `"DIRECT  "` as
/// placeholders: the radio knows it's talking to a local gateway
/// but doesn't know the gateway's callsign. This function is the
/// gateway half of that contract: we rewrite those placeholders into
/// the canonical relay header via [`DstarHeader::for_relay`], then
/// preserve the source flag bytes (the radio may set repeater flag
/// bits the reflector cares about).
fn build_reflector_header(
    station_callsign: Callsign,
    local_module: Module,
    reflector_callsign: Callsign,
    reflector_module: Module,
    header: &DstarHeader,
) -> DstarHeader {
    // Preserve the radio's flag bytes: `for_relay` zeroes them (correct
    // for a TX-from-scratch client), so chain the source flags back on.
    DstarHeader::for_relay(
        station_callsign,
        local_module,
        reflector_callsign,
        reflector_module,
        header.my_call,
        header.my_suffix,
    )
    .with_flags(header.flag1, header.flag2, header.flag3)
}

/// Relay a radio MMDVM event to the reflector, or record for echo test.
///
/// If the URCALL field is `"       E"` (7 spaces + E), the
/// transmission is captured for local echo playback instead of
/// being relayed to any reflector. This matches the `ircDDBGateway`
/// echo test convention.
async fn relay_radio_to_reflector(session: &mut DstarSession, event: &DstarEvent) {
    // Echo test interception (the `echo` command armed the unit, or
    // URCALL is "       E" per ircDDBGateway convention). A VoiceStart
    // during a live recording means the previous stream's end was
    // lost: finish what we have, then treat this stream normally.
    if let DstarEvent::VoiceStart(header) = event {
        if session.echo.is_recording() {
            finish_echo_recording(session);
        }
        if session.echo.on_voice_start(header) {
            println!(
                "Echo test: recording from {}. Transmit up to 60 seconds.",
                render_gateway_callsign(header.my_call)
            );
            return;
        }
    } else if session.echo.is_recording() {
        match event {
            DstarEvent::VoiceData(frame) => {
                session.echo.on_voice_frame(*frame);
                return;
            }
            DstarEvent::VoiceEnd | DstarEvent::VoiceLost => {
                finish_echo_recording(session);
                return;
            }
            _ => {}
        }
    }

    // Normal relay to reflector. Capture copies of the typed
    // station_callsign / local_module / reflector_callsign /
    // reflector_module up front so the session can still be borrowed
    // mutably further down for tx_stream_id / tx_seq updates without
    // aliasing against `session.reflector`.
    let station_callsign = session.callsign;
    let local_module = session.local_module;
    let reflector_callsign = session.reflector_callsign;
    let reflector_module = session.reflector_module;
    let Some(ref mut client) = session.reflector else {
        return;
    };

    match event {
        DstarEvent::VoiceStart(header) => {
            // Generate a new stream ID for this transmission.
            let sid = dstar_gateway::tokio_shell::fresh_stream_id();
            session.tx_stream_id = Some(sid);
            session.tx_seq = 0;
            session.tx_slow_data_idx = 0;

            let ref_header = build_reflector_header(
                station_callsign,
                local_module,
                reflector_callsign,
                reflector_module,
                header,
            );
            if let Err(e) = client.send_header(ref_header, sid).await {
                println!("Error: relay header to reflector: {e}");
            }
        }
        DstarEvent::VoiceData(frame) => {
            let Some(sid) = session.tx_stream_id else {
                return;
            };
            // If outgoing text is set, replace the slow data with
            // encoded text message bytes (cycling through the encoded
            // payloads). Frame 0 of each superframe carries sync and
            // is skipped (seq handled by tx_seq counter).
            let slow_data = if session.tx_slow_data.is_empty() {
                frame.slow_data
            } else {
                let sd = session
                    .tx_slow_data
                    .get(session.tx_slow_data_idx % session.tx_slow_data.len())
                    .copied()
                    .unwrap_or(frame.slow_data);
                session.tx_slow_data_idx += 1;
                sd
            };
            let ref_frame = VoiceFrame {
                ambe: frame.ambe,
                slow_data,
            };
            if let Err(e) = client.send_voice(sid, session.tx_seq, ref_frame).await {
                println!("Error: relay voice to reflector: {e}");
            }
            session.tx_seq = (session.tx_seq + 1) % 21;
        }
        DstarEvent::VoiceEnd => {
            let Some(sid) = session.tx_stream_id else {
                return;
            };
            if let Err(e) = client.send_eot(sid, session.tx_seq).await {
                println!("Error: relay EOT to reflector: {e}");
            }
            session.tx_stream_id = None;
            session.tx_seq = 0;
        }
        _ => {}
    }
}

/// Drive the echo playback state machine.
///
/// Called from the poll cycle. When in `Waiting` state and the delay
/// has elapsed, plays back all buffered frames to the radio, sleeping
/// 15 ms per frame; the BT write latency supplies the remainder of
/// the 20 ms AMBE frame interval, so the modem sees roughly its
/// native 50 fps consumption rate.
async fn echo_playback_tick(session: &mut DstarSession) {
    // The unit hands over the recorded stream exactly once, with the
    // playback header already built per ircDDBGateway EchoUnit.cpp
    // (MY = gateway callsign, suffix "ECHO", YOUR = "CQCQCQ  ").
    let Some(playback) = session
        .echo
        .poll_playback(std::time::Instant::now(), session.callsign)
    else {
        return;
    };

    // Send header to radio.
    if let Err(e) = session.gateway.send_header(&playback.header).await {
        println!("Echo playback error: header: {e}");
        return;
    }

    // Play back each frame with 20ms pacing.
    for frame in &playback.frames {
        if let Err(e) = session.gateway.send_voice_unpaced(frame).await {
            println!("Echo playback error: voice: {e}");
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    }

    // Send EOT.
    if let Err(e) = session.gateway.send_eot().await {
        println!("Echo playback error: EOT: {e}");
    }

    // Drain any stale MMDVM events so they don't leak into the next
    // echo cycle or reflector relay.
    for _ in 0..20 {
        if session.gateway.next_event().await.ok().flatten().is_none() {
            break;
        }
    }

    println!("Echo test: playback complete.");
}

/// Run the poll loop for an echo test cycle.
///
/// Polls the radio MMDVM for the user's TX, records it, plays it
/// back, then returns automatically. Also handles Ctrl-C to cancel.
async fn run_echo_monitor(session: &mut DstarSession) {
    session
        .gateway
        .set_event_timeout(std::time::Duration::from_millis(10));

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    // Track whether we've started recording (so we don't exit
    // immediately before the user has transmitted).
    let mut started = false;

    loop {
        tokio::select! {
            _ = &mut ctrl_c => {
                println!("Echo test cancelled.");
                session.echo = dstar_gateway_core::echo::EchoUnit::new();
                break;
            }
            () = dstar_poll_cycle(session) => {}
        }

        if !started && !session.echo.is_idle() {
            started = true;
        }

        // Exit once playback is complete (idle after having recorded).
        if started && session.echo.is_idle() {
            break;
        }
    }

    session
        .gateway
        .set_event_timeout(std::time::Duration::from_millis(500));
}

/// Build the header to send to the radio's MMDVM modem when relaying
/// a reflector voice frame.
///
/// Convention matches `MMDVMHost`'s `m_remoteGateway` network-to-RF
/// path in `MMDVMHost/DStarControl.cpp:749-754`, since `thd75-repl`
/// is a remote gateway in front of the modem:
/// - `flag1` |= `0x40` (`DSTAR_REPEATER_MASK`) per
///   `MMDVMHost/DStarDefines.h:62`.
/// - `rpt1` = local station callsign (7 bytes) + local module letter.
/// - `rpt2` = same as `rpt1` (callsign + module). `MMDVMHost` uses
///   `rpt2 = m_callsign` (NOT `m_gateway`) in remote-gateway mode.
/// - `ur_call` = `"CQCQCQ  "`, the standard "calling all stations"
///   destination for relayed voice.
/// - `my_call` / `my_suffix` = passed through from the original sender.
fn build_radio_header(
    station_callsign: Callsign,
    local_module: Module,
    header: &DstarHeader,
) -> DstarHeader {
    DstarHeader::for_radio_relay(station_callsign, local_module, header)
}

/// Relay a reflector event to the radio MMDVM modem.
async fn relay_reflector_to_radio(session: &mut DstarSession, event: &RuntimeEvent) {
    let gw = &mut session.gateway;
    match event {
        RuntimeEvent::VoiceStart {
            header, stream_id, ..
        } => {
            // Deduplicate: only send header once per stream.
            if session.rx_stream_id == Some(*stream_id) {
                return;
            }
            session.rx_stream_id = Some(*stream_id);
            // Reset per-stream bookkeeping on the first header of a
            // new stream. The frame counter + start timestamp drive
            // the `VoiceEnd` summary line that distinguishes dead
            // keys from real audio.
            session.rx_frame_count = 0;
            session.rx_stream_start = Some(std::time::Instant::now());
            // Reset slow data decoder for the new stream so a partial
            // message from a previous transmission is not silently
            // reassembled onto this one. Also clear the last-printed
            // text tracker so the new stream's first complete text
            // always prints, even if it happens to match the previous
            // operator's message verbatim.
            session.rx_slow_data.reset();
            session.rx_last_slow_text = None;
            // Reset silence-padding state. `last_rx_voice_frame`
            // stays `None` until the first voice frame of this
            // stream arrives, which keeps the pad timer a no-op
            // during the pre-first-frame window (no content to
            // repeat yet).
            session.last_rx_voice_frame = None;
            session.last_relay_at = None;
            session.pad_frames_emitted = 0;
            // Rewrite rpt1/rpt2/flag1 per `MMDVMHost`'s
            // `m_remoteGateway` convention before forwarding (see
            // `build_radio_header` doc).
            let radio_header = build_radio_header(session.callsign, session.local_module, header);
            // Diagnostic: log the exact header fields going to the
            // radio's MMDVM modem at TRACE so the session log confirms
            // the relay path is firing AND records what the radio's TX
            // header validator is being asked to accept, without
            // cluttering the operator's REPL output.
            tracing::trace!(
                target: "thd75_repl::reflector",
                my_bytes = ?radio_header.my_call.as_bytes(),
                my_text = ?radio_header.my_call.text(),
                suffix_bytes = ?radio_header.my_suffix.as_bytes(),
                suffix_text = ?radio_header.my_suffix.text(),
                ur_bytes = ?radio_header.ur_call.as_bytes(),
                ur_text = ?radio_header.ur_call.text(),
                rpt1_bytes = ?radio_header.rpt1.as_bytes(),
                rpt1_text = ?radio_header.rpt1.text(),
                rpt2_bytes = ?radio_header.rpt2.as_bytes(),
                rpt2_text = ?radio_header.rpt2.text(),
                flag1 = format_args!("{:#04x}", radio_header.flag1),
                "relay → radio: header"
            );
            if let Err(e) = gw.send_header(&radio_header).await {
                println!(
                    "{}",
                    thd75_repl::output::error(format_args!("relaying header to radio: {e}"))
                );
            }
        }
        RuntimeEvent::VoiceFrame { frame, seq, .. } => {
            tracing::trace!(
                target: "thd75_repl::hang_hunt",
                seq = *seq,
                "relay VoiceFrame: enter"
            );
            // Count voice frames per stream so the VoiceEnd line can
            // report whether the transmission actually carried audio
            // or was a dead-key carrier (saturating_add keeps the
            // counter safe for the unlikely 4-billion-frame stream).
            session.rx_frame_count = session.rx_frame_count.saturating_add(1);
            // Feed the raw seq byte from the DSVT header; the decoder
            // treats seq==0 as a sync frame and re-aligns its half-block
            // phase automatically. No external skipping needed.
            session.rx_slow_data.push(frame.slow_data, *seq);
            // D-STAR radios repeat one FIXED 20-char text message
            // across the voice stream continuously so late joiners
            // can see it; it cannot change mid-transmission. Announce
            // only the first complete assembly per stream: later
            // re-assemblies that differ are RF bit corruption (no
            // error correction on the slow-data channel) and printing
            // each variant spams the operator, a real problem for
            // screen-reader users.
            if let Some(bytes) = session.rx_slow_data.take_message()
                && session.rx_last_slow_text.is_none()
            {
                print_slow_data_text_message(&bytes);
                session.rx_last_slow_text = Some(bytes);
            }

            // Use send_voice_unpaced: no host-side pacing. The
            // correct pattern per `MMDVMHost/Modem.cpp:1049` is
            // to query the modem's `dstarSpace` status field and
            // only write when the modem reports buffer room,
            // letting the modem's own buffer state drive the rate.
            // We don't yet implement that status-polling loop, so
            // the second-best option is to let the BT kernel
            // buffer + 9600 baud UART backpressure naturally
            // rate-limit our writes.
            //
            // Host-side 20 ms pacing is wrong because DPlus
            // delivers 21 voice packets per ~440 ms superframe
            // (~47.7 fps, since there's an extra header packet slot
            // each superframe) while the modem's internal AMBE
            // decoder consumes at exactly 50 fps. A ~2 ms/frame
            // shortfall drains the modem's 10-slot buffer after
            // roughly 2 seconds of continuous audio, then every
            // subsequent write hits an empty buffer → constant
            // underrun-driven stutter. Writing as fast as BT
            // accepts (≈64 fps on 9600 baud) is much closer to
            // the modem's expected 50 fps consumption rate.
            //
            // The REPL's inline event processing in
            // [`dstar_poll_cycle`] means each reflector frame
            // flows immediately from UDP → decode → BT write.
            // There's no drain-then-process batching any more, so
            // the original reason to pace (avoid bursting a full
            // superframe followed by 400 ms idle) no longer
            // applies.
            //
            // The mmdvm crate already implements MMDVMHost-style
            // periodic status polling and gates its TxQueue drain on
            // `ModemStatus::dstar_space`, so this path is effectively
            // as close to the reference behavior as we can get.
            tracing::trace!(target: "thd75_repl::hang_hunt", "relay VoiceFrame: awaiting send_voice_unpaced");
            let relay_result = gw.send_voice_unpaced(frame).await;
            tracing::trace!(target: "thd75_repl::hang_hunt", "relay VoiceFrame: send_voice_unpaced returned");
            match relay_result {
                Ok(()) => {
                    // Arm the silence-padding timer: remember this
                    // frame as the repeat template, timestamp the
                    // relay, and zero the pad counter. The 20 ms
                    // pad tick in `run_dstar_monitor` will repeat
                    // this frame only if the next real frame
                    // doesn't arrive before `PAD_INITIAL_THRESHOLD` past
                    // this moment.
                    session.last_rx_voice_frame = Some(*frame);
                    session.last_relay_at = Some(std::time::Instant::now());
                    session.pad_frames_emitted = 0;
                }
                Err(e) => {
                    println!(
                        "{}",
                        thd75_repl::output::error(format_args!("relaying voice to radio: {e}"))
                    );
                }
            }
        }
        RuntimeEvent::VoiceEnd { .. } => {
            // Drain a message that became complete on the very last
            // voice frame; any partial message mid-assembly is silently
            // discarded by the reset below. The same first-assembly
            // latch applies: if a message already printed during this
            // stream, anything different here is RF corruption.
            if let Some(bytes) = session.rx_slow_data.take_message()
                && session.rx_last_slow_text.is_none()
            {
                print_slow_data_text_message(&bytes);
            }
            session.rx_slow_data.reset();
            session.rx_last_slow_text = None;
            session.rx_stream_id = None;
            // Reset the per-stream voice counters last so the
            // immediately-preceding `print_reflector_event(VoiceEnd,
            // session)` call in `dstar_poll_cycle` still sees them.
            session.rx_frame_count = 0;
            session.rx_stream_start = None;
            // Tear down silence padding: the stream ended cleanly,
            // so any further pad ticks should no-op. Without this
            // reset, the first ~2 s after EOT would keep padding
            // the modem with the final voice frame of the prior
            // stream, since the pad timer only checks `rx_stream_id`.
            session.last_rx_voice_frame = None;
            session.last_relay_at = None;
            session.pad_frames_emitted = 0;
            // Diagnostic: pairs with the `relay → radio: header` trace
            // emitted on `VoiceStart` so the session log confirms the
            // EOT closes the stream, and that the relay is calling
            // `send_eot` (not silently failing earlier in the path).
            tracing::trace!(target: "thd75_repl::reflector", "relay → radio: EOT");
            if let Err(e) = gw.send_eot().await {
                println!(
                    "{}",
                    thd75_repl::output::error(format_args!(
                        "relaying end of transmission to radio: {e}"
                    ))
                );
            }
        }
        // Connected / Disconnected / PollEcho carry no voice payload
        // to relay; future erased variants are equally inert here.
        _ => {}
    }
}

/// Announce a complete D-STAR slow data text message from a reflector stream.
///
/// `message` retains the fixed 20-byte value returned by
/// [`SlowDataTextCollector::take_message`]. Printable ASCII is announced
/// without trailing protocol padding. Invalid data is announced as exact hex
/// bytes instead of Unicode replacement characters.
fn print_slow_data_text_message(message: &SlowDataTextMessage) {
    tracing::trace!(
        target: "thd75_repl::reflector",
        bytes = ?message.as_bytes(),
        text = ?message.text(),
        "slow-data text message assembled"
    );

    let rendered = render_slow_data_text(message);
    if !rendered.is_empty() {
        aprintln!("{}", thd75_repl::output::dstar_text_message(&rendered));
    }
}

/// Emit a TRACE-level log entry for every reflector event consumed
/// from the poll loop. Logs the variant discriminant plus any
/// stream ID, source callsign, or sequence number that accompanied
/// the event. Keyed off `target = "thd75_repl::reflector"` so users
/// can filter just this firehose via `RUST_LOG` if they want.
fn trace_reflector_event(event: &RuntimeEvent) {
    match event {
        RuntimeEvent::Connected { .. } => {
            tracing::trace!(target: "thd75_repl::reflector", "event: Connected");
        }
        RuntimeEvent::Disconnected { reason } => {
            tracing::trace!(
                target: "thd75_repl::reflector",
                reason = ?reason,
                "event: Disconnected"
            );
        }
        RuntimeEvent::PollEcho { .. } => {
            tracing::trace!(target: "thd75_repl::reflector", "event: PollEcho");
        }
        RuntimeEvent::VoiceStart {
            header, stream_id, ..
        } => {
            tracing::trace!(
                target: "thd75_repl::reflector",
                stream_id = %stream_id,
                my_call_bytes = ?header.my_call.as_bytes(),
                my_call_text = ?header.my_call.text(),
                my_suffix_bytes = ?header.my_suffix.as_bytes(),
                my_suffix_text = ?header.my_suffix.text(),
                ur_call_bytes = ?header.ur_call.as_bytes(),
                ur_call_text = ?header.ur_call.text(),
                rpt1_bytes = ?header.rpt1.as_bytes(),
                rpt1_text = ?header.rpt1.text(),
                rpt2_bytes = ?header.rpt2.as_bytes(),
                rpt2_text = ?header.rpt2.text(),
                flag1 = format_args!("{:#04x}", header.flag1),
                flag2 = format_args!("{:#04x}", header.flag2),
                flag3 = format_args!("{:#04x}", header.flag3),
                "event: VoiceStart"
            );
        }
        RuntimeEvent::VoiceFrame {
            stream_id,
            seq,
            frame,
        } => {
            // Slow-data bytes are logged raw (pre-descramble) so a
            // packet capture can be cross-checked against the wire.
            // Reverse-engineering a non-standard slow-data scheme
            // (e.g. AMBEserver/Pi-Star variants) starts here.
            tracing::trace!(
                target: "thd75_repl::reflector",
                stream_id = %stream_id,
                seq = *seq,
                slow_data = format_args!(
                    "{:02X} {:02X} {:02X}",
                    frame.slow_data[0], frame.slow_data[1], frame.slow_data[2]
                ),
                "event: VoiceFrame"
            );
        }
        RuntimeEvent::VoiceEnd { stream_id, reason } => {
            tracing::trace!(
                target: "thd75_repl::reflector",
                stream_id = %stream_id,
                reason = ?reason,
                "event: VoiceEnd"
            );
        }
        other => {
            tracing::trace!(target: "thd75_repl::reflector", event = ?other, "event: other");
        }
    }
}

/// Print a reflector event for the user.
///
/// `VoiceEnd` reads the per-stream frame counter and start timestamp
/// off [`DstarSession`] to include them in the closing line. Must be
/// called before [`relay_reflector_to_radio`] for `VoiceEnd` events,
/// because the relay resets those counters so the accessible "ended" line
/// would otherwise see zeros.
fn print_reflector_event(event: &RuntimeEvent, session: &DstarSession) {
    match event {
        RuntimeEvent::Connected { .. } => {
            aprintln!("{}", thd75_repl::output::reflector_event_connected());
        }
        RuntimeEvent::Disconnected { reason } => match reason {
            DisconnectReason::Rejected => {
                aprintln!("{}", thd75_repl::output::reflector_event_rejected());
            }
            _ => {
                aprintln!("{}", thd75_repl::output::reflector_event_disconnected());
            }
        },
        RuntimeEvent::VoiceStart { header, .. } => {
            let my_call = render_gateway_callsign(header.my_call);
            let my_suffix = render_gateway_suffix(header.my_suffix);
            let ur_call = render_gateway_callsign(header.ur_call);
            aprintln!(
                "{}",
                thd75_repl::output::reflector_event_voice_start(&my_call, &my_suffix, &ur_call,)
            );
        }
        RuntimeEvent::VoiceEnd { stream_id, reason } => {
            let frames = session.rx_frame_count;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "voice transmissions are seconds to minutes; u128 → u64 ms is safe for realistic streams"
            )]
            let duration_ms = session
                .rx_stream_start
                .map_or(0_u64, |t| t.elapsed().as_millis() as u64);
            tracing::trace!(
                target: "thd75_repl::reflector",
                stream_id = %stream_id,
                reason = ?reason,
                frames = frames,
                duration_ms = duration_ms,
                "stream summary"
            );
            aprintln!(
                "{}",
                thd75_repl::output::reflector_event_voice_end(frames, duration_ms)
            );
        }
        // Silent: keepalives and individual voice frames are too
        // frequent to announce, and future erased variants have
        // nothing to announce either.
        _ => {}
    }
}

/// Emit a TRACE-level log entry for every D-STAR (radio MMDVM)
/// event consumed from the poll loop. Mirror of
/// [`trace_reflector_event`] for the radio-side event stream so the
/// operator's own callsign, stream header fields, and text/GPS
/// payloads appear in the persistent trace log (not just in the
/// console print via [`print_dstar_event`]). Keyed off
/// `target = "thd75_repl::dstar"` so users can filter just this
/// firehose via `RUST_LOG` if they want.
fn trace_dstar_event(event: &DstarEvent) {
    match event {
        DstarEvent::VoiceStart(header) => {
            tracing::trace!(
                target: "thd75_repl::dstar",
                my_call = %header.my_call,
                my_suffix = %header.my_suffix,
                ur_call = %header.ur_call,
                rpt1 = %header.rpt1,
                rpt2 = %header.rpt2,
                flag1 = format_args!("{:#04x}", header.flag1),
                flag2 = format_args!("{:#04x}", header.flag2),
                flag3 = format_args!("{:#04x}", header.flag3),
                "event: VoiceStart"
            );
        }
        DstarEvent::VoiceData(_) => {
            // Per-frame trace is too noisy at 20 fps; skip.
        }
        DstarEvent::VoiceEnd => {
            tracing::trace!(target: "thd75_repl::dstar", "event: VoiceEnd");
        }
        DstarEvent::VoiceLost => {
            tracing::trace!(target: "thd75_repl::dstar", "event: VoiceLost");
        }
        DstarEvent::EventsDropped { count } => {
            tracing::warn!(
                target: "thd75_repl::dstar",
                count,
                "event: EventsDropped"
            );
        }
        DstarEvent::ProtocolViolation(violation) => {
            tracing::warn!(
                target: "thd75_repl::dstar",
                violation = %violation,
                "event: ProtocolViolation"
            );
        }
        DstarEvent::TextMessage(text) => {
            tracing::trace!(
                target: "thd75_repl::dstar",
                bytes = ?text.as_bytes(),
                text = ?text.text(),
                "event: TextMessage"
            );
        }
        DstarEvent::StationHeard(entry) => {
            tracing::trace!(
                target: "thd75_repl::dstar",
                callsign_bytes = ?entry.callsign.as_bytes(),
                callsign_text = ?entry.callsign.text(),
                "event: StationHeard"
            );
        }
        DstarEvent::UrCallCommand(action) => {
            tracing::trace!(
                target: "thd75_repl::dstar",
                action = ?action,
                "event: UrCallCommand"
            );
        }
        DstarEvent::StatusUpdate(status) => {
            tracing::trace!(
                target: "thd75_repl::dstar",
                status = ?status,
                "event: StatusUpdate"
            );
        }
        DstarEvent::ModemEvent(event) => {
            tracing::debug!(
                target: "thd75_repl::dstar",
                event = ?event,
                "event: untranslated MMDVM event"
            );
        }
    }
}

/// Format and print a D-STAR event as screen-reader-friendly text.
fn print_dstar_event(event: &DstarEvent) {
    match event {
        DstarEvent::VoiceStart(header) => {
            let my_call = render_gateway_callsign(header.my_call);
            let my_suffix = render_gateway_suffix(header.my_suffix);
            let ur_call = render_gateway_callsign(header.ur_call);
            aprintln!(
                "{}",
                thd75_repl::output::dstar_voice_start(&my_call, &my_suffix, &ur_call,)
            );
        }
        DstarEvent::VoiceData(_) => {
            // Don't announce every 20ms frame: too noisy for screen readers.
        }
        DstarEvent::VoiceEnd => {
            aprintln!("{}", thd75_repl::output::dstar_voice_end());
        }
        DstarEvent::VoiceLost => {
            if !thd75_repl::is_verbose() {
                return;
            }
            aprintln!("{}", thd75_repl::output::dstar_voice_lost());
        }
        DstarEvent::EventsDropped { count } => {
            aprintln!("D-STAR modem event stream lost {count} event(s)");
        }
        DstarEvent::ProtocolViolation(violation) => {
            if thd75_repl::is_verbose() {
                aprintln!("D-STAR protocol violation: {violation}");
            }
        }
        DstarEvent::TextMessage(text) => {
            let text = render_slow_data_text(text);
            aprintln!("{}", thd75_repl::output::dstar_text_message(&text));
        }
        DstarEvent::StationHeard(entry) => {
            let callsign = render_gateway_callsign(entry.callsign);
            aprintln!("{}", thd75_repl::output::dstar_station_heard(&callsign));
        }
        DstarEvent::UrCallCommand(action) => {
            use kenwood_thd75::types::UrCallAction;
            let s = match action {
                UrCallAction::Cq => thd75_repl::output::dstar_command_cq().to_string(),
                UrCallAction::Echo => thd75_repl::output::dstar_command_echo().to_string(),
                UrCallAction::Unlink => thd75_repl::output::dstar_command_unlink().to_string(),
                UrCallAction::Info => thd75_repl::output::dstar_command_info().to_string(),
                UrCallAction::Link { reflector, module } => {
                    thd75_repl::output::dstar_command_link(reflector, *module)
                }
                UrCallAction::Callsign(call) => thd75_repl::output::dstar_command_callsign(call),
            };
            aprintln!("{s}");
        }
        DstarEvent::StatusUpdate(status) => {
            // Modem buffer / TX state is an audio-pipeline diagnostic, not a
            // user-facing monitor event, so route it to the trace log only.
            tracing::debug!(
                "{}",
                thd75_repl::output::dstar_modem_status(status.dstar_space, status.tx())
            );
        }
        DstarEvent::ModemEvent(event) => {
            if thd75_repl::is_verbose() {
                aprintln!("D-STAR modem event: {event:?}");
            }
        }
    }
}

#[cfg(test)]
mod offset_tests {
    use super::parse_utc_offset;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn parses_plus_hhmm_colon() -> TestResult {
        assert_eq!(parse_utc_offset("+05:30")?, 5 * 3600 + 30 * 60);
        Ok(())
    }

    #[test]
    fn parses_minus_hhmm_colon() -> TestResult {
        assert_eq!(parse_utc_offset("-08:00")?, -8 * 3600);
        Ok(())
    }

    #[test]
    fn parses_plus_hhmm_no_colon() -> TestResult {
        assert_eq!(parse_utc_offset("+0530")?, 5 * 3600 + 30 * 60);
        Ok(())
    }

    #[test]
    fn parses_plus_h() -> TestResult {
        assert_eq!(parse_utc_offset("+5")?, 5 * 3600);
        Ok(())
    }

    #[test]
    fn parses_no_sign_positive() -> TestResult {
        assert_eq!(parse_utc_offset("03:00")?, 3 * 3600);
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
        assert_eq!(parse_utc_offset("+00:00")?, 0);
        assert_eq!(parse_utc_offset("-00:00")?, 0);
        Ok(())
    }

    #[test]
    fn rejects_non_ascii_without_panicking() {
        // A multi-byte character where the HHMM split lands mid-char
        // used to panic on the byte slice; it must be a plain error.
        assert!(parse_utc_offset("\u{e9}5").is_err());
        assert!(parse_utc_offset("+\u{e9}\u{e9}").is_err());
    }
}

#[cfg(test)]
mod gateway_off_tests {
    use super::{
        DetachedMcpPageUpdate, DstarEntryRadio, InterruptibleMcpOperation,
        PROCESS_SIGNAL_EXIT_CODE, ProcessSignal, ProcessSignalRouter,
        clear_gateway_mode_with_interrupt, ensure_terminal_mode, enter_dstar, route_process_signal,
        run_set_gateway_off, transport, validate_gateway_mcp_target,
        write_terminal_mode_with_interrupt,
    };
    use kenwood_thd75::types::PcOutputInterface;

    /// Wire pins for Menu 985 / Menu 650 scripts below. Production offsets
    /// are registry-pinned inside `kenwood-thd75::radio::terminal_mode`.
    const GATEWAY_INTERFACE_PAGE: u16 = 0x10;
    /// Byte index of the Menu 985 DV Gateway interface value.
    const GATEWAY_INTERFACE_BYTE: usize = 0x93;
    /// Page containing Menu 650.
    const GATEWAY_MODE_PAGE: u16 = 0x1C;
    /// Byte index of the gateway mode value within its page.
    const GATEWAY_MODE_BYTE: usize = 0xA0;
    use kenwood_thd75::memory::MCP_D75_SCHEMA_FIRMWARE_IDENTITIES;
    use kenwood_thd75::protocol::programming;
    use kenwood_thd75::transport::{EitherTransport, MockTransport};
    use kenwood_thd75::{FirmwareIdentity, Radio, RadioModel};
    use std::sync::atomic::Ordering;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    static SIGNAL_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn page_response(page: u16, data: &[u8; programming::PAGE_SIZE]) -> Vec<u8> {
        let [page_hi, page_lo] = page.to_be_bytes();
        let mut response = Vec::with_capacity(programming::W_RESPONSE_SIZE);
        response.extend_from_slice(&[b'W', page_hi, page_lo, 0, 0]);
        response.extend_from_slice(data);
        response
    }

    /// Queue one sparse update that changes both the selected interface and
    /// the persistent gateway mode, including read-back verification.
    fn expect_changed_terminal_mode_update(
        mock: &mut MockTransport,
        target: PcOutputInterface,
    ) -> TestResult {
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);

        let mut interface_original = [0x5A; programming::PAGE_SIZE];
        interface_original[GATEWAY_INTERFACE_BYTE] = match target {
            PcOutputInterface::Usb => u8::from(PcOutputInterface::Bluetooth),
            PcOutputInterface::Bluetooth => u8::from(PcOutputInterface::Usb),
        };
        let mut interface_modified = interface_original;
        interface_modified[GATEWAY_INTERFACE_BYTE] = u8::from(target);
        let mut mode_original = [0x5A; programming::PAGE_SIZE];
        mode_original[GATEWAY_MODE_BYTE] = 0;
        let mut mode_modified = mode_original;
        mode_modified[GATEWAY_MODE_BYTE] = 1;

        let interface_read =
            programming::build_read_command(programming::McpPage::new(GATEWAY_INTERFACE_PAGE)?);
        mock.expect(
            &interface_read,
            &page_response(GATEWAY_INTERFACE_PAGE, &interface_original),
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);
        let mode_read =
            programming::build_read_command(programming::McpPage::new(GATEWAY_MODE_PAGE)?);
        mock.expect(
            &mode_read,
            &page_response(GATEWAY_MODE_PAGE, &mode_original),
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let interface_write = programming::build_write_command(
            programming::WritableMcpPage::new(GATEWAY_INTERFACE_PAGE)?,
            &interface_modified,
        );
        mock.expect(&interface_write, &[programming::ACK]);
        mock.expect(
            &interface_read,
            &page_response(GATEWAY_INTERFACE_PAGE, &interface_modified),
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);
        let mode_write = programming::build_write_command(
            programming::WritableMcpPage::new(GATEWAY_MODE_PAGE)?,
            &mode_modified,
        );
        mock.expect(&mode_write, &[programming::ACK]);
        mock.expect(
            &mode_read,
            &page_response(GATEWAY_MODE_PAGE, &mode_modified),
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);
        mock.expect(&[programming::EXIT], &[programming::ACK]);
        Ok(())
    }

    /// Queue reads proving that Menu 985 and Menu 650 already match.
    fn expect_unchanged_terminal_mode_update(
        mock: &mut MockTransport,
        target: PcOutputInterface,
    ) -> TestResult {
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);
        let mut interface = [0x5A; programming::PAGE_SIZE];
        interface[GATEWAY_INTERFACE_BYTE] = u8::from(target);
        let mut mode = [0x5A; programming::PAGE_SIZE];
        mode[GATEWAY_MODE_BYTE] = 1;
        let interface_read =
            programming::build_read_command(programming::McpPage::new(GATEWAY_INTERFACE_PAGE)?);
        mock.expect(
            &interface_read,
            &page_response(GATEWAY_INTERFACE_PAGE, &interface),
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);
        let mode_read =
            programming::build_read_command(programming::McpPage::new(GATEWAY_MODE_PAGE)?);
        mock.expect(&mode_read, &page_response(GATEWAY_MODE_PAGE, &mode));
        mock.expect(&[programming::ACK], &[programming::ACK]);
        mock.expect(&[programming::EXIT], &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");
        Ok(())
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
        assert!(finish_error.contains("interrupt signal"));
        assert_eq!(exit_code, 130);
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
                error.contains("1.03") && error.contains("1.03.000") && error.contains(firmware),
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
    async fn terminal_mode_enable_refuses_wrong_model_before_mcp_entry() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\x0D", b"ID TH-D74\x0D");

        let radio = Radio::new(EitherTransport::Mock(mock));
        let signals = ProcessSignalRouter::disconnected_for_test();
        let error = match ensure_terminal_mode(radio, &signals, PcOutputInterface::Bluetooth).await
        {
            Ok(_) => return Err("wrong-model terminal-mode write unexpectedly succeeded".into()),
            Err((_, error)) => error,
        };
        assert!(
            error.contains("expected exact TH-D75") && error.contains("TH-D74"),
            "wrong-model terminal-mode refusal lost its safety context: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn terminal_mode_enable_refuses_wrong_firmware_before_mcp_entry() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");
        mock.expect(b"FV\x0D", b"FV 1.04\x0D");

        let radio = Radio::new(EitherTransport::Mock(mock));
        let signals = ProcessSignalRouter::disconnected_for_test();
        let error = match ensure_terminal_mode(radio, &signals, PcOutputInterface::Bluetooth).await
        {
            Ok(_) => {
                return Err("wrong-firmware terminal-mode write unexpectedly succeeded".into());
            }
            Err((_, error)) => error,
        };
        assert!(
            error.contains("vendor firmware 1.03")
                && error.contains("1.03.000")
                && error.contains("1.04"),
            "wrong-firmware terminal-mode refusal lost its safety context: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn terminal_validation_preserves_binary_owner_without_cat_io() -> TestResult {
        let mut mock = MockTransport::new();
        // Validation must reject the malformed reflector before touching the
        // already-proved binary link. The queued ID exchange proves no prior
        // transport operation was attempted.
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");

        let radio = Radio::new(EitherTransport::Mock(mock));
        let signals = ProcessSignalRouter::disconnected_for_test();
        let Err((Some(DstarEntryRadio::Mmdvm(mut radio)), error)) = enter_dstar(
            DstarEntryRadio::Mmdvm(radio),
            &["KQ4NIT", "REF030"],
            &signals,
            transport::EndpointInterface::Known(PcOutputInterface::Bluetooth),
        )
        .await
        else {
            return Err("terminal validation did not preserve its binary owner".into());
        };
        assert!(error.contains("Invalid reflector"));
        assert_eq!(radio.identify().await?.model, RadioModel::ThD75);
        Ok(())
    }

    #[tokio::test]
    async fn invalid_reflector_is_rejected_before_terminal_mode_io() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");

        let radio = Radio::new(EitherTransport::Mock(mock));
        let signals = ProcessSignalRouter::disconnected_for_test();
        let Err((Some(DstarEntryRadio::Cat(mut radio)), error)) = enter_dstar(
            DstarEntryRadio::Cat(radio),
            &["KQ4NIT", "REF030"],
            &signals,
            transport::EndpointInterface::Known(PcOutputInterface::Bluetooth),
        )
        .await
        else {
            return Err("invalid reflector did not preserve the untouched CAT radio".into());
        };
        assert!(
            error.contains("Invalid reflector"),
            "reflector refusal lost its reason: {error}"
        );
        assert_eq!(radio.identify().await?.model, RadioModel::ThD75);
        Ok(())
    }

    #[tokio::test]
    async fn terminal_mode_enable_reopens_until_mmdvm_answers() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");
        mock.expect(b"FV\x0D", b"FV 1.03.AZM\x0D");
        expect_changed_terminal_mode_update(&mut mock, PcOutputInterface::Bluetooth)?;

        // The first binary probe lands before terminal mode is ready. The
        // helper must reopen this same transport, issue no CAT command, and
        // accept only the later complete GET_VERSION response.
        mock.expect(b"\xE0\x03\x00", b"");
        mock.expect_reopen(Ok(()));
        mock.expect(b"\xE0\x03\x00", b"\xE0\x0E\x00\x01MMDVM 2018");

        let radio = Radio::new(EitherTransport::Mock(mock));
        let signals = ProcessSignalRouter::disconnected_for_test();
        let radio = match ensure_terminal_mode(radio, &signals, PcOutputInterface::Bluetooth).await
        {
            Ok(radio) => radio,
            Err((_, error)) => return Err(error.into()),
        };
        tokio::task::LocalSet::new()
            .run_until(async move {
                let _session = radio.into_mmdvm_session().map_err(|(_, error)| error)?;
                Ok::<(), kenwood_thd75::Error>(())
            })
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn ambiguous_explicit_port_refuses_terminal_write_before_io() -> TestResult {
        let mut mock = MockTransport::new();
        // This identity exchange is intentionally queued for after the
        // refusal. If entry touched the wire first, strict ordering fails.
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");

        let radio = Radio::new(EitherTransport::Mock(mock));
        let signals = ProcessSignalRouter::disconnected_for_test();
        let Err((Some(DstarEntryRadio::Cat(mut radio)), error)) = enter_dstar(
            DstarEntryRadio::Cat(radio),
            &["KQ4NIT"],
            &signals,
            transport::EndpointInterface::UnspecifiedExplicitPort,
        )
        .await
        else {
            return Err("ambiguous explicit port did not preserve the untouched CAT radio".into());
        };
        assert!(
            error.contains("--port-interface usb") && error.contains("--port-interface bluetooth"),
            "ambiguous-port refusal lost its exact recovery options: {error}"
        );
        assert_eq!(radio.identify().await?.model, RadioModel::ThD75);
        Ok(())
    }

    #[tokio::test]
    async fn gateway_init_failure_keeps_the_recovered_radio_terminal_guarded() -> TestResult {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut mock = MockTransport::new();
                mock.expect(b"ID\x0D", b"ID TH-D75\x0D");
                mock.expect(b"FV\x0D", b"FV 1.03.AZM\x0D");
                expect_changed_terminal_mode_update(&mut mock, PcOutputInterface::Bluetooth)?;

                // The bounded diagnosis consumes exactly the version frame;
                // the following SetConfig NAK remains for gateway init.
                mock.expect_reads(
                    b"\xE0\x03\x00",
                    &[b"\xE0\x0E\x00\x01MMDVM 2018", b"\xE0\x05\x7F\x02\x04"],
                );
                mock.expect_any_write();
                mock.pend_when_empty();

                let radio = Radio::new(EitherTransport::Mock(mock));
                let signals = ProcessSignalRouter::disconnected_for_test();
                let Err((Some(DstarEntryRadio::Mmdvm(_radio)), error)) = enter_dstar(
                    DstarEntryRadio::Cat(radio),
                    &["KQ4NIT"],
                    &signals,
                    transport::EndpointInterface::Known(PcOutputInterface::Bluetooth),
                )
                .await
                else {
                    return Err::<(), Box<dyn std::error::Error>>(
                        "gateway init failure lost the retryable radio".into(),
                    );
                };

                assert!(
                    error.contains("Gateway init failed") && error.contains("MMDVM ACK for 0x02"),
                    "gateway init failure lost its modem cause: {error}"
                );
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn already_enabled_terminal_mode_still_waits_for_mmdvm() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");
        mock.expect(b"FV\x0D", b"FV 1.03.AZM\x0D");
        expect_unchanged_terminal_mode_update(&mut mock, PcOutputInterface::Bluetooth)?;

        // An unchanged MCP update still takes the normal exit path, whose radio
        // reset and early CAT proof can precede the terminal application by
        // tens of seconds. There are deliberately no page writes here, but
        // the high-level transition must continue until binary mode is proved.
        mock.expect(b"\xE0\x03\x00", b"\xE0\x0E\x00\x01MMDVM 2018");

        let radio = Radio::new(EitherTransport::Mock(mock));
        let signals = ProcessSignalRouter::disconnected_for_test();
        let radio = match ensure_terminal_mode(radio, &signals, PcOutputInterface::Bluetooth).await
        {
            Ok(radio) => radio,
            Err((_, error)) => return Err(error.into()),
        };
        tokio::task::LocalSet::new()
            .run_until(async move {
                let _session = radio.into_mmdvm_session().map_err(|(_, error)| error)?;
                Ok::<(), kenwood_thd75::Error>(())
            })
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn terminal_transition_signal_disconnects_and_preserves_exit_status() -> TestResult {
        let _test_lock = SIGNAL_TEST_LOCK.lock().await;
        PROCESS_SIGNAL_EXIT_CODE.store(0, Ordering::Release);
        let mut mock = MockTransport::new();
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");
        mock.expect(b"FV\x0D", b"FV 1.03.AZM\x0D");
        expect_changed_terminal_mode_update(&mut mock, PcOutputInterface::Bluetooth)?;
        mock.expect_hang(b"\xE0\x03\x00");

        let radio = Radio::new(EitherTransport::Mock(mock));
        let signals = ProcessSignalRouter::disconnected_for_test();
        let critical_slot = signals.critical_slot.clone();
        let interrupter = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            route_process_signal(ProcessSignal::Terminate, &critical_slot);
        });
        let result = ensure_terminal_mode(radio, &signals, PcOutputInterface::Bluetooth).await;
        interrupter.await?;
        let exit_code = PROCESS_SIGNAL_EXIT_CODE.swap(0, Ordering::AcqRel);

        let Err((None, error)) = result else {
            return Err("transition termination did not fail closed".into());
        };
        assert!(
            error.contains("termination signal received")
                && error.contains("connection was closed"),
            "transition termination lost signal or cleanup context: {error}"
        );
        assert_eq!(exit_code, 143);
        Ok(())
    }

    #[tokio::test]
    async fn already_off_disconnects_without_memory_write() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");
        mock.expect(b"FV\x0D", b"FV 1.03\x0D");
        mock.expect(b"GW\x0D", b"GW 0\x0D");

        run_set_gateway_off(EitherTransport::Mock(mock)).await?;
        Ok(())
    }

    #[tokio::test]
    async fn active_gateway_is_cleared_with_verified_memory_write() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");
        mock.expect(b"FV\x0D", b"FV 1.03.000\x0D");
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

        run_set_gateway_off(EitherTransport::Mock(mock)).await?;
        Ok(())
    }

    #[tokio::test]
    async fn azimuth_already_off_uses_mcp_read_without_gw_or_page_write() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");
        mock.expect(b"FV\x0D", b"FV 1.03.AZM\x0D");
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);

        let mut original = [0x5A; programming::PAGE_SIZE];
        original[GATEWAY_MODE_BYTE] = 0;
        let read_command =
            programming::build_read_command(programming::McpPage::new(GATEWAY_MODE_PAGE)?);
        mock.expect(&read_command, &page_response(GATEWAY_MODE_PAGE, &original));
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // An unchanged page takes the normal MCP exit/reconnect path. There
        // is deliberately no page write and no GW exchange in this script.
        mock.expect(&[programming::EXIT], &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");

        run_set_gateway_off(EitherTransport::Mock(mock)).await?;
        Ok(())
    }

    #[tokio::test]
    async fn azimuth_active_gateway_is_cleared_without_gw_query() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\x0D", b"ID TH-D75\x0D");
        mock.expect(b"FV\x0D", b"FV 1.03.AZM\x0D");
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);

        let mut original = [0x5A; programming::PAGE_SIZE];
        original[GATEWAY_MODE_BYTE] = 1;
        let read_command =
            programming::build_read_command(programming::McpPage::new(GATEWAY_MODE_PAGE)?);
        mock.expect(&read_command, &page_response(GATEWAY_MODE_PAGE, &original));
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let mut modified = original;
        modified[GATEWAY_MODE_BYTE] = 0;
        let write_command = programming::build_write_command(
            programming::WritableMcpPage::new(GATEWAY_MODE_PAGE)?,
            &modified,
        );
        mock.expect(&write_command, &[programming::ACK]);
        mock.expect(&read_command, &page_response(GATEWAY_MODE_PAGE, &modified));
        mock.expect(&[programming::ACK], &[programming::ACK]);
        mock.expect(&[programming::EXIT], &[programming::ACK]);

        run_set_gateway_off(EitherTransport::Mock(mock)).await?;
        Ok(())
    }

    #[tokio::test]
    async fn terminal_mode_enable_writes_and_verifies_menu_985_and_menu_650() -> TestResult {
        let mut mock = MockTransport::new();
        expect_changed_terminal_mode_update(&mut mock, PcOutputInterface::Bluetooth)?;

        let mut radio = Radio::new(EitherTransport::Mock(mock));
        let interrupt = std::future::pending::<Result<ProcessSignal, String>>();
        let InterruptibleMcpOperation::Completed(Ok(changed)) =
            write_terminal_mode_with_interrupt(&mut radio, PcOutputInterface::Bluetooth, interrupt)
                .await
        else {
            return Err(
                "uninterrupted terminal update did not return its successful result".into(),
            );
        };
        assert_eq!(changed, DetachedMcpPageUpdate::ChangedRadioRebooting);
        Ok(())
    }

    #[tokio::test]
    async fn terminal_mode_enable_avoids_rewriting_matching_route_and_mode() -> TestResult {
        let mut mock = MockTransport::new();
        expect_unchanged_terminal_mode_update(&mut mock, PcOutputInterface::Bluetooth)?;

        let mut radio = Radio::new(EitherTransport::Mock(mock));
        let interrupt = std::future::pending::<Result<ProcessSignal, String>>();
        let InterruptibleMcpOperation::Completed(Ok(changed)) =
            write_terminal_mode_with_interrupt(&mut radio, PcOutputInterface::Bluetooth, interrupt)
                .await
        else {
            return Err(
                "uninterrupted unchanged update did not return its successful result".into(),
            );
        };
        assert_eq!(changed, DetachedMcpPageUpdate::UnchangedCatReady);
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
        mock.expect(&read_command, &page_response(GATEWAY_MODE_PAGE, &original));
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
    async fn interrupted_terminal_read_with_unproved_boundary_requires_recovery() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);

        let read_command =
            programming::build_read_command(programming::McpPage::new(GATEWAY_INTERFACE_PAGE)?);
        mock.expect_hang(&read_command);

        let mut radio = Radio::new(EitherTransport::Mock(mock));
        let interrupt = async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            Ok(ProcessSignal::Interrupt)
        };
        let InterruptibleMcpOperation::Interrupted {
            signal: Ok(ProcessSignal::Interrupt),
            completion: Err(error),
        } = write_terminal_mode_with_interrupt(&mut radio, PcOutputInterface::Bluetooth, interrupt)
            .await
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
