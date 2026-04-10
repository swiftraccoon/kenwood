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

use clap::Parser;
use kenwood_thd75::Radio;
use kenwood_thd75::transport::EitherTransport;
use kenwood_thd75::{AprsClient, AprsClientConfig, AprsEvent};
use kenwood_thd75::{DStarEvent, DStarGateway, DStarGatewayConfig};

use dstar_gateway::ReflectorClient;
use dstar_gateway::protocol::{ConnectionState, ReflectorEvent};

/// Accessible REPL for the Kenwood TH-D75 transceiver.
///
/// Screen-reader friendly: plain text output, one line at a time.
/// Also scriptable: pipe commands via stdin.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// Serial port path (default: auto-discover USB, then Bluetooth).
    #[arg(short, long)]
    port: Option<String>,

    /// Baud rate for serial connection.
    #[arg(short, long, default_value_t = 115_200)]
    baud: u32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .init();
    }

    println!(
        "Kenwood TH-D75 accessible radio control, version {}.",
        env!("CARGO_PKG_VERSION")
    );

    // Create the tokio runtime first — SerialTransport::open requires
    // a reactor (tokio-serial registers the fd with mio).
    let rt = tokio::runtime::Runtime::new()?;

    // Open transport on the main thread inside the runtime context.
    // IOBluetooth needs the main CFRunLoop, but block_on runs on main
    // so this is safe for both BT and serial paths.
    let (path, transport) =
        rt.block_on(async { transport::discover_and_open(cli.port.as_deref(), cli.baud) })?;
    println!("Connected via {path}.");

    // Run the async REPL.
    rt.block_on(run_repl(transport, cli.port.clone(), cli.baud))?;

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
    /// Reflector-side UDP client (`DExtra` for now).
    reflector: Option<ReflectorClient>,
    /// Station callsign.
    callsign: String,
    /// TX stream ID for radio-to-reflector relay (0 = not transmitting).
    tx_stream_id: u16,
    /// TX sequence counter (0-20 cycle).
    tx_seq: u8,
    /// Reflector module letter.
    reflector_module: char,
    /// Current RX stream ID from reflector (0 = no active stream).
    rx_stream_id: u16,
}

impl std::fmt::Debug for DStarSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DStarSession")
            .field("callsign", &self.callsign)
            .field(
                "reflector_connected",
                &self.reflector.as_ref().map(ReflectorClient::state),
            )
            .finish_non_exhaustive()
    }
}

/// Main REPL loop. Manages three states: CAT (normal radio control),
/// APRS (packet radio), and D-STAR (digital voice gateway). Each state
/// owns the radio transport exclusively.
#[allow(clippy::cognitive_complexity, clippy::too_many_lines)]
async fn run_repl(
    transport: EitherTransport,
    cli_port: Option<String>,
    cli_baud: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    // Try connect_safe (sends TNC exit preamble to recover from stuck modes).
    let mut radio = Radio::connect_safe(transport).await?;

    // Try to identify. If it fails, the radio may be in MMDVM/TERM mode.
    let mut state = match radio.identify().await {
        Ok(info) => {
            let fw = radio.get_firmware_version().await.unwrap_or_default();
            println!("Radio model: {}. Firmware version: {fw}.", info.model);
            println!("Type help for a list of commands, or quit to exit.");
            ReplState::Cat(radio)
        }
        Err(_) => {
            // CAT failed. Check if radio is in MMDVM/gateway mode.
            if detect_mmdvm_mode(&mut radio).await {
                println!("Radio is in D-STAR Reflector Terminal Mode.");
                println!("Type dstar start <callsign> to begin, or quit to exit.");
                ReplState::Cat(radio)
            } else {
                return Err(
                    "Error: could not identify radio. Check connection and radio mode.".into(),
                );
            }
        }
    };

    let mut rl = rustyline::DefaultEditor::new()?;

    loop {
        let prompt = match &state {
            ReplState::Cat(_) => "d75> ",
            ReplState::Aprs(_) => "aprs> ",
            ReplState::Dstar(_) => "dstar> ",
        };

        let Some(line) = read_line_blocking(&mut rl, prompt) else {
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
            println!("Goodbye.");
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
                match &state {
                    ReplState::Cat(_) => commands::help(),
                    ReplState::Aprs(_) => commands::aprs_help(),
                    ReplState::Dstar(_) => commands::dstar_help(),
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
                                println!("Error restoring radio mode: {e}");
                                None
                            }
                        }
                    }
                    ReplState::Cat(r) => Some(r),
                };
                if let Some(r) = radio {
                    let _ = r.disconnect().await;
                }
                println!("Goodbye.");
                return Ok(());
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
                                println!("Error entering APRS mode: {e}");
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
                            Ok(session) => ReplState::Dstar(Box::new(session)),
                            Err((Some(radio_back), e)) => {
                                println!("Error entering D-STAR mode: {e}");
                                ReplState::Cat(radio_back)
                            }
                            Err((None, e)) => {
                                println!("Error entering D-STAR mode: {e}");
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
                            println!("Error stopping APRS: {e}");
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
                            println!("Error exiting D-STAR mode: {e}");
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
                println!("Usage: dstar start <callsign>");
                println!("  Enters D-STAR gateway mode. Type dstar stop to exit.");
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
            println!("APRS mode active. Type aprs stop to exit.");
            println!("Commands: listen, msg, beacon, aprs stop");
            Ok(client)
        }
        Err((radio, e)) => Err((radio, format!("{e}"))),
    }
}

