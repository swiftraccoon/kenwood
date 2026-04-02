mod app;
mod event;
mod radio_task;
mod ui;

use std::io;

use app::App;
use clap::Parser;
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// Terminal UI for the Kenwood TH-D75 transceiver.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// Serial port path (default: auto-discover USB).
    #[arg(short, long)]
    port: Option<String>,

    /// Baud rate for CAT commands.
    #[arg(short, long, default_value_t = 115_200)]
    baud: u32,

    /// MCP transfer speed: safe or fast.
    #[arg(long, default_value = "safe")]
    mcp_speed: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    if std::env::var("RUST_LOG").is_ok() {
        let log_file = std::fs::File::create("thd75-tui.log").expect("failed to create log file");
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(log_file)
            .with_ansi(false)
            .init();
    }

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(panic_info);
    }));

    // Open BT connection on the main thread (IOBluetooth needs main CFRunLoop).
    let transport = radio_task::discover_and_open_transport(cli.port.clone(), cli.baud);

    // Terminal setup on main thread before spawning
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (done_tx, done_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let mcp_speed = cli.mcp_speed;

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");

        let result = rt.block_on(async {
            run_app(&mut terminal, transport, mcp_speed)
                .await
                .map_err(|e| e.to_string())
        });

        let _ = disable_raw_mode();
        let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
        let _ = terminal.show_cursor();

        let _ = done_tx.send(result);
    });

    // Main thread: pump CFRunLoop for IOBluetooth callbacks
    loop {
        #[cfg(target_os = "macos")]
        unsafe {
            unsafe extern "C" {
                fn CFRunLoopRunInMode(
                    mode: *const std::ffi::c_void,
                    seconds: f64,
                    returnAfterSourceHandled: u8,
                ) -> i32;
                static kCFRunLoopDefaultMode: *const std::ffi::c_void;
            }
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.01, 0);
        }

        #[cfg(not(target_os = "macos"))]
        std::thread::sleep(std::time::Duration::from_millis(10));

        if let Ok(result) = done_rx.try_recv() {
            if let Err(e) = result {
                eprintln!("Error: {e}");
            }
            break;
        }
    }

    Ok(())
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    transport: Result<(String, kenwood_thd75::transport::EitherTransport), String>,
    mcp_speed: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut events = event::EventHandler::new();
    let tx = events.sender();
    let cmd_rx = events.take_command_receiver();

    let (path, transport) = transport.map_err(|e| format!("Could not connect to radio: {e}"))?;

    let port_display =
        match radio_task::spawn_with_transport(path, transport, mcp_speed, tx, cmd_rx).await {
            Ok(p) => p,
            Err(e) => return Err(format!("Could not connect to radio: {e}").into()),
        };

    let mut app = App::new(port_display);
    app.connected = true;
    app.cmd_tx = Some(events.command_sender());

    terminal.draw(|frame| ui::render(&app, frame))?;

    loop {
        let msg = events.next().await;
        let needs_render = app.update(msg);
        if app.should_quit {
            break;
        }
        if needs_render {
            terminal.draw(|frame| ui::render(&app, frame))?;
        }
    }

    Ok(())
}
