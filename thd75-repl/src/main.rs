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

use std::net::ToSocketAddrs;
use std::sync::atomic::Ordering;

use thd75_repl::aprintln;

use clap::Parser;
use kenwood_thd75::Radio;
use kenwood_thd75::transport::EitherTransport;
use kenwood_thd75::{AprsClient, AprsClientConfig, AprsEvent};
use kenwood_thd75::{DStarEvent, DStarGateway, DStarGatewayConfig, SlowDataDecoder};

use dstar_gateway::auth::AuthClient;
use dstar_gateway::tokio_shell::{AsyncSession, ShellError};
use dstar_gateway_core::header::DStarHeader as CoreDStarHeader;
use dstar_gateway_core::hosts::HostFile;
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{
    ClientStateKind, Connected, Connecting, DExtra, DPlus, Dcs, DisconnectReason, Event, Session,
    VoiceEndReason,
};
use dstar_gateway_core::types::ProtocolKind;
use dstar_gateway_core::voice::VoiceFrame as CoreVoiceFrame;
use dstar_gateway_core::{Callsign, Module, StreamId, Suffix};

/// Log verbosity level for the opt-in file sink.
///
/// The default is [`Self::Off`]: no file is created and no tracing
/// output is written. File logging is enabled only when the user
/// explicitly passes `--log-level` or `--trace` — this prevents the
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
/// `check` runs the accessibility compliance self-check and exits.
/// Any future non-interactive operations (status dump, send-one,
/// etc.) belong as sibling variants here.
#[derive(clap::Subcommand, Debug)]
enum Subcommand {
    /// Run the accessibility compliance self-check and print a report.
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
#[allow(clippy::struct_excessive_bools)]
struct Cli {
    /// Optional subcommand. Defaults to interactive REPL if omitted.
    #[command(subcommand)]
    command_mode: Option<Subcommand>,

    /// Serial port path (default: auto-discover USB, then Bluetooth).
    #[arg(short, long)]
    port: Option<String>,

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
    /// Every `thd75-repl` invocation creates its own log file — old
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
    /// individual commands are printed but do not halt the script
    /// unless `--script-strict` is also passed.
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
    /// `--yes` to disable the prompt globally — required when running
    /// a script in automation and also useful for interactive sessions
    /// where the operator does not want to be asked every time.
    #[arg(long)]
    yes: bool,

    /// Command to run on startup (e.g. "dstar start KQ4NIT REF030C").
    ///
    /// If provided, the command is executed immediately after connecting.
    /// The REPL continues normally after the command completes.
    #[arg(trailing_var_arg = true)]
    command: Vec<String>,
}

/// Determine the on-disk directory where per-session log files
/// should be written. Returns `None` if no suitable directory can
/// be derived from the environment (extremely rare — only if
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
/// late log lines to be silently discarded — so `main` stores it in
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
/// NOT enable the file sink on its own — file logging is file-flag
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
/// Tries Bluetooth first on macOS (synchronously, no tokio reactor
/// needed) and falls back to serial discovery via the async path.
/// This matches the exact behaviour the binary had before Task 26
/// introduced the optional mock branch; factoring it out keeps the
/// mock + real paths symmetric in `main`.
fn open_real_transport(
    cli_port: Option<&str>,
    cli_baud: u32,
) -> Result<(String, EitherTransport, tokio::runtime::Runtime), Box<dyn std::error::Error>> {
    if let Ok((path, transport)) = transport::discover_and_open(cli_port, cli_baud) {
        let rt = tokio::runtime::Runtime::new()?;
        return Ok((path, transport, rt));
    }
    // BT failed or unavailable — serial needs a tokio reactor.
    let rt = tokio::runtime::Runtime::new()?;
    let (path, transport) =
        rt.block_on(async { transport::discover_and_open(cli_port, cli_baud) })?;
    Ok((path, transport, rt))
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
    let bytes = s.as_bytes();
    let (sign, rest) = match bytes[0] {
        b'+' => (1i32, &s[1..]),
        b'-' => (-1i32, &s[1..]),
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
//
// The `#[cfg(unix)]` body runs a subprocess so it cannot be `const`.
// The `#[cfg(not(unix))]` body is just `None`, which Clippy's nursery
// `missing_const_for_fn` lint flags on Windows builds. Marking the
// function `const fn` would then fail to compile the Unix variant.
// Suppress the lint rather than splitting the function in two.
#[allow(clippy::missing_const_for_fn)]
fn detect_utc_offset_seconds() -> Option<i32> {
    #[cfg(unix)]
    {
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
    #[cfg(not(unix))]
    {
        None
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Subcommands bypass the REPL loop entirely. `check` runs a
    // hardware-free accessibility self-check and exits.
    if matches!(&cli.command_mode, Some(Subcommand::Check)) {
        std::process::exit(thd75_repl::check::run());
    }

    // Configure history buffer capacity before any output is
    // recorded so the first banner line is captured at the correct
    // size.
    thd75_repl::set_history_capacity(cli.history_lines);

    // Apply the --yes flag and script mode settings to the transmit
    // confirmation module so the first transmit command respects them.
    if cli.yes {
        thd75_repl::confirm::set_required(false);
    }
    if cli.script.is_some() {
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
                    eprintln!("Warning: could not detect local time zone. Using UTC.");
                    0
                })
            },
            |spec| {
                parse_utc_offset(spec).unwrap_or_else(|e| {
                    eprintln!("Warning: invalid --utc-offset {spec:?}: {e}. Using UTC.");
                    0
                })
            },
        );
        thd75_repl::UTC_OFFSET_SECS.store(offset_secs, Ordering::Relaxed);
    }

    // Configure logging before anything else so the file captures
    // the full startup sequence. The guard returned here must live
    // until the program exits — dropping it would terminate the
    // background flush thread and silently drop late log lines.
    let _logging_guard = init_logging(&cli);

    println!(
        "{}",
        thd75_repl::output::startup_banner(env!("CARGO_PKG_VERSION"))
    );

    // Try to open transport BEFORE creating the tokio runtime.
    //
    // IOBluetooth RFCOMM uses CoreFoundation callbacks on the main thread.
    // A tokio runtime's reactor can interfere with CFRunLoop dispatch,
    // causing BT connections to fail. The TUI avoids this by opening
    // transport before the runtime — we do the same.
    //
    // If BT isn't available (USB connected, or Linux/Windows), serial
    // transport needs a tokio reactor (mio), so we create the runtime
    // and retry via the serial path.
    //
    // When compiled with the `testing` feature and `--mock-radio
    // <scenario>` is passed, short-circuit the real transport
    // discovery entirely and construct a programmed `MockTransport`
    // instead.
    #[cfg(feature = "testing")]
    let (path, transport, rt) = if let Some(ref scenario) = cli.mock_radio {
        let mock = thd75_repl::mock_scenarios::build(scenario)
            .ok_or_else(|| format!("Unknown mock scenario: {scenario}. Known: simple, empty."))?;
        let rt = tokio::runtime::Runtime::new()?;
        (format!("mock:{scenario}"), EitherTransport::Mock(mock), rt)
    } else {
        open_real_transport(cli.port.as_deref(), cli.baud)?
    };
    #[cfg(not(feature = "testing"))]
    let (path, transport, rt) = open_real_transport(cli.port.as_deref(), cli.baud)?;

    println!("{}", thd75_repl::output::connected_via(&path));

    // Build initial command from trailing args, if any.
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
    let in_script_mode = cli.script.is_some();
    let script_strict = cli.script_strict;

    // Run the async REPL.
    rt.block_on(run_repl(
        transport,
        cli.port.clone(),
        cli.baud,
        initial_command,
        script,
        script_strict,
        in_script_mode,
    ))?;

    Ok(())
}

/// Read a line from stdin on a background thread, send via channel.
/// Returns None on EOF/interrupt.
/// Read one line from the user via rustyline. Returns `None` on
/// EOF, Ctrl-C, or input error.
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

/// The three operating modes of the REPL.
enum ReplState {
    /// Normal CAT control — radio is directly accessible.
    Cat(Radio<EitherTransport>),
    /// APRS/KISS mode — radio consumed by `AprsClient`.
    Aprs(Box<AprsClient<EitherTransport>>),
    /// D-STAR gateway/MMDVM mode — radio consumed by `DStarGateway`.
    Dstar(Box<DStarSession>),
}

/// Active D-STAR session holding both the radio MMDVM gateway and
/// the reflector UDP client.
struct DStarSession {
    /// Radio-side MMDVM gateway.
    gateway: DStarGateway<EitherTransport>,
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
    /// packets whose `rpt1` module byte is not a valid module — see
    /// [`build_reflector_header`] for details.
    reflector_callsign: Callsign,
    /// Current RX stream ID from reflector (`None` = no active stream).
    rx_stream_id: Option<StreamId>,
    /// Echo test state: records TX frames and plays them back.
    echo: EchoState,
    /// When true, the next TX is captured for echo regardless of URCALL.
    echo_armed: bool,
    /// Slow data decoder for incoming reflector voice frames.
    /// Decodes text messages embedded in the slow data bytes.
    rx_slow_data: SlowDataDecoder,
    /// Last 20-byte slow-data text message printed for the current
    /// stream, used to dedupe repeat emissions. D-STAR radios
    /// continuously re-transmit the operator's text message across
    /// the voice stream (so late joiners can see it), so the decoder
    /// legitimately re-emits the same 20 bytes every ~320 ms for the
    /// full duration of a burst. Print it the first time we see it
    /// per stream, and again only if the operator actually changes
    /// the text mid-transmission. Cleared on `VoiceStart` for a new
    /// stream and on `VoiceEnd`.
    rx_last_slow_text: Option<[u8; 20]>,
    /// Outgoing slow data text message to embed in TX voice frames.
    /// Set via the `text` command. Cleared after one transmission.
    tx_text: Option<String>,
    /// Pre-encoded slow data payloads for the current TX text.
    tx_slow_data: Vec<[u8; 3]>,
    /// Index into `tx_slow_data` for the next frame to send.
    tx_slow_data_idx: usize,
}

/// Echo test state machine.
///
/// Records the user's TX audio and plays it back locally through the
/// radio, verifying the full MMDVM voice path without involving a
/// reflector. Triggered by the `echo` REPL command (arms the next TX)
/// or by URCALL `"       E"` per ircDDBGateway convention.
///
/// Max 60 seconds of audio (3000 frames at 50 fps).
#[derive(Debug)]
enum EchoState {
    /// Echo test not active.
    Idle,
    /// Recording TX audio. Stores the original header and AMBE frames.
    Recording {
        /// Original D-STAR header from the TX stream.
        header: kenwood_thd75::DStarHeader,
        /// Buffered AMBE voice frames.
        frames: Vec<kenwood_thd75::DStarVoiceFrame>,
    },
    /// Waiting briefly before playback (per ircDDBGateway `REPLY_TIME`).
    Waiting {
        /// Original D-STAR header from the TX stream.
        header: kenwood_thd75::DStarHeader,
        /// Buffered AMBE voice frames.
        frames: Vec<kenwood_thd75::DStarVoiceFrame>,
        /// When the wait started.
        since: std::time::Instant,
    },
    /// Playing back recorded audio to the radio.
    Playing,
}

impl std::fmt::Debug for DStarSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DStarSession")
            .field("callsign", &self.callsign)
            .field("reflector_connected", &self.reflector.is_some())
            .finish_non_exhaustive()
    }
}

