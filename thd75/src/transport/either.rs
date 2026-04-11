//! Enum transport that dispatches to either Serial, Bluetooth, or Mock.

use crate::error::TransportError;
use crate::transport::Transport;
use crate::transport::mock::MockTransport;
use crate::transport::serial::SerialTransport;

#[cfg(target_os = "macos")]
use crate::transport::bluetooth::BluetoothTransport;

/// A transport that can be USB serial, Bluetooth RFCOMM, or a
/// programmed mock transport used by integration tests.
#[derive(Debug)]
pub enum EitherTransport {
    /// USB CDC ACM serial.
    Serial(SerialTransport),
    /// Native macOS `IOBluetooth` RFCOMM.
    #[cfg(target_os = "macos")]
    Bluetooth(BluetoothTransport),
    /// Programmed mock transport (integration tests, CLI `--mock-radio`).
    Mock(MockTransport),
}

impl Transport for EitherTransport {
    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        match self {
            Self::Serial(s) => s.set_baud_rate(baud),
            #[cfg(target_os = "macos")]
            Self::Bluetooth(_) => Ok(()), // BT has fixed baud
            Self::Mock(m) => m.set_baud_rate(baud),
        }
    }

    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        match self {
            Self::Serial(s) => s.write(data).await,
            #[cfg(target_os = "macos")]
            Self::Bluetooth(b) => b.write(data).await,
            Self::Mock(m) => m.write(data).await,
        }
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        match self {
            Self::Serial(s) => s.read(buf).await,
            #[cfg(target_os = "macos")]
            Self::Bluetooth(b) => b.read(buf).await,
            Self::Mock(m) => m.read(buf).await,
        }
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        match self {
            Self::Serial(s) => s.close().await,
            #[cfg(target_os = "macos")]
            Self::Bluetooth(b) => b.close().await,
            Self::Mock(m) => m.close().await,
        }
    }
}
