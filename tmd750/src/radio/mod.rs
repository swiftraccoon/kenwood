//! The async radio: identity proof over CAT and entry into MCP.

pub mod programming;

use std::time::Duration;

use crate::error::{Error, ProtocolError};
use crate::protocol::cat::{Command, LINE_TERMINATOR, Response, parse_line};
use crate::transport::{Transport, TransportError};
use crate::types::{FirmwareIdentity, MarketType, RadioModel};

pub use programming::{McpJournal, McpSession, McpWriteReport, RecoveryReport, RegionImage};

/// Default reply timeout for one CAT line or one MCP exchange step.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1500);
/// Default CAT baud rate (a day-one hardware finding may change it).
pub const DEFAULT_CAT_BAUD: u32 = 9600;

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
    /// The `TY` byte.
    pub market: MarketType,
}

/// A TM-D750 behind a transport.
#[derive(Debug)]
pub struct Radio<T: Transport> {
    transport: T,
    timeout: Duration,
    cat_baud: u32,
    identity: Option<Identity>,
}

impl<T: Transport> Radio<T> {
    /// Wrap a transport; nothing is sent until [`Radio::identify`].
    pub const fn new(transport: T) -> Self {
        Self {
            transport,
            timeout: DEFAULT_TIMEOUT,
            cat_baud: DEFAULT_CAT_BAUD,
            identity: None,
        }
    }

    /// Change the per-step timeout.
    pub const fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Change the CAT baud rate restored after an MCP session.
    pub const fn set_cat_baud(&mut self, baud: u32) {
        self.cat_baud = baud;
    }

    /// The identity proven by the last [`Radio::identify`].
    #[must_use]
    pub const fn identity(&self) -> Option<&Identity> {
        self.identity.as_ref()
    }

    /// Give the transport back.
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
        let market = match self.command(Command::RadioType).await? {
            Response::RadioType { market } => market,
            other => return Err(unexpected("RadioType", &other)),
        };
        let identity = Identity {
            model,
            firmware,
            market,
        };
        tracing::info!(firmware = %identity.firmware, market = %identity.market, "radio identified");
        self.identity = Some(identity.clone());
        Ok(identity)
    }

    pub(crate) async fn command(&mut self, command: Command) -> Result<Response, Error> {
        self.write_all(&command.encode()).await?;
        let line = self.read_line(command.mnemonic()).await?;
        Ok(parse_line(&line)?)
    }

    pub(crate) async fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.transport.write(bytes).await.map_err(Error::Transport)
    }

    pub(crate) fn set_baud(&mut self, baud: u32) -> Result<(), Error> {
        self.transport.set_baud_rate(baud).map_err(Error::Transport)
    }

    pub(crate) const fn cat_baud(&self) -> u32 {
        self.cat_baud
    }

    /// Read bytes until a carriage return; the terminator is dropped.
    pub(crate) async fn read_line(&mut self, operation: &'static str) -> Result<Vec<u8>, Error> {
        let timeout = self.timeout;
        let transport = &mut self.transport;
        tokio::time::timeout(timeout, async {
            let mut line = Vec::new();
            let mut buf = [0u8; 64];
            loop {
                let count = transport.read(&mut buf).await.map_err(Error::Transport)?;
                if count == 0 {
                    return Err(closed("connection closed while reading a CAT line"));
                }
                let chunk = buf
                    .get(..count)
                    .ok_or_else(|| closed("transport returned more bytes than the buffer holds"))?;
                for &byte in chunk {
                    if byte == LINE_TERMINATOR {
                        return Ok(line);
                    }
                    line.push(byte);
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout {
            operation,
            millis: millis(timeout),
        })?
    }

    /// Read exactly `len` bytes.
    pub(crate) async fn read_exact(
        &mut self,
        len: usize,
        operation: &'static str,
    ) -> Result<Vec<u8>, Error> {
        let timeout = self.timeout;
        let transport = &mut self.transport;
        tokio::time::timeout(timeout, async {
            let mut bytes = Vec::with_capacity(len);
            let mut buf = [0u8; 256];
            while bytes.len() < len {
                let wanted = (len - bytes.len()).min(buf.len());
                let window = buf
                    .get_mut(..wanted)
                    .ok_or_else(|| closed("read window exceeded the buffer"))?;
                let count = transport.read(window).await.map_err(Error::Transport)?;
                if count == 0 {
                    return Err(closed("connection closed during an MCP exchange"));
                }
                let chunk = window
                    .get(..count)
                    .ok_or_else(|| closed("transport returned more bytes than requested"))?;
                bytes.extend_from_slice(chunk);
            }
            Ok(bytes)
        })
        .await
        .map_err(|_| Error::Timeout {
            operation,
            millis: millis(timeout),
        })?
    }
}

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
