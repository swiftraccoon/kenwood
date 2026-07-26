//! High-level async API for controlling a Kenwood TH-D75 transceiver.
//!
//! The [`Radio`] struct provides ergonomic methods for all radio operations,
//! organized by subsystem: frequency control, channel memory, audio settings,
//! APRS (Automatic Packet Reporting System), D-STAR (Digital Smart
//! Technologies for Amateur Radio), GPS, scanning, and system configuration.
//!
//! Generic over [`Transport`], allowing use with
//! USB serial, Bluetooth SPP, or mock transports for testing.

pub mod aprs;
pub mod audio;
pub mod diagnostics;
pub mod dstar;
#[path = "freq.rs"]
pub mod freq;
pub mod gps;
pub mod kiss_session;
pub mod memory;
pub mod memory_read;
pub mod menu;
pub mod mmdvm_session;
pub mod programming;
pub mod scan;
pub mod system;
pub mod tuning;

use std::time::Duration;

use crate::error::{Error, ProtocolError};
use crate::protocol::{self, Codec, Command, Response, command_name};
use crate::transport::Transport;
use crate::types::Band;
use crate::types::radio_params::VfoMemoryMode;

/// Default timeout for command execution (5 seconds).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Information returned by [`Radio::identify`].
#[derive(Debug, Clone)]
pub struct RadioInfo {
    /// Radio model identifier (e.g., "TH-D75").
    pub model: String,
}

/// VFO/Memory mode state for a band.
///
/// Tracked internally by the [`Radio`] struct to detect mode-incompatible
/// commands before they are sent. Values correspond to the VM command:
/// 0 = VFO, 1 = Memory, 2 = Call, 3 = WX.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadioMode {
    /// VFO (Variable Frequency Oscillator) mode: direct frequency entry.
    Vfo,
    /// Memory mode: operating on a stored channel.
    Memory,
    /// Call channel mode.
    Call,
    /// Weather channel mode (WX).
    Wx,
}

impl RadioMode {
    /// Converts a [`VfoMemoryMode`] to a `RadioMode`.
    #[must_use]
    pub const fn from_vfo_mode(mode: VfoMemoryMode) -> Self {
        match mode {
            VfoMemoryMode::Vfo => Self::Vfo,
            VfoMemoryMode::Memory => Self::Memory,
            VfoMemoryMode::Call => Self::Call,
            VfoMemoryMode::Weather => Self::Wx,
        }
    }

    /// Returns the [`VfoMemoryMode`] equivalent.
    #[must_use]
    pub const fn as_vfo_mode(self) -> VfoMemoryMode {
        match self {
            Self::Vfo => VfoMemoryMode::Vfo,
            Self::Memory => VfoMemoryMode::Memory,
            Self::Call => VfoMemoryMode::Call,
            Self::Wx => VfoMemoryMode::Weather,
        }
    }
}

/// Whether the CAT link is currently believed healthy.
///
/// Flips to [`LinkState::Down`] when a command surfaces a transport
/// error, and back to [`LinkState::Up`] after a successful
/// [`Radio::reconnect`]. Observe transitions via
/// [`Radio::link_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    /// Commands are being answered.
    Up,
    /// A transport error was observed; call [`Radio::reconnect`].
    Down,
}

/// Host-side safety phase for an MCP programming session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum McpPhase {
    /// No unresolved MCP session; normal CAT traffic is permitted.
    #[default]
    Inactive,
    /// MCP entry or binary transfer may be active; an exit has not begun.
    Active,
    /// The raw exit byte may have reached the radio and must not be resent.
    ExitSent,
}

/// High-level async API for controlling a Kenwood TH-D75.
///
/// Generic over the transport layer: works with USB serial,
/// Bluetooth SPP, or mock transport for testing.
///
/// The `Radio` struct tracks the VFO/Memory mode of each band when VM
/// commands are sent through it, enabling mode-compatibility warnings.
/// Use the safe tuning methods ([`tune_frequency`](Radio::tune_frequency),
/// [`tune_channel`](Radio::tune_channel)) for automatic mode management.
pub struct Radio<T: Transport> {
    pub(crate) transport: T,
    pub(crate) codec: Codec,
    pub(crate) notifications: tokio::sync::broadcast::Sender<Response>,
    pub(crate) timeout: Duration,
    /// Cached mode for band A. `None` until a VM command is observed.
    pub(crate) mode_a: Option<RadioMode>,
    /// Cached mode for band B. `None` until a VM command is observed.
    pub(crate) mode_b: Option<RadioMode>,
    /// MCP programming mode transfer speed.
    pub(crate) mcp_speed: programming::McpSpeed,
    /// Timestamp of last command sent, for 5ms inter-command spacing.
    /// ARFC-D75 enforces a minimum 5ms gap between commands to avoid
    /// overwhelming the radio's command buffer.
    last_cmd_time: Option<tokio::time::Instant>,
    /// Set when a command timed out: the radio's response may still be
    /// in flight and must be drained before the next command, or a
    /// retry with the same mnemonic would consume the stale answer.
    desynced: bool,
    /// A strict GM exchange failed or was cancelled with bytes potentially in
    /// flight. Unlike an ordinary timeout, this cannot be cleared by a short
    /// stale-input drain; only a fresh transport or a completed strict
    /// exchange can make the stream trustworthy again.
    pub(crate) gm_poisoned: bool,
    /// MCP safety phase. Any phase other than `Inactive` blocks CAT; the
    /// `ExitSent` phase additionally prevents recovery from sending a second
    /// raw exit byte after cancellation.
    pub(crate) mcp_phase: McpPhase,
    /// The CAT timeout saved while an MCP session temporarily raises
    /// it; restored on session end or interrupted-session recovery.
    pub(crate) mcp_saved_timeout: Option<Duration>,
    /// An MCP exit error retained across cancellation while reset settling
    /// or CAT reconnection is still in progress.
    pub(crate) mcp_pending_exit_error: Option<Error>,
    /// Publishes link-health transitions; see [`Radio::link_state`].
    link_state_tx: tokio::sync::watch::Sender<LinkState>,
    /// Whether auto-info was enabled by the caller; re-asserted by
    /// [`Radio::reconnect`].
    pub(crate) auto_info_enabled: bool,
    /// Last successful GPS config `(gps_enabled, pc_output)`;
    /// re-asserted by [`Radio::reconnect`].
    pub(crate) gps_config: Option<(bool, bool)>,
    /// Last successful GPS NMEA sentence flags
    /// `(gga, gll, gsa, gsv, rmc, vtg)`; re-asserted by
    /// [`Radio::reconnect`].
    pub(crate) gps_sentences: Option<(bool, bool, bool, bool, bool, bool)>,
}

