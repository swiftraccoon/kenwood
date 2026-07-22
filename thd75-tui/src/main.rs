//! Ratatui dashboard for the Kenwood TH-D75 transceiver.
//!
//! Built on top of [`kenwood_thd75`]. Displays live VFO state, S-meter,
//! squelch, and channel memories pulled from the radio over USB CDC or
//! Bluetooth SPP, plus APRS, D-STAR reflector, GPS, and FM-radio panels.
//! Edits to channel memory and radio settings round-trip back to the radio.
//!
//! Run with `cargo run -p thd75-tui` for auto-discovery, or pass
//! `--port /dev/cu.TH-D75` for a paired Bluetooth radio.

mod app;
mod event;
mod radio_task;
mod ui;

use std::io::{self, Write};
use std::time::Duration;

use app::App;
use clap::Parser;
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use kenwood_thd75::LinkDiagnosis;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// Terminal UI for the Kenwood TH-D75 transceiver.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// Serial port path (default: auto-discover USB, then Bluetooth).
    #[arg(short, long)]
    port: Option<String>,

    /// Baud rate for CAT commands.
    #[arg(short, long, default_value_t = 115_200)]
    baud: u32,

    /// MCP transfer speed: safe or fast.
    #[arg(long, default_value = "safe")]
    mcp_speed: String,

    /// If the radio is found in Reflector Terminal Mode, guide an exit
    /// (prompt for the Menu 650 change) and reconnect, instead of just
    /// reporting it and quitting.
    #[arg(long)]
    exit_terminal_mode: bool,
}

/// What [`run_app`] did before returning control to `main`.
enum RunOutcome {
    /// The dashboard ran and the operator quit.
    Quit,
    /// The radio connection was never established. Carries the rendered
    /// failure message and, when available, the link diagnosis.
    ConnectFailed(radio_task::ConnectFailure),
}

/// Set up file logging when `RUST_LOG` is present. A read-only cwd
/// must not abort the TUI just because `RUST_LOG` was set: run
/// without file logging and say so.
fn init_logging() {
    if std::env::var("RUST_LOG").is_ok() {
        match std::fs::File::create("thd75-tui.log") {
            Ok(log_file) => {
                tracing_subscriber::fmt()
                    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                    .with_writer(log_file)
                    .with_ansi(false)
                    .init();
            }
            Err(err) => {
                eprintln!("warning: cannot create thd75-tui.log ({err}); logging disabled");
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    type BtResult = Result<(String, kenwood_thd75::transport::EitherTransport), String>;

    let cli = Cli::parse();

    init_logging();

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _cleanup = disable_raw_mode();
        let _cleanup = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(panic_info);
    }));

    // Connection-attempt loop. The body brings up the dashboard once; it
    // repeats only when `--exit-terminal-mode` is set and the radio is
    // found in Reflector Terminal Mode, after the operator has been
    // guided through the Menu 650 change.
    'retry: loop {
        // Open BT connection on the main thread (IOBluetooth needs main CFRunLoop).
        let transport = radio_task::discover_and_open_transport(cli.port.as_deref(), cli.baud);

        // Terminal setup on main thread before spawning
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let (done_tx, done_rx) = std::sync::mpsc::channel::<Result<RunOutcome, String>>();

        // Channel for BT reconnect requests from the tokio thread.
        // IOBluetooth RFCOMM must be opened on the main thread (needs CFRunLoop).
        // The tokio thread sends (port, baud) and the main thread replies with the transport.
        let (bt_req_tx, bt_req_rx) = std::sync::mpsc::channel::<(Option<String>, u32)>();
        let (bt_resp_tx, bt_resp_rx) = std::sync::mpsc::channel::<BtResult>();

        let mcp_speed = cli.mcp_speed.clone();

        let _thread = std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(err) => {
                    // Restore the terminal before reporting: the main
                    // thread is blocked pumping the CFRunLoop and only
                    // reacts to `done_tx`.
                    let _cleanup = disable_raw_mode();
                    let _cleanup = execute!(terminal.backend_mut(), LeaveAlternateScreen);
                    let _cleanup = terminal.show_cursor();
                    let _send = done_tx.send(Err(format!("failed to build tokio runtime: {err}")));
                    return;
                }
            };

            let result = rt.block_on(async {
                run_app(&mut terminal, transport, mcp_speed, bt_req_tx, bt_resp_rx)
                    .await
                    .map_err(|e| e.to_string())
            });

            let _cleanup = disable_raw_mode();
            let _cleanup = execute!(terminal.backend_mut(), LeaveAlternateScreen);
            let _cleanup = terminal.show_cursor();

            let _send = done_tx.send(result);
        });

        // Main thread: pump CFRunLoop for IOBluetooth callbacks until the
        // tokio thread reports its outcome.
        let outcome = loop {
            #[cfg(target_os = "macos")]
            #[expect(
                unsafe_code,
                reason = "macOS IOBluetooth RFCOMM delivers packet callbacks on the main \
                          thread's CFRunLoop, so a non-Cocoa binary must pump that run loop \
                          itself; otherwise incoming BT frames never arrive. The Rust \
                          ecosystem has no safe wrapper for CFRunLoopRunInMode; this is \
                          Apple's only documented API for pumping the run loop from a \
                          non-Cocoa binary. `seconds=0.01` keeps the main thread responsive \
                          while yielding to IOBluetooth's internal queue. `unsafe_code` fires \
                          here because the FFI block transits through Apple's C ABI; the \
                          signatures are verified against the CoreFoundation headers in this \
                          machine's SDK."
            )]
            // SAFETY: `CFRunLoopRunInMode` and `kCFRunLoopDefaultMode`
            // are declared exactly as in the CoreFoundation headers of
            // this machine's SDK (mode: CFRunLoopMode as *const void,
            // seconds: CFTimeInterval as f64, returnAfterSourceHandled:
            // Boolean as u8, returning SInt32). The call runs the main
            // thread's own run loop for at most 10 ms and touches no
            // Rust-managed memory.
            unsafe {
                unsafe extern "C" {
                    fn CFRunLoopRunInMode(
                        mode: *const std::ffi::c_void,
                        seconds: f64,
                        returnAfterSourceHandled: u8,
                    ) -> i32;
                    static kCFRunLoopDefaultMode: *const std::ffi::c_void;
                }
                let _ = CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.01, 0);
            }

            #[cfg(not(target_os = "macos"))]
            std::thread::sleep(Duration::from_millis(10));

            // Handle BT reconnect requests from the tokio thread.
            // BluetoothTransport::open() must happen on the main thread.
            if let Ok((port, baud)) = bt_req_rx.try_recv() {
                let result = radio_task::discover_and_open_transport(port.as_deref(), baud);
                let _send = bt_resp_tx.send(result);
            }

            if let Ok(result) = done_rx.try_recv() {
                break result;
            }
        };

        match outcome {
            Ok(RunOutcome::Quit) => break 'retry,
            Ok(RunOutcome::ConnectFailed(failure)) => {
                // The teardown above already restored the terminal, so the
                // guidance and prompt below print to a plain screen.
                if cli.exit_terminal_mode && failure.diagnosis == Some(LinkDiagnosis::MmdvmMode) {
                    guide_terminal_mode_exit(&failure.message);
                    continue 'retry;
                }
                eprintln!("{}", failure.message);
                break 'retry;
            }
            Err(e) => {
                eprintln!("Error: {e}");
                break 'retry;
            }
        }
    }

    Ok(())
}

