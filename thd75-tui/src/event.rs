use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind};
use tokio::sync::mpsc;

use crate::app::Message;
use crate::settings_edit::SettingsEdit;

/// Maximum keyboard-poll delay before a stop request is observed.
const TICK_RATE: Duration = Duration::from_millis(16);

/// Maximum queued UI operations while a radio command is in progress.
const COMMAND_CAPACITY: usize = 32;

/// Commands sent from the app to the radio task.
#[derive(Debug, Clone)]
pub(crate) enum RadioCommand {
    /// Trigger a full MCP memory read from the radio.
    ReadMemory,
    /// Tune the given band to a regular memory channel.
    TuneChannel {
        /// The band to tune.
        band: kenwood_thd75::types::Band,
        /// The validated regular memory channel (0–999).
        channel: kenwood_thd75::types::RegularChannel,
    },
    /// Step frequency up by one increment on the given band.
    FreqUp(kenwood_thd75::types::Band),
    /// Step frequency down by one increment on the given band.
    FreqDown(kenwood_thd75::types::Band),
    /// Set the squelch level for the given band through CAT SQ.
    SetSquelch {
        band: kenwood_thd75::types::Band,
        level: kenwood_thd75::types::SquelchLevel,
    },
    /// Set the attenuator for the given band through CAT RA.
    SetAttenuator {
        band: kenwood_thd75::types::Band,
        enabled: bool,
    },
    /// Set the operating mode for the given band (MD write: may return N in some modes).
    SetOperatingMode {
        band: kenwood_thd75::types::Band,
        mode: kenwood_thd75::types::OperatingMode,
    },
    /// Select single-band or dual-band presentation (DL write).
    SetBandMode(kenwood_thd75::types::BandMode),
    /// Set Bluetooth through CAT BT.
    SetBluetooth(bool),
    /// Set VOX through CAT VX.
    SetVox(bool),
    /// Set VOX gain through CAT VG.
    SetVoxGain(kenwood_thd75::types::VoxGain),
    /// Set VOX delay through CAT VD.
    SetVoxDelay(kenwood_thd75::types::VoxDelay),
    /// Set packet data rate through CAT AS.
    SetPacketDataRate(kenwood_thd75::types::PacketDataRate),
    /// Set beacon mode through CAT PT.
    SetBeaconMode(kenwood_thd75::types::BeaconMode),
    /// Set GPS settings through CAT GP.
    SetGpsSettings(kenwood_thd75::types::GpsSettings),
    /// Set the step size for the given band through CAT SF.
    SetStepSize {
        band: kenwood_thd75::types::Band,
        step: kenwood_thd75::types::StepSize,
    },
    /// Compare the exact retained page before writing a staged typed setting.
    ApplySetting(Box<SettingsEdit>),
    /// Set the transmit power level for the given band.
    SetPower {
        /// The band to adjust.
        band: kenwood_thd75::types::Band,
        /// The desired power level.
        level: kenwood_thd75::types::PowerLevel,
    },
    /// Set D-STAR URCALL callsign via CAT (works in normal CAT mode).
    SetUrcall {
        /// Construction-validated radio callsign.
        callsign: kenwood_thd75::types::DstarCallsign,
        /// Construction-validated suffix.
        suffix: kenwood_thd75::types::DstarSuffix,
    },
    /// Prepare a D-STAR reflector link by setting URCALL through CAT.
    PrepareReflectorLink {
        /// Reflector callsign (e.g. "REF030").
        name: kenwood_thd75::types::ReflectorCallsign,
        /// Reflector module letter (e.g. 'C').
        module: kenwood_thd75::types::Module,
    },
    /// Prepare a D-STAR reflector unlink by setting URCALL through CAT.
    PrepareReflectorUnlink,
    /// Set URCALL to CQCQCQ via CAT.
    SetCQ,
    /// Enter APRS/KISS mode. The radio task enters KISS mode and starts
    /// processing APRS packets instead of CAT polling.
    EnterAprs {
        /// APRS client configuration (callsign, SSID, etc.).
        ///
        /// Boxed because `AprsClientConfig` contains vector and option
        /// fields that make it significantly larger than other variants.
        config: Box<kenwood_thd75::AprsClientConfig>,
    },
    /// Exit APRS/KISS mode. Returns to CAT polling.
    ExitAprs,
    /// Send an APRS message to a station while in APRS mode.
    SendAprsMessage {
        /// Destination callsign.
        addressee: kenwood_thd75::DirectMessageAddressee,
        /// Message text.
        text: kenwood_thd75::MessageText,
    },
}

