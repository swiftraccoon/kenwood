#![doc = include_str!("../README.md")]

use std::io::{self, BufRead, Write};
use std::num::NonZeroU32;

use clap::Parser;
use kenwood_thd75::LinkDiagnosis;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::app::App;

mod app;
mod cache;
mod event;
mod radio_task;
mod settings_edit;
mod terminal;
mod ui;

/// Maximum complete guidance line retained before retry refusal.
const GUIDANCE_INPUT_CAPACITY: u64 = 1024;

/// Terminal dashboard for the Kenwood TH-D75 transceiver.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// Serial port path; omission discovers one USB port, then native Bluetooth.
    #[arg(short, long)]
    port: Option<String>,
    /// Nonzero serial CAT baud rate.
    #[arg(short, long, default_value = "115200")]
    baud: NonZeroU32,
    /// Offer a retry after a diagnosed Terminal-mode connection failure.
    #[arg(long)]
    exit_terminal_mode: bool,
}

#[derive(Debug)]
enum RunOutcome {
    Quit,
    ConnectFailed(Box<radio_task::ConnectFailure>),
}

/// Independent UI, keyboard, radio and terminal cleanup outcomes.
#[derive(Debug, Default)]
struct RunFailure {
    operation: Option<Box<dyn std::error::Error + Send + Sync>>,
    keyboard: Option<event::KeyboardPanicked>,
    radio: Option<radio_task::WorkerCompletion>,
    join: Option<tokio::task::JoinError>,
    terminal: Vec<terminal::TerminalError>,
}
impl RunFailure {
    fn is_empty(&self) -> bool {
        self.operation.is_none()
            && self.keyboard.is_none()
            && self.radio.is_none()
            && self.join.is_none()
            && self.terminal.is_empty()
    }
}
impl std::fmt::Display for RunFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(error) = &self.operation {
            writeln!(formatter, "Dashboard failed: {error}")?;
        }
        if let Some(error) = &self.keyboard {
            writeln!(formatter, "{error}")?;
        }
        if let Some(error) = &self.radio {
            writeln!(formatter, "{error}")?;
        }
        if let Some(error) = &self.join {
            writeln!(formatter, "Radio task failed: {error}")?;
        }
        for error in &self.terminal {
            writeln!(formatter, "{error}")?;
        }
        Ok(())
    }
}
impl std::error::Error for RunFailure {}

