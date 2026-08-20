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
pub mod automation;
#[cfg(any(feature = "aprs", feature = "dstar", test))]
mod cat_restore_state;
pub mod diagnostics;
pub mod dstar;
pub mod freq;
pub mod gps;
pub mod if_tap;
#[cfg(feature = "aprs")]
pub mod kiss_session;
mod mcp_offsets;
pub mod memory;
pub mod memory_read;
pub mod menu;
#[cfg(feature = "dstar")]
pub mod mmdvm_session;
pub mod packet;
pub mod programming;
pub mod raw_protocol_session;
mod recovery;
pub use recovery::DesyncedRadio;
mod response_correlation;
pub mod scan;
pub mod state_monitor;
pub mod system;
pub mod terminal_mode;
pub mod tuning;

use std::time::Duration;

use crate::error::{Error, ProtocolError, TransportError};
use crate::protocol::{self, Codec, Command, Response, command_name};
use crate::transport::Transport;
use crate::types::{Band, FirmwareIdentity, RadioModel, TuningMode};
use response_correlation::correlate;

/// Default timeout for command execution (5 seconds).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Required idle interval after the packet-mode recovery preamble.
const CAT_RECOVERY_QUIET_WINDOW: Duration = Duration::from_millis(500);

/// Hard wall-clock bound for draining replies and binary residue produced by
/// the recovery preamble.
const CAT_RECOVERY_DRAIN_LIMIT: Duration = Duration::from_secs(5);

/// Hard byte bound for recovery residue. Packet-mode exit replies are tiny;
/// crossing this bound means the line is still actively owned by another
/// protocol and must not be presented as CAT-ready.
const CAT_RECOVERY_DRAIN_BYTE_LIMIT: usize = 64 * 1024;

/// Exact firmware identity used by the Azimuth automation build.
pub const AZIMUTH_AUTOMATION_FIRMWARE: &str = "1.03.AZM";

/// Exact vendor firmware identities observed with the standard bare `GM` and
/// `GW` command meanings.
pub const STANDARD_CAT_FIRMWARE_IDENTITIES: &[&str] = &["1.03", "1.03.000"];

/// CAT command profile selected from the radio's exact firmware identity.
///
/// The Azimuth automation firmware deliberately repurposes bare `GM` and
/// `GW`, so high-level stock operations using those mnemonics must not put
/// them on the wire. Unrecognized identities are also denied those commands;
/// a future or custom image must not inherit stock meanings by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareProfile {
    /// Standard Kenwood CAT command meanings.
    StandardCat,
    /// Azimuth automation commands occupy the stock bare `GM`/`GW` slots.
    AzimuthAutomation,
    /// Firmware whose colliding CAT command meanings have not been qualified.
    Unknown,
}

impl FirmwareProfile {
    /// Classify one exact CAT `FV` response value.
    #[must_use]
    pub fn from_identity(identity: &FirmwareIdentity) -> Self {
        match identity.as_str() {
            AZIMUTH_AUTOMATION_FIRMWARE => Self::AzimuthAutomation,
            version if STANDARD_CAT_FIRMWARE_IDENTITIES.contains(&version) => Self::StandardCat,
            _ => Self::Unknown,
        }
    }

    /// Whether bare `GM` retains its stock GPS-mode meaning.
    #[must_use]
    pub const fn supports_bare_gps_mode(self) -> bool {
        matches!(self, Self::StandardCat)
    }

    /// Whether bare `GW` retains its stock gateway-mode meaning.
    #[must_use]
    pub const fn supports_bare_gateway(self) -> bool {
        matches!(self, Self::StandardCat)
    }
}

/// Information returned by [`Radio::identify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadioInfo {
    /// Exact TH-D75 model identity.
    pub model: RadioModel,
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

/// Whether the MCP byte stream is between complete exchanges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum McpWireBoundary {
    /// No MCP command, response, or ACK handshake is partially in flight.
    #[default]
    Quiescent,
    /// A command or handshake was polled but did not reach its exact terminal
    /// response; sending another byte could corrupt the radio's parser.
    Ambiguous,
}

/// Whether ordinary CAT framing has a proved request/response boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum CatState {
    /// No command or binary transition is unresolved.
    #[default]
    Ready,
    /// A complete binary-protocol response or a correlated CAT transition
    /// proved that ownership may move into the corresponding typed binary
    /// session. Ordinary CAT remains blocked.
    BinaryProven,
    /// A write may have landed without a correlated terminal response.
    RecoveryRequired,
}

/// Marks the link down if an in-flight CAT future is dropped before it proves
/// one correlated terminal response.
struct CatExchangeGuard {
    link_state_tx: tokio::sync::watch::Sender<LinkState>,
    armed: bool,
}

impl CatExchangeGuard {
    const fn new(link_state_tx: tokio::sync::watch::Sender<LinkState>) -> Self {
        Self {
            link_state_tx,
            armed: true,
        }
    }