/// Protocol-generic wrapper over [`AsyncSession<P>`] for runtime dispatch.
///
/// thd75-repl supports all three reflector protocols at runtime via
/// the `link` command, but the new typestate API is protocol-parametric
/// at compile time. This enum fans the handle out into per-protocol
/// variants so the repl can hold a single session value regardless of
/// which protocol the user connected to, and dispatch to the right
/// inner `AsyncSession<P>` for voice TX / event reception.
#[derive(Debug)]
enum ReflectorSession {
    /// `DPlus` (REF) session handle.
    DPlus(AsyncSession<DPlus>),
    /// `DExtra` (XRF/XLX) session handle.
    DExtra(AsyncSession<DExtra>),
    /// `DCS` session handle.
    Dcs(AsyncSession<Dcs>),
}

/// Runtime-unified event mirror of [`Event<P>`].
///
/// The inner event data is identical across the three protocols —
/// this enum exists only to erase the `P: Protocol` parameter for the
/// runtime event-handling functions (`relay_reflector_to_radio`,
/// `trace_reflector_event`, `print_reflector_event`) that don't care
/// which protocol produced a given event.
#[derive(Debug)]
enum RuntimeEvent {
    /// Session reached `Connected`.
    Connected,
    /// Session left `Connected` with a reason.
    Disconnected(DisconnectReason),
    /// Keepalive echo from the reflector.
    PollEcho,
    /// A new voice stream started.
    VoiceStart {
        /// Stream id of the new stream.
        stream_id: StreamId,
        /// Decoded D-STAR header.
        header: Box<CoreDStarHeader>,
    },
    /// A voice frame within an active stream.
    VoiceFrame {
        /// Stream id the frame belongs to.
        stream_id: StreamId,
        /// Frame sequence (0-20).
        seq: u8,
        /// Voice frame payload.
        frame: Box<CoreVoiceFrame>,
    },
    /// A voice stream ended.
    VoiceEnd {
        /// Stream id of the terminated stream.
        stream_id: StreamId,
        /// Why the stream ended.
        #[allow(dead_code, reason = "reason reserved for future trace output")]
        reason: VoiceEndReason,
    },
}

impl<P> From<Event<P>> for RuntimeEvent
where
    P: dstar_gateway_core::session::client::Protocol,
{
    fn from(e: Event<P>) -> Self {
        match e {
            Event::Connected { .. } => Self::Connected,
            Event::Disconnected { reason } => Self::Disconnected(reason),
            Event::PollEcho { .. } => Self::PollEcho,
            Event::VoiceStart {
                stream_id, header, ..
            } => Self::VoiceStart {
                stream_id,
                header: Box::new(header),
            },
            Event::VoiceFrame {
                stream_id,
                seq,
                frame,
            } => Self::VoiceFrame {
                stream_id,
                seq,
                frame: Box::new(frame),
            },
            Event::VoiceEnd { stream_id, reason } => Self::VoiceEnd { stream_id, reason },
            // `Event` is `#[non_exhaustive]` and carries a private
            // `__Phantom` variant to thread the `P` type parameter.
            // Catch any future additions here.
            _ => unreachable!("Event<P> is exhaustively matched above"),
        }
    }
}

impl ReflectorSession {
    /// Drain the next runtime event from whichever protocol variant is active.
    async fn next_event(&mut self) -> Option<RuntimeEvent> {
        match self {
            Self::DPlus(s) => s.next_event().await.map(RuntimeEvent::from),
            Self::DExtra(s) => s.next_event().await.map(RuntimeEvent::from),
            Self::Dcs(s) => s.next_event().await.map(RuntimeEvent::from),
        }
    }

    /// Send a voice header and start a new outbound voice stream.
    async fn send_header(
        &mut self,
        header: CoreDStarHeader,
        stream_id: StreamId,
    ) -> Result<(), ShellError> {
        match self {
            Self::DPlus(s) => s.send_header(header, stream_id).await,
            Self::DExtra(s) => s.send_header(header, stream_id).await,
            Self::Dcs(s) => s.send_header(header, stream_id).await,
        }
    }

    /// Send a voice data frame on the active stream.
    async fn send_voice(
        &mut self,
        stream_id: StreamId,
        seq: u8,
        frame: CoreVoiceFrame,
    ) -> Result<(), ShellError> {
        match self {
            Self::DPlus(s) => s.send_voice(stream_id, seq, frame).await,
            Self::DExtra(s) => s.send_voice(stream_id, seq, frame).await,
            Self::Dcs(s) => s.send_voice(stream_id, seq, frame).await,
        }
    }

    /// Send a voice EOT packet, ending the outbound stream.
    async fn send_eot(&mut self, stream_id: StreamId, seq: u8) -> Result<(), ShellError> {
        match self {
            Self::DPlus(s) => s.send_eot(stream_id, seq).await,
            Self::DExtra(s) => s.send_eot(stream_id, seq).await,
            Self::Dcs(s) => s.send_eot(stream_id, seq).await,
        }
    }

    /// Request a graceful disconnect from the reflector.
    async fn disconnect(&mut self) -> Result<(), ShellError> {
        match self {
            Self::DPlus(s) => s.disconnect().await,
            Self::DExtra(s) => s.disconnect().await,
            Self::Dcs(s) => s.disconnect().await,
        }
    }
}