/// Why a validated request was rejected before its radio operation started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandRejection {
    /// The bounded command queue contains its maximum pending operations.
    Full,
    /// The radio worker has stopped accepting requests.
    Closed,
    /// The active binary session cannot execute this CAT request.
    WrongMode,
}
impl std::fmt::Display for CommandRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Full => "radio command queue is full",
            Self::Closed => "radio worker is closed",
            Self::WrongMode => "command is unavailable in the active radio mode",
        })
    }
}
impl std::error::Error for CommandRejection {}

/// Bounded synchronous submission with typed rejection feedback to the reducer.
#[derive(Debug, Clone)]
pub(crate) struct CommandSender {
    sender: mpsc::Sender<RadioCommand>,
    feedback: mpsc::UnboundedSender<Message>,
}
impl CommandSender {
    /// Queue one request or report its original payload and rejection reason.
    pub(crate) fn send(&self, command: RadioCommand) -> Result<(), CommandRejection> {
        match self.sender.try_send(command) {
            Ok(()) => Ok(()),
            Err(error) => {
                let (reason, command) = match error {
                    mpsc::error::TrySendError::Full(command) => (CommandRejection::Full, command),
                    mpsc::error::TrySendError::Closed(command) => {
                        (CommandRejection::Closed, command)
                    }
                };
                let _feedback = self
                    .feedback
                    .send(Message::CommandRejected { command, reason });
                Err(reason)
            }
        }
    }
}

/// Construct the bounded UI command queue and its typed feedback path.
pub(crate) fn command_channel(
    feedback: mpsc::UnboundedSender<Message>,
) -> (CommandSender, mpsc::Receiver<RadioCommand>) {
    let (sender, receiver) = mpsc::channel(COMMAND_CAPACITY);
    (CommandSender { sender, feedback }, receiver)
}

/// Merges terminal key events with messages from background tasks.
pub(crate) struct EventHandler {
    rx: mpsc::UnboundedReceiver<Message>,
    tx: mpsc::UnboundedSender<Message>,
    cmd_tx: CommandSender,
    input_stop: Arc<AtomicBool>,
    input_thread: Option<std::thread::JoinHandle<()>>,
}

impl EventHandler {
    /// Create a new event handler with internal message and command channels.
    pub(crate) fn new() -> (Self, mpsc::Receiver<RadioCommand>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = command_channel(tx.clone());

        let input_stop = Arc::new(AtomicBool::new(false));
        (
            Self {
                rx,
                tx,
                cmd_tx,
                input_stop,
                input_thread: None,
            },
            cmd_rx,
        )
    }

    /// Start the sole keyboard reader after terminal setup and connection guidance.
    pub(crate) fn start_input(&mut self) -> std::io::Result<()> {
        if self.input_thread.is_some() || self.input_stop.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "keyboard reader already started or stopped",
            ));
        }
        let input_tx = self.tx.clone();
        let thread_stop = Arc::clone(&self.input_stop);
        self.input_thread = Some(
            std::thread::Builder::new()
                .name("tui-keyboard".to_owned())
                .spawn(move || {
                    read_input(&thread_stop, &input_tx, event::poll, event::read);
                })?,
        );
        Ok(())
    }

    /// Returns a sender that background tasks can use to push messages.
    pub(crate) fn sender(&self) -> mpsc::UnboundedSender<Message> {
        self.tx.clone()
    }

    /// Returns a sender the app can use to send commands to the radio task.
    pub(crate) fn command_sender(&self) -> CommandSender {
        self.cmd_tx.clone()
    }

    /// Wait for the next message from any source (terminal input or background tasks).
    pub(crate) async fn next(&mut self) -> Message {
        match tokio::time::timeout(TICK_RATE, self.rx.recv()).await {
            Ok(message) => message.unwrap_or(Message::Quit),
            Err(_elapsed)
                if self
                    .input_thread
                    .as_ref()
                    .is_some_and(std::thread::JoinHandle::is_finished) =>
            {
                Message::Quit
            }
            Err(_elapsed) => Message::Tick,
        }
    }

    /// Stop polling and join the sole keyboard reader before another input flow.
    pub(crate) fn shutdown(&mut self) -> Result<(), KeyboardPanicked> {
        self.input_stop.store(true, Ordering::Release);
        self.rx.close();
        if let Some(thread) = self.input_thread.take() {
            thread.join().map_err(|_panic| KeyboardPanicked)?;
        }
        Ok(())
    }
}

impl Drop for EventHandler {
    fn drop(&mut self) {
        self.input_stop.store(true, Ordering::Release);
    }
}

/// The keyboard thread ended by unwinding rather than normal completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyboardPanicked;
impl std::fmt::Display for KeyboardPanicked {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("keyboard input task panicked")
    }
}
impl std::error::Error for KeyboardPanicked {}