    const fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CatExchangeGuard {
    fn drop(&mut self) {
        if self.armed {
            let _previous = self.link_state_tx.send_replace(LinkState::Down);
        }
    }
}

/// High-level async API for controlling a Kenwood TH-D75.
///
/// Generic over the transport layer: works with USB serial,
/// Bluetooth SPP, or mock transport for testing.
///
/// The `Radio` struct tracks the tuning mode of each band when VM
/// commands are sent through it, enabling mode-compatibility warnings.
/// [`tune_channel`](Radio::tune_channel) performs verified memory-mode
/// management and recall. Arbitrary `FO` writes are not exposed because their
/// exact write and read-back behavior has not been qualified.
pub struct Radio<T: Transport> {
    pub(crate) transport: T,
    pub(crate) codec: Codec,
    pub(crate) notifications: tokio::sync::broadcast::Sender<Response>,
    pub(crate) timeout: Duration,
    /// Last exact firmware identity returned by a successful `FV` command.
    pub(crate) firmware_version: Option<FirmwareIdentity>,
    /// Cached tuning mode for band A. `None` until a VM command is observed.
    pub(crate) tuning_mode_a: Option<TuningMode>,
    /// Cached tuning mode for band B. `None` until a VM command is observed.
    pub(crate) tuning_mode_b: Option<TuningMode>,
    /// Timestamp of the last command sent, used to maintain the observed
    /// minimum 5 ms gap between CAT commands.
    last_cmd_time: Option<tokio::time::Instant>,
    /// Set when a command timed out: the radio's response may still be
    /// in flight and must be drained before the next command, or a
    /// retry with the same mnemonic would consume the stale answer.
    desynced: bool,
    /// A write future or binary-mode transition ended without a proved CAT
    /// boundary. Unlike `desynced`, this must never be cleared by a short stale
    /// reply drain; only an isolated identity proof may make CAT reusable.
    pub(crate) cat_state: CatState,
    /// A strict GM exchange failed or was cancelled with bytes potentially in
    /// flight. Unlike an ordinary timeout, this cannot be cleared by a short
    /// stale-input drain; only a fresh transport or a completed strict
    /// exchange can make the stream trustworthy again.
    pub(crate) gm_poisoned: bool,
    /// MCP safety phase. Any phase other than `Inactive` blocks CAT; the
    /// `ExitSent` phase additionally prevents recovery from sending a second
    /// raw exit byte after cancellation.
    pub(crate) mcp_phase: McpPhase,
    /// Framing boundary within an active MCP session.
    pub(crate) mcp_wire_boundary: McpWireBoundary,
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
    /// Last successful GPS receiver settings, re-asserted by
    /// [`Radio::reconnect`].
    pub(crate) gps_settings: Option<crate::types::GpsSettings>,
    /// Last successful NMEA sentence selection, re-asserted by
    /// [`Radio::reconnect`].
    pub(crate) gps_sentences: Option<crate::types::NmeaSentences>,
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
            .field("tuning_mode_a", &self.tuning_mode_a)
            .field("tuning_mode_b", &self.tuning_mode_b)
            .field("last_cmd_time", &self.last_cmd_time)
            .field("cat_state", &self.cat_state)
            .field("gm_poisoned", &self.gm_poisoned)
            .finish_non_exhaustive()
    }
}

impl<T: Transport> Radio<T> {
    /// Build pristine host-side CAT state around one already-open transport.
    ///
    /// This constructor performs no I/O. Callers that are crossing a raw
    /// protocol boundary must independently reopen and identify the radio
    /// before exposing the returned value.
    fn from_transport(transport: T) -> Self {
        let (tx, _rx) = tokio::sync::broadcast::channel(64);
        let (link_tx, _link_rx) = tokio::sync::watch::channel(LinkState::Up);
        Self {
            transport,
            codec: Codec::new(),
            notifications: tx,
            timeout: DEFAULT_TIMEOUT,
            firmware_version: None,
            tuning_mode_a: None,
            tuning_mode_b: None,
            last_cmd_time: None,
            desynced: false,
            cat_state: CatState::Ready,
            gm_poisoned: false,
            mcp_phase: McpPhase::Inactive,
            mcp_wire_boundary: McpWireBoundary::Quiescent,
            mcp_saved_timeout: None,
            mcp_pending_exit_error: None,
            link_state_tx: link_tx,
            auto_info_enabled: false,
            gps_settings: None,
            gps_sentences: None,
        }
    }

    /// Create host-side CAT state around an already-open transport.
    ///
    /// This constructor performs no I/O and does not verify the radio or
    /// recover a transport left in a packet mode. Use
    /// [`Self::connect_with_tnc_exit`] when a transient `TN`-selected packet
    /// mode may still own the link.
    #[must_use]
    pub fn new(transport: T) -> Self {
        tracing::info!("creating radio state");
        Self::from_transport(transport)
    }

    /// Connect with a TNC exit preamble for robustness.
    ///
    /// If the radio was left in KISS/TNC mode (e.g., by a crashed application),
    /// normal CAT commands will fail. This method sends the same exit sequence
    /// used by Kenwood's desktop software before starting CAT communication:
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
    /// 6. `TN 0,0\r` (returns from transient `TN`-selected packet modes)
    ///
    /// This preamble targets transient APRS, KISS, and MMDVM modes selected by
    /// `TN`. It cannot disable persistent DV Gateway / Reflector Terminal
    /// Mode selected by Menu No. 650; that mode keeps the link's CAT parser
    /// unavailable until the setting is changed through another control path.
    /// This method also does not identify the radio or otherwise prove that CAT
    /// is answering; call [`Self::identify`] when that proof is required.
    ///
    /// # Errors
    ///
    /// Returns an error if any preamble write fails, the transport disconnects
    /// during the bounded residue drain, or the line never reaches a complete
    /// quiet window. Reporting success in any of those cases could let stale
    /// bytes satisfy the caller's first CAT exchange.
    pub async fn connect_with_tnc_exit(transport: T) -> Result<Self, Error> {
        tracing::info!("creating radio with TNC exit preamble");
        let mut radio = Self::new(transport);
        radio.send_cat_recovery_preamble().await?;
        Ok(radio)
    }