/// Main REPL loop. Manages three states: CAT (normal radio control),
/// APRS (packet radio), and D-STAR (digital voice gateway). Each state
/// owns the radio transport exclusively.
#[allow(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    clippy::too_many_arguments
)]
async fn run_repl(
    transport: EitherTransport,
    cli_port: Option<String>,
    cli_baud: u32,
    initial_command: Option<String>,
    script: Option<thd75_repl::script::Script>,
    script_strict: bool,
    in_script_mode: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // `script_strict` is reserved for a future enhancement that halts
    // the REPL loop on the first command error. Silencing the
    // unused-var lint keeps the signature stable now so later tasks
    // don't have to reshape the calls.
    let _ = script_strict;

    // Try connect_safe (sends TNC exit preamble to recover from stuck modes).
    let mut radio = Radio::connect_safe(transport).await?;

    // Try to identify. If it fails, the radio may be in MMDVM/TERM mode.
    let mut state = match radio.identify().await {
        Ok(info) => {
            let fw = radio.get_firmware_version().await.unwrap_or_default();
            println!(
                "{}",
                thd75_repl::output::startup_identified(&info.model.clone(), &fw)
            );
            println!("{}", thd75_repl::output::type_help_hint());
            ReplState::Cat(radio)
        }
        Err(_) => {
            // CAT failed. Check if radio is in MMDVM/gateway mode.
            if detect_mmdvm_mode(&mut radio).await {
                println!("Radio is in D-STAR Reflector Terminal Mode.");
                println!("Type dstar start <callsign> [reflector] to begin, or quit to exit.");
                println!("Example: dstar start W1AW REF030C");
                ReplState::Cat(radio)
            } else {
                return Err(
                    "Error: could not identify radio. Check connection and radio mode.".into(),
                );
            }
        }
    };

    let mut rl = rustyline::DefaultEditor::new()?;
    let mut pending_command = initial_command;
    let mut script_commands: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    if let Some(s) = script {
        for cmd in s.commands {
            script_commands.push_back(cmd);
        }
    }

    loop {
        let prompt = match &state {
            ReplState::Cat(_) => "d75> ",
            ReplState::Aprs(_) => "aprs> ",
            ReplState::Dstar(_) => "dstar> ",
        };

        // Command source priority: initial command, then script queue,
        // then interactive input. When in script mode and the queue
        // drains, return None so the main loop exits cleanly instead
        // of falling into the rustyline prompt. Clippy wants us to
        // rewrite this as `map_or_else`, but the three-way chain is
        // clearer as an if-ladder.
        #[allow(clippy::option_if_let_else)]
        let line = if let Some(cmd) = pending_command.take() {
            println!("{prompt}{cmd}");
            Some(cmd)
        } else if let Some(cmd) = script_commands.pop_front() {
            println!("{prompt}{cmd}");
            Some(cmd)
        } else if in_script_mode {
            None
        } else {
            read_line_blocking(&mut rl, prompt)
        };
        let Some(line) = line else {
            // EOF or Ctrl-C: disconnect cleanly.
            let radio = match state {
                ReplState::Cat(r) => Some(r),
                ReplState::Aprs(c) => c.stop().await.ok(),
                ReplState::Dstar(mut s) => {
                    if let Some(ref mut r) = s.reflector {
                        let _ = r.disconnect().await;
                    }
                    s.gateway.stop().await.ok()
                }
            };
            if let Some(r) = radio {
                let _ = r.disconnect().await;
            }
            println!("{}", thd75_repl::output::goodbye());
            return Ok(());
        };

        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }
        let _ = rl.add_history_entry(&line);

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        let cmd = parts[0].to_lowercase();

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
                        client.stop().await.ok()
                    }
                    ReplState::Dstar(mut session) => {
                        println!("Exiting D-STAR mode and restoring normal radio mode.");
                        if let Some(ref mut refl) = session.reflector {
                            let _ = refl.disconnect().await;
                        }
                        match exit_dstar(session.gateway, cli_port.as_deref(), cli_baud).await {
                            Ok(r) => Some(r),
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
                    ReplState::Cat(r) => Some(r),
                };
                if let Some(r) = radio {
                    let _ = r.disconnect().await;
                }
                println!("{}", thd75_repl::output::goodbye());
                return Ok(());
            }
            "last" | "repeat" => {
                let count = if let Some(arg) = parts.get(1) {
                    if *arg == "all" {
                        thd75_repl::HISTORY_CAPACITY_DEFAULT
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
            _ => {}
        }

        // Mode-specific dispatch.
        state = match state {
            ReplState::Cat(mut radio) => {
                dispatch_cat(&mut radio, &cmd, &parts).await;

                // Check for mode transitions.
                if cmd == "aprs" && parts.get(1).is_some_and(|s| *s == "start") {
                    if parts.get(2).is_none() {
                        println!("Error: callsign required. Usage: aprs start <callsign> [ssid]");
                        println!("Example: aprs start W1AW 7");
                        ReplState::Cat(radio)
                    } else {
                        match enter_aprs(radio, &parts[2..]).await {
                            Ok(client) => ReplState::Aprs(Box::new(client)),
                            Err((radio_back, e)) => {
                                println!(
                                    "{}",
                                    thd75_repl::output::error(format_args!(
                                        "entering APRS mode: {e}"
                                    ))
                                );
                                ReplState::Cat(radio_back)
                            }
                        }
                    }
                } else if cmd == "dstar" && parts.get(1).is_some_and(|s| *s == "start") {
                    if parts.get(2).is_none() {
                        println!(
                            "Error: callsign required. Usage: dstar start <callsign> [reflector]"
                        );
                        println!("Example: dstar start W1AW XRF030C");
                        ReplState::Cat(radio)
                    } else {
                        match enter_dstar(radio, &parts[2..], cli_port.as_deref(), cli_baud).await {
                            Ok(mut session) => {
                                // If a reflector was connected, auto-enter monitor.
                                if session.reflector.is_some() {
                                    println!("Monitoring. Press Ctrl-C to return to prompt.");
                                    run_dstar_monitor(&mut session).await;
                                }
                                ReplState::Dstar(Box::new(session))
                            }
                            Err((Some(radio_back), e)) => {
                                println!(
                                    "{}",
                                    thd75_repl::output::error(format_args!(
                                        "entering D-STAR mode: {e}"
                                    ))
                                );
                                ReplState::Cat(radio_back)
                            }
                            Err((None, e)) => {
                                println!(
                                    "{}",
                                    thd75_repl::output::error(format_args!(
                                        "entering D-STAR mode: {e}"
                                    ))
                                );
                                println!(
                                    "Error: radio connection lost. Please close and reopen the program."
                                );
                                break;
                            }
                        }
                    }
                } else {
                    ReplState::Cat(radio)
                }
            }

            ReplState::Aprs(mut client) => {
                if cmd == "aprs" && parts.get(1).is_some_and(|s| *s == "stop") {
                    match client.stop().await {
                        Ok(radio) => {
                            println!("APRS mode stopped. Returned to CAT mode.");
                            ReplState::Cat(radio)
                        }
                        Err(e) => {
                            println!(
                                "{}",
                                thd75_repl::output::error(format_args!("stopping APRS: {e}"))
                            );
                            println!(
                                "Error: radio connection lost. Please close and reopen the program."
                            );
                            break;
                        }
                    }
                } else {
                    dispatch_aprs(&mut client, &cmd, &parts).await;
                    ReplState::Aprs(client)
                }
            }

            ReplState::Dstar(mut session) => {
                if cmd == "dstar" && parts.get(1).is_some_and(|s| *s == "stop") {
                    if let Some(ref mut refl) = session.reflector {
                        let _ = refl.disconnect().await;
                    }
                    match exit_dstar(session.gateway, cli_port.as_deref(), cli_baud).await {
                        Ok(radio) => {
                            println!("D-STAR mode stopped. Returned to normal radio control.");
                            ReplState::Cat(radio)
                        }
                        Err(e) => {
                            println!(
                                "{}",
                                thd75_repl::output::error(format_args!("exiting D-STAR mode: {e}"))
                            );
                            println!(
                                "Error: radio connection lost. Please close and reopen the program."
                            );
                            break;
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
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// CAT mode dispatch
// ---------------------------------------------------------------------------

/// Dispatch a command in CAT mode to the appropriate handler function.
#[allow(clippy::cognitive_complexity)]
async fn dispatch_cat(radio: &mut Radio<EitherTransport>, cmd: &str, parts: &[&str]) {
    match cmd {
        "id" | "identify" => commands::identify(radio).await,
        "freq" | "frequency" => commands::frequency(radio, &parts[1..]).await,
        "mode" => commands::set_mode(radio, &parts[1..]).await,
        "squelch" | "sq" => commands::squelch(radio, &parts[1..]).await,
        "power" | "pwr" => commands::set_power(radio, &parts[1..]).await,
        "battery" | "bat" => commands::battery(radio).await,
        "lock" => commands::lock(radio, &parts[1..]).await,
        "dualband" | "dual" => commands::dual_band(radio, &parts[1..]).await,
        "bluetooth" | "bt" => commands::bluetooth(radio, &parts[1..]).await,
        "vox" => commands::vox(radio, &parts[1..]).await,
        "fm" => commands::fm_radio(radio, &parts[1..]).await,
        "attenuator" | "att" => commands::attenuator(radio, &parts[1..]).await,
        "step" => commands::step_size(radio, &parts[1..]).await,
        "up" => commands::step_up(radio, &parts[1..]).await,
        "down" => commands::step_down(radio, &parts[1..]).await,
        "channel" | "ch" => commands::channel(radio, &parts[1..]).await,
        "channels" => commands::channels(radio, &parts[1..]).await,
        "tune" => commands::tune(radio, &parts[1..]).await,
        "recall" => commands::recall(radio, &parts[1..]).await,
        "meter" | "smeter" => commands::smeter(radio, &parts[1..]).await,
        "vfo" => commands::vfo(radio, &parts[1..]).await,
        "clock" | "time" => commands::clock(radio).await,
        "gps" => commands::gps(radio, &parts[1..]).await,
        "urcall" | "ur" => commands::urcall(radio, &parts[1..]).await,
        "cq" => commands::cq(radio).await,
        "reflector" | "ref" => commands::reflector(radio, &parts[1..]).await,
        "unreflector" | "unref" | "unlink" => commands::unreflector(radio).await,
        "status" => commands::status(radio).await,
        "aprs" => {
            if parts.get(1).is_some_and(|s| *s == "start") {
                // Handled by caller after dispatch.
            } else {
                println!("Usage: aprs start <callsign>");
                println!("  Enters APRS KISS mode. Type aprs stop to exit.");
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
/// On error, returns the radio so the REPL can continue in CAT mode.
async fn enter_aprs(
    radio: Radio<EitherTransport>,
    args: &[&str],
) -> Result<AprsClient<EitherTransport>, (Radio<EitherTransport>, String)> {
    let callsign = args[0];
    let ssid: u8 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    println!("Leaving normal radio control. Entering APRS mode as {callsign}-{ssid}.");

    let config = AprsClientConfig::new(callsign, ssid);
    match AprsClient::start(radio, config).await {
        Ok(client) => {
            println!("{}", thd75_repl::output::aprs_mode_active());
            println!("Commands: monitor, msg, position, beacon, stations, igate, aprs stop");
            Ok(client)
        }
        Err((radio, e)) => Err((radio, format!("{e}"))),
    }
}

/// Dispatch a command in APRS mode.
#[allow(clippy::too_many_lines)]
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
            if !thd75_repl::confirm::tx_confirm() {
                return;
            }
            let addressee = parts[1];
            let text = parts[2..].join(" ");
            match client.send_message(addressee, &text).await {
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
            match client.send_status("REPL beacon").await {
                Ok(()) => println!("Status beacon sent."),
                Err(e) => println!(
                    "{}",
                    thd75_repl::output::error(format_args!("sending beacon: {e}"))
                ),
            }
        }
        "position" | "pos" => {
            if parts.len() < 3 {
                println!("Usage: position <lat> <lon> [comment]");
                println!("  Example: position 35.30 -82.46 Portable");
                return;
            }
            let Ok(lat) = parts[1].parse::<f64>() else {
                println!("Error: invalid latitude. Use decimal degrees (e.g. 35.30).");
                return;
            };
            let Ok(lon) = parts[2].parse::<f64>() else {
                println!("Error: invalid longitude. Use decimal degrees (e.g. -82.46).");
                return;
            };
            if !thd75_repl::confirm::tx_confirm() {
                return;
            }
            let comment = if parts.len() > 3 {
                parts[3..].join(" ")
            } else {
                String::new()
            };
            match client.beacon_position(lat, lon, &comment).await {
                Ok(()) => println!(
                    "Position beacon sent: {lat:.4}, {lon:.4}{}.",
                    if comment.is_empty() {
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
                println!(
                    "{}",
                    thd75_repl::output::aprs_stations_summary(stations.len())
                );
            }
        }
        "igate" => {
            if parts.len() < 2 {
                println!("Usage: igate <filter>");
                println!("  Connects to APRS-IS and bridges RF to internet.");
                println!("  Example: igate r/35.30/-82.46/100");
                println!("  (receive stations within 100km of the given lat/lon)");
                println!("  Press Ctrl-C to disconnect.");
                return;
            }
            let filter = parts[1..].join(" ");
            run_igate(client, &filter).await;
        }
        _ => println!(
            "APRS command not recognized: {cmd}. \
             Commands: monitor, msg, position, beacon, stations, igate, aprs stop"
        ),
    }
}

/// Run the `IGate` bridge: APRS-IS ↔ RF.
///
/// Connects to the default APRS-IS server, forwards received RF packets
/// to IS, and gates appropriate IS packets back to RF. Runs until the
/// user presses Ctrl-C or the connection is lost.
async fn run_igate(client: &mut AprsClient<EitherTransport>, filter: &str) {
    use kenwood_thd75::{AprsIsClient, AprsIsConfig, AprsIsEvent};

    let callsign = client.config().callsign.clone();
    let ssid = client.config().ssid;
    let login_call = if ssid > 0 {
        format!("{callsign}-{ssid}")
    } else {
        callsign.clone()
    };

    println!("Connecting to APRS-IS as {login_call}.");
    let mut is_config = AprsIsConfig::new(&login_call);
    filter.clone_into(&mut is_config.filter);

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
                let _ = is_client.shutdown().await;
                break;
            }
            // Poll APRS-IS for incoming packets.
            is_result = is_client.next_event() => {
                match is_result {
                    Ok(AprsIsEvent::Packet(line)) => {
                        aprintln!("{}", thd75_repl::output::aprs_is_incoming(&line));
                        // Gate to RF if appropriate. The helper checks
                        // whether the packet should be forwarded per
                        // IGate rules (station heard on RF recently, etc).
                        if let Err(e) = client.gate_from_is(&line).await {
                            println!("Error: gate to RF: {e}");
                        }
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
                    Ok(Some(event)) => {
                        print_aprs_event(&event);
                        // Forward raw packets to APRS-IS.
                        if let AprsEvent::RawPacket(ref pkt) = event {
                            let is_line = client.format_for_is(pkt);
                            if let Err(e) = is_client.send_raw_line(&is_line).await {
                                println!("Error: gate to IS: {e}");
                            }
                        }
                    }
                    Ok(None) => {}
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

// ---------------------------------------------------------------------------
// D-STAR mode
// ---------------------------------------------------------------------------

/// Detect whether the radio is currently in MMDVM mode by sending
/// an MMDVM `GET_VERSION` frame and checking for an `0xE0` response.
async fn detect_mmdvm_mode(radio: &mut Radio<EitherTransport>) -> bool {
    // Send MMDVM GET_VERSION: E0 03 00
    let get_version = [0xE0, 0x03, 0x00];
    if radio.transport_write(&get_version).await.is_err() {
        return false;
    }

    // Read with short timeout — MMDVM responds in ~20ms, CAT won't respond.
    let mut buf = [0u8; 64];
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            radio.transport_read(&mut buf),
        )
        .await,
        Ok(Ok(n)) if n > 0 && buf[0] == 0xE0
    )
}

/// MCP offset for DV Gateway mode setting (0=Off, 1=Reflector Terminal, 2=Access Point).
/// Firmware-verified: GW CAT handler reads from this offset (read-only via CAT).
const GATEWAY_MODE_MCP_OFFSET: u16 = 0x1CA0;

/// MCP page containing the gateway mode byte.
const GATEWAY_MODE_PAGE: u16 = GATEWAY_MODE_MCP_OFFSET / 256;

/// Byte index within the page.
const GATEWAY_MODE_BYTE: usize = (GATEWAY_MODE_MCP_OFFSET % 256) as usize;

/// Enter D-STAR gateway (MMDVM) mode.
///
/// Enables Reflector Terminal Mode via MCP write to offset 0x1CA0,
/// which causes the connection to drop. After reconnecting, the
/// radio's BT SPP speaks MMDVM binary framing (no TN command needed).
///
/// On error, returns the radio so the REPL can continue in CAT mode.
async fn enter_dstar(
    mut radio: Radio<EitherTransport>,
    args: &[&str],
    cli_port: Option<&str>,
    cli_baud: u32,
) -> Result<DStarSession, (Option<Radio<EitherTransport>>, String)> {
    let callsign = args[0];
    // Optional reflector argument: e.g. "XRF030C" → name="XRF030", module='C'
    let reflector_arg = args.get(1).copied();

    // Check if radio is already in MMDVM/gateway mode by probing.
    // Send MMDVM GET_VERSION — if we get an E0 response, skip MCP write.
    println!("Checking if radio is already in D-STAR gateway mode.");
    let already_mmdvm = detect_mmdvm_mode(&mut radio).await;

    if already_mmdvm {
        println!("Radio is already in Reflector Terminal Mode.");
    } else {
        // Enable Reflector Terminal Mode via MCP write.
        println!("Enabling D-STAR Reflector Terminal Mode via memory write.");
        if let Err(e) = radio
            .modify_memory_page(GATEWAY_MODE_PAGE, |data| {
                data[GATEWAY_MODE_BYTE] = 1; // ReflectorTerminal
            })
            .await
        {
            return Err((Some(radio), format!("MCP write failed: {e}")));
        }

        // Connection dropped after MCP exit. Close and wait for reboot.
        println!("Closing connection. Waiting for radio to reboot into gateway mode.");
        let _ = radio.disconnect().await;
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        println!("Reconnecting.");

        let (_path, new_transport) = match transport::discover_and_open(cli_port, cli_baud) {
            Ok(t) => t,
            Err(e) => return Err((None, format!("Reconnect failed: {e}"))),
        };
        radio = match Radio::connect(new_transport).await {
            Ok(r) => r,
            Err(e) => return Err((None, format!("Connect failed: {e}"))),
        };
    }

    // Radio is now in MMDVM mode. Start the gateway.
    println!("Starting D-STAR gateway as {callsign}.");

    let config = DStarGatewayConfig::new(callsign);
    let gateway = match DStarGateway::start_gateway_mode(radio, config).await {
        Ok(gw) => gw,
        Err(e) => return Err((None, format!("Gateway init failed: {e}"))),
    };
    println!("MMDVM modem initialized.");

    // Validate the callsign once. Everything downstream uses the typed form.
    let callsign_typed = match Callsign::try_from_str(callsign) {
        Ok(cs) => cs,
        Err(e) => {
            return Err((None, format!("Invalid station callsign {callsign}: {e}")));
        }
    };

    // Connect to reflector if specified.
    let (reflector, link_arg) = if let Some(ref_str) = reflector_arg {
        match parse_link_arg(ref_str) {
            Ok(arg) => match connect_reflector(callsign_typed, &arg).await {
                Ok(client) => (Some(client), Some(arg)),
                Err(e) => {
                    println!("Error: could not connect to reflector: {e}");
                    println!(
                        "Gateway active without reflector. Use link command to connect later."
                    );
                    (None, None)
                }
            },
            Err(e) => {
                println!("Error: {e}");
                (None, None)
            }
        }
    } else {
        println!("No reflector specified. Use link command to connect.");
        (None, None)
    };

    println!("D-STAR gateway active. Type dstar stop to exit.");
    println!("Commands: monitor, link, unlink, echo, text, heard, status, dstar stop");
    let default_module = Module::try_from_char('C').expect("'C' is valid");
    let (local_module, reflector_module) = link_arg
        .as_ref()
        .map_or((default_module, default_module), |arg| {
            (arg.local_module, arg.reflector_module)
        });
    // Parse the reflector name into a Callsign. This is used in the
    // radio-to-reflector relay path to build the outbound rpt2 field
    // per ircDDBGateway convention (see build_reflector_header). If
    // no reflector was specified on the command line, we fall back
    // to the station callsign as a placeholder — the relay path only
    // cares once link has actually connected.
    let reflector_callsign = link_arg.as_ref().map_or(callsign_typed, |arg| {
        Callsign::try_from_str(&arg.reflector_name).unwrap_or(callsign_typed)
    });
    Ok(DStarSession {
        gateway,
        reflector,
        callsign: callsign_typed,
        tx_stream_id: None,
        tx_seq: 0,
        local_module,
        reflector_module,
        reflector_callsign,
        rx_stream_id: None,
        echo: EchoState::Idle,
        echo_armed: false,
        rx_slow_data: SlowDataDecoder::new(),
        rx_last_slow_text: None,
        tx_text: None,
        tx_slow_data: Vec::new(),
        tx_slow_data_idx: 0,
    })
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
/// - `XRF030C` — link to `XRF030` module `C`, with our local module
///   matching the reflector module (`C`).
/// - `B:XRF030C` — link to `XRF030` module `C`, but present our local
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
        "http://www.pistar.uk/downloads/DExtra_Hosts.txt",
        30001,
    ),
    (
        "DPlus_Hosts.txt",
        "http://www.pistar.uk/downloads/DPlus_Hosts.txt",
        20001,
    ),
    (
        "DCS_Hosts.txt",
        "http://www.pistar.uk/downloads/DCS_Hosts.txt",
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
    let _ = std::fs::create_dir_all(&dir);

    for (name, url, _) in HOST_FILES {
        let path = dir.join(name);
        if path.exists() {
            continue;
        }
        // Use a simple TCP GET since we don't have an HTTP client dep.
        // Shell out to curl which is available on macOS/Linux.
        match tokio::process::Command::new("curl")
            .args(["-sL", "-o"])
            .arg(&path)
            .arg(url)
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                println!("Downloaded {name}.");
            }
            Ok(output) => {
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

/// Load Pi-Star host files from `~/.config/thd75-repl/`.
fn load_host_files() -> HostFile {
    let mut hosts = HostFile::new();
    let dir = host_files_dir();

    for (name, _, port) in HOST_FILES {
        let path = dir.join(name);
        if let Ok(content) = std::fs::read_to_string(&path) {
            hosts.parse(&content, *port);
        }
    }
    hosts
}

/// Connect timeout for the manual handshake loop.
const REFLECTOR_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Bind an ephemeral local UDP socket for a new reflector session.
async fn bind_reflector_socket() -> Result<std::sync::Arc<tokio::net::UdpSocket>, String> {
    tokio::net::UdpSocket::bind("0.0.0.0:0")
        .await
        .map(std::sync::Arc::new)
        .map_err(|e| format!("UDP bind failed: {e}"))
}

/// Drive a `Session<P, Connecting>` through the reflector handshake
/// until the sans-io core reports [`ClientStateKind::Connected`] or
/// [`ClientStateKind::Closed`] (rejected), or the configured deadline
/// elapses. Returns the promoted `Session<P, Connected>` on success.
///
/// Shared between [`connect_dextra`], [`connect_dplus`], and
/// [`connect_dcs`] — the handshake packet count differs across
/// protocols but the polling loop is identical (drain outbound,
/// recv with short timeout, feed `handle_input`, repeat).
async fn drive_handshake_to_connected<P>(
    mut session: Session<P, Connecting>,
    socket: &tokio::net::UdpSocket,
) -> Result<Session<P, Connected>, String>
where
    P: dstar_gateway_core::session::client::Protocol,
{
    let deadline = std::time::Instant::now() + REFLECTOR_CONNECT_TIMEOUT;
    let mut buf = [0u8; 2048];

    loop {
        match session.state_kind() {
            ClientStateKind::Connected => break,
            ClientStateKind::Closed => {
                return Err("reflector rejected the connection".to_string());
            }
            _ => {}
        }

        if std::time::Instant::now() >= deadline {
            return Err("timeout waiting for reflector acknowledgement".to_string());
        }

        // Drain any outbound packets the core wants to send.
        while let Some(tx) = session.poll_transmit(std::time::Instant::now()) {
            let _ = socket
                .send_to(tx.payload, tx.dst)
                .await
                .map_err(|e| format!("handshake send failed: {e}"))?;
        }

        // Wait for either an inbound datagram or a short polling tick.
        match tokio::time::timeout(
            std::time::Duration::from_millis(100),
            socket.recv_from(&mut buf),
        )
        .await
        {
            Ok(Ok((n, src))) => {
                let Some(bytes) = buf.get(..n) else {
                    continue;
                };
                session
                    .handle_input(std::time::Instant::now(), src, bytes)
                    .map_err(|e| format!("handshake decode failed: {e}"))?;
            }
            Ok(Err(e)) => return Err(format!("handshake recv failed: {e}")),
            Err(_) => {
                // No datagram within the polling window — let the core
                // fire any timers (keepalives, retransmits).
                session.handle_timeout(std::time::Instant::now());
            }
        }
    }

    session
        .promote()
        .map_err(|f| format!("promote to Connected failed: {}", f.error))
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

/// Connect to a reflector using a parsed [`LinkArg`].
///
/// The caller validates the reflector string into `LinkArg` first so
/// this function takes already-typed parameters and can focus on
/// protocol selection, host lookup, and driving the sans-io
/// typestate session through the handshake.
async fn connect_reflector(callsign: Callsign, link: &LinkArg) -> Result<ReflectorSession, String> {
    let ref_name = &link.reflector_name;

    let protocol = ProtocolKind::from_reflector_prefix(ref_name).ok_or_else(|| {
        format!(
            "Unsupported reflector prefix: {ref_name}. \
             Supported: REF (DPlus), XRF/XLX (DExtra), DCS."
        )
    })?;

    // Ensure host files exist, downloading if needed.
    ensure_host_files().await;
    let hosts = load_host_files();
    let entry = hosts.lookup(ref_name).ok_or_else(|| {
        format!(
            "Reflector {ref_name} not found in host files. \
             Download host files to ~/.config/thd75-repl/ from pistar.uk/downloads/"
        )
    })?;

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

    println!("Connected to {ref_name} module {}.", link.reflector_module);
    Ok(session)
}

/// Exit D-STAR gateway mode and restore CAT mode.
///
/// Stops the gateway, disconnects, then does an MCP write to set
/// the gateway mode back to Off (0x1CA0 = 0). Reconnects in CAT mode.
async fn exit_dstar(
    gw: DStarGateway<EitherTransport>,
    cli_port: Option<&str>,
    cli_baud: u32,
) -> Result<Radio<EitherTransport>, String> {
    // Stop the gateway — this sends TN 0,0 which won't actually exit
    // Reflector Terminal Mode (that's a firmware setting, not a TNC
    // mode). But it cleanly ends our MMDVM session.
    println!("Stopping D-STAR gateway.");
    let radio = gw
        .stop()
        .await
        .map_err(|e| format!("Gateway stop failed: {e}"))?;

    // Disconnect BT to release the RFCOMM channel.
    let _ = radio.disconnect().await;

    // The radio is still in Reflector Terminal Mode. We cannot
    // MCP-write it back to Off because MCP requires CAT mode, but
    // the radio speaks MMDVM binary in TERM mode. This is the same
    // limitation d75link has — the user must change Menu 650 manually.
    println!("D-STAR gateway stopped.");
    println!("Please set Menu 650 (DV Gateway) to Off on the radio.");
    println!("Press Enter when done.");

    let mut input = String::new();
    let _ = std::io::stdin().read_line(&mut input);

    // Reconnect — should be in CAT mode now if user changed Menu 650.
    println!("Reconnecting.");
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let (_path, transport) = transport::discover_and_open(cli_port, cli_baud)
        .map_err(|e| format!("Reconnect failed: {e}"))?;

    let mut radio = Radio::connect_safe(transport)
        .await
        .map_err(|e| format!("Connect failed: {e}"))?;

    // Verify we're back in CAT mode.
    if radio.identify().await.is_ok() {
        println!("Radio restored to normal mode.");
        Ok(radio)
    } else {
        println!("Error: radio is still in Reflector Terminal Mode.");
        println!("Please set Menu 650 to Off and restart the REPL.");
        Err("Radio still in MMDVM mode".into())
    }
}

/// Dispatch a command in D-STAR mode.
///
/// Run continuous D-STAR monitoring with background keepalives and
/// voice relay. Polls both radio MMDVM and reflector UDP in a loop
/// until the user sends a SIGINT (Ctrl-C).
async fn run_dstar_monitor(session: &mut DStarSession) {
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
/// phase — typically 30-80 ms waiting for `poll()` to return `None`
/// at the end of a superframe — during which the 20 already-drained
/// voice frames sat in the Vec instead of going to the radio. The
/// MMDVM modem's small voice buffer underran during those gaps,
/// dropping the receive popup. Observed live on REF030 C with 85 ms
/// gaps between BT write bursts mid-stream; confirmed in the trace
/// log at `kenwood_thd75::transport::bluetooth::inner: BT write`
/// timestamps. With inline processing, each frame is handed off to
/// the paced `send_voice` immediately, so the modem sees a steady
/// 20 ms cadence with no >20 ms gaps inside a stream.
async fn dstar_poll_cycle(session: &mut DStarSession) {
    // Matches the legacy `ReflectorClient::poll` 100 ms inner recv
    // timeout — gives the reflector session task a short window to
    // deliver a frame before we yield control back to the outer
    // `select!` for radio polling and ctrl_c.
    const EVENT_POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);

    for _ in 0..MAX_EVENTS_PER_CYCLE {
        let Some(client) = session.reflector.as_mut() else {
            break;
        };
        let event = match tokio::time::timeout(EVENT_POLL_TIMEOUT, client.next_event()).await {
            Ok(Some(e)) => e,
            Ok(None) => {
                // Channel closed — session task exited. Drop the
                // dead handle so subsequent iterations short-circuit
                // via the `is_none()` check above.
                session.reflector = None;
                break;
            }
            Err(_) => break,
        };
        trace_reflector_event(&event);
        // Only print VoiceStart for new streams (avoid duplicate
        // announcements on superframe-boundary header refreshes
        // that the parser's stream tracker did not suppress).
        if let RuntimeEvent::VoiceStart { stream_id, .. } = &event {
            if session.rx_stream_id != Some(*stream_id) {
                print_reflector_event(&event);
            }
        } else {
            print_reflector_event(&event);
        }
        relay_reflector_to_radio(session, &event).await;
    }

    // Poll radio — drain MMDVM responses (ACK/NAK/status + PTT voice).
    // Short timeout so this doesn't block the reflector relay.
    if let Ok(Some(event)) = session.gateway.next_event().await {
        trace_dstar_event(&event);
        print_dstar_event(&event);
        relay_radio_to_reflector(session, &event).await;
    }

    // Drive echo playback state machine.
    echo_playback_tick(session).await;
}

/// `listen` polls both the radio MMDVM and reflector UDP, relaying
/// voice frames between them.
#[allow(clippy::too_many_lines)]
async fn dispatch_dstar(session: &mut DStarSession, cmd: &str, parts: &[&str]) {
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
            let link = match parse_link_arg(parts[1]) {
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
                    // Update the stored reflector callsign too — the
                    // relay path uses it to build the outbound rpt2
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
            if let Some(ref mut client) = session.reflector {
                match client.disconnect().await {
                    Ok(()) => {
                        println!("Disconnected from reflector.");
                        session.reflector = None;
                    }
                    Err(e) => println!(
                        "{}",
                        thd75_repl::output::error(format_args!("disconnecting: {e}"))
                    ),
                }
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
                        entry.callsign,
                        entry.destination,
                        commands::fmt_elapsed(entry.timestamp.elapsed())
                    );
                }
                println!("{} stations heard.", list.len());
            }
        }
        "status" => {
            match session.gateway.poll_status().await {
                Ok(status) => println!(
                    "Modem status: D-STAR buffer {}, transmit {}.",
                    status.dstar_buffer,
                    if status.tx { "active" } else { "idle" }
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
            session.echo_armed = true;
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
                println!("Usage: text <message up to 20 chars>");
                println!("  Sets text to embed in your next voice transmission.");
                println!("  text clear: Remove the outgoing text.");
                return;
            }
            if parts[1] == "clear" || parts[1] == "off" || parts[1] == "none" {
                session.tx_text = None;
                session.tx_slow_data.clear();
                session.tx_slow_data_idx = 0;
                println!("Outgoing text cleared.");
            } else {
                let text = parts[1..].join(" ");
                let truncated = if text.len() > 20 {
                    println!("Text truncated to 20 characters.");
                    text[..20].to_owned()
                } else {
                    text
                };
                session.tx_slow_data =
                    DStarGateway::<EitherTransport>::encode_text_message(&truncated);
                session.tx_slow_data_idx = 0;
                println!(
                    "Outgoing text set: \"{truncated}\". \
                     Will be embedded in your next transmission."
                );
                session.tx_text = Some(truncated);
            }
        }
        _ => println!(
            "D-STAR command not recognized: {cmd}. \
             Commands: monitor, link, unlink, echo, text, heard, status, dstar stop"
        ),
    }
}

/// Maximum echo recording length (60 seconds at 50 frames/sec).
const ECHO_MAX_FRAMES: usize = 60 * 50;

/// Transition echo from Recording to Waiting (ready for playback).
fn finish_echo_recording(session: &mut DStarSession) {
    let echo = std::mem::replace(&mut session.echo, EchoState::Idle);
    if let EchoState::Recording { header, frames } = echo {
        #[allow(clippy::cast_precision_loss)]
        let secs = frames.len() as f64 / 50.0;
        println!("Echo test: recorded {secs:.1} seconds of audio. Playing back.");
        session.echo = EchoState::Waiting {
            header,
            frames,
            since: std::time::Instant::now(),
        };
    }
}

/// Delay before echo playback starts (milliseconds).
///
/// Per ircDDBGateway `REPLY_TIME`, a short pause so the user hears
/// a clear break between their TX and the playback.
const ECHO_REPLY_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// Build the outbound reflector [`CoreDStarHeader`] from the
/// kenwood-thd75 header emitted by the radio.
///
/// The TH-D75 in Reflector Terminal Mode emits its TX header with
/// `rpt1` / `rpt2` both set to the literal string `"DIRECT  "` as
/// placeholders — the radio knows it's talking to a local gateway
/// but doesn't know the gateway's callsign. This function is the
/// gateway half of that contract: we rewrite those placeholders
/// into the real `rpt1` / `rpt2` that a `DPlus` / `DExtra` / `DCS`
/// reflector expects.
///
/// The D-STAR hotspot convention (per
/// `ircDDBGateway/Common/DPlusHandler.cpp:77-79` and `:865`, and
/// `Common/HeaderData.cpp::getDPlusData`) is:
///
/// - `rpt1[0..7]` = operator's own callsign (first 7 bytes,
///   space-padded)
/// - `rpt1[7]`   = operator's local module letter (A–E)
/// - `rpt2[0..7]` = reflector callsign (e.g. `"REF030 "`)
/// - `rpt2[7]`   = reflector module letter (A–E)
///
/// **Critical: both rpt1[7] and rpt2[7] are actual module letters,
/// NEVER the literal `'G'`.** xlxd's `cdplusprotocol.cpp:209` reads
/// rpt1's byte 7 as the module letter and silently drops the
/// packet if `IsValidModule` fails — `'G'` is not a valid module
/// so any packet we send with `rpt1 = "KQ4NIT G"` is discarded by
/// the reflector with no NAK, no log line, no retry. Pre-fix we
/// were building exactly that, which is why the operator's TX was
/// received by every peer's tokio socket but never echoed back by
/// the reflector to any other client.
fn build_reflector_header(
    station_callsign: Callsign,
    local_module: Module,
    reflector_callsign: Callsign,
    reflector_module: Module,
    header: &kenwood_thd75::DStarHeader,
) -> CoreDStarHeader {
    // rpt1 = operator's own callsign + local module letter.
    let mut rpt1_buf = [b' '; 8];
    let cs_bytes = station_callsign.as_bytes();
    rpt1_buf[..7].copy_from_slice(&cs_bytes[..7]);
    rpt1_buf[7] = local_module.as_byte();
    // Infallible: bytes came from a validated Callsign + Module, so
    // every byte is already ASCII and the 8-byte form is well-formed.
    let rpt1 = Callsign::from_wire_bytes(rpt1_buf);

    // rpt2 = reflector callsign + reflector module letter.
    let mut rpt2_buf = [b' '; 8];
    let refl_bytes = reflector_callsign.as_bytes();
    rpt2_buf[..7].copy_from_slice(&refl_bytes[..7]);
    rpt2_buf[7] = reflector_module.as_byte();
    let rpt2 = Callsign::from_wire_bytes(rpt2_buf);

    // Convert the kenwood-thd75 header's string fields into typed
    // wire-format callsigns. If the radio emits an unexpectedly
    // malformed field, fall back to a safe default rather than
    // dropping the relay altogether.
    let my_call = Callsign::try_from_str(header.my_call.trim()).unwrap_or(station_callsign);
    let my_suffix = Suffix::try_from_str(header.my_suffix.trim()).unwrap_or(Suffix::EMPTY);
    let ur_call = Callsign::try_from_str("CQCQCQ").expect("static constant");

    CoreDStarHeader {
        flag1: header.flag1,
        flag2: header.flag2,
        flag3: header.flag3,
        rpt2,
        rpt1,
        ur_call,
        my_call,
        my_suffix,
    }
}

/// Relay a radio MMDVM event to the reflector, or record for echo test.
///
/// If the URCALL field is `"       E"` (7 spaces + E), the
/// transmission is captured for local echo playback instead of
/// being relayed to any reflector. This matches the `ircDDBGateway`
/// echo test convention.
async fn relay_radio_to_reflector(session: &mut DStarSession, event: &DStarEvent) {
    // Echo test interception: either the `echo` command armed it, or
    // URCALL is "       E" (per ircDDBGateway convention).
    if let DStarEvent::VoiceStart(header) = event
        && (session.echo_armed || header.ur_call.trim() == "E")
    {
        session.echo_armed = false;
        println!(
            "Echo test: recording from {}. Transmit up to 60 seconds.",
            header.my_call.trim()
        );
        session.echo = EchoState::Recording {
            header: header.clone(),
            frames: Vec::with_capacity(256),
        };
        return;
    }

    // If we're currently recording for echo, buffer frames.
    if matches!(session.echo, EchoState::Recording { .. }) {
        match event {
            DStarEvent::VoiceData(frame) => {
                if let EchoState::Recording { frames, .. } = &mut session.echo
                    && frames.len() < ECHO_MAX_FRAMES
                {
                    frames.push(frame.clone());
                }
                return;
            }
            DStarEvent::VoiceEnd | DStarEvent::VoiceLost => {
                finish_echo_recording(session);
                return;
            }
            DStarEvent::VoiceStart(_) => {
                // New stream started before VoiceEnd — the previous
                // stream's end was lost. Finish what we have.
                finish_echo_recording(session);
                // Don't return — let this VoiceStart fall through to
                // normal processing below.
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
        DStarEvent::VoiceStart(header) => {
            // Generate a new stream ID for this transmission.
            let sid = rand_stream_id();
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
        DStarEvent::VoiceData(frame) => {
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
                let sd =
                    session.tx_slow_data[session.tx_slow_data_idx % session.tx_slow_data.len()];
                session.tx_slow_data_idx += 1;
                sd
            };
            let ref_frame = CoreVoiceFrame {
                ambe: frame.ambe,
                slow_data,
            };
            if let Err(e) = client.send_voice(sid, session.tx_seq, ref_frame).await {
                println!("Error: relay voice to reflector: {e}");
            }
            session.tx_seq = (session.tx_seq + 1) % 21;
        }
        DStarEvent::VoiceEnd => {
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
/// has elapsed, plays back all buffered frames to the radio with
/// proper 20ms pacing per AMBE frame.
async fn echo_playback_tick(session: &mut DStarSession) {
    // Check if we're in Waiting and the delay has elapsed.
    let should_play = matches!(
        &session.echo,
        EchoState::Waiting { since, .. } if since.elapsed() >= ECHO_REPLY_DELAY
    );

    if !should_play {
        return;
    }

    // Move to Playing state and extract the buffered data.
    let echo = std::mem::replace(&mut session.echo, EchoState::Playing);
    let EchoState::Waiting { header, frames, .. } = echo else {
        return;
    };

    // Build the echo playback header per ircDDBGateway EchoUnit.cpp:
    // MY = gateway callsign, MY suffix = "ECHO", YOUR = "CQCQCQ  ".
    let echo_header = kenwood_thd75::DStarHeader {
        flag1: header.flag1,
        flag2: header.flag2,
        flag3: header.flag3,
        rpt2: header.rpt2.clone(),
        rpt1: header.rpt1.clone(),
        ur_call: "CQCQCQ  ".to_owned(),
        my_call: format!("{:<8}", session.callsign.as_str()),
        my_suffix: "ECHO".to_owned(),
    };

    // Send header to radio.
    if let Err(e) = session.gateway.send_header(&echo_header).await {
        println!("Echo playback error: header: {e}");
        session.echo = EchoState::Idle;
        return;
    }

    // Play back each frame with 20ms pacing.
    for frame in &frames {
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
    session.echo = EchoState::Idle;
}

/// Run the poll loop for an echo test cycle.
///
/// Polls the radio MMDVM for the user's TX, records it, plays it
/// back, then returns automatically. Also handles Ctrl-C to cancel.
async fn run_echo_monitor(session: &mut DStarSession) {
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
                session.echo_armed = false;
                session.echo = EchoState::Idle;
                break;
            }
            () = dstar_poll_cycle(session) => {}
        }

        if !started && !matches!(session.echo, EchoState::Idle) {
            started = true;
        }

        // Exit once playback is complete (Idle after having recorded).
        if started && matches!(session.echo, EchoState::Idle) {
            break;
        }
    }

    session
        .gateway
        .set_event_timeout(std::time::Duration::from_millis(500));
}

/// Generate a random non-zero stream ID.
///
/// Uses system time entropy (not cryptographic — just needs to be
/// per-stream unique). The low bit is forced to 1 so the result is
/// always non-zero, making the `StreamId::new` unwrap trivial.
fn rand_stream_id() -> StreamId {
    use std::time::SystemTime;
    let t = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    #[allow(clippy::cast_possible_truncation)]
    let id = ((t.subsec_nanos() ^ (t.as_secs() as u32)) as u16) | 0x0001;
    StreamId::new(id).expect("low bit forced to 1 guarantees non-zero")
}

/// Relay a reflector event to the radio MMDVM modem.
async fn relay_reflector_to_radio(session: &mut DStarSession, event: &RuntimeEvent) {
    let gw = &mut session.gateway;
    match event {
        RuntimeEvent::VoiceStart { header, stream_id } => {
            // Deduplicate: only send header once per stream.
            if session.rx_stream_id == Some(*stream_id) {
                return;
            }
            session.rx_stream_id = Some(*stream_id);
            // Reset slow data decoder for the new stream so a partial
            // message from a previous transmission is not silently
            // reassembled onto this one. Also clear the last-printed
            // text tracker so the new stream's first complete text
            // always prints, even if it happens to match the previous
            // operator's message verbatim.
            session.rx_slow_data.reset();
            session.rx_last_slow_text = None;
            // Convert typed core header to kenwood-thd75 header.
            // Both sides are already ASCII-validated so
            // `String::from_utf8_lossy` is a cheap direct conversion.
            let radio_header = kenwood_thd75::DStarHeader {
                flag1: header.flag1,
                flag2: header.flag2,
                flag3: header.flag3,
                rpt2: String::from_utf8_lossy(header.rpt2.as_bytes()).into_owned(),
                rpt1: String::from_utf8_lossy(header.rpt1.as_bytes()).into_owned(),
                ur_call: String::from_utf8_lossy(header.ur_call.as_bytes()).into_owned(),
                my_call: String::from_utf8_lossy(header.my_call.as_bytes()).into_owned(),
                my_suffix: String::from_utf8_lossy(header.my_suffix.as_bytes()).into_owned(),
            };
            if let Err(e) = gw.send_header(&radio_header).await {
                println!(
                    "{}",
                    thd75_repl::output::error(format_args!("relaying header to radio: {e}"))
                );
            }
        }
        RuntimeEvent::VoiceFrame { frame, seq, .. } => {
            // Feed the raw seq byte from the DSVT header; the decoder
            // treats seq==0 as a sync frame and re-aligns its half-block
            // phase automatically. No external skipping needed.
            session.rx_slow_data.add_frame(&frame.slow_data, *seq);
            // D-STAR radios repeat the 20-char text message across
            // the voice stream continuously (~320 ms per full cycle)
            // so late joiners can see it. The decoder correctly
            // re-assembles on each cycle, which would print the same
            // line 5-10 times per burst. Dedupe against the last
            // printed text for this stream so we only announce the
            // message when it changes (either fresh stream or the
            // operator changed text mid-transmission).
            if let Some(bytes) = session.rx_slow_data.take_message()
                && session.rx_last_slow_text.as_ref() != Some(&bytes)
            {
                print_slow_data_text_message(&bytes);
                session.rx_last_slow_text = Some(bytes);
            }

            let radio_frame = kenwood_thd75::DStarVoiceFrame {
                ambe: frame.ambe,
                slow_data: frame.slow_data,
            };
            // Use send_voice_unpaced — no host-side pacing. The
            // correct pattern per `ref/MMDVMHost/Modem.cpp:1049` is
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
            // (~47.7 fps — there's an extra header packet slot
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
            // Proper fix: implement MMDVMHost-style periodic
            // status polling and gate writes on
            // `ModemStatus::dstar_buffer`. Tracked as future work.
            if let Err(e) = gw.send_voice_unpaced(&radio_frame).await {
                println!(
                    "{}",
                    thd75_repl::output::error(format_args!("relaying voice to radio: {e}"))
                );
            }
        }
        RuntimeEvent::VoiceEnd { .. } => {
            // Drain any message that became complete on the very last
            // voice frame; any partial message mid-assembly is silently
            // discarded by the reset below. Apply the same dedupe
            // against `rx_last_slow_text` so we don't double-print
            // the message on VoiceEnd if it already printed during
            // VoiceFrame processing.
            if let Some(bytes) = session.rx_slow_data.take_message()
                && session.rx_last_slow_text.as_ref() != Some(&bytes)
            {
                print_slow_data_text_message(&bytes);
            }
            session.rx_slow_data.reset();
            session.rx_last_slow_text = None;
            session.rx_stream_id = None;
            if let Err(e) = gw.send_eot().await {
                println!(
                    "{}",
                    thd75_repl::output::error(format_args!(
                        "relaying end of transmission to radio: {e}"
                    ))
                );
            }
        }
        RuntimeEvent::Connected | RuntimeEvent::Disconnected(_) | RuntimeEvent::PollEcho => {}
    }
}

/// Announce a complete D-STAR slow data text message from a reflector stream.
///
/// `bytes` is the fixed 20-byte buffer returned by
/// [`SlowDataDecoder::take_message`]. Trailing spaces and non-printable
/// characters are stripped before display so screen readers don't
/// announce padding.
fn print_slow_data_text_message(bytes: &[u8; 20]) {
    // Lossy UTF-8 is fine: the decoder already masks to 7-bit ASCII via
    // `& 0x7F`, but a rogue stream could still carry 0x00..0x1F control
    // bytes that we don't want to feed the terminal.
    let raw = String::from_utf8_lossy(bytes);
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_ascii_control() { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    if !trimmed.is_empty() {
        aprintln!("{}", thd75_repl::output::dstar_text_message(trimmed));
    }
}

/// Emit a TRACE-level log entry for every reflector event consumed
/// from the poll loop. Logs the variant discriminant plus any
/// stream ID, source callsign, or sequence number that accompanied
/// the event. Keyed off `target = "thd75_repl::reflector"` so users
/// can filter just this firehose via `RUST_LOG` if they want.
fn trace_reflector_event(event: &RuntimeEvent) {
    match event {
        RuntimeEvent::Connected => {
            tracing::trace!(target: "thd75_repl::reflector", "event: Connected");
        }
        RuntimeEvent::Disconnected(reason) => {
            tracing::trace!(
                target: "thd75_repl::reflector",
                reason = ?reason,
                "event: Disconnected"
            );
        }
        RuntimeEvent::PollEcho => {
            tracing::trace!(target: "thd75_repl::reflector", "event: PollEcho");
        }
        RuntimeEvent::VoiceStart { header, stream_id } => {
            tracing::trace!(
                target: "thd75_repl::reflector",
                stream_id = %stream_id,
                my_call = %header.my_call.as_str(),
                my_suffix = %header.my_suffix.as_str(),
                ur_call = %header.ur_call.as_str(),
                rpt1 = %header.rpt1.as_str(),
                rpt2 = %header.rpt2.as_str(),
                flag1 = format_args!("{:#04x}", header.flag1),
                flag2 = format_args!("{:#04x}", header.flag2),
                flag3 = format_args!("{:#04x}", header.flag3),
                "event: VoiceStart"
            );
        }
        RuntimeEvent::VoiceFrame { stream_id, seq, .. } => {
            tracing::trace!(
                target: "thd75_repl::reflector",
                stream_id = %stream_id,
                seq = *seq,
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
    }
}

/// Print a reflector event for the user.
fn print_reflector_event(event: &RuntimeEvent) {
    match event {
        RuntimeEvent::Connected => {
            aprintln!("{}", thd75_repl::output::reflector_event_connected());
        }
        RuntimeEvent::Disconnected(reason) => match reason {
            DisconnectReason::Rejected => {
                aprintln!("{}", thd75_repl::output::reflector_event_rejected());
            }
            _ => {
                aprintln!("{}", thd75_repl::output::reflector_event_disconnected());
            }
        },
        RuntimeEvent::PollEcho | RuntimeEvent::VoiceFrame { .. } => {
            // Silent: keepalives and individual voice frames are too
            // frequent to announce.
        }
        RuntimeEvent::VoiceStart { header, .. } => {
            aprintln!(
                "{}",
                thd75_repl::output::reflector_event_voice_start(
                    header.my_call.as_str().as_ref(),
                    header.my_suffix.as_str().as_ref(),
                    header.ur_call.as_str().as_ref(),
                )
            );
        }
        RuntimeEvent::VoiceEnd { .. } => {
            aprintln!("{}", thd75_repl::output::reflector_event_voice_end());
        }
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
fn trace_dstar_event(event: &DStarEvent) {
    match event {
        DStarEvent::VoiceStart(header) => {
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
        DStarEvent::VoiceData(_) => {
            // Per-frame trace is too noisy at 20 fps; skip.
        }
        DStarEvent::VoiceEnd => {
            tracing::trace!(target: "thd75_repl::dstar", "event: VoiceEnd");
        }
        DStarEvent::VoiceLost => {
            tracing::trace!(target: "thd75_repl::dstar", "event: VoiceLost");
        }
        DStarEvent::TextMessage(text) => {
            tracing::trace!(
                target: "thd75_repl::dstar",
                text = %text,
                "event: TextMessage"
            );
        }
        DStarEvent::GpsData(data) => {
            tracing::trace!(
                target: "thd75_repl::dstar",
                bytes_len = data.len(),
                "event: GpsData"
            );
        }
        DStarEvent::StationHeard(entry) => {
            tracing::trace!(
                target: "thd75_repl::dstar",
                callsign = %entry.callsign,
                "event: StationHeard"
            );
        }
        DStarEvent::UrCallCommand(action) => {
            tracing::trace!(
                target: "thd75_repl::dstar",
                action = ?action,
                "event: UrCallCommand"
            );
        }
        DStarEvent::StatusUpdate(status) => {
            tracing::trace!(
                target: "thd75_repl::dstar",
                status = ?status,
                "event: StatusUpdate"
            );
        }
    }
}

/// Format and print a D-STAR event as screen-reader-friendly text.
fn print_dstar_event(event: &DStarEvent) {
    match event {
        DStarEvent::VoiceStart(header) => {
            aprintln!(
                "{}",
                thd75_repl::output::dstar_voice_start(
                    &header.my_call,
                    &header.my_suffix,
                    &header.ur_call,
                )
            );
        }
        DStarEvent::VoiceData(_) => {
            // Don't announce every 20ms frame — too noisy for screen readers.
        }
        DStarEvent::VoiceEnd => {
            aprintln!("{}", thd75_repl::output::dstar_voice_end());
        }
        DStarEvent::VoiceLost => {
            if !thd75_repl::is_verbose() {
                return;
            }
            aprintln!("{}", thd75_repl::output::dstar_voice_lost());
        }
        DStarEvent::TextMessage(text) => {
            aprintln!("{}", thd75_repl::output::dstar_text_message(text));
        }
        DStarEvent::GpsData(data) => {
            // GPS/DPRS data is raw NMEA-like bytes. Show as text if valid ASCII.
            let text = String::from_utf8_lossy(data);
            aprintln!("{}", thd75_repl::output::dstar_gps(text.trim()));
        }
        DStarEvent::StationHeard(entry) => {
            aprintln!(
                "{}",
                thd75_repl::output::dstar_station_heard(&entry.callsign)
            );
        }
        DStarEvent::UrCallCommand(action) => {
            use kenwood_thd75::types::dstar::UrCallAction;
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
        DStarEvent::StatusUpdate(status) => {
            println!(
                "{}",
                thd75_repl::output::dstar_modem_status(status.dstar_buffer, status.tx)
            );
        }
    }
}

#[cfg(test)]
mod offset_tests {
    use super::parse_utc_offset;

    #[test]
    fn parses_plus_hhmm_colon() {
        assert_eq!(parse_utc_offset("+05:30").unwrap(), 5 * 3600 + 30 * 60);
    }

    #[test]
    fn parses_minus_hhmm_colon() {
        assert_eq!(parse_utc_offset("-08:00").unwrap(), -8 * 3600);
    }

    #[test]
    fn parses_plus_hhmm_no_colon() {
        assert_eq!(parse_utc_offset("+0530").unwrap(), 5 * 3600 + 30 * 60);
    }

    #[test]
    fn parses_plus_h() {
        assert_eq!(parse_utc_offset("+5").unwrap(), 5 * 3600);
    }

    #[test]
    fn parses_no_sign_positive() {
        assert_eq!(parse_utc_offset("03:00").unwrap(), 3 * 3600);
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
    fn parses_zero_offset() {
        assert_eq!(parse_utc_offset("+00:00").unwrap(), 0);
        assert_eq!(parse_utc_offset("-00:00").unwrap(), 0);
    }
}
