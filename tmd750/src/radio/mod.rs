//! Async CAT control and explicitly owned MCP programming sessions.
//!
//! Start with [`Radio::new`] around an already selected transport, then
//! [`Radio::identify`] to obtain the complete model, firmware, and type tuple.
//! Opening a transport or constructing a [`Radio`] sends nothing; identity
//! comes only from [`Radio::identify`].
//!
//! # Choose an operation
//!
//! - Read ordinary state with the `get_*` methods: the demodulation mode,
//!   frequency, channel record, power, tuning mode, squelch, meter, memory
//!   selection, band roles, GPS, VOX and Bluetooth state, and
//!   [`Radio::get_dv_gateway_mode`]. The `set_*` methods and
//!   [`Radio::frequency_up`]/[`Radio::frequency_down`] change the same state
//!   with an exact echo plus readback; every setter admits only an identity
//!   in [`WRITE_TARGETS`]. Direct frequency writes, `UP`/`DW` and memory
//!   recall are the tuning writes.
//! - Transmit with [`Radio::transmit`], [`Radio::receive`] and
//!   [`Radio::transmit_for`]; send an APRS beacon with
//!   [`Radio::send_aprs_beacon`]; select the TNC mode, VOX and power with
//!   [`Radio::set_tnc_mode`], [`Radio::set_vox`] and [`Radio::power_off`].
//! - Capture standard configuration with [`Radio::backup_mcp_until_exit`], or
//!   use [`Radio::probe_mcp`] for a smaller, fixed two-fragment read.
//!   Neither operation writes settings; both interrupt ordinary radio operation.
//! - Inspect selected menu fields through [`McpSession::read_menu_snapshot`].
//!   Prepare ordinary changes with [`menu::MenuUpdatePlan`] and persistent
//!   Gateway changes with [`terminal::TerminalPlan`]. A plan is local data;
//!   applying it is a separate session.
//! - [`text_field_update`] writes one text field (the PM1 name, the PM Off MY1
//!   callsign or a channel name) and verifies it in a second session.
//!
//! # Ownership and failure
//!
//! [`Radio`] owns its transport; [`McpSession`] borrows it exclusively. Await
//! active operations to completion, retain operation and cleanup failures
//! independently, and explicitly close/drop the extracted transport. See
//! [`Radio`]'s CAT contract and [`programming`]'s executable offline example.
//! MCP exit retires the original connection; fresh connection selection and
//! identity verification belong to the caller, not an automatic library retry.

pub mod backup;
mod cat;
mod channel_batch;
pub mod menu;
mod pm1_page;
pub mod probe;
pub mod programming;
pub mod readiness;
pub mod session;
pub mod terminal;
pub mod text_field_update;
mod transmit;

use std::collections::VecDeque;
use std::fmt;
use std::time::Duration;

use crate::error::{Error, McpError, ProtocolError};
use crate::protocol::cat::{Command, LINE_TERMINATOR, Response, parse_line};
use crate::types::{
    Band, DvGatewayMode, FirmwareIdentity, OperatingMode, RadioModel, RadioType, SelectableMode,
};
use kenwood_transport::{Transport, TransportError};

pub use cat::{STEP_READBACK_ATTEMPTS, STEP_READBACK_INTERVAL, STEP_SETTLE, STEP_SPACING};
pub use channel_batch::{ChannelBatchError, ChannelWrite, ProgrammedChannels};
pub use programming::{McpJournal, McpSession, RegionImage};
pub use transmit::VOX_SETTLE;

/// Default timeout for one serial write, CAT line, or MCP exchange step.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1500);