/// Dispatch a command in APRS mode (listen, msg, beacon).
async fn dispatch_aprs(client: &mut AprsClient<EitherTransport>, cmd: &str, parts: &[&str]) {
    match cmd {
        "listen" | "poll" => match client.next_event().await {
            Ok(Some(event)) => print_aprs_event(&event),
            Ok(None) => println!("No APRS activity."),
            Err(e) => println!("Error: {e}"),
        },
        "msg" | "message" => {
            if parts.len() < 3 {
                println!("Usage: msg <callsign> <message text>");
                return;
            }
            let addressee = parts[1];
            let text = parts[2..].join(" ");
            match client.send_message(addressee, &text).await {
                Ok(msg_id) => println!("Message queued to {addressee}: {text} (ID: {msg_id})"),
                Err(e) => println!("Error sending message: {e}"),
            }
        }
        "beacon" => match client.send_status("REPL beacon").await {
            Ok(()) => println!("Status beacon sent."),
            Err(e) => println!("Error sending beacon: {e}"),
        },
        _ => println!(
            "APRS command not recognized: {cmd}. \
             Commands: listen, msg, beacon, aprs stop"
        ),
    }
}

/// Format and print an APRS event as screen-reader-friendly text.
fn print_aprs_event(event: &AprsEvent) {
    match event {
        AprsEvent::StationHeard(entry) => {
            println!("APRS station heard: {}", entry.callsign);
        }
        AprsEvent::MessageReceived(msg) => {
            println!("APRS message received for {}: {}", msg.addressee, msg.text);
        }
        AprsEvent::MessageDelivered(id) => {
            println!("APRS message delivered, ID {id}.");
        }
        AprsEvent::MessageRejected(id) => {
            println!("APRS message rejected by remote station, ID {id}.");
        }
        AprsEvent::MessageExpired(id) => {
            println!("APRS message expired after all retries, ID {id}.");
        }
        AprsEvent::PositionReceived { source, position } => {
            println!(
                "APRS position from {source}: latitude {:.4}, longitude {:.4}.",
                position.latitude, position.longitude
            );
        }
        AprsEvent::WeatherReceived { source, .. } => {
            println!("APRS weather report from {source}.");
        }
        AprsEvent::PacketDigipeated { source } => {
            println!("APRS packet relayed from {source}.");
        }
        AprsEvent::RawPacket(pkt) => {
            println!("APRS packet from {}.", pkt.source);
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

    // Connect to reflector if specified.
    let reflector = if let Some(ref_str) = reflector_arg {
        match connect_reflector(callsign, ref_str).await {
            Ok(client) => Some(client),
            Err(e) => {
                println!("Error: could not connect to reflector: {e}");
                println!("Gateway active without reflector. Use link command to connect later.");
                None
            }
        }
    } else {
        println!("No reflector specified. Use link command to connect.");
        None
    };

    println!("D-STAR gateway active. Type dstar stop to exit.");
    println!("Commands: listen, link, unlink, heard, status, dstar stop");
    let module = reflector_arg
        .and_then(parse_reflector_arg)
        .map_or('C', |(_, m)| m);
    Ok(DStarSession {
        gateway,
        reflector,
        callsign: callsign.to_owned(),
        tx_stream_id: 0,
        tx_seq: 0,
        reflector_module: module,
        rx_stream_id: 0,
    })
}

/// Parse a reflector string like "XRF030C" into (name, module).
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
fn load_host_files() -> dstar_gateway::HostFile {
    let mut hosts = dstar_gateway::HostFile::new();
    let dir = host_files_dir();

    for (name, _, port) in HOST_FILES {
        let path = dir.join(name);
        if let Ok(content) = std::fs::read_to_string(&path) {
            hosts.parse(&content, *port);
        }
    }
    hosts
}

/// Connect to a reflector by name+module string (e.g. "XRF030C").
async fn connect_reflector(callsign: &str, ref_str: &str) -> Result<ReflectorClient, String> {
    let (ref_name, module) = parse_reflector_arg(ref_str)
        .ok_or_else(|| format!("Invalid reflector format: {ref_str}. Expected e.g. XRF030C"))?;

    let prefix = ref_name.get(..3).unwrap_or("");
    if prefix != "XRF" && prefix != "XLX" && prefix != "REF" {
        return Err(format!(
            "Unsupported reflector prefix: {ref_name}. \
             Supported: REF (DPlus), XRF/XLX (DExtra)."
        ));
    }

    // Ensure host files exist, downloading if needed.
    ensure_host_files().await;
    let hosts = load_host_files();
    let entry = hosts.lookup(&ref_name).ok_or_else(|| {
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

    println!("Connecting to {ref_name} module {module} at {addr}.");
    let mut client = ReflectorClient::new(callsign, module, addr, prefix)
        .await
        .map_err(|e| format!("UDP socket error: {e}"))?;

    // DPlus (REF) requires TCP auth before UDP connect.
    if prefix == "REF" {
        println!("Authenticating with D-STAR gateway server.");
        match client.authenticate().await {
            Ok(()) => println!("Authentication successful."),
            Err(e) => {
                println!(
                    "Warning: authentication failed: {e}. \
                     Trying to connect anyway (previous auth may still be valid)."
                );
            }
        }
    }

    client
        .connect()
        .await
        .map_err(|e| format!("Connect send failed: {e}"))?;

    // Wait for connect ACK.
    println!("Waiting for reflector acknowledgement.");
    for _ in 0..50 {
        // 50 * 100ms = 5 seconds max wait
        match client.poll().await {
            Ok(Some(ReflectorEvent::Connected)) => {
                println!("Connected to {ref_name} module {module}.");
                return Ok(client);
            }
            Ok(Some(ReflectorEvent::VoiceStart { .. })) => {
                // REF reflectors start sending voice immediately after
                // link — receiving any DSVT packet confirms the link.
                println!("Connected to {ref_name} module {module}.");
                return Ok(client);
            }
            Ok(Some(ReflectorEvent::Rejected)) => {
                return Err(format!("Reflector {ref_name} rejected the connection."));
            }
            Ok(_) => {} // keep waiting
            Err(e) => return Err(format!("UDP error: {e}")),
        }
    }

    Err(format!(
        "Timeout waiting for {ref_name} to acknowledge connection."
    ))
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
    // Stop the gateway (exits MMDVM session).
    println!("Stopping D-STAR gateway.");
    let radio = gw
        .stop()
        .await
        .map_err(|e| format!("Gateway stop failed: {e}"))?;
    drop(radio);

    // Reconnect — radio is still in MMDVM mode (setting persists).
    // We need CAT mode to do MCP write, but we're in MMDVM.
    // Disconnect and reconnect with connect_safe which sends TNC exit preamble.
    println!("Reconnecting to restore normal radio mode.");
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let (_path, transport) = transport::discover_and_open(cli_port, cli_baud)
        .map_err(|e| format!("Reconnect failed: {e}"))?;

    let mut radio = Radio::connect_safe(transport)
        .await
        .map_err(|e| format!("Connect failed: {e}"))?;

    // Try MCP write to disable gateway mode.
    println!("Disabling Reflector Terminal Mode via memory write.");
    if let Err(e) = radio
        .modify_memory_page(GATEWAY_MODE_PAGE, |data| {
            data[GATEWAY_MODE_BYTE] = 0; // Off
        })
        .await
    {
        // If MCP write fails, we're still in MMDVM mode.
        println!("Error: could not disable gateway mode: {e}");
        println!("Please set Menu 650 to Off on the radio manually.");
        // Reconnect one more time after manual change.
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let (_path, transport) = transport::discover_and_open(cli_port, cli_baud)
            .map_err(|e| format!("Reconnect failed: {e}"))?;
        let radio = Radio::connect_safe(transport)
            .await
            .map_err(|e| format!("Connect failed: {e}"))?;
        return Ok(radio);
    }

    // MCP write succeeded — connection dropped. Reconnect in CAT mode.
    println!("Reconnecting in normal radio mode.");
    drop(radio);
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let (_path, transport) = transport::discover_and_open(cli_port, cli_baud)
        .map_err(|e| format!("Reconnect failed: {e}"))?;

    Radio::connect_safe(transport)
        .await
        .map_err(|e| format!("Connect failed: {e}"))
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
                println!("Monitor stopped.");
                // Restore default timeout.
                session.gateway.set_event_timeout(std::time::Duration::from_millis(500));
                break;
            }
            () = dstar_poll_cycle(session) => {}
        }
    }
}

/// One cycle of the D-STAR poll loop: poll reflector (keepalive + rx),
/// poll radio (mmdvm events), relay between them.
async fn dstar_poll_cycle(session: &mut DStarSession) {
    // Poll reflector — collect events then relay (avoids borrow conflict).
    let mut reflector_events = Vec::new();
    if let Some(ref mut client) = session.reflector {
        loop {
            match client.poll().await {
                Ok(Some(event)) => reflector_events.push(event),
                Ok(None) => break,
                Err(e) => {
                    println!("Error: reflector UDP: {e}");
                    break;
                }
            }
        }
    }
    for event in &reflector_events {
        // Only print VoiceStart for new streams (avoid duplicate announcements).
        if let ReflectorEvent::VoiceStart { stream_id, .. } = event {
            if *stream_id != session.rx_stream_id {
                print_reflector_event(event);
            }
        } else {
            print_reflector_event(event);
        }
        relay_reflector_to_radio(session, event).await;
        // Pace frames to radio at ~20ms like d75link's processTimers.
        // Without pacing, the entire UDP burst dumps into the MMDVM
        // buffer at wire speed, overflowing the 127-slot buffer.
        if matches!(event, ReflectorEvent::VoiceData { .. }) {
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        }
    }

    // Poll radio — drain MMDVM responses (ACK/NAK/status + PTT voice).
    // Short timeout so this doesn't block the reflector relay.
    if let Ok(Some(event)) = session.gateway.next_event().await {
        print_dstar_event(&event);
        relay_radio_to_reflector(session, &event).await;
    }
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
                return;
            }
            match connect_reflector(&session.callsign, parts[1]).await {
                Ok(client) => {
                    if let Some((_, m)) = parse_reflector_arg(parts[1]) {
                        session.reflector_module = m;
                    }
                    session.reflector = Some(client);
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
                    Err(e) => println!("Error disconnecting: {e}"),
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
            if let Some(ref client) = session.reflector {
                println!(
                    "Reflector connection: {}.",
                    match client.state() {
                        ConnectionState::Disconnected => "disconnected",
                        ConnectionState::Connecting => "connecting",
                        ConnectionState::Connected => "connected",
                        ConnectionState::Disconnecting => "disconnecting",
                    }
                );
            } else {
                println!("Reflector: not connected.");
            }
        }
        _ => println!(
            "D-STAR command not recognized: {cmd}. \
             Commands: listen, link, unlink, heard, status, dstar stop"
        ),
    }
}