    /// Send the universal packet-mode exit sequence without assuming which
    /// protocol currently owns the transport.
    async fn send_cat_recovery_preamble(&mut self) -> Result<(), Error> {
        // A quiet radio need not answer these writes, but every write must
        // complete. A failed write means the recovery sequence did not reach
        // a trustworthy boundary.
        self.transport
            .write(b"\r")
            .await
            .map_err(Error::Transport)?;
        self.transport
            .write(b"\r")
            .await
            .map_err(Error::Transport)?;
        tokio::time::sleep(Duration::from_millis(300)).await;

        self.transport
            .write(&[0x03])
            .await
            .map_err(Error::Transport)?;
        self.transport
            .write(&[0xC0, 0xFF, 0xC0])
            .await
            .map_err(Error::Transport)?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.transport
            .write(b"\rTC 1\r")
            .await
            .map_err(Error::Transport)?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.transport
            .write(b"TN 0,0\r")
            .await
            .map_err(Error::Transport)?;
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Replies vary with the starting mode and may be split across several
        // reads. Drain until a complete quiet window, rather than discarding a
        // single chunk and letting a queued stale frame satisfy the caller's
        // next CAT proof.
        self.drain_cat_recovery_residue().await?;
        self.codec.clear();
        self.last_cmd_time = None;
        Ok(())
    }