/// Reply deadline for a `BT` write.
///
/// Switching Bluetooth on restarts the radio's Bluetooth stack, and on
/// firmware 1.02 the echo arrived about 1.6 s after the command.
pub const BLUETOOTH_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Reply deadline for the first command on a new Bluetooth connection.
///
/// On firmware 1.02 the radio holds the reply to the first command of a
/// Bluetooth connection opened within about five seconds of the previous
/// connection's close until about five seconds after that close; the command
/// is answered late, not dropped. [`Radio::new_bluetooth`] applies it to the
/// first command; after a programming exit [`Radio::new_bluetooth_after_exit`]
/// applies [`bluetooth_first_reply_timeout_after_exit`], which never returns
/// less. A host bound sized from observation, not a firmware guarantee.
pub const BLUETOOTH_FIRST_REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Time after a programming-exit acknowledgment by which the first reply on a
/// fresh Bluetooth connection arrives.
///
/// On firmware 1.02 that reply arrived 20 to 22 seconds after the exit
/// acknowledgment however soon the command was sent: a command sent earlier
/// was held, not dropped. A host bound sized from observation, not a firmware
/// guarantee.
pub const BLUETOOTH_EXIT_REPLY_BOUND: Duration = Duration::from_secs(30);

/// Reply deadline for the first command on a fresh Bluetooth connection sent
/// `since_exit` after a programming-exit acknowledgment.
///
/// Returns the time left until [`BLUETOOTH_EXIT_REPLY_BOUND`], or
/// [`BLUETOOTH_FIRST_REPLY_TIMEOUT`] when that is longer, so the deadline
/// covers the post-exit hold however quickly the connection reopened. An
/// overestimate of `since_exit` shortens the deadline; measure it from a
/// point no earlier than the acknowledgment.
#[must_use]
pub const fn bluetooth_first_reply_timeout_after_exit(since_exit: Duration) -> Duration {
    let remaining = BLUETOOTH_EXIT_REPLY_BOUND.saturating_sub(since_exit);
    if remaining.as_nanos() > BLUETOOTH_FIRST_REPLY_TIMEOUT.as_nanos() {
        remaining
    } else {
        BLUETOOTH_FIRST_REPLY_TIMEOUT
    }
}

/// Time [`Radio`] keeps reading and discarding after a reply that follows a
/// `?` rejection.
///
/// Firmware 1.02 repeats the reply to the command that follows certain
/// rejected commands, one to two milliseconds after the first copy. Each of
/// the [`REJECTION_SETTLE_COMMANDS`] commands after a `?` therefore discards
/// the bytes still buffered after its reply and anything that arrives within
/// this window.
pub const REJECTION_SETTLE_WINDOW: Duration = Duration::from_millis(20);

/// Number of commands after a `?` reply that settle for
/// [`REJECTION_SETTLE_WINDOW`] before returning.
pub const REJECTION_SETTLE_COMMANDS: u8 = 2;

/// Maximum accepted CAT reply length, excluding its carriage return.
///
/// A longer line returns [`ProtocolError::CatLineTooLong`]. This is a host-side
/// resource bound on malformed or unterminated replies; the radio's own limit
/// is unknown.
pub const MAX_CAT_LINE_BYTES: usize = 1024;

/// Reply lines that do not answer the command in flight are discarded up to
/// this many times before the exchange fails.
pub const MAX_STALE_LINES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProtocolState {
    CatReady,
    /// A CAT exchange did not complete; only `RX` is sent until its reply
    /// correlates.
    CatUncertain,
    McpReady,
    RecoveryRequired,
    Retired,
}

/// One exact `FV` and `TY` payload pair admitted by every write in this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WriteTarget {
    firmware: &'static str,
    radio_type: &'static str,
}

impl WriteTarget {
    /// The exact `FV` payload.
    #[must_use]
    pub const fn firmware(self) -> &'static str {
        self.firmware
    }

    /// The exact `TY` payload.
    #[must_use]
    pub const fn radio_type(self) -> &'static str {
        self.radio_type
    }

    /// Whether `identity` is a TM-D750 reporting exactly this pair.
    #[must_use]
    pub fn matches(self, identity: &Identity) -> bool {
        identity.model == RadioModel::TmD750
            && identity.firmware.as_str() == self.firmware
            && identity.radio_type.as_str() == self.radio_type
    }
}

impl fmt::Display for WriteTarget {
    /// Model, firmware and type separated by slashes: `TM-D750 / 1.02 / K,2,1`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} / {} / {}",
            RadioModel::TmD750,
            self.firmware,
            self.radio_type
        )
    }
}

