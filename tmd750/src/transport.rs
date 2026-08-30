//! The only module that names the TH-D75 crate: its radio-agnostic
//! transports, plus serial port discovery.
//!
//! Every other module of this crate is TM-D750 shaped. When the transports
//! move into a shared crate, this file is the one that changes.

pub use kenwood_thd75::error::TransportError;
#[cfg(target_os = "macos")]
pub use kenwood_thd75::transport::{BluetoothTransport, PairedBluetoothDevice};
pub use kenwood_thd75::transport::{EitherTransport, MockTransport, SerialTransport, Transport};

/// JVCKENWOOD's USB vendor id.
pub const KENWOOD_VID: u16 = 0x2166;

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
}

/// Enumerate serial ports, JVCKENWOOD devices first.
///
/// The official program probes every serial port with `ID`; so does this
/// crate's caller. Nothing here opens a port.
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

/// Stable partition: JVCKENWOOD ports first, then the rest, each in input order.
#[must_use]
pub fn prioritize(candidates: Vec<SerialCandidate>) -> Vec<SerialCandidate> {
    let (kenwood, other): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .partition(SerialCandidate::is_kenwood);
    kenwood.into_iter().chain(other).collect()
}

/// Open a serial port at `baud` with the TH-D75 crate's serial transport.
///
/// # Errors
///
/// Returns the transport's open error (no tokio runtime, missing port, permissions).
pub fn open_serial(path: &str, baud: u32) -> Result<SerialTransport, TransportError> {
    SerialTransport::open_with_baud(path, baud)
}