impl<T: Transport> std::fmt::Debug for Radio<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Radio")
            .field("codec", &self.codec)
            .field(
                "notifications",
                &format_args!("broadcast::Sender({})", self.notifications.receiver_count()),
            )
            .field("timeout", &self.timeout)
            .field("mode_a", &self.mode_a)
            .field("mode_b", &self.mode_b)
            .field("mcp_speed", &self.mcp_speed)
            .field("last_cmd_time", &self.last_cmd_time)
            .field("gm_poisoned", &self.gm_poisoned)
            .finish_non_exhaustive()
    }
}

impl<T: Transport> Radio<T> {
    /// Create a new `Radio` instance over the given transport.
    ///
    /// # Errors
    ///
    /// Returns an error if the initial connection setup fails.
    #[expect(
        clippy::unused_async,
        reason = "Public API contract: `connect` is async so callers can `.await` it uniformly \
                  with sibling constructors like `connect_with_tnc_exit` which do perform I/O. \
                  Keeping both async lets users swap constructors without changing call sites."
    )]
    pub async fn connect(transport: T) -> Result<Self, Error> {
        tracing::info!("connecting to radio");
        let (tx, _rx) = tokio::sync::broadcast::channel(64);
        let (link_tx, _link_rx) = tokio::sync::watch::channel(LinkState::Up);
        Ok(Self {
            transport,
            codec: Codec::new(),
            notifications: tx,
            timeout: DEFAULT_TIMEOUT,
            mode_a: None,
            mode_b: None,
            mcp_speed: programming::McpSpeed::default(),
            last_cmd_time: None,
            desynced: false,
            gm_poisoned: false,
            mcp_phase: McpPhase::Inactive,
            mcp_saved_timeout: None,
            mcp_pending_exit_error: None,
            link_state_tx: link_tx,
            auto_info_enabled: false,
            gps_config: None,
            gps_sentences: None,
        })
    }

    /// Connect with a TNC exit preamble for robustness.
    ///
    /// If the radio was left in KISS/TNC mode (e.g., by a crashed application),
    /// normal CAT commands will fail. This method sends the same exit sequence
    /// that Kenwood's ARFC-D75 software uses before starting CAT communication:
    ///
    /// 1. Two empty frames
    /// 2. 300ms delay
    /// 3. ETX byte (0x03)
    /// 4. KISS Return frame (`C0 FF C0`), the exit the KISS protocol
    ///    itself defines. A radio left in KISS mode (e.g. by a crashed
    ///    APRS session) ignores every ASCII byte below, so this frame
    ///    is the only thing that can bring it back; to a radio in CAT
    ///    mode the three bytes are line noise flushed by the next
    ///    leading `\r`.
    /// 5. `\rTC 1\r` (TNC exit command)
    /// 6. `TN 0,0\r` (returns from MMDVM/packet modes)
    ///
    /// After the preamble, the radio should be in normal CAT mode regardless
    /// of its previous state.
    ///
    /// # Errors
    ///
    /// Returns an error if the transport connection fails or if any
    /// preamble write fails: a write failure means the recovery
    /// sequence never reached the radio, and reporting success would
    /// leave the caller debugging mysterious first-command failures.
    pub async fn connect_safe(transport: T) -> Result<Self, Error> {
        tracing::info!("connecting with TNC exit preamble");
        let mut radio = Self::connect(transport).await?;

        // The radio may legitimately not RESPOND to any of these (it
        // was never in TNC mode), but the WRITES must succeed: a
        // failed write means a broken port, not a quiet radio.
        // Send empty frames to wake up any stale connection.
        radio
            .transport
            .write(b"\r")
            .await
            .map_err(Error::Transport)?;
        radio
            .transport
            .write(b"\r")
            .await
            .map_err(Error::Transport)?;
        tokio::time::sleep(Duration::from_millis(300)).await;

        // ETX (part of the ARFC-D75 wake sequence).
        radio
            .transport
            .write(&[0x03])
            .await
            .map_err(Error::Transport)?;
        // KISS Return frame: the actual KISS-mode exit. A radio stuck
        // in KISS mode discards all the ASCII bytes in this preamble as
        // inter-frame garbage; this FEND-framed command is the only
        // recovery path. Same bytes `AprsClient::stop` sends.
        radio
            .transport
            .write(&[0xC0, 0xFF, 0xC0])
            .await
            .map_err(Error::Transport)?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        // TC 1 exits the built-in packet TNC mode.
        radio
            .transport
            .write(b"\rTC 1\r")
            .await
            .map_err(Error::Transport)?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        // TN 0,0 turns the built-in TNC off entirely (exits APRS, KISS,
        // and MMDVM packet modes; plain CAT operation remains).
        radio
            .transport
            .write(b"TN 0,0\r")
            .await
            .map_err(Error::Transport)?;
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Drain any buffered responses from the mode exit commands.
        let mut drain_buf = [0u8; 4096];
        drop(
            tokio::time::timeout(
                Duration::from_millis(500),
                radio.transport.read(&mut drain_buf),
            )
            .await,
        );

        Ok(radio)
    }

    /// Subscribe to auto-info notifications.
    ///
    /// When auto-info is enabled (`set_auto_info(true)`), the radio pushes
    /// unsolicited status updates. These are routed to all subscribers.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Response> {
        self.notifications.subscribe()
    }

    /// Verify the radio identity. Sends the ID command and checks the response.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] with [`ProtocolError::UnexpectedResponse`]
    /// if the radio does not return a `RadioId` response.
    /// Returns [`Error::Transport`] if communication fails.
    pub async fn identify(&mut self) -> Result<RadioInfo, Error> {
        tracing::info!("identifying radio");
        let response = self.execute(Command::GetRadioId).await?;
        match response {
            Response::RadioId { model } => {
                tracing::info!(model = %model, "radio identified");
                Ok(RadioInfo { model })
            }
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "RadioId".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the timeout duration for command execution.
    ///
    /// Defaults to 5 seconds. Commands that do not receive a response
    /// within this duration return [`Error::Timeout`].
    pub const fn set_timeout(&mut self, duration: Duration) {
        self.timeout = duration;
    }

    /// Set the MCP transfer speed for programming mode operations.
    ///
    /// The default is [`McpSpeed::Safe`] (9600 baud throughout, ~55 s
    /// for a full dump). Set to [`McpSpeed::Fast`] to switch the serial
    /// port to 115200 baud after the handshake (~8 s for a full dump),
    /// matching the fast MCP transfer mode.
    ///
    /// See [`McpSpeed`] for platform compatibility caveats.
    ///
    /// [`McpSpeed`]: programming::McpSpeed
    /// [`McpSpeed::Safe`]: programming::McpSpeed::Safe
    /// [`McpSpeed::Fast`]: programming::McpSpeed::Fast
    pub const fn set_mcp_speed(&mut self, speed: programming::McpSpeed) {
        self.mcp_speed = speed;
    }

    /// Execute a raw command and return the parsed response.
    ///
    /// Before sending, this method checks whether the command is compatible
    /// with the cached band mode. If a mismatch is detected, a
    /// `tracing::warn` is emitted but the command is **not** blocked --
    /// advanced users may have valid reasons to send raw commands in any
    /// state.
    ///
    /// After a successful response, mode state is automatically updated
    /// when VM commands are observed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RadioError`] if the radio replies with `?`.
    /// Returns [`Error::NotAvailable`] if the radio replies with `N`.
    /// Returns [`Error::Timeout`] if no response arrives within the configured timeout.
    /// Returns [`Error::Transport`] if the connection is lost or I/O fails.
    /// Returns [`Error::Protocol`] if the response cannot be parsed.
    pub async fn execute(&mut self, cmd: Command) -> Result<Response, Error> {
        let cmd_name = command_name(&cmd);
        let timeout_dur = self.timeout;
        tracing::debug!(cmd = %cmd_name, "executing command");

        // 0. Refuse CAT while an interrupted MCP session may have left
        //    the radio in PROG MCP mode (binary protocol, CAT dead).
        if self.mcp_phase != McpPhase::Inactive {
            return Err(Error::McpInterrupted);
        }

        self.require_unpoisoned_gm_stream()?;

        // The repurposed GM command is available only through the borrowed
        // reader returned by `qualify_mem_read_for`. A raw command has no proof
        // token and must never reach the wire.
        if matches!(cmd, Command::ReadMemory { .. }) {
            return Err(Error::MemoryReadNotQualified);
        }

        // 0.5. Warn if the command is likely to fail in the current mode.
        if let Some(warning) = self.check_mode_compatibility(&cmd) {
            tracing::warn!(cmd = %cmd_name, warning, "command may fail in current mode");
        }

        // 1. Enforce 5ms minimum inter-command spacing (per ARFC-D75 RE).
        if let Some(last) = self.last_cmd_time {
            let elapsed = last.elapsed();
            if elapsed < Duration::from_millis(5) {
                tokio::time::sleep(Duration::from_millis(5).saturating_sub(elapsed)).await;
            }
        }

        // 1.5. After a timeout, the previous command's response may
        //      still arrive late. Drain and reroute it BEFORE sending
        //      a new command, or a retry with the same mnemonic would
        //      consume the stale answer as its own.
        if self.desynced {
            self.drain_stale_input().await;
            self.desynced = false;
        }

        // 2. Serialize command to wire format.
        let wire = protocol::serialize(&cmd);

        // 3. Write to transport, bounded by the command timeout: a
        //    dying link can wedge inside a blocking platform write
        //    (macOS IOBluetooth `writeSync:` against a rebooting radio
        //    never returns), and an unbounded await here would hang
        //    the whole command loop instead of surfacing a timeout.
        tracing::trace!(cmd = %cmd_name, wire = ?String::from_utf8_lossy(&wire).trim(), "TX");
        match tokio::time::timeout(timeout_dur, self.transport.write(&wire)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = self.link_state_tx.send_replace(LinkState::Down);
                return Err(Error::Transport(e));
            }
            Err(_elapsed) => {
                tracing::error!(cmd = %cmd_name, timeout = ?timeout_dur, "transport write timed out");
                let _ = self.link_state_tx.send_replace(LinkState::Down);
                self.codec.clear();
                self.desynced = true;
                return Err(Error::Timeout(timeout_dur));
            }
        }
        self.last_cmd_time = Some(tokio::time::Instant::now());

        // 4. Read response bytes (loop until codec has a complete frame),
        //    wrapped in a timeout. With AI mode enabled, unsolicited
        //    notifications may arrive interleaved with command responses.
        //    Match the frame's mnemonic to the command we sent; route
        //    mismatches to the notification broadcast channel.
        let result = tokio::time::timeout(timeout_dur, self.read_matched_response(&cmd)).await;

        match result {
            Ok(inner) => {
                // A transport-level failure during the read means the
                // link itself is gone, not just this command.
                if matches!(&inner, Err(Error::Transport(_))) {
                    let _ = self.link_state_tx.send_replace(LinkState::Down);
                }
                // 4. Track mode changes from successful VM responses.
                self.track_mode_from_response(&cmd, &inner);
                inner
            }
            Err(_elapsed) => {
                tracing::error!(cmd = %cmd_name, timeout = ?timeout_dur, "command timed out");
                // The response (whole or partial) may still arrive.
                // Drop any partial frame now and drain the rest before
                // the next command (see `desynced`).
                self.codec.clear();
                self.desynced = true;
                Err(Error::Timeout(timeout_dur))
            }
        }
    }

    /// Read frames until one answers `cmd`.
    ///
    /// `?`/`N` are always taken as answers to the in-flight command.
    /// Anything else that doesn't match the command's mnemonic (or
    /// matches it but carries the wrong band, since AI pushes reuse the
    /// read mnemonics) is unsolicited: parse successes go to
    /// subscribers, failures are dropped as diagnostics, and neither
    /// is ever fatal to the in-flight command.
    async fn read_matched_response(&mut self, cmd: &Command) -> Result<Response, Error> {
        let cmd_name = command_name(cmd);
        let expected_mnemonic = cmd_name;
        let mut buf = [0u8; 1024];
        loop {
            let n = self
                .transport
                .read(&mut buf)
                .await
                .map_err(Error::Transport)?;
            if n == 0 {
                tracing::error!(cmd = %cmd_name, "transport disconnected during read");
                return Err(Error::Transport(
                    crate::error::TransportError::Disconnected(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "connection closed",
                    )),
                ));
            }
            if let Some(chunk) = buf.get(..n) {
                self.codec.feed(chunk);
            }
            while let Some(frame) = self.codec.next_frame() {
                // Frames are CR-terminated ASCII: "MNEMONIC PAYLOAD\r"
                // e.g. "FQ 0,0145520000\r", "BY 1,1\r", "?\r", "N\r".
                // Extract the 2-letter mnemonic before the space.
                let frame_str = String::from_utf8_lossy(&frame);
                let frame_mnemonic = frame_str
                    .split_once(' ')
                    .map_or_else(|| frame_str.trim(), |(m, _)| m);

                tracing::trace!(cmd = %cmd_name, frame = ?frame_str.trim(), "RX");

                // Error/not-available are always responses to the current command.
                if frame_mnemonic == "?" {
                    return Err(Error::RadioError);
                }
                if frame_mnemonic == "N" {
                    return Err(Error::NotAvailable);
                }

                if frame_mnemonic != expected_mnemonic {
                    match protocol::parse(&frame) {
                        Ok(unsolicited) => {
                            tracing::debug!(
                                expected = expected_mnemonic,
                                got = frame_mnemonic,
                                "unsolicited AI notification"
                            );
                            drop(self.notifications.send(unsolicited));
                        }
                        Err(e) => {
                            tracing::debug!(
                                error = %e,
                                frame = ?frame_str.trim(),
                                "discarding unparseable unsolicited frame"
                            );
                        }
                    }
                    continue;
                }

                // Our mnemonic: a parse failure here IS a real
                // protocol error.
                let response = protocol::parse(&frame).map_err(Error::Protocol)?;

                // Same mnemonic, wrong band: a band-B push must not
                // answer a band-A query.
                if let (Some(cmd_band), Some(resp_band)) = (
                    protocol::command_band(cmd),
                    protocol::response_band(&response),
                ) && cmd_band != resp_band
                {
                    tracing::debug!(
                        expected_band = ?cmd_band,
                        got_band = ?resp_band,
                        "band-mismatched push routed as unsolicited"
                    );
                    drop(self.notifications.send(response));
                    continue;
                }

                return Ok(response);
            }
        }
    }

    /// Drain frames the radio sent while no command was in flight,
    /// typically a late response arriving after its command already
    /// timed out. Parseable frames are rerouted to the notification
    /// channel; `?`/`N` and garbage are dropped, since they cannot be
    /// attributed to any command.
    async fn drain_stale_input(&mut self) {
        let mut buf = [0u8; 1024];
        loop {
            match tokio::time::timeout(Duration::from_millis(2), self.transport.read(&mut buf))
                .await
            {
                Ok(Ok(n)) if n > 0 => {
                    if let Some(chunk) = buf.get(..n) {
                        self.codec.feed(chunk);
                    }
                }
                // Timeout, EOF, or read error: nothing more to drain.
                // A broken transport surfaces on the next command.
                _ => break,
            }
        }
        while let Some(frame) = self.codec.next_frame() {
            match protocol::parse(&frame) {
                Ok(stale) => {
                    tracing::warn!(
                        frame = ?String::from_utf8_lossy(&frame).trim(),
                        "rerouting stale response received after a timeout"
                    );
                    drop(self.notifications.send(stale));
                }
                Err(e) => {
                    tracing::debug!(error = %e, "dropping stale unparseable frame");
                }
            }
        }
    }

    /// Returns the cached VFO/Memory mode for a band, if known.
    ///
    /// Mode is only tracked for Band A and Band B (the two main VFOs).
    /// Returns `None` for other bands or until the first VM command for
    /// that band is observed.
    #[must_use]
    pub const fn get_cached_mode(&self, band: Band) -> Option<RadioMode> {
        match band {
            Band::A => self.mode_a,
            Band::B => self.mode_b,
            _ => None,
        }
    }

    /// Check if a command is likely to fail in the current cached mode.
    ///
    /// Returns a human-readable warning string if a mismatch is detected,
    /// or `None` if the command is compatible (or the mode is unknown).
    const fn check_mode_compatibility(&self, cmd: &Command) -> Option<&'static str> {
        match cmd {
            Command::SetFrequency { band, .. } | Command::SetFrequencyFull { band, .. } => {
                match self.get_cached_mode(*band) {
                    Some(RadioMode::Vfo) | None => None,
                    Some(_) => {
                        Some("SetFrequency requires VFO mode \u{2014} use tune_frequency() instead")
                    }
                }
            }
            Command::RecallMemoryChannel { band, .. } => match self.get_cached_mode(*band) {
                Some(RadioMode::Memory) | None => None,
                Some(_) => Some(
                    "RecallMemoryChannel requires Memory mode \u{2014} use tune_channel() instead",
                ),
            },
            _ => None,
        }
    }

    /// Update cached mode state from a command/response pair.
    fn track_mode_from_response(&mut self, cmd: &Command, response: &Result<Response, Error>) {
        // Only track on successful VM responses.
        if let Ok(Response::VfoMemoryMode { band, mode }) = response {
            self.update_cached_mode(*band, *mode);
        }
        // Also track mode when we send a SetVfoMemoryMode command and it succeeds.
        if let Command::SetVfoMemoryMode { band, mode } = cmd
            && response.is_ok()
        {
            self.update_cached_mode(*band, *mode);
        }
    }

    /// Update the cached mode for a band from a [`VfoMemoryMode`] value.
    fn update_cached_mode(&mut self, band: Band, mode: VfoMemoryMode) {
        let radio_mode = RadioMode::from_vfo_mode(mode);
        match band {
            Band::A => {
                tracing::debug!(?radio_mode, "updated cached mode for band A");
                self.mode_a = Some(radio_mode);
            }
            Band::B => {
                tracing::debug!(?radio_mode, "updated cached mode for band B");
                self.mode_b = Some(radio_mode);
            }
            _ => {
                // Sub-bands don't have independent mode tracking.
            }
        }
    }

    /// Disconnect from the radio, consuming the `Radio` instance.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if closing the connection fails.
    pub async fn disconnect(mut self) -> Result<(), Error> {
        tracing::info!("disconnecting from radio");
        self.transport.close().await.map_err(Error::Transport)
    }

    /// Write raw bytes to the underlying transport.
    ///
    /// Use this for protocol detection (e.g. sending MMDVM frames to
    /// check if the radio is in gateway mode). No framing or parsing
    /// is applied.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MemoryReadStreamPoisoned`] after an incomplete strict
    /// GM exchange, or [`Error::Transport`] if the write fails.
    pub async fn transport_write(&mut self, data: &[u8]) -> Result<(), Error> {
        self.require_unpoisoned_gm_stream()?;
        self.transport.write(data).await.map_err(Error::Transport)
    }

    /// Read raw bytes from the underlying transport.
    ///
    /// Use this for protocol detection. No framing or parsing is applied.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MemoryReadStreamPoisoned`] after an incomplete strict
    /// GM exchange, or [`Error::Transport`] if the read fails.
    pub async fn transport_read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        self.require_unpoisoned_gm_stream()?;
        self.transport.read(buf).await.map_err(Error::Transport)
    }

    /// Refuse every non-reconnect I/O path after an incomplete strict GM
    /// exchange.
    pub(crate) const fn require_unpoisoned_gm_stream(&self) -> Result<(), Error> {
        if self.gm_poisoned {
            Err(Error::MemoryReadStreamPoisoned)
        } else {
            Ok(())
        }
    }

    /// Close the underlying transport without consuming the `Radio`.
    ///
    /// This is used before reconnecting to ensure Bluetooth RFCOMM
    /// resources are fully released before a new connection is opened.
    /// The `Radio` is left in a non-functional state: only reassignment
    /// or drop should follow.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if closing fails.
    pub async fn close_transport(&mut self) -> Result<(), Error> {
        tracing::info!("closing transport for reconnect");
        self.transport.close().await.map_err(Error::Transport)
    }

    /// Watch link-health transitions.
    ///
    /// The receiver observes [`LinkState::Down`] when a command
    /// surfaces a transport error and [`LinkState::Up`] again after a
    /// successful [`Radio::reconnect`].
    #[must_use]
    pub fn link_state(&self) -> tokio::sync::watch::Receiver<LinkState> {
        self.link_state_tx.subscribe()
    }

    /// Re-establish a dropped link on the same transport identity.
    ///
    /// Closes what remains of the old connection, asks the transport to
    /// [`reopen`](crate::transport::Transport::reopen), verifies the
    /// radio answers by re-running [`identify`](Radio::identify), and
    /// restores auto-information and GPS streaming state if they were
    /// enabled. In-flight commands are never replayed: whatever failed
    /// stays failed, and the caller decides what to re-issue.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the transport cannot reopen, or
    /// any error from the identify and state-restore commands.
    pub async fn reconnect(&mut self) -> Result<(), Error> {
        self.reopen_and_identify().await?;
        self.restore_state_after_reconnect().await
    }

    /// Reopen the transport and prove that the new link answers CAT.
    ///
    /// Kept separate from cached-state restoration so MCP recovery can
    /// clear its programming-mode poison at the exact point identity is
    /// proved, before any optional AI/GPS state is re-applied.
    async fn reopen_and_identify(&mut self) -> Result<(), Error> {
        tracing::info!("reconnecting radio link");
        if self.mcp_phase != McpPhase::Inactive {
            return Err(Error::McpInterrupted);
        }
        // Keep the public stream poisoned across every await. The private
        // identity command below is the only operation allowed to bypass this
        // gate. Cancellation therefore cannot expose an in-flight ID reply to
        // a later public command.
        self.gm_poisoned = true;
        self.desynced = true;
        let _ = self.link_state_tx.send_replace(LinkState::Down);
        if let Err(e) = self.close_transport().await {
            tracing::debug!(error = %e, "close before reopen failed (link already dead)");
        }
        self.transport.reopen().await.map_err(Error::Transport)?;
        // Fresh link: drop any half-parsed frame and stale-response
        // bookkeeping from the dead connection.
        self.codec.clear();
        self.desynced = false;
        self.last_cmd_time = None;

        self.prove_reopened_thd75_identity().await?;

        // No await may appear between proof and clearing the poison.
        self.desynced = false;
        self.gm_poisoned = false;
        Ok(())
    }

    /// Restore caller-selected streaming state after CAT identity is proved.
    ///
    /// General [`Radio::reconnect`] semantics remain all-or-nothing: the
    /// link is announced as up only after every requested state restore
    /// succeeds.
    async fn restore_state_after_reconnect(&mut self) -> Result<(), Error> {
        if self.auto_info_enabled {
            self.set_auto_info(true).await?;
        }
        if let Some((gps_enabled, pc_output)) = self.gps_config {
            self.set_gps_config(gps_enabled, pc_output).await?;
        }
        if let Some((gga, gll, gsa, gsv, rmc, vtg)) = self.gps_sentences {
            self.set_gps_sentences(gga, gll, gsa, gsv, rmc, vtg).await?;
        }
        let _ = self.link_state_tx.send_replace(LinkState::Up);
        tracing::info!("radio link restored");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;
    use crate::types::Band;
    use std::time::Duration;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn radio_connect_and_identify() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        let mut radio = Radio::connect(mock).await?;
        let info = radio.identify().await?;
        assert!(info.model.contains("TH-D75"));
        Ok(())
    }

    #[tokio::test]
    async fn radio_execute_raw_command() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"FV\r", b"FV 1.03.000\r");
        let mut radio = Radio::connect(mock).await?;
        let response = radio.execute(Command::GetFirmwareVersion).await?;
        let Response::FirmwareVersion { version } = &response else {
            return Err(format!("expected FirmwareVersion, got {response:?}").into());
        };
        assert_eq!(version, "1.03.000");
        Ok(())
    }

    #[tokio::test]
    async fn radio_error_response() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"FQ 0\r", b"?\r");
        let mut radio = Radio::connect(mock).await?;
        let result = radio.execute(Command::GetFrequency { band: Band::A }).await;
        assert!(
            matches!(result, Err(Error::RadioError)),
            "expected RadioError, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn radio_disconnect() -> TestResult {
        let mock = MockTransport::new();
        let radio = Radio::connect(mock).await?;
        radio.disconnect().await?;
        Ok(())
    }

    #[tokio::test]
    async fn subscribe_returns_receiver() -> TestResult {
        let mock = MockTransport::new();
        let radio = Radio::connect(mock).await?;
        let _rx = radio.subscribe();
        // Just verify it compiles and doesn't panic
        Ok(())
    }

    #[tokio::test]
    async fn set_auto_info_sends_command() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"AI 1\r", b"AI 1\r");
        let mut radio = Radio::connect(mock).await?;
        radio.set_auto_info(true).await?;
        Ok(())
    }

    #[tokio::test]
    async fn multiple_subscribers_receive_notifications() -> TestResult {
        let mock = MockTransport::new();
        let radio = Radio::connect(mock).await?;
        let _rx1 = radio.subscribe();
        let _rx2 = radio.subscribe();
        // Sending to the broadcast channel should succeed with 2 receivers
        let receiver_count = radio
            .notifications
            .send(Response::AutoInfo { enabled: true })
            .map_err(|e| format!("broadcast send failed: {e}"))?;
        assert_eq!(receiver_count, 2);
        Ok(())
    }

    #[tokio::test]
    async fn debug_impl_works() -> TestResult {
        let mock = MockTransport::new();
        let radio = Radio::connect(mock).await?;
        let debug_str = format!("{radio:?}");
        assert!(debug_str.contains("Radio"));
        Ok(())
    }

    #[tokio::test]
    async fn radio_not_available_response() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"BE\r", b"N\r");
        let mut radio = Radio::connect(mock).await?;
        let result = radio.execute(Command::GetBeep).await;
        assert!(
            matches!(result, Err(Error::NotAvailable)),
            "expected NotAvailable, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn channel_number_above_999_is_validation_error() -> TestResult {
        // The `{channel:03}` wire format silently emits 4+ digits for
        // channel > 999 (e.g. `MR 0,1500`), a malformed command the
        // radio answers with `?`. Validate before the wire.
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        let recall = radio.recall_channel(Band::A, 1500).await;
        assert!(
            matches!(recall, Err(Error::Validation(_))),
            "recall_channel(1500) must fail validation: {recall:?}"
        );
        let read = radio.read_channel(1500).await;
        assert!(
            matches!(read, Err(Error::Validation(_))),
            "read_channel(1500) must fail validation: {read:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn dstar_callsign_write_validates_length() -> TestResult {
        // DC writes previously took raw strings, so an over-length
        // callsign flowed to the wire unchecked.
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        let result = radio
            .set_dstar_callsign(crate::types::DstarSlot::new(1)?, "TOOLONGCALLSIGN", "")
            .await;
        assert!(
            matches!(result, Err(Error::Validation(_))),
            "an over-length D-STAR callsign must fail validation: {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn connect_safe_propagates_preamble_write_failure() {
        // A mock with no expectations rejects every write: the
        // TNC-exit recovery preamble never reaches the radio. That
        // must not be reported as a successful safe-connect.
        let result = Radio::connect_safe(MockTransport::new()).await;
        assert!(result.is_err(), "a dead write path must fail connect_safe");
    }

    #[tokio::test]
    async fn set_af_gain_rejects_write_out_of_range() -> TestResult {
        // AG accepts 0-99 on write (reads can exceed 99, so the type
        // is lenient), so the write path must validate.
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        let result = radio
            .set_af_gain(Band::A, crate::types::AfGainLevel::new(150))
            .await;
        assert!(
            matches!(result, Err(Error::Validation(_))),
            "AG write above 99 must fail validation: {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn nmea_interleave_does_not_fail_command() -> TestResult {
        // With GPS PC output enabled, NMEA sentences interleave with
        // CAT responses on the same stream. They must be skipped, not
        // kill the in-flight command.
        let mut mock = MockTransport::new();
        mock.expect_reads(
            b"MD 0\r",
            &[
                b"$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47\r\n",
                b"MD 0,0\r",
            ],
        );
        let mut radio = Radio::connect(mock).await?;
        let response = radio.execute(Command::GetMode { band: Band::A }).await?;
        assert!(
            matches!(response, Response::Mode { band: Band::A, .. }),
            "NMEA interleave must not fail the command: {response:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn unknown_unsolicited_mnemonic_is_skipped() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_reads(b"MD 0\r", &[b"ZZ 1\r", b"MD 0,0\r"]);
        let mut radio = Radio::connect(mock).await?;
        let response = radio.execute(Command::GetMode { band: Band::A }).await?;
        assert!(
            matches!(response, Response::Mode { band: Band::A, .. }),
            "unknown unsolicited frame must be skipped: {response:?}"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn stale_response_after_timeout_not_consumed_by_retry() -> TestResult {
        let mut mock = MockTransport::new();
        // First attempt: the link hangs and the command times out.
        mock.expect_hang(b"SQ 0\r");
        let mut radio = Radio::connect(mock).await?;
        let first = radio.execute(Command::GetSquelch { band: Band::A }).await;
        assert!(
            matches!(first, Err(Error::Timeout(_))),
            "hung link must time out: {first:?}"
        );

        // The original response arrives late (stale), then the retry
        // gets its own fresh response. The retry must return the
        // FRESH value, not the stale one.
        radio.transport.queue_read(b"SQ 0,2\r");
        radio.transport.expect(b"SQ 0\r", b"SQ 0,5\r");
        let second = radio.execute(Command::GetSquelch { band: Band::A }).await?;
        let Response::Squelch { level, .. } = second else {
            return Err(format!("expected Squelch, got {second:?}").into());
        };
        assert_eq!(
            level.as_u8(),
            5,
            "retry must not consume the stale pre-timeout response"
        );
        Ok(())
    }

    #[tokio::test]
    async fn disconnect_mid_command_returns_transport_error() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_eof(b"MD 0\r");
        let mut radio = Radio::connect(mock).await?;
        let result = radio.execute(Command::GetMode { band: Band::A }).await;
        assert!(
            matches!(result, Err(Error::Transport(_))),
            "EOF mid-command must surface as a transport error: {result:?}"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_reconnect_identity_keeps_gm_stream_poisoned() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_reopen(Ok(()));
        mock.expect_hang(b"ID\r");
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"ID\r", b"ID TH-D75\r");
        let mut radio = Radio::connect(mock).await?;
        radio.gm_poisoned = true;

        let cancelled = tokio::time::timeout(Duration::from_millis(1), radio.reconnect()).await;
        assert!(cancelled.is_err(), "outer cancellation should win");
        assert!(
            radio.gm_poisoned,
            "cancelled reconnect proof must preserve the hard poison"
        );
        assert!(matches!(
            radio.identify().await,
            Err(Error::MemoryReadStreamPoisoned)
        ));
        assert!(matches!(
            radio.transport_write(b"ID\r").await,
            Err(Error::MemoryReadStreamPoisoned)
        ));

        radio.reconnect().await?;
        assert!(
            !radio.gm_poisoned,
            "a fresh transport plus exact identity must clear the poison"
        );
        assert_eq!(radio.identify().await?.model, "TH-D75");
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn reconnect_rejects_the_wrong_radio_and_keeps_gm_poisoned() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID OTHER\r");
        let mut radio = Radio::connect(mock).await?;

        let result = radio.reconnect().await;
        assert!(matches!(result, Err(Error::Protocol(_))));
        assert!(
            radio.gm_poisoned,
            "wrong reconnect identity must retain the hard poison"
        );
        assert!(matches!(
            radio.identify().await,
            Err(Error::MemoryReadStreamPoisoned)
        ));
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn reconnect_rejects_trailing_identity_bytes_and_keeps_gm_poisoned() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\rFV 1.03\r");
        let mut radio = Radio::connect(mock).await?;

        let result = radio.reconnect().await;
        assert!(matches!(result, Err(Error::Protocol(_))));
        assert!(
            radio.gm_poisoned,
            "a non-isolated identity frame must retain the hard poison"
        );
        assert!(matches!(
            radio.identify().await,
            Err(Error::MemoryReadStreamPoisoned)
        ));
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn reconnect_refuses_unresolved_mcp_before_transport_io() -> TestResult {
        for phase in [McpPhase::Active, McpPhase::ExitSent] {
            let mut radio = Radio::connect(MockTransport::new()).await?;
            radio.mcp_phase = phase;

            let result = radio.reconnect().await;
            assert!(
                matches!(result, Err(Error::McpInterrupted)),
                "phase {phase:?} should reject reconnect: {result:?}"
            );
            assert_eq!(radio.mcp_phase, phase);
            assert!(
                !radio.gm_poisoned,
                "preflight rejection must not mutate the GM poison"
            );
            radio.transport.assert_complete();
        }
        Ok(())
    }

    #[tokio::test]
    async fn parse_failure_of_matching_response_is_protocol_error() -> TestResult {
        let mut mock = MockTransport::new();
        // Squelch level 9 is out of range (0-6). OUR response failing
        // to parse is a real protocol error, unlike unsolicited noise.
        mock.expect(b"SQ 0\r", b"SQ 0,9\r");
        let mut radio = Radio::connect(mock).await?;
        let result = radio.execute(Command::GetSquelch { band: Band::A }).await;
        assert!(
            matches!(result, Err(Error::Protocol(_))),
            "out-of-range matching response must be a protocol error: {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn band_mismatched_response_routed_as_unsolicited() -> TestResult {
        // AI pushes carry the same mnemonics as reads (MD/FQ/SQ/BY).
        // A band-B push must not answer a band-A query.
        let mut mock = MockTransport::new();
        mock.expect_reads(b"MD 0\r", &[b"MD 1,1\r", b"MD 0,0\r"]);
        let mut radio = Radio::connect(mock).await?;
        let mut notifications = radio.subscribe();
        let response = radio.execute(Command::GetMode { band: Band::A }).await?;
        assert!(
            matches!(response, Response::Mode { band: Band::A, .. }),
            "band-B push must not answer a band-A query: {response:?}"
        );
        let pushed = notifications.try_recv();
        assert!(
            matches!(pushed, Ok(Response::Mode { band: Band::B, .. })),
            "the band-B push must reach subscribers: {pushed:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn set_timeout_configurable() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        radio.set_timeout(Duration::from_millis(100));
        assert_eq!(radio.timeout, Duration::from_millis(100));
        Ok(())
    }
}
