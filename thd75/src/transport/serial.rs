//! Serial port transport for USB CDC ACM and Bluetooth SPP connections.
//!
//! USB uses 115200 baud (CDC ACM ignores line coding, so this is nominal).
//! Bluetooth SPP requires 9600 baud with RTS/CTS hardware flow control.
//! [`open`](SerialTransport::open) auto-detects BT ports and applies the
//! correct settings.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_serial::{FlowControl, SerialPort, SerialStream};

use crate::error::TransportError;

use super::Transport;

/// Baud rate for Bluetooth SPP connections.
const BT_BAUD: u32 = 9600;

/// Serial port transport for USB CDC ACM and Bluetooth SPP connections.
///
/// Port naming by platform:
/// - Linux: `/dev/ttyACM*` (USB), `/dev/rfcomm*` (BT)
/// - macOS: `/dev/cu.usbmodem*` (USB), `/dev/cu.TH-D75` (BT)
/// - Windows: `COM*` for both
#[derive(Debug)]
pub struct SerialTransport {
    port: SerialStream,
}

impl SerialTransport {
    /// Default baud rate for USB CDC ACM.
    pub const DEFAULT_BAUD: u32 = 115_200;

    /// USB Vendor ID (VID) for JVCKENWOOD Corporation.
    pub const USB_VID: u16 = 0x2166;

    /// USB Product ID (PID) for the TH-D75 transceiver.
    pub const USB_PID: u16 = 0x9023;

    /// Returns `true` if the port path looks like a Bluetooth SPP device.
    #[must_use]
    pub fn is_bluetooth_port(path: &str) -> bool {
        let lower = path.to_lowercase();
        lower.contains("th-d75")
            || lower.contains("rfcomm")
            || (lower.contains("bluetooth") && !lower.contains("incoming"))
    }

    /// Open a serial port by path.
    ///
    /// Bluetooth SPP ports are auto-detected by name and configured with
    /// 9600 baud and RTS/CTS flow control. USB ports use the provided
    /// baud rate with no flow control.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Open`] if the port cannot be opened.
    pub fn open(path: &str, baud: u32) -> Result<Self, TransportError> {
        let is_bt = Self::is_bluetooth_port(path);
        let actual_baud = if is_bt { BT_BAUD } else { baud };
        let flow = if is_bt {
            FlowControl::Hardware
        } else {
            FlowControl::None
        };

        tracing::info!(
            path = %path,
            baud = actual_baud,
            bluetooth = is_bt,
            flow_control = ?flow,
            "opening serial port"
        );

        let builder = tokio_serial::new(path, actual_baud).flow_control(flow);
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
    /// Returns [`TransportError::Open`] if port enumeration fails.
    pub fn discover_usb() -> Result<Vec<tokio_serial::SerialPortInfo>, TransportError> {
        tracing::debug!(
            vid = %format_args!("0x{:04X}", Self::USB_VID),
            pid = %format_args!("0x{:04X}", Self::USB_PID),
            "scanning for TH-D75 USB devices"
        );
        let ports = tokio_serial::available_ports().map_err(|e| TransportError::Open {
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

    /// Discover TH-D75 radios available via Bluetooth SPP.
    ///
    /// Looks for serial ports matching known BT naming patterns.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Open`] if port enumeration fails.
    pub fn discover_bluetooth() -> Result<Vec<tokio_serial::SerialPortInfo>, TransportError> {
        tracing::debug!("scanning for TH-D75 Bluetooth SPP devices");
        let ports = tokio_serial::available_ports().map_err(|e| TransportError::Open {
            path: "<enumeration>".to_owned(),
            source: e.into(),
        })?;

        let matching: Vec<_> = ports
            .into_iter()
            .filter(|p| Self::is_bluetooth_port(&p.port_name))
            .collect();

        tracing::info!(
            count = matching.len(),
            "discovered TH-D75 Bluetooth devices"
        );
        Ok(matching)
    }
}

impl Transport for SerialTransport {
    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        tracing::info!(baud, "changing serial baud rate");
        self.port
            .set_baud_rate(baud)
            .map_err(|e| TransportError::Open {
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

    #[test]
    fn bluetooth_port_detection() {
        assert!(SerialTransport::is_bluetooth_port("/dev/cu.TH-D75"));
        assert!(SerialTransport::is_bluetooth_port("/dev/tty.TH-D75"));
        assert!(SerialTransport::is_bluetooth_port("/dev/rfcomm0"));
        assert!(!SerialTransport::is_bluetooth_port("/dev/cu.usbmodem1101"));
        assert!(!SerialTransport::is_bluetooth_port(
            "/dev/cu.Bluetooth-Incoming-Port"
        ));
        assert!(!SerialTransport::is_bluetooth_port("COM3"));
    }
}