/// Identities whose CAT setters, MCP plans and transmit commands this crate
/// sends.
///
/// The echo, readback and page-layout behavior the write paths depend on was
/// observed on a radio reporting these payloads. Every write checks the
/// connected radio against this list through [`Identity::is_write_target`]
/// before sending a byte and returns [`Error::UnsupportedWriteTarget`]
/// otherwise.
pub const WRITE_TARGETS: &[WriteTarget] = &[WriteTarget {
    firmware: "1.02",
    radio_type: "K,2,1",
}];

/// Progress of a multi-page transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Pages finished.
    pub done: usize,
    /// Pages in total.
    pub total: usize,
}

/// The `ID`, `FV` and `TY` payloads of one radio.
///
/// [`Radio::identify`] reads them from the connected radio. A plan built from
/// a captured identity is compared with the session's fresh identity before
/// any page is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The model (always TM-D750).
    pub model: RadioModel,
    /// Exact `FV` payload.
    pub firmware: FirmwareIdentity,
    /// Exact opaque `TY` payload.
    pub radio_type: RadioType,
}

impl Identity {
    /// Whether this identity is one of [`WRITE_TARGETS`].
    #[must_use]
    pub fn is_write_target(&self) -> bool {
        WRITE_TARGETS.iter().any(|target| target.matches(self))
    }
}

/// [`WRITE_TARGETS`] as `TM-D750 / 1.02 / K,2,1`, comma separated.
pub(crate) fn write_targets_list() -> String {
    WRITE_TARGETS
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

impl fmt::Display for Identity {
    /// Model, firmware and type separated by slashes: `TM-D750 / 1.02 / K,2,1`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} / {} / {}",
            self.model, self.firmware, self.radio_type
        )
    }
}

/// Typed CAT access to one caller-selected transport, owning its connection.
///
/// Construction sends no bytes. [`Self::identify`] obtains and caches the full
/// identity; ordinary getters do not implicitly identify. Setters identify
/// when needed and apply the firmware and type gate. The cache holds the
/// last successful identify result and is not invalidated by later failures, so
/// it can be stale.
///
/// # One command, one reply
///
/// Each command reads reply lines until one answers it: `?`, `N`, or the
/// typed reply whose identifying fields match ([`Command::correlates`]). Up
/// to [`MAX_STALE_LINES`] other lines, empty lines included, are discarded
/// first, which covers bytes left unread on the connection before this
/// `Radio` was constructed. After a `?` reply
/// the next [`REJECTION_SETTLE_COMMANDS`] commands also discard whatever
/// follows their own reply within [`REJECTION_SETTLE_WINDOW`], which is where
/// firmware 1.02 places the repeated reply. A `?` reply returns
/// [`ProtocolError::Rejected`] and an `N` reply [`ProtocolError::NotAvailable`].
///
/// # Deadlines and cancellation
///
/// [`Self::set_timeout`] sets a separate deadline for each transport write and
/// reply-reading step, not one deadline for an entire multi-command method.
/// Await a polled CAT operation to completion. Dropping its future during I/O,
/// a write/read failure, a timeout, or an undecodable reply leaves the managed
/// connection uncertain. After a timeout it is unknown whether the bytes
/// arrived or the requested change took effect.
///
/// Subsequent CAT and MCP operations then return
/// [`McpError::RecoveryRequired`] before I/O, with one exception: after a CAT
/// failure (not an MCP one), [`Self::receive`] still sends `RX`, so a
/// transmitter keyed by an unanswered `TX` can be released, and its correlated
/// reply makes the connection ready again. A complete, parsed rejection or
/// unexpected response leaves a synchronized boundary. No command is
/// automatically replayed, and no setting is rolled back.
///
/// # Releasing and recovering ownership
///
/// [`Self::into_transport`] permits explicit close and drop after success or
/// failure; retain a close error independently of the operation error. Dropping
/// a `Radio` cannot await asynchronous cleanup. This crate provides no generic
/// reset or CAT-resynchronization procedure: the caller must establish the
/// radio's protocol boundary and explicitly select a fresh connection before
/// further traffic. Rewrapping the old transport is not recovery.
///
/// MCP has additional borrowed-session and exit rules in [`McpSession`].
#[derive(Debug)]
pub struct Radio<T: Transport> {
    transport: T,
    timeout: Duration,
    next_reply_timeout: Option<Duration>,
    identity: Option<Identity>,
    receive_buffer: VecDeque<u8>,
    protocol_state: ProtocolState,
    mcp_entry_reply: Option<Vec<u8>>,
    settle_commands: u8,
    last_step: Option<tokio::time::Instant>,
}

