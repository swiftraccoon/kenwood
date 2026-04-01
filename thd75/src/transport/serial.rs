//! Serial port transport for USB CDC ACM (Communications Device Class
//! Abstract Control Model) and Bluetooth SPP (Serial Port Profile) connections.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_serial::{SerialPort, SerialStream};

use crate::error::TransportError;

use super::Transport;

/// Serial port transport for USB CDC ACM and Bluetooth SPP connections.
///
/// On all platforms, the TH-D75 appears as a standard serial port:
/// - Linux: `/dev/ttyACM*` (USB), `/dev/rfcomm*` (BT)
/// - macOS: `/dev/cu.usbmodem*` (USB), `/dev/cu.Bluetooth*` (BT)
/// - Windows: `COM*` for both
#[derive(Debug)]
pub struct SerialTransport {
    port: SerialStream,
}

impl SerialTransport {
    /// Default baud rate. CDC ACM ignores line coding, so this is arbitrary.
    pub const DEFAULT_BAUD: u32 = 115_200;

    /// USB Vendor ID (VID) for JVCKENWOOD Corporation.
    pub const USB_VID: u16 = 0x2166;

    /// USB Product ID (PID) for the TH-D75 transceiver.
    pub const USB_PID: u16 = 0x9023;

    /// Open a serial port by path.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Open`] if the port cannot be opened.
    pub fn open(path: &str, baud: u32) -> Result<Self, TransportError> {
        tracing::info!(path = %path, baud = baud, "opening serial port");
        let builder = tokio_serial::new(path, baud);
        let port = SerialStream::open(&builder).map_err(|e| TransportError::Open {
            path: path.to_owned(),
            source: e.into(),
        })?;
        tracing::info!(path = %path, "serial port opened successfully");
        Ok(Self { port })
    }

    /// Discover TH-D75 radios connected via USB.
    ///
    /// Filters available serial ports by VID:PID `2166:9023`.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::NotFound`] if no matching ports are found, or
    /// a [`TransportError::Open`] if port enumeration fails.
    pub fn discover_usb() -> Result<Vec<tokio_serial::SerialPortInfo>, TransportError> {
        tracing::debug!(vid = %format_args!("0x{:04X}", Self::USB_VID), pid = %format_args!("0x{:04X}", Self::USB_PID), "scanning for TH-D75 USB devices");
        let ports =
            tokio_serial::available_ports().map_err(|e| TransportError::Open {
                path: "<enumeration>".to_owned(),
                source: e.into(),
            })?;

        let matching: Vec<_> = ports
            .into_iter()
            .filter(|p| {
                matches!(
                    &p.port_type,
                    tokio_serial::SerialPortType::UsbPort(info)
                        if info.vid == Self::USB_VID && info.pid == Self::USB_PID
                )
            })
            .collect();

        tracing::info!(count = matching.len(), "discovered TH-D75 USB devices");
        Ok(matching)
    }
}

impl Transport for SerialTransport {
    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        tracing::info!(baud, "changing serial baud rate");
        self.port.set_baud_rate(baud).map_err(|e| TransportError::Open {
            path: String::new(),
            source: std::io::Error::other(e.to_string()),
        })
    }

    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        tracing::debug!(bytes = data.len(), "writing to transport");
        tracing::trace!(raw = ?data, "raw bytes sent");
        self.port
            .write_all(data)
            .await
            .map_err(TransportError::Write)?;
        self.port.flush().await.map_err(TransportError::Write)?;
        Ok(())
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let n = self.port.read(buf).await.map_err(TransportError::Read)?;
        tracing::debug!(bytes = n, "read from transport");
        tracing::trace!(raw = ?&buf[..n], "raw bytes received");
        Ok(n)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        tracing::info!("closing serial transport");
        self.port
            .shutdown()
            .await
            .map_err(TransportError::Disconnected)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_re_data() {
        assert_eq!(SerialTransport::USB_VID, 0x2166);
        assert_eq!(SerialTransport::USB_PID, 0x9023);
        assert_eq!(SerialTransport::DEFAULT_BAUD, 115_200);
    }
}
