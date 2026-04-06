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
    /// Tune the given band to a memory channel number.
    TuneChannel {
        /// The band to tune.
        band: kenwood_thd75::types::Band,
        /// The memory channel number (0–1199).
        channel: u16,
    },
    /// Step frequency up by one increment on the given band.
    FreqUp(kenwood_thd75::types::Band),
    /// Step frequency down by one increment on the given band.
    FreqDown(kenwood_thd75::types::Band),
    /// Tune to a specific frequency on the given band.
    TuneFreq {
        /// The band to tune.
        band: kenwood_thd75::types::Band,
        /// The frequency in Hz.
        freq: u32,
    },
    /// Set the squelch level for the given band (SQ write — verified working).
    SetSquelch {
        band: kenwood_thd75::types::Band,
        level: kenwood_thd75::types::SquelchLevel,
    },
    /// Toggle the attenuator for the given band (RA write — verified working).
    SetAttenuator {
        band: kenwood_thd75::types::Band,
        enabled: bool,
    },
    /// Set the operating mode for the given band (MD write — may return N in some modes).
    SetMode {
        band: kenwood_thd75::types::Band,
        mode: kenwood_thd75::types::Mode,
    },
    /// Toggle lock on/off (LC write — verified working, value inverted on D75).
    SetLock(bool),
    /// Toggle dual band on/off (DL write — verified working, value inverted on D75).
    SetDualBand(bool),
    /// Toggle bluetooth on/off (BT write — verified working).
    SetBluetooth(bool),
    /// Toggle VOX on/off (VX write — verified working).
    SetVox(bool),
    /// Set VOX gain (VG write — verified working).
    SetVoxGain(kenwood_thd75::types::VoxGain),
    /// Set VOX delay (VD write — verified working).
    SetVoxDelay(kenwood_thd75::types::VoxDelay),
    /// Set TNC baud rate (AS write — verified working).
    SetTncBaud(kenwood_thd75::types::TncBaud),
    /// Set beacon type (PT write — verified working).
    SetBeaconType(kenwood_thd75::types::BeaconMode),
    /// Set GPS config (GP write — verified working).
    SetGpsConfig(bool, bool),
    /// Set FM radio on/off (FR write — verified working).
    SetFmRadio(bool),
    /// Set D-STAR callsign slot (CS write — verified working).
    /// Not yet wired to `adjust_setting` (requires polling current slot first).
    #[allow(dead_code)]
    SetCallsignSlot(kenwood_thd75::types::CallsignSlot),
    /// Set D-STAR slot (DS write — verified working).
    /// Not yet wired to `adjust_setting` (requires polling current slot first).
    #[allow(dead_code)]
    SetDstarSlot(kenwood_thd75::types::DstarSlot),
    /// Set the step size for the given band (SF write — verified working).
    SetStepSize {
        band: kenwood_thd75::types::Band,
        step: kenwood_thd75::types::StepSize,
    },
    /// Set the scan resume method (SR write — write-only on D75).
    SetScanResumeCat(kenwood_thd75::types::ScanResumeMethod),
    /// Write a single byte to MCP memory via `modify_memory_page`.
    /// Enters MCP mode, modifies one byte, exits. USB drops and reconnects.
    /// Used for settings where CAT writes are rejected by D75 firmware.
    McpWriteByte { offset: u16, value: u8 },
    /// Set the transmit power level for the given band.
    SetPower {
        /// The band to adjust.
        band: kenwood_thd75::types::Band,
        /// The desired power level.
        level: kenwood_thd75::types::PowerLevel,
    },
    /// Enter APRS/KISS mode. The radio task enters KISS mode and starts
    /// processing APRS packets instead of CAT polling.
    EnterAprs {
        /// APRS client configuration (callsign, SSID, etc.).
        config: kenwood_thd75::AprsClientConfig,
    },
    /// Exit APRS/KISS mode. Returns to CAT polling.
    ExitAprs,
    /// Send an APRS message to a station while in APRS mode.
    SendAprsMessage {
        /// Destination callsign.
        addressee: String,
        /// Message text.
        text: String,
    },
    /// Transmit a manual position beacon while in APRS mode.
    BeaconPosition {
        /// Latitude in decimal degrees.
        lat: f64,
        /// Longitude in decimal degrees.
        lon: f64,
        /// Beacon comment text.
        comment: String,
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
        // This avoids blocking a tokio worker thread.
        let input_tx = tx.clone();
        let _handle = std::thread::spawn(move || {
            loop {
                if event::poll(TICK_RATE).expect("event poll failed")
                    && let Event::Key(key) = event::read().expect("event read failed")
                    && key.kind == KeyEventKind::Press
                    && input_tx.send(Message::Key(key)).is_err()
                {
                    return;
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
    pub(crate) const fn take_command_receiver(&mut self) -> mpsc::UnboundedReceiver<RadioCommand> {
        self.cmd_rx.take().expect("command receiver already taken")
    }

    /// Wait for the next message from any source (terminal input or background tasks).
    pub(crate) async fn next(&mut self) -> Message {
        self.rx.recv().await.unwrap_or(Message::Quit)
    }
}
