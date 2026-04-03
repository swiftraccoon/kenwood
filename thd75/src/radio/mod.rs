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
pub mod memory;
pub mod programming;
pub mod scan;
pub mod system;
pub mod tuning;

use std::time::Duration;

use crate::error::{Error, ProtocolError};
use crate::protocol::{self, Codec, Command, Response, command_name};
use crate::transport::Transport;
use crate::types::Band;

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
    /// Converts a raw VM mode value to a `RadioMode`.
    ///
    /// Returns `None` if the value is not a recognized mode.
    #[must_use]
    pub const fn from_vm_value(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Vfo),
            1 => Some(Self::Memory),
            2 => Some(Self::Call),
            3 => Some(Self::Wx),
            _ => None,
        }
    }

    /// Returns the VM command value for this mode.
    #[must_use]
    pub const fn as_vm_value(self) -> u8 {
        match self {
            Self::Vfo => 0,
            Self::Memory => 1,
            Self::Call => 2,
            Self::Wx => 3,
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
    transport: T,
    codec: Codec,
    notifications: tokio::sync::broadcast::Sender<Response>,
    timeout: Duration,
    /// Cached mode for band A. `None` until a VM command is observed.
    mode_a: Option<RadioMode>,
    /// Cached mode for band B. `None` until a VM command is observed.
    mode_b: Option<RadioMode>,
    /// MCP programming mode transfer speed.
    pub(crate) mcp_speed: programming::McpSpeed,
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
            .finish_non_exhaustive()
    }
}

impl<T: Transport> Radio<T> {
    /// Create a new `Radio` instance over the given transport.
    ///
    /// # Errors
    ///
    /// Returns an error if the initial connection setup fails.
    #[allow(clippy::unused_async)]
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
        })
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

        // 1. Serialize command to wire format.
        let wire = protocol::serialize(&cmd);

        // 2. Write to transport.
        tracing::trace!(cmd = %cmd_name, wire = ?String::from_utf8_lossy(&wire).trim(), "TX");
        self.transport
            .write(&wire)
            .await
            .map_err(Error::Transport)?;

        // 3. Read response bytes (loop until codec has a complete frame),
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
                self.codec.feed(&buf[..n]);
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
        if let Command::SetVfoMemoryMode { band, mode } = cmd {
            if response.is_ok() {
                self.update_cached_mode(*band, *mode);
            }
        }
    }

    /// Update the cached mode for a band from a raw VM mode value.
    fn update_cached_mode(&mut self, band: Band, vm_value: u8) {
        if let Some(radio_mode) = RadioMode::from_vm_value(vm_value) {
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

    #[tokio::test]
    async fn radio_connect_and_identify() {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        let mut radio = Radio::connect(mock).await.unwrap();
        let info = radio.identify().await.unwrap();
        assert!(info.model.contains("TH-D75"));
    }

    #[tokio::test]
    async fn radio_execute_raw_command() {
        let mut mock = MockTransport::new();
        mock.expect(b"FV\r", b"FV 1.03.000\r");
        let mut radio = Radio::connect(mock).await.unwrap();
        let response = radio.execute(Command::GetFirmwareVersion).await.unwrap();
        match response {
            Response::FirmwareVersion { version } => assert_eq!(version, "1.03.000"),
            other => panic!("expected FirmwareVersion, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn radio_error_response() {
        let mut mock = MockTransport::new();
        mock.expect(b"FQ 0\r", b"?\r");
        let mut radio = Radio::connect(mock).await.unwrap();
        let result = radio.execute(Command::GetFrequency { band: Band::A }).await;
        assert!(matches!(result, Err(Error::RadioError)));
    }

    #[tokio::test]
    async fn radio_disconnect() {
        let mock = MockTransport::new();
        let radio = Radio::connect(mock).await.unwrap();
        radio.disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn subscribe_returns_receiver() {
        let mock = MockTransport::new();
        let radio = Radio::connect(mock).await.unwrap();
        let _rx = radio.subscribe();
        // Just verify it compiles and doesn't panic
    }

    #[tokio::test]
    async fn set_auto_info_sends_command() {
        let mut mock = MockTransport::new();
        mock.expect(b"AI 1\r", b"AI 1\r");
        let mut radio = Radio::connect(mock).await.unwrap();
        radio.set_auto_info(true).await.unwrap();
    }

    #[tokio::test]
    async fn multiple_subscribers_receive_notifications() {
        let mock = MockTransport::new();
        let radio = Radio::connect(mock).await.unwrap();
        let _rx1 = radio.subscribe();
        let _rx2 = radio.subscribe();
        // Sending to the broadcast channel should succeed with 2 receivers
        let sent = radio
            .notifications
            .send(Response::AutoInfo { enabled: true });
        assert!(sent.is_ok());
        assert_eq!(sent.unwrap(), 2);
    }

    #[tokio::test]
    async fn debug_impl_works() {
        let mock = MockTransport::new();
        let radio = Radio::connect(mock).await.unwrap();
        let debug_str = format!("{radio:?}");
        assert!(debug_str.contains("Radio"));
    }

    #[tokio::test]
    async fn radio_not_available_response() {
        let mut mock = MockTransport::new();
        mock.expect(b"BE\r", b"N\r");
        let mut radio = Radio::connect(mock).await.unwrap();
        let result = radio.execute(Command::GetBeep).await;
        assert!(matches!(result, Err(Error::NotAvailable)));
    }

    #[tokio::test]
    async fn set_timeout_configurable() {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await.unwrap();
        radio.set_timeout(Duration::from_millis(100));
        assert_eq!(radio.timeout, Duration::from_millis(100));
    }
}