impl<T: Transport> Radio<T> {
    /// Wrap a transport without sending commands.
    ///
    /// The connection must be eligible for CAT. This constructor does not reset
    /// the radio or repair an incomplete exchange. Rewrapping an old transport
    /// does not make it a fresh connection after MCP exit.
    pub const fn new(transport: T) -> Self {
        Self {
            transport,
            timeout: DEFAULT_TIMEOUT,
            next_reply_timeout: None,
            identity: None,
            receive_buffer: VecDeque::new(),
            protocol_state: ProtocolState::CatReady,
            mcp_entry_reply: None,
            settle_commands: 0,
            last_step: None,
        }
    }

    /// Wrap a newly opened native Bluetooth connection without sending
    /// commands.
    ///
    /// As [`Self::new`], with the first command's reply allowed
    /// [`BLUETOOTH_FIRST_REPLY_TIMEOUT`]: the radio holds that reply until
    /// about five seconds after the previous Bluetooth connection closed.
    /// Every later exchange uses [`DEFAULT_TIMEOUT`].
    pub const fn new_bluetooth(transport: T) -> Self {
        let mut radio = Self::new(transport);
        radio.set_next_reply_timeout(BLUETOOTH_FIRST_REPLY_TIMEOUT);
        radio
    }

    /// Wrap a native Bluetooth connection opened `since_exit` after a
    /// programming-exit acknowledgment, without sending commands.
    ///
    /// As [`Self::new`], with the first command's reply allowed
    /// [`bluetooth_first_reply_timeout_after_exit`] of `since_exit`, so the
    /// deadline reaches [`BLUETOOTH_EXIT_REPLY_BOUND`] after the exit however
    /// quickly the connection reopened.
    pub const fn new_bluetooth_after_exit(transport: T, since_exit: Duration) -> Self {
        let mut radio = Self::new(transport);
        radio.set_next_reply_timeout(bluetooth_first_reply_timeout_after_exit(since_exit));
        radio
    }

    /// Change the deadline for each serial write and each reply-reading step.
    ///
    /// After a write timeout, whether any bytes reached the radio is unknown. A
    /// CAT or MCP exchange that is interrupted remains unavailable for further
    /// commands until the connection and radio protocol boundary are recovered.
    /// A `BT` write keeps at least [`BLUETOOTH_WRITE_TIMEOUT`].
    pub const fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Give the reply to the next CAT command up to `timeout`, once.
    ///
    /// The longer of `timeout` and the regular reply deadline applies to that
    /// one command, which consumes the setting when it is sent, whatever its
    /// outcome; later commands use the regular deadline, and MCP exchanges
    /// never use it. [`Self::new_bluetooth`] and
    /// [`Self::new_bluetooth_after_exit`] set it for a new Bluetooth
    /// connection.
    pub const fn set_next_reply_timeout(&mut self, timeout: Duration) {
        self.next_reply_timeout = Some(timeout);
    }

    /// The identity proven by the last [`Radio::identify`].
    ///
    /// Returns the cached value without I/O; a later failed exchange does not
    /// clear it.
    #[must_use]
    pub const fn identity(&self) -> Option<&Identity> {
        self.identity.as_ref()
    }

    /// Give the transport back.
    ///
    /// Extraction does not restore CAT readiness. After MCP exit, close and
    /// drop this transport before opening and identifying a fresh connection.
    /// After an incomplete exchange, also restore the radio's normal protocol
    /// boundary before further traffic; a new wrapper alone is not recovery.
    #[must_use]
    pub fn into_transport(self) -> T {
        self.transport
    }

