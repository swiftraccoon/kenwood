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
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Init tracing — only if RUST_LOG is set, write to a file to avoid corrupting TUI
    if std::env::var("RUST_LOG").is_ok() {
        let log_file = std::fs::File::create("thd75-tui.log")
            .expect("failed to create log file");
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(log_file)
            .with_ansi(false)
            .init();
    }

    // Install panic hook that restores terminal before printing panic
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(panic_info);
    }));

    // Terminal setup
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Run the app, catching panics to restore terminal
    let result = run_app(&mut terminal, cli.port, cli.baud, cli.mcp_speed).await;

    // Terminal teardown (always runs)
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        eprintln!("Error: {e}");
    }

    Ok(())
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    port: Option<String>,
    baud: u32,
    mcp_speed: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut events = event::EventHandler::new();
    let tx = events.sender();
    let cmd_rx = events.take_command_receiver();

    // Connect to radio in background
    let port_display = match radio_task::spawn(port, baud, mcp_speed, tx, cmd_rx).await {
        Ok(path) => path,
        Err(e) => {
            return Err(format!("Could not connect to radio: {e}").into());
        }
    };

    let mut app = App::new(port_display);
    app.connected = true;
    app.cmd_tx = Some(events.command_sender());

    // Initial render
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
