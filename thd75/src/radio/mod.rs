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
pub mod dstar;
#[path = "freq.rs"]
pub mod freq;
pub mod gps;
pub mod kiss_session;
pub mod memory;
pub mod mmdvm_session;
pub mod programming;
pub mod scan;
pub mod service;
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
    /// VFO (Variable Frequency Oscillator) mode — direct frequency entry.
    Vfo,
    /// Memory mode — operating on a stored channel.
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

/// High-level async API for controlling a Kenwood TH-D75.
///
/// Generic over the transport layer — works with USB serial,
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
        Ok(Self {
            transport,
            codec: Codec::new(),
            notifications: tx,
            timeout: DEFAULT_TIMEOUT,
            mode_a: None,
            mode_b: None,
            mcp_speed: programming::McpSpeed::default(),
            last_cmd_time: None,
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
    /// 4. `\rTC 1\r` (TNC exit command)
    ///
    /// After the preamble, the radio should be in normal CAT mode regardless
    /// of its previous state.
    ///
    /// # Errors
    ///
    /// Returns an error if the transport connection fails.
    pub async fn connect_safe(transport: T) -> Result<Self, Error> {
        tracing::info!("connecting with TNC exit preamble");
        let mut radio = Self::connect(transport).await?;

        // Send empty frames to wake up any stale connection.
        let _ = radio.transport.write(b"\r").await;
        let _ = radio.transport.write(b"\r").await;
        tokio::time::sleep(Duration::from_millis(300)).await;

        // ETX (exit KISS mode if active).
        let _ = radio.transport.write(&[0x03]).await;
        // TC 1 exits KISS TNC mode.
        let _ = radio.transport.write(b"\rTC 1\r").await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        // TN 0,0 exits MMDVM mode (returns to APRS/normal TNC).
        let _ = radio.transport.write(b"TN 0,0\r").await;
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Drain any buffered responses from the mode exit commands.
        let mut drain_buf = [0u8; 4096];
        let _ = tokio::time::timeout(
            Duration::from_millis(500),
            radio.transport.read(&mut drain_buf),
        )
        .await;

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

        // 0. Warn if the command is likely to fail in the current mode.
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

        // 2. Serialize command to wire format.
        let wire = protocol::serialize(&cmd);

        // 3. Write to transport.
        tracing::trace!(cmd = %cmd_name, wire = ?String::from_utf8_lossy(&wire).trim(), "TX");
        self.transport
            .write(&wire)
            .await
            .map_err(Error::Transport)?;
        self.last_cmd_time = Some(tokio::time::Instant::now());

        // 4. Read response bytes (loop until codec has a complete frame),
        //    wrapped in a timeout. With AI mode enabled, unsolicited
        //    notifications may arrive interleaved with command responses.
        //    Match the frame's mnemonic to the command we sent; route
        //    mismatches to the notification broadcast channel.
        let expected_mnemonic = command_name(&cmd);
        let result = tokio::time::timeout(timeout_dur, async {
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

                    let response = protocol::parse(&frame).map_err(Error::Protocol)?;

                    // If this frame's mnemonic doesn't match what we sent,
                    // it's an unsolicited AI notification — route it to
                    // subscribers and keep waiting for our actual response.
                    if frame_mnemonic != expected_mnemonic {
                        tracing::debug!(
                            expected = expected_mnemonic,
                            got = frame_mnemonic,
                            "unsolicited AI notification"
                        );
                        let _ = self.notifications.send(response);
                        continue;
                    }

                    return Ok(response);
                }
            }
        })
        .await;

        match result {
            Ok(inner) => {
                // 4. Track mode changes from successful VM responses.
                self.track_mode_from_response(&cmd, &inner);
                inner
            }
            Err(_elapsed) => {
                tracing::error!(cmd = %cmd_name, timeout = ?timeout_dur, "command timed out");
                Err(Error::Timeout(timeout_dur))
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
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn transport_write(&mut self, data: &[u8]) -> Result<(), Error> {
        self.transport.write(data).await.map_err(Error::Transport)
    }

    /// Read raw bytes from the underlying transport.
    ///
    /// Use this for protocol detection. No framing or parsing is applied.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the read fails.
    pub async fn transport_read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        self.transport.read(buf).await.map_err(Error::Transport)
    }

    /// Close the underlying transport without consuming the `Radio`.
    ///
    /// This is used before reconnecting to ensure Bluetooth RFCOMM
    /// resources are fully released before a new connection is opened.
    /// The `Radio` is left in a non-functional state — only reassignment
    /// or drop should follow.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if closing fails.
    pub async fn close_transport(&mut self) -> Result<(), Error> {
        tracing::info!("closing transport for reconnect");
        self.transport.close().await.map_err(Error::Transport)
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
    async fn set_timeout_configurable() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        radio.set_timeout(Duration::from_millis(100));
        assert_eq!(radio.timeout, Duration::from_millis(100));
        Ok(())
    }
}