    /// Prove the radio is a TM-D750 and record its firmware and type.
    ///
    /// Sends `ID`, `FV`, and `TY` in that order, replacing the cached identity
    /// only after all three succeed. A failed refresh retains the earlier
    /// cache. Follow [`Radio`]'s deadline, cancellation, and ownership contract.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::UnexpectedIdentity`] for any other model,
    /// [`Error::Timeout`] when a write or reply step expires, and transport,
    /// parse, unexpected-response, or session-state errors otherwise.
    pub async fn identify(&mut self) -> Result<Identity, Error> {
        tracing::debug!("identifying radio");
        let model = self
            .query(Command::Identify, "Identity", |response| match response {
                Response::Identity { model } => Some(*model),
                _ => None,
            })
            .await?;
        let firmware = self
            .query(
                Command::FirmwareVersion,
                "FirmwareVersion",
                |response| match response {
                    Response::FirmwareVersion { version } => Some(version.clone()),
                    _ => None,
                },
            )
            .await?;
        let radio_type = self
            .query(Command::RadioType, "RadioType", |response| match response {
                Response::RadioType(radio_type) => Some(radio_type.clone()),
                _ => None,
            })
            .await?;
        let identity = Identity {
            model,
            firmware,
            radio_type,
        };
        tracing::debug!(
            firmware = %identity.firmware,
            radio_type = %identity.radio_type,
            "radio identified"
        );
        self.identity = Some(identity.clone());
        Ok(identity)
    }

    /// Read the current demodulation mode for one band.
    ///
    /// Sends one band-indexed `MD` query without an implicit identity check.
    /// Unknown reported values remain [`OperatingMode::Unnamed`]. Follow
    /// [`Radio`]'s deadline, cancellation, and post-error ownership contract.
    ///
    /// # Errors
    ///
    /// Returns transport, timeout, parse, rejection, unexpected-response, or
    /// session-state errors. After an error the connection may be uncertain;
    /// see [`Radio`]'s ownership contract.
    pub async fn get_operating_mode(&mut self, band: Band) -> Result<OperatingMode, Error> {
        self.query(
            Command::GetOperatingMode { band },
            "OperatingMode",
            |response| match response {
                Response::OperatingMode { mode, .. } => Some(*mode),
                _ => None,
            },
        )
        .await
    }

    /// Select a demodulation mode and prove the result with an immediate readback.
    ///
    /// FM, DV, AM and NFM were each accepted and read back on Band A at
    /// 145.190 MHz; a band refuses a mode outside its range with `N`. DR is
    /// selected with [`Self::set_tuning_mode`] and
    /// [`crate::types::TuningMode::DStarRepeater`], not here. A failed echo or
    /// readback can follow an applied change; no rollback occurs. Follow
    /// [`Radio`]'s deadline, cancellation, and post-error ownership contract.
    ///
    /// # Errors
    ///
    /// Identifies the radio first when needed and refuses any identity outside
    /// [`WRITE_TARGETS`] with [`Error::UnsupportedWriteTarget`].
    /// Returns an error when that gate fails, the write is rejected, its reply
    /// does not match, or the immediate readback differs from the request.
    pub async fn set_operating_mode(
        &mut self,
        band: Band,
        mode: SelectableMode,
    ) -> Result<(), Error> {
        self.apply(
            Command::SetOperatingMode { band, mode },
            Command::GetOperatingMode { band },
            "OperatingMode",
            OperatingMode::from(mode),
            |response| match response {
                Response::OperatingMode { mode, .. } => Some(*mode),
                _ => None,
            },
        )
        .await
    }

    /// Put one band into D-STAR DV mode with verified readback.
    ///
    /// This changes the ordinary RF demodulation mode. It does not enable the
    /// separate persistent DV Gateway or Terminal Mode setting.
    /// It has the same identity gate and cancellation contract as
    /// [`Self::set_operating_mode`].
    ///
    /// # Errors
    ///
    /// Returns the errors from [`Radio::set_operating_mode`].
    pub async fn enter_dstar(&mut self, band: Band) -> Result<(), Error> {
        self.set_operating_mode(band, SelectableMode::Dv).await
    }

