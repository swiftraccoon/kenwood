//! TM-D750 endpoint discovery and model-specific serial line settings.
//!
//! Physical I/O is provided by [`kenwood_transport`]. The model wrapper selects
//! RTS/CTS flow control with DTR and RTS asserted, including over USB, and
//! discovery prioritizes the observed TM-D750 USB identities.
//! Automatic reopening is unsupported:
//! a device path alone cannot prove which radio owns a re-enumerated endpoint.
//! [`reenumeration`] classifies a fresh enumeration against a pinned endpoint
//! so a caller can decide when the same endpoint may be opened again.

pub mod reenumeration;

use std::num::NonZeroU32;

use kenwood_transport::serial::{
    CloseMode, FlowControl, LineState, SerialOptions, SerialTransport as SerialIo,
};
use kenwood_transport::{Transport, TransportError};

/// JVCKENWOOD's USB vendor id.
pub const KENWOOD_VID: u16 = 0x2166;
/// Observed main-unit USB CDC product id.
pub const TMD750_MAIN_PID: u16 = 0x9030;
/// Observed control-panel USB CDC product id.
pub const TMD750_PANEL_PID: u16 = 0x9032;
/// Hardware-validated CAT baud rate, also used by the official MCP program.
pub const DEFAULT_BAUD: u32 = 9600;

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
    inner: SerialIo,
}

impl SerialTransport {
    /// Open a TM-D750 serial endpoint at `baud` with RTS/CTS, DTR, and RTS.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Open`] when no Tokio runtime is active or
    /// `baud` is zero, or the endpoint or required modem-line configuration
    /// cannot be opened.
    pub fn open(path: &str, baud: u32) -> Result<Self, TransportError> {
        let baud = NonZeroU32::new(baud).ok_or_else(|| {
            open_error(
                path,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "baud rate must be nonzero",
                ),
            )
        })?;
        Ok(Self {
            inner: SerialIo::open(path, serial_options(baud))?,
        })
    }
}

impl Transport for SerialTransport {
    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.inner.write(data).await
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        self.inner.read(buffer).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.inner.close().await
    }

    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        self.inner.set_baud_rate(baud)
    }
}

/// Model-owned line settings; shared I/O never selects a radio preset.
const fn serial_options(baud: NonZeroU32) -> SerialOptions {
    SerialOptions {
        baud,
        flow_control: FlowControl::Hardware,
        dtr: LineState::Assert,
        rts: LineState::Assert,
        close_mode: CloseMode::Shutdown,
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

    #[test]
    fn hardware_serial_settings_are_pinned() -> TestResult {
        let options = serial_options(NonZeroU32::new(DEFAULT_BAUD).ok_or("zero default baud")?);
        assert_eq!(DEFAULT_BAUD, 9600);
        assert_eq!(options.baud.get(), DEFAULT_BAUD);
        assert!(matches!(options.flow_control, FlowControl::Hardware));
        assert!(matches!(options.dtr, LineState::Assert));
        assert!(matches!(options.rts, LineState::Assert));
        assert!(matches!(options.close_mode, CloseMode::Shutdown));
        assert_eq!(TMD750_MAIN_PID, 0x9030);
        assert_eq!(TMD750_PANEL_PID, 0x9032);
        Ok(())
    }

    #[test]
    fn zero_baud_is_rejected_before_opening_an_endpoint() -> TestResult {
        let result = SerialTransport::open("/dev/tmd750-test", 0);
        let Err(TransportError::Open { path, source }) = result else {
            return Err(format!("expected invalid baud error, got {result:?}").into());
        };
        assert_eq!(path, "/dev/tmd750-test");
        assert_eq!(source.kind(), std::io::ErrorKind::InvalidInput);
        Ok(())
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
