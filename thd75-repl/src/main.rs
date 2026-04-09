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

use clap::Parser;
use kenwood_thd75::Radio;
use kenwood_thd75::transport::EitherTransport;
use kenwood_thd75::{AprsClient, AprsClientConfig, AprsEvent};
use kenwood_thd75::{DStarEvent, DStarGateway, DStarGatewayConfig};

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

    // Open transport on main thread (IOBluetooth needs main CFRunLoop).
    let (path, transport) = transport::discover_and_open(cli.port.as_deref(), cli.baud)?;
    println!(
        "Kenwood TH-D75 accessible radio control, version {}.",
        env!("CARGO_PKG_VERSION")
    );
    println!("Connected via {path}.");

    // Run the async REPL.
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_repl(transport))?;

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
    Aprs(AprsClient<EitherTransport>),
    /// D-STAR gateway/MMDVM mode — radio consumed by `DStarGateway`.
    Dstar(DStarGateway<EitherTransport>),
}

/// Main REPL loop. Manages three states: CAT (normal radio control),
/// APRS (packet radio), and D-STAR (digital voice gateway). Each state
/// owns the radio transport exclusively.
#[allow(clippy::cognitive_complexity, clippy::too_many_lines)]
async fn run_repl(transport: EitherTransport) -> Result<(), Box<dyn std::error::Error>> {
    let mut radio = Radio::connect(transport).await?;

    // Identify the radio.
    let info = radio.identify().await?;
    let fw = radio.get_firmware_version().await.unwrap_or_default();
    println!("Radio model: {}. Firmware version: {fw}.", info.model);
    println!("Type help for a list of commands, or quit to exit.");

    let mut rl = rustyline::DefaultEditor::new()?;
    let mut state = ReplState::Cat(radio);

    loop {
        let prompt = match &state {
            ReplState::Cat(_) => "d75> ",
            ReplState::Aprs(_) => "aprs> ",
            ReplState::Dstar(_) => "dstar> ",
        };

        let Some(line) = read_line_blocking(&mut rl, prompt) else {
            println!("Goodbye.");
            break;
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
                // Exit any active mode first.
                match state {
                    ReplState::Aprs(client) => {
                        println!("Exiting APRS mode.");
                        match client.stop().await {
                            Ok(_radio) => println!("APRS mode stopped."),
                            Err(e) => println!("Error stopping APRS: {e}"),
                        }
                    }
                    ReplState::Dstar(gw) => {
                        println!("Exiting D-STAR mode.");
                        match gw.stop().await {
                            Ok(_radio) => println!("D-STAR mode stopped."),
                            Err(e) => println!("Error stopping D-STAR: {e}"),
                        }
                    }
                    ReplState::Cat(_) => {}
                }
                println!("Goodbye.");
                break;
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
                            Ok(client) => ReplState::Aprs(client),
                            Err(e) => {
                                println!("Error entering APRS mode: {e}");
                                println!(
                                    "Error: radio connection lost. Please close and reopen the program."
                                );
                                break;
                            }
                        }
                    }
                } else if cmd == "dstar" && parts.get(1).is_some_and(|s| *s == "start") {
                    if parts.get(2).is_none() {
                        println!("Error: callsign required. Usage: dstar start <callsign>");
                        println!("Example: dstar start W1AW");
                        ReplState::Cat(radio)
                    } else {
                        match enter_dstar(radio, &parts[2..]).await {
                            Ok(gw) => ReplState::Dstar(gw),
                            Err(e) => {
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

            ReplState::Dstar(mut gw) => {
                if cmd == "dstar" && parts.get(1).is_some_and(|s| *s == "stop") {
                    match gw.stop().await {
                        Ok(radio) => {
                            println!("D-STAR mode stopped. Returned to CAT mode.");
                            ReplState::Cat(radio)
                        }
                        Err(e) => {
                            println!("Error stopping D-STAR: {e}");
                            println!(
                                "Error: radio connection lost. Please close and reopen the program."
                            );
                            break;
                        }
                    }
                } else {
                    dispatch_dstar(&mut gw, &cmd, &parts).await;
                    ReplState::Dstar(gw)
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

/// Enter APRS KISS mode. Consumes the radio; returns an `AprsClient`.
/// On error, the radio is lost (KISS entry is non-recoverable).
async fn enter_aprs(
    radio: Radio<EitherTransport>,
    args: &[&str],
) -> Result<AprsClient<EitherTransport>, String> {
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
        Err(e) => Err(format!("{e}")),
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

/// Enter D-STAR gateway (MMDVM) mode. Consumes the radio; returns
/// a `DStarGateway`. On error, the radio is lost.
async fn enter_dstar(
    radio: Radio<EitherTransport>,
    args: &[&str],
) -> Result<DStarGateway<EitherTransport>, String> {
    let callsign = args[0];
    println!("Leaving normal radio control. Entering D-STAR gateway mode as {callsign}.");

    let config = DStarGatewayConfig::new(callsign);
    match DStarGateway::start(radio, config).await {
        Ok(gw) => {
            println!("D-STAR gateway active. Type dstar stop to exit.");
            println!("Commands: listen, heard, status, dstar stop");
            Ok(gw)
        }
        Err(e) => Err(format!("{e}")),
    }
}

/// Dispatch a command in D-STAR mode (listen, heard, status).
async fn dispatch_dstar(gw: &mut DStarGateway<EitherTransport>, cmd: &str, _parts: &[&str]) {
    match cmd {
        "listen" | "poll" => match gw.next_event().await {
            Ok(Some(event)) => print_dstar_event(&event),
            Ok(None) => println!("No D-STAR activity."),
            Err(e) => println!("Error: {e}"),
        },
        "heard" => {
            let list = gw.last_heard();
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
        "status" => match gw.poll_status().await {
            Ok(status) => println!(
                "Modem status: D-STAR buffer {}, transmit {}.",
                status.dstar_buffer,
                if status.tx { "active" } else { "idle" }
            ),
            Err(e) => println!("Error: {e}"),
        },
        _ => println!(
            "D-STAR command not recognized: {cmd}. \
             Commands: listen, heard, status, dstar stop"
        ),
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