/// Relay a radio MMDVM event to the reflector.
async fn relay_radio_to_reflector(session: &mut DStarSession, event: &DStarEvent) {
    let Some(ref client) = session.reflector else {
        return;
    };

    match event {
        DStarEvent::VoiceStart(header) => {
            // Generate a new stream ID for this transmission.
            session.tx_stream_id = rand_stream_id();
            session.tx_seq = 0;

            // Build the reflector header with proper routing.
            let cs = dstar_gateway::DStarHeader::pad_callsign(&session.callsign);
            let mut rpt2 = [b' '; 8];
            rpt2[..cs.len().min(7)].copy_from_slice(&cs[..cs.len().min(7)]);
            rpt2[7] = session.reflector_module as u8;
            let mut rpt1 = rpt2;
            rpt1[7] = b'G';

            let ref_header = dstar_gateway::DStarHeader {
                flag1: header.flag1,
                flag2: header.flag2,
                flag3: header.flag3,
                rpt2,
                rpt1,
                ur_call: *b"CQCQCQ  ",
                my_call: dstar_gateway::DStarHeader::pad_callsign(header.my_call.trim()),
                my_suffix: dstar_gateway::DStarHeader::pad_suffix(header.my_suffix.trim()),
            };

            if let Err(e) = client.send_header(&ref_header, session.tx_stream_id).await {
                println!("Error: relay header to reflector: {e}");
            }
        }
        DStarEvent::VoiceData(frame) => {
            if session.tx_stream_id == 0 {
                return;
            }
            let ref_frame = dstar_gateway::VoiceFrame {
                ambe: frame.ambe,
                slow_data: frame.slow_data,
            };
            if let Err(e) = client
                .send_voice(session.tx_stream_id, session.tx_seq, &ref_frame)
                .await
            {
                println!("Error: relay voice to reflector: {e}");
            }
            session.tx_seq = (session.tx_seq + 1) % 21;
        }
        DStarEvent::VoiceEnd => {
            if session.tx_stream_id == 0 {
                return;
            }
            if let Err(e) = client.send_eot(session.tx_stream_id, session.tx_seq).await {
                println!("Error: relay EOT to reflector: {e}");
            }
            session.tx_stream_id = 0;
            session.tx_seq = 0;
        }
        _ => {}
    }
}