    async fn drain_cat_recovery_residue(&mut self) -> Result<(), Error> {
        let started = tokio::time::Instant::now();
        let mut drained = 0_usize;
        let mut buffer = [0_u8; 4096];

        loop {
            let elapsed = started.elapsed();
            let Some(remaining) = CAT_RECOVERY_DRAIN_LIMIT.checked_sub(elapsed) else {
                return Err(cat_recovery_drain_limit_error(drained));
            };
            let wait = remaining.min(CAT_RECOVERY_QUIET_WINDOW);
            match tokio::time::timeout(wait, self.transport.read(&mut buffer)).await {
                Err(_elapsed) if wait == CAT_RECOVERY_QUIET_WINDOW => return Ok(()),
                Err(_elapsed) => return Err(cat_recovery_drain_limit_error(drained)),
                Ok(Err(TransportError::Read(source)))
                    if source.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    return Ok(());
                }
                Ok(Err(error)) => {
                    let _previous = self.link_state_tx.send_replace(LinkState::Down);
                    return Err(Error::Transport(error));
                }
                Ok(Ok(0)) => {
                    let _previous = self.link_state_tx.send_replace(LinkState::Down);
                    return Err(Error::Transport(TransportError::Disconnected(
                        std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "radio disconnected while draining packet-mode recovery residue",
                        ),
                    )));
                }
                Ok(Ok(count)) => {
                    if count > buffer.len() {
                        let _previous = self.link_state_tx.send_replace(LinkState::Down);
                        return Err(Error::Transport(TransportError::Read(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!(
                                "transport reported {count} bytes for a {}-byte CAT recovery buffer",
                                buffer.len()
                            ),
                        ))));
                    }
                    drained = drained.saturating_add(count);
                    if drained > CAT_RECOVERY_DRAIN_BYTE_LIMIT {
                        return Err(cat_recovery_drain_limit_error(drained));
                    }
                }
            }
        }
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

    /// Return the last exact firmware identity observed from `FV`, if any.
    #[must_use]
    pub const fn cached_firmware_version(&self) -> Option<&FirmwareIdentity> {
        self.firmware_version.as_ref()
    }

    /// Return the cached CAT command profile, if firmware has been queried.
    #[must_use]
    pub fn cached_firmware_profile(&self) -> Option<FirmwareProfile> {
        self.cached_firmware_version()
            .map(FirmwareProfile::from_identity)
    }

    /// Query firmware once when necessary and reject a repurposed high-level
    /// command before its colliding mnemonic reaches the wire.
    pub(crate) async fn require_firmware_command(
        &mut self,
        command: &'static str,
        supported: fn(FirmwareProfile) -> bool,
    ) -> Result<(), Error> {
        let firmware = match &self.firmware_version {
            Some(version) => version.clone(),
            None => self.get_firmware_version().await?,
        };
        if supported(FirmwareProfile::from_identity(&firmware)) {
            Ok(())
        } else {
            Err(Error::CommandUnavailableOnFirmware { command, firmware })
        }
    }

    /// Execute one typed CAT command and return its correlated response.
    ///
    /// This is the shared implementation boundary for the radio module's
    /// public, operation-specific methods. It is deliberately crate-private:
    /// exposing the complete [`Command`] enum here would let callers bypass
    /// firmware qualification, write verification, and multi-command
    /// sequencing enforced by those methods.
    ///
    /// Before sending, this method checks whether the command is compatible
    /// with the cached band mode. A mismatch remains diagnostic rather than
    /// fatal because a few qualified operations intentionally cross modes.
    ///
    /// After a successful response, mode state is automatically updated
    /// when VM commands are observed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CommandRejected`] if the radio replies with `?`.
    /// Returns [`Error::NotAvailableInCurrentMode`] if the radio replies with `N`.
    /// Returns [`Error::Timeout`] if no response arrives within the configured timeout.
    /// Returns [`Error::Transport`] if the connection is lost or I/O fails.
    /// Returns [`Error::Protocol`] if the response cannot be parsed.
    pub(crate) async fn execute(&mut self, cmd: Command) -> Result<Response, Error> {
        let cmd_name = command_name(&cmd);
        let timeout_dur = self.timeout;
        tracing::debug!(cmd = %cmd_name, "executing command");

        // 0. Refuse CAT while an interrupted MCP session may have left
        //    the radio in PROG MCP mode (binary protocol, CAT dead).
        if self.mcp_phase != McpPhase::Inactive {
            return Err(Error::McpInterrupted);
        }

        self.require_cat_ready()?;

        // The repurposed GM command is available only through the borrowed
        // reader returned by `qualify_mem_read_for`. A raw command has no proof
        // token and must never reach the wire.
        if matches!(cmd, Command::ReadMemory { .. }) {
            return Err(Error::MemoryReadNotQualified);
        }

        // Once FV has identified the Azimuth automation build, do not let raw
        // callers bypass the same GM/GW collision guard used by the typed
        // helpers. Those bare mnemonics are automation opcodes on that image,
        // not the stock GPS/gateway reads.
        if let Some(firmware) = &self.firmware_version {
            let profile = FirmwareProfile::from_identity(firmware);
            let blocked = match &cmd {
                Command::GetGpsMode if !profile.supports_bare_gps_mode() => Some("GM"),
                Command::GetGateway if !profile.supports_bare_gateway() => Some("GW"),
                _ => None,
            };
            if let Some(command) = blocked {
                return Err(Error::CommandUnavailableOnFirmware {
                    command,
                    firmware: firmware.clone(),
                });
            }
        }

        // 0.5. Warn if the command is likely to fail in the current mode.
        if let Some(warning) = self.check_tuning_mode_compatibility(&cmd) {
            tracing::warn!(cmd = %cmd_name, warning, "command may fail in current mode");
        }

        // 1. Enforce the observed 5 ms minimum inter-command spacing.
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
            if let Err(error) = self.drain_stale_input().await {
                let _ = self.link_state_tx.send_replace(LinkState::Down);
                return Err(error);
            }
            self.desynced = false;
        }

        // 2. Serialize command to wire format.
        let wire = protocol::serialize(&cmd);

        // 3. Write to transport, bounded by the command timeout: a
        //    dying link can wedge inside a blocking platform write
        //    (macOS IOBluetooth `writeSync:` against a rebooting radio
        //    never returns), and an unbounded await here would hang
        //    the whole command loop instead of surfacing a timeout.
        tracing::trace!(cmd = %cmd_name, wire = ?wire, "TX");
        // Set this before the write await. If the caller drops this future at
        // any point from here through response correlation, the field remains
        // set and every later CAT operation is refused. This closes the gap an
        // outer timeout, select, or task abort would otherwise leave.
        self.cat_state = CatState::RecoveryRequired;
        let mut exchange_guard = CatExchangeGuard::new(self.link_state_tx.clone());
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
                let framing_failed = matches!(
                    &inner,
                    Err(Error::Protocol(ProtocolError::FrameTooLong { .. }))
                );
                if matches!(&inner, Err(Error::Transport(_))) || framing_failed {
                    let _ = self.link_state_tx.send_replace(LinkState::Down);
                }
                if framing_failed {
                    self.desynced = true;
                }
                let aligned_terminal = matches!(
                    &inner,
                    Ok(_)
                        | Err(
                            Error::CommandRejected { .. } | Error::NotAvailableInCurrentMode { .. }
                        )
                );
                if aligned_terminal {
                    // Clear synchronously, with no await after the correlated
                    // response proof. A malformed matching frame remains
                    // recovery-required because its trailing bytes cannot be
                    // distinguished from a later response.
                    self.cat_state = CatState::Ready;
                    exchange_guard.disarm();
                }
                // 4. Track mode changes only from the exact VM response that
                //    completed this command.
                self.track_tuning_mode_from_response(&inner);
                if let Ok(Response::FirmwareVersion { version }) = &inner {
                    self.firmware_version = Some(version.clone());
                }
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
    /// Anything else that doesn't match the command's mnemonic, identifying
    /// fields, or exact setter echo is unsolicited: parse successes go to
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
                return Err(Error::Transport(TransportError::Disconnected(
                    std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "connection closed"),
                )));
            }
            let chunk = buf.get(..n).ok_or_else(|| {
                Error::Transport(TransportError::Read(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "transport reported {n} bytes for a {}-byte CAT response buffer",
                        buf.len()
                    ),
                )))
            })?;
            self.codec.feed(chunk).map_err(Error::Protocol)?;
            while let Some(frame) = self.codec.next_frame() {
                // Frames are CR-terminated ASCII: "MNEMONIC PAYLOAD\r"
                // e.g. "FQ 0,0145520000\r", "BY 1,1\r", "?\r", "N\r".
                // Extract the 2-letter mnemonic before the space.
                let frame_mnemonic = frame.get(..2);

                tracing::trace!(cmd = %cmd_name, frame = ?frame, "RX");

                // Error/not-available are always responses to the current command.
                if frame == b"?" {
                    return Err(Error::CommandRejected {
                        mnemonic: expected_mnemonic.to_string(),
                    });
                }
                if frame == b"N" {
                    return Err(Error::NotAvailableInCurrentMode {
                        mnemonic: expected_mnemonic.to_string(),
                    });
                }

                if frame_mnemonic != Some(expected_mnemonic.as_bytes()) {
                    match protocol::parse(&frame) {
                        Ok(unsolicited) => {
                            tracing::debug!(
                                expected = expected_mnemonic,
                                got = ?frame_mnemonic,
                                "unsolicited AI notification"
                            );
                            drop(self.notifications.send(unsolicited));
                        }
                        Err(e) => {
                            tracing::debug!(
                                error = %e,
                                frame = ?frame,
                                "discarding unparseable unsolicited frame"
                            );
                        }
                    }
                    continue;
                }

                // Our mnemonic: a parse failure here IS a real
                // protocol error.
                let response = protocol::parse(&frame).map_err(Error::Protocol)?;

                let correlation = correlate(cmd, &response);
                if !correlation.completes_command() {
                    tracing::debug!(
                        ?correlation,
                        command = ?cmd,
                        response = ?response,
                        "same-mnemonic response did not correlate; routing it as unsolicited"
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
    async fn drain_stale_input(&mut self) -> Result<(), Error> {
        let mut buf = [0u8; 1024];
        loop {
            match tokio::time::timeout(Duration::from_millis(2), self.transport.read(&mut buf))
                .await
            {
                Ok(Ok(0)) => {
                    let _previous = self.link_state_tx.send_replace(LinkState::Down);
                    return Err(Error::Transport(TransportError::Disconnected(
                        std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "radio disconnected while draining stale CAT input",
                        ),
                    )));
                }
                Ok(Ok(n)) => {
                    let Some(chunk) = buf.get(..n) else {
                        let _previous = self.link_state_tx.send_replace(LinkState::Down);
                        return Err(Error::Transport(TransportError::Read(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!(
                                "transport reported {n} bytes for a {}-byte stale-input buffer",
                                buf.len()
                            ),
                        ))));
                    };
                    if let Err(error) = self.codec.feed(chunk) {
                        let _previous = self.link_state_tx.send_replace(LinkState::Down);
                        return Err(Error::Protocol(error));
                    }
                }
                Ok(Err(error)) => {
                    if matches!(
                        &error,
                        TransportError::Read(source)
                            if source.kind() == std::io::ErrorKind::WouldBlock
                    ) {
                        break;
                    }
                    let _previous = self.link_state_tx.send_replace(LinkState::Down);
                    return Err(Error::Transport(error));
                }
                // No bytes arrived within the quiet-window deadline, so the
                // stale-input drain is complete.
                Err(_elapsed) => break,
            }
        }
        while let Some(frame) = self.codec.next_frame() {
            match protocol::parse(&frame) {
                Ok(stale) => {
                    tracing::warn!(
                        frame = ?frame,
                        "rerouting stale response received after a timeout"
                    );
                    drop(self.notifications.send(stale));
                }
                Err(e) => {
                    tracing::debug!(error = %e, "dropping stale unparseable frame");
                }
            }
        }
        Ok(())
    }

    /// Returns the cached tuning mode for a band, if known.
    ///
    /// Returns `None` until the first VM command for that band is observed.
    #[must_use]
    pub const fn cached_tuning_mode(&self, band: Band) -> Option<TuningMode> {
        match band {
            Band::A => self.tuning_mode_a,
            Band::B => self.tuning_mode_b,
        }
    }

    /// Check if a command is likely to fail in the current cached tuning mode.
    ///
    /// Returns a human-readable warning string if a mismatch is detected,
    /// or `None` if the command is compatible (or the mode is unknown).
    const fn check_tuning_mode_compatibility(&self, cmd: &Command) -> Option<&'static str> {
        match cmd {
            Command::RecallMemoryChannel { band, .. } => match self.cached_tuning_mode(*band) {
                Some(TuningMode::Memory) | None => None,
                Some(_) => {
                    Some("RecallMemoryChannel requires Memory mode; use tune_channel() instead")
                }
            },
            _ => None,
        }
    }

    /// Update cached tuning-mode state from an exactly correlated response.
    fn track_tuning_mode_from_response(&mut self, response: &Result<Response, Error>) {
        // Only track on successful VM responses.
        if let Ok(Response::TuningMode { band, mode }) = response {
            self.update_cached_tuning_mode(*band, *mode);
        }
    }

    /// Update the cached tuning mode for a band.
    fn update_cached_tuning_mode(&mut self, band: Band, mode: TuningMode) {
        match band {
            Band::A => {
                tracing::debug!(?mode, "updated cached tuning mode for band A");
                self.tuning_mode_a = Some(mode);
            }
            Band::B => {
                tracing::debug!(?mode, "updated cached tuning mode for band B");
                self.tuning_mode_b = Some(mode);
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

    /// Test-only assertion that an incomplete strict GM exchange blocks every
    /// direct transport write. Production raw I/O is available only through
    /// the consuming [`raw_protocol_session::RawProtocolSession`].
    #[cfg(test)]
    pub(crate) async fn transport_write(&mut self, data: &[u8]) -> Result<(), Error> {
        self.require_cat_ready()?;
        self.transport.write(data).await.map_err(Error::Transport)
    }

    /// Refuse ordinary CAT and raw probes until every hard stream boundary is
    /// independently recovered.
    pub(crate) const fn require_cat_ready(&self) -> Result<(), Error> {
        if self.gm_poisoned {
            return Err(Error::MemoryReadStreamPoisoned);
        }
        match self.cat_state {
            CatState::BinaryProven | CatState::RecoveryRequired => Err(Error::CatRecoveryRequired),
            CatState::Ready => Ok(()),
        }
    }

    /// Report whether ordinary CAT commands require explicit link recovery.
    ///
    /// This state is stronger than inspecting the most recent error. For
    /// example, a malformed response can return [`Error::Protocol`] while
    /// still leaving an ambiguous frame tail that makes the next command
    /// unsafe. Applications with long-lived radio handles should reconnect
    /// when this returns `true` before attempting another CAT operation.
    #[must_use]
    pub const fn cat_recovery_required(&self) -> bool {
        !matches!(self.cat_state, CatState::Ready)
            || self.gm_poisoned
            || !matches!(self.mcp_phase, McpPhase::Inactive)
            || !matches!(self.mcp_wire_boundary, McpWireBoundary::Quiescent)
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

    /// Close before an internal reopen while retaining ownership of `T`.
    async fn close_transport(&mut self) -> Result<(), Error> {
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
        self.cat_state = CatState::RecoveryRequired;
        let _ = self.link_state_tx.send_replace(LinkState::Down);
        // A reopened transport may resolve to a different physical radio or a
        // newly flashed image. Cached firmware and VFO qualifications must not
        // cross that boundary.
        self.firmware_version = None;
        self.tuning_mode_a = None;
        self.tuning_mode_b = None;
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
        self.cat_state = CatState::Ready;
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
        if let Some(settings) = self.gps_settings {
            self.set_gps_settings(settings).await?;
        }
        if let Some(sentences) = self.gps_sentences {
            self.set_gps_sentences(sentences).await?;
        }
        let _ = self.link_state_tx.send_replace(LinkState::Up);
        tracing::info!("radio link restored");
        Ok(())
    }
}

fn cat_recovery_drain_limit_error(drained: usize) -> Error {
    Error::Protocol(ProtocolError::UnexpectedResponse {
        expected: format!(
            "a quiet CAT line for {} ms after packet-mode recovery",
            CAT_RECOVERY_QUIET_WINDOW.as_millis()
        ),
        actual: format!(
            "recovery residue remained active after draining {drained} bytes within {} ms",
            CAT_RECOVERY_DRAIN_LIMIT.as_millis()
        )
        .into_bytes(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;
    use crate::types::Band;
    use std::time::Duration;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    struct InvalidReadCountTransport;

    impl Transport for InvalidReadCountTransport {
        async fn write(&mut self, _data: &[u8]) -> Result<(), TransportError> {
            Ok(())
        }

        async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
            Ok(buffer.len() + 1)
        }

        async fn close(&mut self) -> Result<(), TransportError> {
            Ok(())
        }
    }

    struct HangingWriteTransport {
        writes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Transport for HangingWriteTransport {
        async fn write(&mut self, _data: &[u8]) -> Result<(), TransportError> {
            let _previous = self
                .writes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::future::pending().await
        }

        async fn read(&mut self, _buffer: &mut [u8]) -> Result<usize, TransportError> {
            std::future::pending().await
        }

        async fn close(&mut self) -> Result<(), TransportError> {
            Ok(())
        }
    }

    #[test]
    fn firmware_profiles_require_an_exact_qualified_identity() -> TestResult {
        for identity in STANDARD_CAT_FIRMWARE_IDENTITIES {
            let identity = FirmwareIdentity::new(identity)?;
            let profile = FirmwareProfile::from_identity(&identity);
            assert_eq!(profile, FirmwareProfile::StandardCat);
            assert!(profile.supports_bare_gps_mode());
            assert!(profile.supports_bare_gateway());
        }

        let automation = FirmwareIdentity::new(AZIMUTH_AUTOMATION_FIRMWARE)?;
        let automation = FirmwareProfile::from_identity(&automation);
        assert_eq!(automation, FirmwareProfile::AzimuthAutomation);
        assert!(!automation.supports_bare_gps_mode());
        assert!(!automation.supports_bare_gateway());

        for identity in ["1.04", "custom"] {
            let identity = FirmwareIdentity::new(identity)?;
            let profile = FirmwareProfile::from_identity(&identity);
            assert_eq!(profile, FirmwareProfile::Unknown);
            assert!(!profile.supports_bare_gps_mode());
            assert!(!profile.supports_bare_gateway());
        }
        Ok(())
    }

    #[tokio::test]
    async fn radio_new_and_identify() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        let mut radio = Radio::new(mock);
        let info = radio.identify().await?;
        assert_eq!(info.model, RadioModel::ThD75);
        Ok(())
    }

    #[tokio::test]
    async fn radio_execute_raw_command() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"FV\r", b"FV 1.03.000\r");
        let mut radio = Radio::new(mock);
        let response = radio.execute(Command::GetFirmwareVersion).await?;
        let Response::FirmwareVersion { version } = &response else {
            return Err(format!("expected FirmwareVersion, got {response:?}").into());
        };
        assert_eq!(version.as_str(), "1.03.000");
        Ok(())
    }

    #[tokio::test]
    async fn radio_error_response_names_the_rejected_mnemonic() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"FQ 0\r", b"?\r");
        let mut radio = Radio::new(mock);
        let result = radio.execute(Command::GetFrequency { band: Band::A }).await;
        assert!(
            matches!(&result, Err(Error::CommandRejected { mnemonic }) if mnemonic == "FQ"),
            "rejection must carry the mnemonic the radio refused: {result:?}"
        );
        let message = result.map_or_else(|e| e.to_string(), |r| format!("{r:?}"));
        assert!(
            message.contains("FQ"),
            "operator-facing message names the command: {message}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn radio_disconnect() -> TestResult {
        let mock = MockTransport::new();
        let radio = Radio::new(mock);
        radio.disconnect().await?;
        Ok(())
    }

    #[tokio::test]
    async fn subscribe_returns_receiver() -> TestResult {
        let mock = MockTransport::new();
        let radio = Radio::new(mock);
        let _rx = radio.subscribe();
        // Just verify it compiles and doesn't panic
        Ok(())
    }

    #[tokio::test]
    async fn set_auto_info_sends_command() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"AI 1\r", b"AI 1\r");
        let mut radio = Radio::new(mock);
        radio.set_auto_info(true).await?;
        Ok(())
    }

    #[tokio::test]
    async fn state_free_auto_info_ack_requires_exact_readback() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"AI 1\r", b"AI\r");
        mock.expect(b"AI\r", b"AI 1\r");
        let mut radio = Radio::new(mock);

        radio.set_auto_info(true).await?;

        assert!(radio.auto_info_enabled);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn state_free_auto_info_ack_rejects_wrong_readback() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"AI 1\r", b"AI\r");
        mock.expect(b"AI\r", b"AI 0\r");
        let mut radio = Radio::new(mock);

        let result = radio.set_auto_info(true).await;

        assert!(
            matches!(result, Err(Error::Protocol(_))),
            "an acknowledgment without the requested state must fail: {result:?}"
        );
        assert!(
            !radio.auto_info_enabled,
            "failed readback must not update reconnect state"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn raw_colliding_commands_are_blocked_after_azimuth_firmware_is_known() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"FV\r", b"FV 1.03.AZM\r");
        let mut radio = Radio::new(mock);
        assert_eq!(radio.get_firmware_version().await?.as_str(), "1.03.AZM");

        for (command, mnemonic) in [(Command::GetGpsMode, "GM"), (Command::GetGateway, "GW")] {
            let result = radio.execute(command).await;
            assert!(
                matches!(
                    result,
                    Err(Error::CommandUnavailableOnFirmware { command, ref firmware })
                        if command == mnemonic && firmware.as_str() == "1.03.AZM"
                ),
                "expected cached-firmware collision guard for {mnemonic}, got {result:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn multiple_subscribers_receive_notifications() -> TestResult {
        let mock = MockTransport::new();
        let radio = Radio::new(mock);
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
        let radio = Radio::new(mock);
        let debug_str = format!("{radio:?}");
        assert!(debug_str.contains("Radio"));
        Ok(())
    }

    #[tokio::test]
    async fn radio_not_available_response() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"N\r");
        let mut radio = Radio::new(mock);
        let result = radio.execute(Command::GetRadioId).await;
        assert!(
            matches!(&result, Err(Error::NotAvailableInCurrentMode { mnemonic }) if mnemonic == "ID"),
            "the not-available error must carry the refused mnemonic: {result:?}"
        );
        assert_eq!(radio.cat_state, CatState::Ready);
        assert_eq!(*radio.link_state().borrow(), LinkState::Up);
        Ok(())
    }

    #[test]
    fn channel_number_above_999_is_not_constructible() {
        let invalid = crate::types::RegularChannel::new(1500);
        assert!(
            matches!(
                invalid,
                Err(crate::error::ValidationError::ChannelOutOfRange { .. })
            ),
            "channel 1500 must fail validation: {invalid:?}"
        );
    }

    #[test]
    fn dstar_callsign_write_type_rejects_overlength_input() {
        let result = crate::types::DstarCallsign::new("TOOLONGCALLSIGN");
        assert!(
            result.is_err(),
            "an over-length D-STAR callsign must not be constructible: {result:?}"
        );
    }

    #[tokio::test]
    async fn connect_with_tnc_exit_propagates_preamble_write_failure() {
        // A mock with no expectations rejects every write: the
        // TNC-exit recovery preamble never reaches the radio. That
        // must not be reported as a successful TNC-exit connection.
        let result = Radio::connect_with_tnc_exit(MockTransport::new()).await;
        assert!(
            result.is_err(),
            "a dead write path must fail connect_with_tnc_exit"
        );
    }

    #[tokio::test]
    async fn connect_with_tnc_exit_drains_every_queued_residue_chunk() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r", b"");
        mock.expect(b"\r", b"");
        mock.expect(&[0x03], b"");
        mock.expect(&[0xC0, 0xFF, 0xC0], b"");
        mock.expect(b"\rTC 1\r", b"");
        mock.expect(b"TN 0,0\r", b"");
        mock.queue_read(b"TN 0,0\r");
        mock.queue_read_delayed(b"ID TH-D75\r", 50);
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect_with_tnc_exit(mock).await?;
        let identity = radio.identify().await?;

        assert_eq!(identity.model, RadioModel::ThD75);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn set_af_gain_accepts_valid_upper_bound() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"AG 200\r", b"AG 200\r");
        let mut radio = Radio::new(mock);
        radio
            .set_af_gain(crate::types::AfGainLevel::new(200)?)
            .await?;
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
        let mut radio = Radio::new(mock);
        let response = radio
            .execute(Command::GetOperatingMode { band: Band::A })
            .await?;
        assert!(
            matches!(response, Response::OperatingMode { band: Band::A, .. }),
            "NMEA interleave must not fail the command: {response:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn unknown_unsolicited_mnemonic_is_skipped() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_reads(b"MD 0\r", &[b"ZZ 1\r", b"MD 0,0\r"]);
        let mut radio = Radio::new(mock);
        let response = radio
            .execute(Command::GetOperatingMode { band: Band::A })
            .await?;
        assert!(
            matches!(response, Response::OperatingMode { band: Band::A, .. }),
            "unknown unsolicited frame must be skipped: {response:?}"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn response_timeout_blocks_retry_until_reconnect() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_hang(b"SQ 0\r");
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"SQ 0\r", b"SQ 0,5\r");
        let mut radio = Radio::new(mock);
        let first = radio.execute(Command::GetSquelch { band: Band::A }).await;
        assert!(
            matches!(first, Err(Error::Timeout(_))),
            "hung link must time out: {first:?}"
        );

        // Even a complete late response cannot make a same-mnemonic retry
        // safe: CAT has no request IDs that could bind it to the old command.
        radio.transport.queue_read(b"SQ 0,2\r");
        let blocked = radio.execute(Command::GetSquelch { band: Band::A }).await;
        assert!(matches!(blocked, Err(Error::CatRecoveryRequired)));
        assert_eq!(*radio.link_state().borrow(), LinkState::Down);

        radio.reconnect().await?;
        let second = radio.execute(Command::GetSquelch { band: Band::A }).await?;
        let Response::Squelch { level, .. } = second else {
            return Err(format!("expected Squelch, got {second:?}").into());
        };
        assert_eq!(
            level.as_raw(),
            5,
            "reopened retry must consume only its post-recovery response"
        );
        radio.transport.assert_complete();
        radio.transport.assert_reopen_script_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn response_timeout_blocks_retry_before_any_stale_read() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_hang(b"SQ 0\r");
        let mut radio = Radio::new(mock);
        let first = radio.execute(Command::GetSquelch { band: Band::A }).await;
        assert!(matches!(first, Err(Error::Timeout(_))));

        radio.transport.queue_read(b"");
        let retry = radio.execute(Command::GetSquelch { band: Band::A }).await;
        assert!(
            matches!(retry, Err(Error::CatRecoveryRequired)),
            "recovery gate must reject before reading stale EOF: {retry:?}"
        );
        assert_eq!(*radio.link_state().borrow(), LinkState::Down);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn outer_cancellation_after_write_requires_recovery() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_partial_then_hang_with_late(b"SQ 0\r", b"SQ 0,", b"2\r");
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"SQ 0\r", b"SQ 0,5\r");
        let mut radio = Radio::new(mock);

        let cancelled = tokio::time::timeout(
            Duration::from_millis(1),
            radio.execute(Command::GetSquelch { band: Band::A }),
        )
        .await;
        assert!(cancelled.is_err(), "outer cancellation should win");
        assert_eq!(radio.cat_state, CatState::RecoveryRequired);
        assert_eq!(*radio.link_state().borrow(), LinkState::Down);

        let blocked = radio.execute(Command::GetSquelch { band: Band::A }).await;
        assert!(matches!(blocked, Err(Error::CatRecoveryRequired)));

        radio.reconnect().await?;
        let response = radio.execute(Command::GetSquelch { band: Band::A }).await?;
        assert!(matches!(
            response,
            Response::Squelch { level, .. } if level.as_raw() == 5
        ));
        radio.transport.assert_complete();
        radio.transport.assert_reopen_script_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn outer_cancellation_during_write_blocks_all_later_cat_io() -> TestResult {
        let writes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let transport = HangingWriteTransport {
            writes: std::sync::Arc::clone(&writes),
        };
        let mut radio = Radio::new(transport);

        let cancelled = tokio::time::timeout(Duration::from_millis(1), radio.identify()).await;
        assert!(cancelled.is_err(), "outer cancellation should win");
        assert_eq!(radio.cat_state, CatState::RecoveryRequired);
        assert_eq!(*radio.link_state().borrow(), LinkState::Down);
        assert_eq!(writes.load(std::sync::atomic::Ordering::SeqCst), 1);

        assert!(matches!(
            radio.identify().await,
            Err(Error::CatRecoveryRequired)
        ));
        assert_eq!(
            writes.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "blocked retry must not poll the transport write again"
        );
        Ok(())
    }

    #[tokio::test]
    async fn impossible_transport_read_count_is_rejected_and_marks_link_down() -> TestResult {
        let mut radio = Radio::new(InvalidReadCountTransport);
        let result = radio
            .execute(Command::GetOperatingMode { band: Band::A })
            .await;
        assert!(
            matches!(
                result,
                Err(Error::Transport(TransportError::Read(ref source)))
                    if source.kind() == std::io::ErrorKind::InvalidData
            ),
            "out-of-buffer read count must be rejected exactly: {result:?}"
        );
        assert_eq!(*radio.link_state().borrow(), LinkState::Down);
        Ok(())
    }

    #[tokio::test]
    async fn disconnect_mid_command_returns_transport_error() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_eof(b"MD 0\r");
        let mut radio = Radio::new(mock);
        let result = radio
            .execute(Command::GetOperatingMode { band: Band::A })
            .await;
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
        let mut radio = Radio::new(mock);
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
        assert_eq!(radio.identify().await?.model, RadioModel::ThD75);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn reconnect_rejects_the_wrong_radio_and_keeps_gm_poisoned() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID OTHER\r");
        let mut radio = Radio::new(mock);

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
        let mut radio = Radio::new(mock);

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
            let mut radio = Radio::new(MockTransport::new());
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
        let mut radio = Radio::new(mock);
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
        let mut radio = Radio::new(mock);
        let mut notifications = radio.subscribe();
        let response = radio
            .execute(Command::GetOperatingMode { band: Band::A })
            .await?;
        assert!(
            matches!(response, Response::OperatingMode { band: Band::A, .. }),
            "band-B push must not answer a band-A query: {response:?}"
        );
        let pushed = notifications.try_recv();
        assert!(
            matches!(pushed, Ok(Response::OperatingMode { band: Band::B, .. })),
            "the band-B push must reach subscribers: {pushed:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn wrong_setter_echo_is_routed_until_exact_echo_arrives() -> TestResult {
        let desired = crate::types::SquelchLevel::new(4)?;
        let mut mock = MockTransport::new();
        mock.expect_reads(b"SQ 0,4\r", &[b"SQ 0,2\r", b"SQ 0,4\r"]);
        let mut radio = Radio::new(mock);
        let mut notifications = radio.subscribe();

        let response = radio
            .execute(Command::SetSquelch {
                band: Band::A,
                level: desired,
            })
            .await?;
        assert!(
            matches!(
                response,
                Response::Squelch {
                    band: Band::A,
                    level,
                } if level == desired
            ),
            "only the exact setter echo may complete the command: {response:?}"
        );
        let pushed = notifications.try_recv();
        assert!(
            matches!(
                pushed,
                Ok(Response::Squelch {
                    band: Band::A,
                    level,
                }) if level.as_raw() == 2
            ),
            "the nonmatching state remains observable as a notification: {pushed:?}"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn wrong_vm_echo_never_completes_or_updates_the_tuning_mode_cache() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_reads(b"VM 0,1\r", &[b"VM 0,0\r"]);
        mock.pend_when_empty();
        let mut radio = Radio::new(mock);
        radio.set_timeout(Duration::from_millis(20));
        let mut notifications = radio.subscribe();

        let result = radio
            .execute(Command::SetTuningMode {
                band: Band::A,
                mode: TuningMode::Memory,
            })
            .await;
        assert!(
            matches!(result, Err(Error::Timeout(_))),
            "a wrong echo must not report setter success: {result:?}"
        );
        assert_eq!(
            radio.cached_tuning_mode(Band::A),
            None,
            "a response that did not complete the command must not poison the cache"
        );
        let pushed = notifications.try_recv();
        assert!(
            matches!(
                pushed,
                Ok(Response::TuningMode {
                    band: Band::A,
                    mode: TuningMode::Vfo,
                })
            ),
            "the observed nonmatching VM state must remain available: {pushed:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn set_timeout_configurable() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::new(mock);
        radio.set_timeout(Duration::from_millis(100));
        assert_eq!(radio.timeout, Duration::from_millis(100));
        Ok(())
    }
}