fn read_input(
    stop: &AtomicBool,
    sender: &mpsc::UnboundedSender<Message>,
    mut poll: impl FnMut(Duration) -> std::io::Result<bool>,
    mut read: impl FnMut() -> std::io::Result<Event>,
) {
    while !stop.load(Ordering::Acquire) && !sender.is_closed() {
        let result = match poll(TICK_RATE) {
            Ok(false) => continue,
            Ok(true) => read(),
            Err(error) => Err(error),
        };
        match result {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                if sender.send(Message::Key(key)).is_err() {
                    return;
                }
            }
            Ok(_) => {}
            Err(error) => {
                let _send = sender.send(Message::TerminalFailure(error));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use super::{CommandRejection, EventHandler, RadioCommand, command_channel, read_input};
    use crate::app::Message;

    #[test]
    fn queue_capacity_and_closed_receiver_preserve_rejected_requests()
    -> Result<(), Box<dyn std::error::Error>> {
        let (feedback, mut feedback_rx) = tokio::sync::mpsc::unbounded_channel();
        let (sender, mut receiver) = command_channel(feedback);
        for _ in 0..32 {
            sender.send(RadioCommand::ReadMemory)?;
        }
        assert_eq!(
            sender.send(RadioCommand::SetCQ),
            Err(CommandRejection::Full)
        );
        assert!(matches!(
            feedback_rx.try_recv()?,
            Message::CommandRejected {
                command: RadioCommand::SetCQ,
                reason: CommandRejection::Full
            }
        ));
        receiver.close();
        assert_eq!(
            sender.send(RadioCommand::ReadMemory),
            Err(CommandRejection::Closed)
        );
        assert!(matches!(
            feedback_rx.try_recv()?,
            Message::CommandRejected {
                command: RadioCommand::ReadMemory,
                reason: CommandRejection::Closed
            }
        ));
        Ok(())
    }

    #[test]
    fn closed_ui_receiver_prevents_another_keyboard_poll() {
        let stop = AtomicBool::new(false);
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        drop(receiver);
        let polls = AtomicUsize::new(0);
        read_input(
            &stop,
            &sender,
            |_timeout| {
                let _previous = polls.fetch_add(1, Ordering::AcqRel);
                Err(std::io::Error::other("unexpected poll"))
            },
            || Err(std::io::Error::other("unexpected read")),
        );
        assert_eq!(
            polls.load(Ordering::Acquire),
            0,
            "closed UI left a keyboard reader active"
        );
    }
    #[test]
    fn keyboard_polling_stops_without_a_key_and_reports_io_errors()
    -> Result<(), Box<dyn std::error::Error>> {
        let stop = AtomicBool::new(false);
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let count = AtomicUsize::new(0);
        read_input(
            &stop,
            &sender,
            |_timeout| {
                let previous = count.fetch_add(1, Ordering::AcqRel);
                if previous >= 3 {
                    return Err(std::io::Error::other("stop was ignored"));
                }
                if previous == 2 {
                    stop.store(true, Ordering::Release);
                }
                Ok(false)
            },
            || Err(std::io::Error::other("unexpected terminal read")),
        );
        assert_eq!(count.load(Ordering::Acquire), 3);
        assert!(receiver.try_recv().is_err());
        stop.store(false, Ordering::Release);
        read_input(
            &stop,
            &sender,
            |_timeout| Err(std::io::Error::other("injected poll failure")),
            || Err(std::io::Error::other("unexpected terminal read")),
        );
        assert!(matches!(receiver.try_recv()?, Message::TerminalFailure(_)));
        drop(receiver);
        read_input(
            &stop,
            &sender,
            |_timeout| {
                let _old = count.fetch_add(1, Ordering::AcqRel);
                Ok(false)
            },
            || Err(std::io::Error::other("unexpected terminal read")),
        );
        assert_eq!(
            count.load(Ordering::Acquire),
            3,
            "closed receiver retained keyboard polling"
        );
        Ok(())
    }

    #[test]
    fn explicit_input_shutdown_joins_the_reader() -> Result<(), Box<dyn std::error::Error>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (cmd_tx, _cmd_rx) = command_channel(tx.clone());
        let input_stop = Arc::new(AtomicBool::new(false));
        let completed = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&input_stop);
        let thread_completed = Arc::clone(&completed);
        let input_thread = std::thread::Builder::new().spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            thread_completed.store(true, Ordering::Release);
        })?;
        let mut events = EventHandler {
            rx,
            tx,
            cmd_tx,
            input_stop,
            input_thread: Some(input_thread),
        };
        events.shutdown()?;
        assert!(
            completed.load(Ordering::Acquire),
            "input task was not joined"
        );
        assert!(events.input_thread.is_none());
        Ok(())
    }
}