/// Generate a random 16-bit stream ID.
fn rand_stream_id() -> u16 {
    use std::time::SystemTime;
    let t = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    // Mix time nanos into a u16. Not cryptographic, just needs to be unique per stream.
    #[allow(clippy::cast_possible_truncation)]
    let id = (t.subsec_nanos() ^ (t.as_secs() as u32)) as u16;
    if id == 0 { 1 } else { id }
}

/// Relay a reflector event to the radio MMDVM modem.
async fn relay_reflector_to_radio(session: &mut DStarSession, event: &ReflectorEvent) {
    let gw = &mut session.gateway;
    match event {
        ReflectorEvent::VoiceStart { header, stream_id } => {
            // Deduplicate: only send header once per stream.
            if *stream_id == session.rx_stream_id {
                return;
            }
            session.rx_stream_id = *stream_id;
            // Convert dstar-gateway header to kenwood-thd75 header.
            let radio_header = kenwood_thd75::DStarHeader {
                flag1: header.flag1,
                flag2: header.flag2,
                flag3: header.flag3,
                rpt2: String::from_utf8_lossy(&header.rpt2).to_string(),
                rpt1: String::from_utf8_lossy(&header.rpt1).to_string(),
                ur_call: String::from_utf8_lossy(&header.ur_call).to_string(),
                my_call: String::from_utf8_lossy(&header.my_call).to_string(),
                my_suffix: String::from_utf8_lossy(&header.my_suffix).to_string(),
            };
            if let Err(e) = gw.send_header(&radio_header).await {
                println!("Error relaying header to radio: {e}");
            }
        }
        ReflectorEvent::VoiceData { frame, .. } => {
            let radio_frame = kenwood_thd75::DStarVoiceFrame {
                ambe: frame.ambe,
                slow_data: frame.slow_data,
            };
            // Use unpaced send — the radio's MMDVM buffer handles
            // pacing internally. Paced send would block the reflector
            // poll loop for 20ms per frame, starving the UDP receive.
            if let Err(e) = gw.send_voice_unpaced(&radio_frame).await {
                println!("Error relaying voice to radio: {e}");
            }
        }
        ReflectorEvent::VoiceEnd { .. } => {
            session.rx_stream_id = 0;
            if let Err(e) = gw.send_eot().await {
                println!("Error relaying end of transmission to radio: {e}");
            }
        }
        _ => {}
    }
}

