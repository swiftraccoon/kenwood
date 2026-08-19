use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind};
use tokio::sync::mpsc;

use crate::app::Message;

/// Tick rate for the UI refresh loop.
const TICK_RATE: Duration = Duration::from_millis(16); // ~60fps

/// Commands sent from the app to the radio task.
#[derive(Debug)]
pub(crate) enum RadioCommand {
    /// Trigger a full MCP memory read from the radio.
    ReadMemory,
    /// Trigger a full MCP memory write to the radio.
    WriteMemory(Vec<u8>),
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
    /// Set the squelch level for the given band (SQ write: verified working).
    SetSquelch {
        band: kenwood_thd75::types::Band,
        level: kenwood_thd75::types::SquelchLevel,
    },
    /// Toggle the attenuator for the given band (RA write: verified working).
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
    /// Toggle bluetooth on/off (BT write: verified working).
    SetBluetooth(bool),
    /// Toggle VOX on/off (VX write: verified working).
    SetVox(bool),
    /// Set VOX gain (VG write: verified working).
    SetVoxGain(kenwood_thd75::types::VoxGain),
    /// Set VOX delay (VD write: verified working).
    SetVoxDelay(kenwood_thd75::types::VoxDelay),
    /// Set packet data rate (AS write: verified working).
    SetPacketDataRate(kenwood_thd75::types::PacketDataRate),
    /// Set beacon mode (PT write: verified working).
    SetBeaconMode(kenwood_thd75::types::BeaconMode),
    /// Set GPS settings (GP write: verified working).
    SetGpsSettings(kenwood_thd75::types::GpsSettings),
    /// Set the step size for the given band (SF write: verified working).
    SetStepSize {
        band: kenwood_thd75::types::Band,
        step: kenwood_thd75::types::StepSize,
    },
    /// Write a single byte to MCP memory via `modify_memory_page`.
    /// Enters MCP mode, modifies one byte, exits, and restores CAT.
    /// Used for settings where CAT writes are rejected by D75 firmware.
    McpWriteByte { offset: u16, value: u8 },
    /// Set the transmit power level for the given band.
    SetPower {
        /// The band to adjust.
        band: kenwood_thd75::types::Band,
        /// The desired power level.
        level: kenwood_thd75::types::PowerLevel,
    },
    /// Set D-STAR URCALL callsign via CAT (works in normal CAT mode).
    SetUrcall {
        /// Callsign (up to 8 chars).
        callsign: String,
        /// Suffix (up to 4 chars).
        suffix: String,
    },
    /// Prepare a D-STAR reflector link by setting URCALL through CAT.
    PrepareReflectorLink {
        /// Reflector callsign (e.g. "REF030").
        name: String,
        /// Reflector module letter (e.g. 'C').
        module: char,
    },
    /// Prepare a D-STAR reflector unlink by setting URCALL through CAT.
    PrepareReflectorUnlink,
    /// Set URCALL to CQCQCQ via CAT.
    SetCQ,
    /// Enter D-STAR gateway mode using the MMDVM transport.
    EnterDstar {
        /// D-STAR gateway configuration.
        config: kenwood_thd75::DstarGatewayConfig,
    },
    /// Exit D-STAR gateway mode.
    ExitDstar,
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
        addressee: kenwood_thd75::MessageAddressee,
        /// Message text.
        text: kenwood_thd75::MessageText,
    },
    /// Transmit a manual position beacon while in APRS mode.
    BeaconPosition {
        /// Validated latitude.
        latitude: kenwood_thd75::Latitude,
        /// Validated longitude.
        longitude: kenwood_thd75::Longitude,
        /// Beacon comment text.
        comment: kenwood_thd75::PositionReportText,
    },
}

/// Merges terminal key events with messages from background tasks.
pub(crate) struct EventHandler {
    rx: mpsc::UnboundedReceiver<Message>,
    tx: mpsc::UnboundedSender<Message>,
    cmd_tx: mpsc::UnboundedSender<RadioCommand>,
    cmd_rx: Option<mpsc::UnboundedReceiver<RadioCommand>>,
}

impl EventHandler {
    /// Create a new event handler with internal message and command channels.
    pub(crate) fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        // Spawn a dedicated thread for blocking crossterm event polling.
        // This avoids blocking a tokio worker thread. A poll/read error
        // (terminal detached, tty closed) means keyboard input is gone
        // for good: request an app quit rather than dying silently in
        // this background thread.
        let input_tx = tx.clone();
        let _handle = std::thread::spawn(move || {
            loop {
                match event::poll(TICK_RATE) {
                    Ok(false) => {}
                    Ok(true) => match event::read() {
                        Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                            if input_tx.send(Message::Key(key)).is_err() {
                                return;
                            }
                        }
                        Ok(_) => {}
                        Err(err) => {
                            tracing::error!("terminal event read failed: {err}");
                            let _send = input_tx.send(Message::Quit);
                            return;
                        }
                    },
                    Err(err) => {
                        tracing::error!("terminal event poll failed: {err}");
                        let _send = input_tx.send(Message::Quit);
                        return;
                    }
                }
            }
        });

        Self {
            rx,
            tx,
            cmd_tx,
            cmd_rx: Some(cmd_rx),
        }
    }

    /// Returns a sender that background tasks can use to push messages.
    pub(crate) fn sender(&self) -> mpsc::UnboundedSender<Message> {
        self.tx.clone()
    }

    /// Returns a sender the app can use to send commands to the radio task.
    pub(crate) fn command_sender(&self) -> mpsc::UnboundedSender<RadioCommand> {
        self.cmd_tx.clone()
    }

    /// Takes the command receiver (can only be called once).
    ///
    /// # Panics
    ///
    /// Panics if the command receiver has already been taken.
    pub(crate) fn take_command_receiver(&mut self) -> mpsc::UnboundedReceiver<RadioCommand> {
        self.cmd_rx
            .take()
            .unwrap_or_else(|| unreachable!("command receiver already taken"))
    }

    /// Wait for the next message from any source (terminal input or background tasks).
    pub(crate) async fn next(&mut self) -> Message {
        self.rx.recv().await.unwrap_or(Message::Quit)
    }
}
