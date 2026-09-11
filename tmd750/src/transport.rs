//! The only module that names the TH-D75 crate: its radio-agnostic transport
//! trait and mock, plus TM-D750 serial transport and discovery.
//!
//! The TM-D750 transport is local because the radio requires RTS/CTS flow
//! control with DTR and RTS asserted even over USB. It also has different USB
//! product identifiers from the TH-D75. Automatic reopening is unsupported:
//! a device path alone cannot prove which radio owns a re-enumerated endpoint.

pub use kenwood_thd75::error::TransportError;
pub use kenwood_thd75::transport::{MockTransport, Transport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_serial::{FlowControl, SerialPort, SerialStream};

/// JVCKENWOOD's USB vendor id.
pub const KENWOOD_VID: u16 = 0x2166;
/// Observed main-unit USB CDC product id.
pub const TMD750_MAIN_PID: u16 = 0x9030;
/// Observed control-panel USB CDC product id.
pub const TMD750_PANEL_PID: u16 = 0x9032;
/// Hardware-validated CAT baud rate, also used by the official MCP program.
pub const DEFAULT_BAUD: u32 = 9600;

const FLOW_CONTROL: FlowControl = FlowControl::Hardware;
const ASSERT_DTR: bool = true;
const ASSERT_RTS: bool = true;

/// A serial port that may be a TM-D750; only an `ID` reply proves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialCandidate {
    /// Device path or port name.
    pub path: String,
    /// USB vendor id, when the port is USB.
    pub vid: Option<u16>,
    /// USB product id, when the port is USB.
    pub pid: Option<u16>,
}

impl SerialCandidate {
    /// Whether the port belongs to a JVCKENWOOD USB device.
    #[must_use]
    pub fn is_kenwood(&self) -> bool {
        self.vid == Some(KENWOOD_VID)
    }

    /// Whether this is one of the TM-D750's two observed USB endpoints.
    #[must_use]
    pub fn is_tmd750(&self) -> bool {
        self.vid == Some(KENWOOD_VID)
            && (self.pid == Some(TMD750_MAIN_PID) || self.pid == Some(TMD750_PANEL_PID))
    }
}

/// Enumerate serial ports, known TM-D750 endpoints first, then JVCKENWOOD.
///
/// The official program probes every serial port with `ID`. This function
/// returns all ports in priority order; callers choose their own probe policy.
/// Nothing here opens a port.
///
/// # Errors
///
/// Returns [`TransportError::Open`] when the platform enumeration fails.
pub fn discover_serial() -> Result<Vec<SerialCandidate>, TransportError> {
    let ports = tokio_serial::available_ports().map_err(|error| TransportError::Open {
        path: "<enumeration>".to_owned(),
        source: error.into(),
    })?;
    let candidates = ports
        .into_iter()
        .map(|port| {
            let (vid, pid) = match &port.port_type {
                tokio_serial::SerialPortType::UsbPort(usb) => (Some(usb.vid), Some(usb.pid)),
                _ => (None, None),
            };
            SerialCandidate {
                path: port.port_name,
                vid,
                pid,
            }
        })
        .collect();
    Ok(prioritize(candidates))
}

/// Stable partition: TM-D750 ports, other JVCKENWOOD ports, then the rest.
#[must_use]
pub fn prioritize(candidates: Vec<SerialCandidate>) -> Vec<SerialCandidate> {
    let (tmd750, remainder): (Vec<_>, Vec<_>) =
        candidates.into_iter().partition(SerialCandidate::is_tmd750);
    let (kenwood, other): (Vec<_>, Vec<_>) =
        remainder.into_iter().partition(SerialCandidate::is_kenwood);
    tmd750.into_iter().chain(kenwood).chain(other).collect()
}

/// USB serial transport configured for the TM-D750's required modem lines.
///
/// [`Transport::reopen`] returns [`TransportError::ReopenUnsupported`]. After
/// a disconnect, select and open an endpoint explicitly and repeat the radio
/// identity proof. USB device paths can change or be reassigned to another
/// device; this transport never treats a stored path as a stable identity.
#[derive(Debug)]
pub struct SerialTransport {
    port: Option<SerialStream>,
    path: String,
    baud: u32,
}