/// Print a reflector event for the user.
fn print_reflector_event(event: &ReflectorEvent) {
    match event {
        ReflectorEvent::Connected => println!("Reflector: connected."),
        ReflectorEvent::Rejected => println!("Reflector: connection rejected."),
        ReflectorEvent::Disconnected => println!("Reflector: disconnected."),
        ReflectorEvent::PollEcho | ReflectorEvent::VoiceData { .. } => {
            // Silent: keepalives and individual voice frames are too
            // frequent to announce.
        }
        ReflectorEvent::VoiceStart { header, .. } => {
            println!(
                "Reflector: voice from {} to {}.",
                header.my_call_str(),
                header.ur_call_str()
            );
        }
        ReflectorEvent::VoiceEnd { .. } => {
            println!("Reflector: voice transmission ended.");
        }
    }
}

/// Format and print a D-STAR event as screen-reader-friendly text.
fn print_dstar_event(event: &DStarEvent) {
    match event {
        DStarEvent::VoiceStart(header) => {
            println!(
                "D-STAR voice transmission from {} to {}.",
                header.my_call.trim(),
                header.ur_call.trim()
            );
        }
        DStarEvent::VoiceData(_) => {
            // Don't announce every 20ms frame — too noisy for screen readers.
        }
        DStarEvent::VoiceEnd => {
            println!("D-STAR voice transmission ended.");
        }
        DStarEvent::VoiceLost => {
            println!("D-STAR voice signal lost, no clean end of transmission.");
        }
        DStarEvent::TextMessage(text) => {
            println!("D-STAR text message received: {text}");
        }
        DStarEvent::GpsData(_) => {
            println!("D-STAR GPS position data received.");
        }
        DStarEvent::StationHeard(entry) => {
            println!("D-STAR station heard: {}.", entry.callsign);
        }
        DStarEvent::UrCallCommand(action) => {
            use kenwood_thd75::types::dstar::UrCallAction;
            match action {
                UrCallAction::Cq => println!("D-STAR command: call CQ."),
                UrCallAction::Echo => println!("D-STAR command: echo test."),
                UrCallAction::Unlink => println!("D-STAR command: unlink reflector."),
                UrCallAction::Info => println!("D-STAR command: request info."),
                UrCallAction::Link { reflector, module } => {
                    println!("D-STAR command: link to {reflector} module {module}.");
                }
                UrCallAction::Callsign(call) => {
                    println!("D-STAR command: route to callsign {call}.");
                }
            }
        }
        DStarEvent::StatusUpdate(status) => {
            println!(
                "D-STAR modem: buffer {}, transmit {}.",
                status.dstar_buffer,
                if status.tx { "active" } else { "idle" }
            );
        }
    }
}