/// Print the Reflector Terminal Mode guidance, then block until the
/// operator confirms they have changed Menu 650.
///
/// Used only under `--exit-terminal-mode`: the caller retries the
/// connection once this returns. A short delay covers the radio's
/// reboot after the menu change (USB re-enumerates).
fn guide_terminal_mode_exit(message: &str) {
    println!("\n{message}\n");
    print!("Set Menu 650 to Off on the radio, then press Enter to retry (Ctrl-C to quit)... ");
    let _flush = io::stdout().flush();
    let mut line = String::new();
    let _bytes = io::stdin().read_line(&mut line);
    println!("Reconnecting...");
    std::thread::sleep(Duration::from_secs(4));
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    transport: Result<(String, kenwood_thd75::transport::EitherTransport), String>,
    mcp_speed: String,
    bt_req_tx: std::sync::mpsc::Sender<(Option<String>, u32)>,
    bt_resp_rx: std::sync::mpsc::Receiver<
        Result<(String, kenwood_thd75::transport::EitherTransport), String>,
    >,
) -> Result<RunOutcome, Box<dyn std::error::Error>> {
    let mut events = event::EventHandler::new();
    let tx = events.sender();
    let cmd_rx = events.take_command_receiver();

    let (path, transport) = match transport {
        Ok(found) => found,
        Err(e) => {
            return Ok(RunOutcome::ConnectFailed(
                radio_task::ConnectFailure::generic(format!("Could not find the radio: {e}")),
            ));
        }
    };

    let port_display = match radio_task::spawn_with_transport(
        path, transport, mcp_speed, tx, cmd_rx, bt_req_tx, bt_resp_rx,
    )
    .await
    {
        Ok(p) => p,
        Err(failure) => return Ok(RunOutcome::ConnectFailed(failure)),
    };

    let mut app = App::new(port_display);
    app.connected = true;
    app.cmd_tx = Some(events.command_sender());

    let _ = terminal.draw(|frame| ui::render(&app, frame))?;

    loop {
        let msg = events.next().await;
        let needs_render = app.update(msg);
        if app.should_quit {
            break;
        }
        if needs_render {
            let _ = terminal.draw(|frame| ui::render(&app, frame))?;
        }
    }

    Ok(RunOutcome::Quit)
}
