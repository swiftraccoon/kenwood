//! The async radio: typed CAT control and entry into MCP.

pub mod backup;
pub mod menu;
pub mod my1_callsign_update;
pub mod pm1_name_update;
mod pm1_page;
pub mod pm_name_trial;
pub mod programming;
pub mod qualification;
pub mod terminal_exit_trial;

use std::collections::VecDeque;
use std::time::Duration;

use crate::error::{Error, McpError, ProtocolError};
use crate::protocol::cat::{Command, LINE_TERMINATOR, Response, parse_line};
use crate::types::{
    Band, DvGatewayMode, FirmwareIdentity, OperatingMode, RadioModel, RadioType, SelectableMode,
};
use kenwood_transport::{Transport, TransportError};

pub use programming::{McpJournal, McpSession, McpWriteReport, RecoveryReport, RegionImage};

/// Default timeout for one serial write, CAT line, or MCP exchange step.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1500);

/// Maximum accepted CAT reply length, excluding its carriage return.
///
/// This host-side resource limit bounds malformed or unterminated replies;
/// it does not describe a measured radio firmware limit.
pub const MAX_CAT_LINE_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProtocolState {
    CatReady,
    McpReady,
    RecoveryRequired,
    Retired,
}

const QUALIFIED_MODE_WRITE_FIRMWARE: &str = "1.02";
const QUALIFIED_MODE_WRITE_RADIO_TYPE: &str = "K,2,1";

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

/// A TM-D750 behind a transport.
#[derive(Debug)]
pub struct Radio<T: Transport> {
    transport: T,
    timeout: Duration,
    identity: Option<Identity>,
    receive_buffer: VecDeque<u8>,
    protocol_state: ProtocolState,
    mcp_entry_reply: Option<Vec<u8>>,
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
        }
    }

    /// Change the deadline for each serial write and each reply-reading step.
    ///
    /// A write timeout does not prove that no bytes reached the radio. A CAT
    /// or MCP exchange that is interrupted remains unavailable for further
    /// commands until the connection and radio protocol boundary are recovered.
    pub const fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// The identity proven by the last [`Radio::identify`].
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
    /// # Errors
    ///
    /// Returns [`ProtocolError::UnexpectedIdentity`] for any other model,
    /// [`Error::Timeout`] on silence, and transport or parse errors otherwise.
    pub async fn identify(&mut self) -> Result<Identity, Error> {
        tracing::info!("identifying radio");
        let model = match self.command(Command::Identify).await? {
            Response::Identity { model } => model,
            other => return Err(unexpected("Identity", &other)),
        };
        let firmware = match self.command(Command::FirmwareVersion).await? {
            Response::FirmwareVersion { version } => version,
            other => return Err(unexpected("FirmwareVersion", &other)),
        };
        let radio_type = match self.command(Command::RadioType).await? {
            Response::RadioType(radio_type) => radio_type,
            other => return Err(unexpected("RadioType", &other)),
        };
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

    /// Read the current operating mode for one band.
    ///
    /// # Errors
    ///
    /// Returns a transport, timeout, parse, rejection, or unexpected-response error.
    pub async fn get_operating_mode(&mut self, band: Band) -> Result<OperatingMode, Error> {
        match self.command(Command::GetOperatingMode { band }).await? {
            Response::OperatingMode {
                band: response_band,
                mode,
            } if response_band == band => Ok(mode),
            other => Err(unexpected("matching OperatingMode", &other)),
        }
    }

    /// Select an operating mode and prove the result with an immediate readback.
    ///
    /// Only FM and DV are exposed because both were accepted and read back on
    /// live Band A and Band B hardware. A DR write was rejected, and its read
    /// value has not yet been observed; it is not CAT-selectable through this API.
    ///
    /// # Errors
    ///
    /// Proves the radio identity first when needed and refuses targets other
    /// than the exact firmware and radio type qualified on live hardware.
    /// Returns an error when that gate fails, the write is rejected, its reply
    /// does not match, or the immediate readback differs from the request.
    pub async fn set_operating_mode(
        &mut self,
        band: Band,
        mode: SelectableMode,
    ) -> Result<(), Error> {
        let identity = match self.identity().cloned() {
            Some(identity) => identity,
            None => self.identify().await?,
        };
        if identity.firmware.as_str() != QUALIFIED_MODE_WRITE_FIRMWARE
            || identity.radio_type.as_str() != QUALIFIED_MODE_WRITE_RADIO_TYPE
        {
            return Err(Error::UnsupportedCatWriteTarget {
                expected_firmware: QUALIFIED_MODE_WRITE_FIRMWARE,
                expected_radio_type: QUALIFIED_MODE_WRITE_RADIO_TYPE,
                actual_firmware: identity.firmware.to_string(),
                actual_radio_type: identity.radio_type.to_string(),
            });
        }
        let requested = OperatingMode::from(mode);
        match self
            .command(Command::SetOperatingMode { band, mode })
            .await?
        {
            Response::OperatingMode {
                band: response_band,
                mode: response_mode,
            } if response_band == band && response_mode == requested => {}
            other => return Err(unexpected("matching OperatingMode write echo", &other)),
        }
        let observed = self.get_operating_mode(band).await?;
        if observed == requested {
            Ok(())
        } else {
            Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "operating-mode readback equal to requested mode",
                actual: format!("band {band} reported {observed}, requested {requested}"),
            }))
        }
    }

    /// Put one band into D-STAR DV mode with verified readback.
    ///
    /// This changes the ordinary RF demodulation mode. It does not enable the
    /// separate persistent DV Gateway or Terminal Mode setting.
    ///
    /// # Errors
    ///
    /// Returns the errors from [`Radio::set_operating_mode`].
    pub async fn enter_dstar(&mut self, band: Band) -> Result<(), Error> {
        self.set_operating_mode(band, SelectableMode::Dv).await
    }

    /// Read the persistent DV Gateway state through the read-only `GW` command.
    ///
    /// # Errors
    ///
    /// Returns a transport, timeout, parse, rejection, or unexpected-response error.
    pub async fn get_dv_gateway_mode(&mut self) -> Result<DvGatewayMode, Error> {
        match self.command(Command::GetGatewayMode).await? {
            Response::GatewayMode(mode) => Ok(mode),
            other => Err(unexpected("GatewayMode", &other)),
        }
    }

    pub(crate) async fn command(&mut self, command: Command) -> Result<Response, Error> {
        self.require_cat()?;
        self.protocol_state = ProtocolState::RecoveryRequired;
        self.write_all(&command.encode()).await?;
        let line = self.read_line(command.mnemonic()).await?;
        let response = parse_line(&line)?;
        self.mark_cat_ready();
        Ok(response)
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