fn init_logging() {
    if std::env::var_os("RUST_LOG").is_some() {
        match std::fs::File::create("thd75-tui.log") {
            Ok(file) => {
                if let Err(error) = tracing_subscriber::fmt()
                    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                    .with_writer(file)
                    .with_ansi(false)
                    .try_init()
                {
                    eprintln!("Logging unavailable: {error}");
                }
            }
            Err(error) => eprintln!("Cannot create thd75-tui.log: {error}; logging disabled"),
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    init_logging();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let mut retry = None;
    loop {
        match runtime.block_on(run_app(&cli, retry.take()))? {
            RunOutcome::Quit => return Ok(()),
            RunOutcome::ConnectFailed(mut failure) => {
                if cli.exit_terminal_mode
                    && failure.diagnosis == Some(LinkDiagnosis::MmdvmMode)
                    && failure.release.is_none()
                {
                    if guide_terminal_mode_exit(
                        &failure.message,
                        &mut io::stdin().lock(),
                        &mut io::stdout(),
                    )? {
                        retry = failure.take_retry_endpoint();
                        if retry.is_some() {
                            continue;
                        }
                    }
                } else {
                    eprintln!("{failure}");
                }
                return Err(failure);
            }
        }
    }
}

/// Retry only after a complete input line; EOF and input/output failures stop guidance.
fn guide_terminal_mode_exit(
    message: &str,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> io::Result<bool> {
    writeln!(output, "{message}")?;
    write!(
        output,
        "Set Menu 650 to Off, then press Enter to retry; EOF quits: "
    )?;
    output.flush()?;
    let mut line = Vec::new();
    let _bytes = io::Read::take(input, GUIDANCE_INPUT_CAPACITY + 1).read_until(b'\n', &mut line)?;
    if u64::try_from(line.len()).map_err(io::Error::other)? > GUIDANCE_INPUT_CAPACITY {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "retry input exceeds 1024 bytes",
        ));
    }
    Ok(line.last() == Some(&b'\n'))
}

fn apply_ui_message(app: &mut App, message: app::Message) -> io::Result<bool> {
    match message {
        app::Message::TerminalFailure(error) => Err(error),
        message => Ok(app.update(message)),
    }
}

async fn run_app(
    cli: &Cli,
    retry: Option<radio_task::Endpoint>,
) -> Result<RunOutcome, Box<RunFailure>> {
    let request = retry.map_or_else(
        || radio_task::ConnectionRequest::Discover {
            port: cli.port.clone(),
            baud: cli.baud,
        },
        radio_task::ConnectionRequest::Retained,
    );
    let baud = cli.baud;
    let transport = tokio::task::spawn_blocking(move || request.open()).await;
    let (path, transport) = match transport {
        Ok(Ok(transport)) => transport,
        Ok(Err(error)) => {
            return Ok(RunOutcome::ConnectFailed(Box::new(
                radio_task::ConnectFailure::generic(error),
            )));
        }
        Err(error) => {
            return Err(Box::new(RunFailure {
                operation: Some(Box::new(error)),
                ..RunFailure::default()
            }));
        }
    };
    let (mut events, commands) = event::EventHandler::new();
    let (path, mut worker) =
        match radio_task::spawn_with_transport(path, baud, transport, events.sender(), commands)
            .await
        {
            Ok(connected) => connected,
            Err(error) => return Ok(RunOutcome::ConnectFailed(error)),
        };
    let mut failure = RunFailure::default();
    let mut session = terminal::TerminalSession::default();
    match session.enter() {
        Ok(()) => {
            let result = async {
                let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
                events.start_input()?;
                let mut app = App::new(path);
                app.connected = true;
                app.cmd_tx = Some(events.command_sender());
                let _frame = terminal.draw(|frame| ui::render(&app, frame))?;
                loop {
                    let redraw = apply_ui_message(&mut app, events.next().await)?;
                    if app.should_quit || worker.is_finished() {
                        break;
                    }
                    if redraw {
                        let _frame = terminal.draw(|frame| ui::render(&app, frame))?;
                    }
                }
                Ok::<(), io::Error>(())
            }
            .await;
            failure.operation = result
                .err()
                .map(|error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(error) });
        }
        Err(errors) => failure.terminal = errors,
    }
    worker.request_stop();
    failure.keyboard = events.shutdown().err();
    match worker.shutdown().await {
        Ok(completion) if !completion.is_clean() => failure.radio = Some(completion),
        Ok(_) => {}
        Err(error) => failure.join = Some(error),
    }
    failure.terminal.extend(session.restore());
    if failure.is_empty() {
        Ok(RunOutcome::Quit)
    } else {
        Err(Box::new(failure))
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, apply_ui_message, guide_terminal_mode_exit};
    #[test]
    fn terminal_input_failure_is_a_session_error() -> Result<(), Box<dyn std::error::Error>> {
        let mut app = crate::app::App::with_cache_path(String::new(), None);
        let result = apply_ui_message(
            &mut app,
            crate::app::Message::TerminalFailure(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "keyboard detached",
            )),
        );
        let Err(error) = result else {
            return Err("terminal failure became successful quit".into());
        };
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        assert!(error.to_string().contains("keyboard detached"));
        Ok(())
    }
    #[test]
    fn zero_baud_and_eof_cannot_start_a_connection_retry() -> Result<(), Box<dyn std::error::Error>>
    {
        assert!(
            Cli::try_parse_from(["thd75-tui", "--baud", "0"]).is_err(),
            "zero baud passed CLI validation"
        );
        let mut output = Vec::new();
        assert!(
            !guide_terminal_mode_exit("Terminal mode", &mut std::io::Cursor::new([]), &mut output)?,
            "EOF requested another connection"
        );
        assert!(guide_terminal_mode_exit(
            "Terminal mode",
            &mut std::io::Cursor::new(b"\n"),
            &mut output
        )?);
        assert!(
            !guide_terminal_mode_exit(
                "Terminal mode",
                &mut std::io::Cursor::new(b"unfinished"),
                &mut output
            )?,
            "partial EOF line authorized retry"
        );
        assert!(
            guide_terminal_mode_exit(
                "Terminal mode",
                &mut std::io::Cursor::new([b'x'; 1025]),
                &mut output
            )
            .is_err(),
            "oversized guidance input was retained"
        );
        Ok(())
    }
}