    /// Read the persistent DV Gateway state through the read-only `GW` command.
    ///
    /// Sends one query without an implicit identity check. `GW` reports the
    /// Gateway mode alone; the subtype and route are stored settings read
    /// through MCP, and the modem is a separate connection. Follow
    /// [`Radio`]'s deadline, cancellation, and post-error ownership contract.
    ///
    /// # Errors
    ///
    /// Returns transport, timeout, parse, rejection, unexpected-response, or
    /// session-state errors.
    pub async fn get_dv_gateway_mode(&mut self) -> Result<DvGatewayMode, Error> {
        self.query(
            Command::GetGatewayMode,
            "GatewayMode",
            |response| match response {
                Response::GatewayMode(mode) => Some(*mode),
                _ => None,
            },
        )
        .await
    }

    /// Send one command and return the reply that answers it.
    pub(crate) async fn command(&mut self, command: &Command) -> Result<Response, Error> {
        self.require_cat()?;
        self.exchange(command).await
    }

    /// Send `RX` on a ready connection or one whose last CAT exchange did not
    /// complete, and return the reply that answers it.
    ///
    /// A correlated reply leaves the connection ready. Every other state
    /// returns the error [`Self::require_cat`] reports, so no CAT byte enters
    /// an uncertain programming session.
    pub(crate) async fn release_command(&mut self) -> Result<Response, Error> {
        if self.protocol_state != ProtocolState::CatUncertain {
            self.require_cat()?;
        }
        self.exchange(&Command::Receive).await
    }

    /// Write `command` and read until a line answers it.
    async fn exchange(&mut self, command: &Command) -> Result<Response, Error> {
        self.protocol_state = ProtocolState::CatUncertain;
        let timeout = self.reply_timeout(command);
        self.write_all(&command.encode()).await?;
        let mut stale = 0;
        loop {
            let line = self.read_line_with(timeout, command.mnemonic()).await?;
            if line.is_empty() {
                stale += 1;
                tracing::debug!(
                    command = command.mnemonic(),
                    stale,
                    "discarded an empty line"
                );
                if stale >= MAX_STALE_LINES {
                    return Err(ProtocolError::EmptyLine {
                        line: String::new(),
                    }
                    .into());
                }
                continue;
            }
            let response = parse_line(&line)?;
            if command.correlates(&response) {
                if matches!(response, Response::Rejected) {
                    self.settle_commands = REJECTION_SETTLE_COMMANDS;
                } else if self.settle_commands > 0 {
                    self.settle_commands -= 1;
                    self.discard_pending_input().await;
                }
                self.mark_cat_ready();
                return Ok(response);
            }
            stale += 1;
            tracing::debug!(
                command = command.mnemonic(),
                line = %String::from_utf8_lossy(&line),
                stale,
                "discarded a reply line that does not answer the command"
            );
            if stale >= MAX_STALE_LINES {
                return Err(unexpected(command.mnemonic(), &response));
            }
        }
    }

    /// Send one command and extract its typed reply.
    ///
    /// `?` becomes [`ProtocolError::Rejected`], `N` becomes
    /// [`ProtocolError::NotAvailable`], and any correlated reply that
    /// `extract` does not accept becomes [`ProtocolError::UnexpectedResponse`].
    pub(crate) async fn query<V>(
        &mut self,
        command: Command,
        expected: &'static str,
        extract: fn(&Response) -> Option<V>,
    ) -> Result<V, Error> {
        match self.command(&command).await? {
            Response::Rejected => Err(ProtocolError::Rejected {
                command: command.mnemonic(),
            }
            .into()),
            Response::NotAvailable => Err(ProtocolError::NotAvailable {
                command: command.mnemonic(),
            }
            .into()),
            other => extract(&other).ok_or_else(|| unexpected(expected, &other)),
        }
    }

    /// Write a setting, require the echo to equal the request, then read the
    /// setting back and require the same value.
    ///
    /// The write-target gate runs first and sends nothing when it fails. A
    /// differing echo is [`ProtocolError::EchoMismatch`] and sends no
    /// readback; a differing readback is [`ProtocolError::ReadbackMismatch`].
    pub(crate) async fn apply<V>(
        &mut self,
        write: Command,
        read: Command,
        expected: &'static str,
        requested: V,
        extract: fn(&Response) -> Option<V>,
    ) -> Result<(), Error>
    where
        V: PartialEq + fmt::Display + Send + Sync,
    {
        self.require_write_target().await?;
        let command = write.mnemonic();
        let echoed = self.query(write, expected, extract).await?;
        if echoed != requested {
            return Err(ProtocolError::EchoMismatch {
                command,
                requested: requested.to_string(),
                echoed: echoed.to_string(),
            }
            .into());
        }
        self.confirm(read, expected, &requested, extract).await
    }

