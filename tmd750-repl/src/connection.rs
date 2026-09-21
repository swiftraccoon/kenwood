//! One owned transport, either a serial port or, on macOS, a native Bluetooth
//! RFCOMM connection.

use kenwood_tmd750::transport::SerialTransport;
use kenwood_transport::{Transport, TransportError};

/// One open connection, dispatching `Transport` to the backend it holds.
///
/// Endpoint choice and reopen policy stay with the model workflows.
pub(crate) enum Connection {
    Serial(SerialTransport),
    #[cfg(target_os = "macos")]
    NativeBluetooth(kenwood_transport::bluetooth::BluetoothTransport),
}

impl Transport for Connection {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        match self {
            Self::Serial(transport) => transport.write(bytes).await,
            #[cfg(target_os = "macos")]
            Self::NativeBluetooth(transport) => transport.write(bytes).await,
        }
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        match self {
            Self::Serial(transport) => transport.read(bytes).await,
            #[cfg(target_os = "macos")]
            Self::NativeBluetooth(transport) => transport.read(bytes).await,
        }
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        match self {
            Self::Serial(transport) => transport.close().await,
            #[cfg(target_os = "macos")]
            Self::NativeBluetooth(transport) => transport.close().await,
        }
    }

    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        match self {
            Self::Serial(transport) => transport.set_baud_rate(baud),
            #[cfg(target_os = "macos")]
            Self::NativeBluetooth(transport) => transport.set_baud_rate(baud),
        }
    }
}