impl SerialTransport {
    /// Open a TM-D750 serial endpoint at `baud` with RTS/CTS, DTR, and RTS.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Open`] when no Tokio runtime is active or
    /// the endpoint or required modem-line configuration cannot be opened.
    pub fn open(path: &str, baud: u32) -> Result<Self, TransportError> {
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(open_error(
                path,
                std::io::Error::other("no Tokio runtime is active on this thread"),
            ));
        }
        tracing::info!(
            path,
            baud,
            flow_control = ?FLOW_CONTROL,
            dtr = ASSERT_DTR,
            rts = ASSERT_RTS,
            "opening TM-D750 serial endpoint"
        );
        let builder = tokio_serial::new(path, baud)
            .flow_control(FLOW_CONTROL)
            .dtr_on_open(ASSERT_DTR);
        #[cfg(unix)]
        let builder = builder.exclusive(true);
        let mut port = SerialStream::open(&builder).map_err(|error| open_error(path, error))?;
        port.write_data_terminal_ready(ASSERT_DTR)
            .map_err(|error| open_error(path, error))?;
        port.write_request_to_send(ASSERT_RTS)
            .map_err(|error| open_error(path, error))?;
        tracing::info!(path, "TM-D750 serial endpoint opened");
        Ok(Self {
            port: Some(port),
            path: path.to_owned(),
            baud,
        })
    }

    fn port_mut(&mut self) -> Result<&mut SerialStream, TransportError> {
        self.port.as_mut().ok_or_else(|| {
            TransportError::Disconnected(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "serial endpoint is closed",
            ))
        })
    }
}

impl Transport for SerialTransport {
    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        tracing::trace!(path = %self.path, raw = ?data, "serial write requested");
        let port = self.port_mut()?;
        port.write_all(data).await.map_err(TransportError::Write)?;
        port.flush().await.map_err(TransportError::Write)?;
        tracing::debug!(path = %self.path, bytes = data.len(), "serial write completed");
        Ok(())
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        let count = self
            .port_mut()?
            .read(buffer)
            .await
            .map_err(TransportError::Read)?;
        tracing::debug!(path = %self.path, bytes = count, "serial bytes received");
        if let Some(received) = buffer.get(..count) {
            tracing::trace!(path = %self.path, raw = ?received, "serial bytes received");
        }
        Ok(count)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        tracing::info!(path = %self.path, "closing TM-D750 serial endpoint");
        let Some(mut port) = self.port.take() else {
            return Ok(());
        };
        port.shutdown().await.map_err(TransportError::Disconnected)
    }

    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        let path = self.path.clone();
        tracing::debug!(
            path,
            previous_baud = self.baud,
            baud,
            "changing serial baud rate"
        );
        self.port_mut()?
            .set_baud_rate(baud)
            .map_err(|error| open_error(&path, error))?;
        self.baud = baud;
        Ok(())
    }
}

fn open_error(path: &str, source: impl Into<std::io::Error>) -> TransportError {
    TransportError::Open {
        path: path.to_owned(),
        source: source.into(),
    }
}

/// Open a TM-D750 serial port at `baud` with its required line settings.
///
/// # Errors
///
/// Returns the transport's open error (no tokio runtime, missing port, permissions).
pub fn open_serial(path: &str, baud: u32) -> Result<SerialTransport, TransportError> {
    SerialTransport::open(path, baud)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn serial_open_errors_preserve_their_kind_and_description() -> TestResult {
        for (serial_kind, expected_kind) in [
            (
                tokio_serial::ErrorKind::NoDevice,
                std::io::ErrorKind::NotFound,
            ),
            (
                tokio_serial::ErrorKind::Io(std::io::ErrorKind::PermissionDenied),
                std::io::ErrorKind::PermissionDenied,
            ),
            (
                tokio_serial::ErrorKind::InvalidInput,
                std::io::ErrorKind::InvalidInput,
            ),
        ] {
            let error = open_error(
                "test-endpoint",
                tokio_serial::Error::new(serial_kind, "source description"),
            );
            let TransportError::Open { path, source } = error else {
                return Err(format!("expected an open error, got {error:?}").into());
            };
            assert_eq!(path, "test-endpoint");
            assert_eq!(source.kind(), expected_kind);
            assert_eq!(source.to_string(), "source description");
        }
        Ok(())
    }

    #[tokio::test]
    async fn reopening_without_a_stable_identity_is_unsupported() {
        let mut transport = SerialTransport {
            port: None,
            path: "unqualified-endpoint".to_owned(),
            baud: DEFAULT_BAUD,
        };
        let result = transport.reopen().await;
        assert!(
            matches!(result, Err(TransportError::ReopenUnsupported)),
            "{result:?}"
        );
        assert!(transport.port.is_none());
    }

    #[test]
    fn hardware_serial_settings_are_pinned() {
        assert_eq!(DEFAULT_BAUD, 9600);
        assert!(matches!(FLOW_CONTROL, FlowControl::Hardware));
        const { assert!(ASSERT_DTR) };
        const { assert!(ASSERT_RTS) };
        assert_eq!(TMD750_MAIN_PID, 0x9030);
        assert_eq!(TMD750_PANEL_PID, 0x9032);
    }

    #[test]
    fn open_without_a_runtime_is_a_typed_error() {
        let result = SerialTransport::open("/dev/tmd750-test", DEFAULT_BAUD);
        assert!(
            matches!(result, Err(TransportError::Open { .. })),
            "{result:?}"
        );
    }
}