    /// Read a setting back and require it to equal `requested`.
    ///
    /// A differing value is [`ProtocolError::ReadbackMismatch`].
    pub(crate) async fn confirm<V>(
        &mut self,
        read: Command,
        expected: &'static str,
        requested: &V,
        extract: fn(&Response) -> Option<V>,
    ) -> Result<(), Error>
    where
        V: PartialEq + fmt::Display + Send + Sync,
    {
        let command = read.mnemonic();
        let observed = self.query(read, expected, extract).await?;
        if observed == *requested {
            Ok(())
        } else {
            Err(ProtocolError::ReadbackMismatch {
                command,
                requested: requested.to_string(),
                observed: observed.to_string(),
            }
            .into())
        }
    }

    /// Identify the radio when no identity is cached and refuse every identity
    /// outside [`WRITE_TARGETS`].
    pub(crate) async fn require_write_target(&mut self) -> Result<(), Error> {
        let identity = match self.identity().cloned() {
            Some(identity) => identity,
            None => self.identify().await?,
        };
        if identity.is_write_target() {
            Ok(())
        } else {
            Err(Error::UnsupportedWriteTarget { actual: identity })
        }
    }

    /// The reply deadline for `command`, consuming a pending
    /// [`Self::set_next_reply_timeout`].
    fn reply_timeout(&mut self, command: &Command) -> Duration {
        let regular = if matches!(command, Command::SetBluetooth { .. }) {
            self.timeout.max(BLUETOOTH_WRITE_TIMEOUT)
        } else {
            self.timeout
        };
        self.next_reply_timeout
            .take()
            .map_or(regular, |next| next.max(regular))
    }

