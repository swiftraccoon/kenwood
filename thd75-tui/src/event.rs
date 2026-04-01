use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind};
use tokio::sync::mpsc;

use crate::app::Message;

/// Tick rate for the UI refresh loop.
const TICK_RATE: Duration = Duration::from_millis(16); // ~60fps

/// Commands sent from the app to the radio task.
#[derive(Debug)]
pub enum RadioCommand {
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
        level: u8,
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
    SetVoxGain(u8),
    /// Set VOX delay (VD write — verified working).
    SetVoxDelay(u8),
    /// Set TNC baud rate (AS write — verified working).
    SetTncBaud(u8),
    /// Set beacon type (PT write — verified working).
    SetBeaconType(u8),
    /// Set GPS config (GP write — verified working).
    SetGpsConfig(bool, bool),
    /// Set FM radio on/off (FR write — verified working).
    SetFmRadio(bool),
    /// Set D-STAR callsign slot (CS write — verified working).
    SetCallsignSlot(u8),
    /// Set D-STAR slot (DS write — verified working).
    SetDstarSlot(u8),
    /// Write a single byte to MCP memory via modify_memory_page.
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
}

/// Merges terminal key events with messages from background tasks.
pub struct EventHandler {
    rx: mpsc::UnboundedReceiver<Message>,
    tx: mpsc::UnboundedSender<Message>,
    cmd_tx: mpsc::UnboundedSender<RadioCommand>,
    cmd_rx: Option<mpsc::UnboundedReceiver<RadioCommand>>,
}

impl EventHandler {
    /// Create a new event handler with internal message and command channels.
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        // Spawn a dedicated thread for blocking crossterm event polling.
        // This avoids blocking a tokio worker thread.
        let input_tx = tx.clone();
        std::thread::spawn(move || {
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
    pub fn sender(&self) -> mpsc::UnboundedSender<Message> {
        self.tx.clone()
    }

    /// Returns a sender the app can use to send commands to the radio task.
    pub fn command_sender(&self) -> mpsc::UnboundedSender<RadioCommand> {
        self.cmd_tx.clone()
    }

    /// Takes the command receiver (can only be called once).
    ///
    /// # Panics
    ///
    /// Panics if the command receiver has already been taken.
    pub fn take_command_receiver(&mut self) -> mpsc::UnboundedReceiver<RadioCommand> {
        self.cmd_rx.take().expect("command receiver already taken")
    }

    /// Wait for the next message from any source (terminal input or background tasks).
    pub async fn next(&mut self) -> Message {
        self.rx.recv().await.unwrap_or(Message::Quit)
    }
}
