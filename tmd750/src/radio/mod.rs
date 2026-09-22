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
//!   with an exact echo plus readback; every setter admits only firmware
//!   `1.02` with type `K,2,1`. Direct frequency writes, `UP`/`DW` and memory
//!   recall are the only tuning writes; there is no transmit, beacon, TNC or
//!   power-off command.
//! - Capture standard configuration with [`Radio::backup_mcp_until_exit`], or
//!   use [`Radio::probe_mcp`] for a smaller, fixed two-fragment read.
//!   Neither operation writes settings; both interrupt ordinary radio operation.
//! - Inspect selected menu fields through [`McpSession::read_menu_snapshot`].
//!   Prepare ordinary changes with [`menu::MenuUpdatePlan`] and persistent
//!   Gateway changes with [`terminal::TerminalPlan`]. A plan is local data;
//!   applying it is a separate session.
//! - The PM1/MY1 update and fixed-trial modules perform single-page writes to
//!   fixed targets under stricter guards. They are not prerequisites for the
//!   ordinary menu API.
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
pub mod menu;
pub mod my1_callsign_update;
pub mod pm1_name_update;
mod pm1_page;
pub mod pm_name_trial;
pub mod programming;
pub mod qualification;
pub mod readiness;
pub mod terminal;
pub mod terminal_exit_trial;

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
pub use programming::{McpJournal, McpSession, McpWriteReport, RecoveryReport, RegionImage};

/// Default timeout for one serial write, CAT line, or MCP exchange step.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1500);

/// Reply deadline for a `BT` write.
///
/// Switching Bluetooth on restarts the radio's Bluetooth stack, and on
/// firmware 1.02 the echo arrived about 1.6 s after the command.
pub const BLUETOOTH_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

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
    McpReady,
    RecoveryRequired,
    Retired,
}

const QUALIFIED_WRITE_FIRMWARE: &str = "1.02";
const QUALIFIED_WRITE_RADIO_TYPE: &str = "K,2,1";

/// Progress of a multi-page transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Pages finished.
    pub done: usize,
    /// Pages in total.
    pub total: usize,
}

/// A proven radio identity.
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
    /// Whether this is the exact identity every write in this crate admits:
    /// a TM-D750 with firmware `1.02` and type `K,2,1`.
    #[must_use]
    pub fn is_qualified_write_target(&self) -> bool {
        self.model == RadioModel::TmD750
            && self.firmware.as_str() == QUALIFIED_WRITE_FIRMWARE
            && self.radio_type.as_str() == QUALIFIED_WRITE_RADIO_TYPE
    }
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
/// connection recovery-required. After a timeout it is unknown whether the
/// bytes arrived or the requested change took effect.
///
/// Subsequent CAT and MCP operations then return
/// [`McpError::RecoveryRequired`] before I/O, even when CAT caused the failure.
/// A complete, parsed rejection or unexpected response instead leaves a
/// synchronized boundary. No command is automatically replayed, and no setting
/// is rolled back.
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
/// [`Self::recover`] only inspects journaled patch bits on a usable connection;
/// it does not repair an uncertain protocol stream.
#[derive(Debug)]
pub struct Radio<T: Transport> {
    transport: T,
    timeout: Duration,
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
            identity: None,
            receive_buffer: VecDeque::new(),
            protocol_state: ProtocolState::CatReady,
            mcp_entry_reply: None,
            settle_commands: 0,
            last_step: None,
        }
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
        tracing::info!("identifying radio");
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
        tracing::info!(
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
    /// Unknown reported values remain [`OperatingMode::Unqualified`]. Follow
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
    /// Proves the radio identity first when needed and refuses any other
    /// firmware or radio type with [`Error::UnsupportedCatWriteTarget`].
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
        self.protocol_state = ProtocolState::RecoveryRequired;
        self.write_all(&command.encode()).await?;
        let timeout = self.reply_timeout(command);
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
    /// The firmware and type gate runs first and sends nothing when it fails.
    pub(crate) async fn apply<V>(
        &mut self,
        write: Command,
        read: Command,
        expected: &'static str,
        requested: V,
        extract: fn(&Response) -> Option<V>,
    ) -> Result<(), Error>
    where
        V: PartialEq + fmt::Debug + Copy,
    {
        self.require_qualified_write_target().await?;
        let echoed = self.query(write, expected, extract).await?;
        if echoed != requested {
            return Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "write echo equal to the request",
                actual: format!("{expected} write echoed {echoed:?}, requested {requested:?}"),
            }));
        }
        let observed = self.query(read, expected, extract).await?;
        if observed != requested {
            return Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "readback equal to the request",
                actual: format!("{expected} read back {observed:?}, requested {requested:?}"),
            }));
        }
        Ok(())
    }

    /// Prove the identity when it is not cached and refuse every target other
    /// than firmware `1.02` with type `K,2,1`.
    pub(crate) async fn require_qualified_write_target(&mut self) -> Result<(), Error> {
        let identity = match self.identity().cloned() {
            Some(identity) => identity,
            None => self.identify().await?,
        };
        if identity.firmware.as_str() != QUALIFIED_WRITE_FIRMWARE
            || identity.radio_type.as_str() != QUALIFIED_WRITE_RADIO_TYPE
        {
            return Err(Error::UnsupportedCatWriteTarget {
                expected_firmware: QUALIFIED_WRITE_FIRMWARE,
                expected_radio_type: QUALIFIED_WRITE_RADIO_TYPE,
                actual_firmware: identity.firmware.to_string(),
                actual_radio_type: identity.radio_type.to_string(),
            });
        }
        Ok(())
    }

    fn reply_timeout(&self, command: &Command) -> Duration {
        if matches!(command, Command::SetBluetooth { .. }) {
            self.timeout.max(BLUETOOTH_WRITE_TIMEOUT)
        } else {
            self.timeout
        }
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
                millis: millis(timeout),
            })?
            .map_err(Error::Transport)
    }

    pub(crate) fn require_cat(&self) -> Result<(), Error> {
        match self.protocol_state {
            ProtocolState::CatReady => Ok(()),
            ProtocolState::McpReady => Err(McpError::SessionActive.into()),
            ProtocolState::RecoveryRequired => Err(McpError::RecoveryRequired.into()),
            ProtocolState::Retired => Err(McpError::ConnectionRetired.into()),
        }
    }

    pub(crate) fn require_mcp_ready(&self) -> Result<(), Error> {
        match self.protocol_state {
            ProtocolState::McpReady => Ok(()),
            ProtocolState::CatReady => Err(McpError::SessionNotActive.into()),
            ProtocolState::RecoveryRequired => Err(McpError::RecoveryRequired.into()),
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
        .map_err(|_| Error::Timeout {
            operation,
            millis: millis(timeout),
        })?
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
        .map_err(|_| Error::Timeout {
            operation,
            millis: millis(timeout),
        })?
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

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