    /// Discard buffered bytes and anything that arrives within
    /// [`REJECTION_SETTLE_WINDOW`].
    ///
    /// The drain never fails: end of input, a read error or an invalid read
    /// count ends it, and the next reply read reports the fault.
    async fn discard_pending_input(&mut self) {
        let mut discarded = self.receive_buffer.len();
        self.receive_buffer.clear();
        let mut buffer = [0; 256];
        let deadline = tokio::time::Instant::now() + REJECTION_SETTLE_WINDOW;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, self.transport.read(&mut buffer)).await {
                Ok(Ok(count)) if (1..=buffer.len()).contains(&count) => discarded += count,
                Ok(Ok(_) | Err(_)) | Err(_) => break,
            }
        }
        if discarded > 0 {
            tracing::debug!(discarded, "discarded stale input after a CAT reply");
        }
    }

    pub(crate) async fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let timeout = self.timeout;
        tokio::time::timeout(timeout, self.transport.write(bytes))
            .await
            .map_err(|_| Error::Timeout {
                operation: "serial write",
                timeout,
            })?
            .map_err(Error::Transport)
    }

    pub(crate) fn require_cat(&self) -> Result<(), Error> {
        match self.protocol_state {
            ProtocolState::CatReady => Ok(()),
            ProtocolState::McpReady => Err(McpError::SessionActive.into()),
            ProtocolState::CatUncertain | ProtocolState::RecoveryRequired => {
                Err(McpError::RecoveryRequired.into())
            }
            ProtocolState::Retired => Err(McpError::ConnectionRetired.into()),
        }
    }

    pub(crate) fn require_mcp_ready(&self) -> Result<(), Error> {
        match self.protocol_state {
            ProtocolState::McpReady => Ok(()),
            ProtocolState::CatReady => Err(McpError::SessionNotActive.into()),
            ProtocolState::CatUncertain | ProtocolState::RecoveryRequired => {
                Err(McpError::RecoveryRequired.into())
            }
            ProtocolState::Retired => Err(McpError::ConnectionRetired.into()),
        }
    }

    pub(crate) fn mark_cat_ready(&mut self) {
        self.protocol_state = ProtocolState::CatReady;
        self.mcp_entry_reply = None;
    }

    pub(crate) const fn mark_mcp_ready(&mut self) {
        self.protocol_state = ProtocolState::McpReady;
    }

    pub(crate) const fn mark_mcp_uncertain(&mut self) {
        self.protocol_state = ProtocolState::RecoveryRequired;
    }

    /// An acknowledged exit retires the old handle without claiming CAT readiness.
    pub(crate) const fn mark_retired(&mut self) {
        self.protocol_state = ProtocolState::Retired;
    }

    pub(crate) const fn mcp_ready(&self) -> bool {
        matches!(self.protocol_state, ProtocolState::McpReady)
    }

    pub(crate) fn record_mcp_entry_reply(&mut self, reply: Vec<u8>) {
        self.mcp_entry_reply = Some(reply);
    }

    pub(crate) fn mcp_entry_reply(&self) -> Option<&[u8]> {
        self.mcp_entry_reply.as_deref()
    }

    pub(crate) fn set_baud(&mut self, baud: u32) -> Result<(), Error> {
        self.transport.set_baud_rate(baud).map_err(Error::Transport)
    }

    /// Read one bounded CAT line while retaining all bytes after its terminator.
    ///
    /// Partial input remains owned by the radio if this future is cancelled.
    pub(crate) async fn read_line(&mut self, operation: &'static str) -> Result<Vec<u8>, Error> {
        let timeout = self.timeout;
        self.read_line_with(timeout, operation).await
    }

    async fn read_line_with(
        &mut self,
        timeout: Duration,
        operation: &'static str,
    ) -> Result<Vec<u8>, Error> {
        tokio::time::timeout(timeout, async {
            loop {
                if let Some(length) = self
                    .receive_buffer
                    .iter()
                    .position(|byte| *byte == LINE_TERMINATOR)
                {
                    if length > MAX_CAT_LINE_BYTES {
                        return Err(ProtocolError::CatLineTooLong {
                            limit: MAX_CAT_LINE_BYTES,
                        }
                        .into());
                    }
                    let line = self.receive_buffer.drain(..length).collect();
                    let _terminator = self.receive_buffer.pop_front();
                    return Ok(line);
                }
                if self.receive_buffer.len() > MAX_CAT_LINE_BYTES {
                    return Err(ProtocolError::CatLineTooLong {
                        limit: MAX_CAT_LINE_BYTES,
                    }
                    .into());
                }
                self.receive_more().await?;
            }
        })
        .await
        .map_err(|_| Error::Timeout { operation, timeout })?
    }

    /// Read exactly `len` buffered or incoming bytes, retaining any remainder.
    ///
    /// Partial input remains owned by the radio if this future is cancelled.
    pub(crate) async fn read_exact(
        &mut self,
        len: usize,
        operation: &'static str,
    ) -> Result<Vec<u8>, Error> {
        let timeout = self.timeout;
        tokio::time::timeout(timeout, async {
            while self.receive_buffer.len() < len {
                self.receive_more().await?;
            }
            Ok(self.receive_buffer.drain(..len).collect())
        })
        .await
        .map_err(|_| Error::Timeout { operation, timeout })?
    }

    async fn receive_more(&mut self) -> Result<(), Error> {
        let mut buffer = [0; 256];
        let count = self
            .transport
            .read(&mut buffer)
            .await
            .map_err(Error::Transport)?;
        if count == 0 {
            return Err(closed("connection closed while reading a radio reply"));
        }
        let chunk = buffer
            .get(..count)
            .ok_or_else(|| closed("transport returned more bytes than the buffer holds"))?;
        self.receive_buffer.extend(chunk);
        Ok(())
    }
}

#[cfg(test)]
mod identity_tests;
#[cfg(test)]
mod io_tests;

fn unexpected(expected: &'static str, actual: &Response) -> Error {
    Error::Protocol(ProtocolError::UnexpectedResponse {
        expected,
        actual: format!("{actual:?}"),
    })
}

fn closed(detail: &'static str) -> Error {
    Error::Transport(TransportError::Disconnected(std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        detail,
    )))
}
