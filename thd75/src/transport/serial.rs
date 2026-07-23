//! Serial port transport for USB CDC ACM and Bluetooth SPP connections.
//!
//! The MAIN MPU (IC2005, OMAP-L138) communicates with the PC via USB
//! Type-C (J1) using a Mentor Graphics MUSB USB 2.0 OTG controller
//! with `MatrixQuest` CDC ACM stack. VID `0x2166` (JVCKENWOOD), PID
//! `0x9023`. The USB interface presents as CDC ACM (class 0x02,
//! subclass 0x02, protocol 0x01 V.25ter) with 3 endpoints: interrupt
//! IN, bulk OUT, bulk IN. The endpoints span two USB interfaces
//! (hardware-verified via ioreg): the class 02/02 control interface
//! (`bInterfaceNumber` 0) carries the interrupt IN endpoint, and the
//! class 0x0A CDC Data interface (`bInterfaceNumber` 1) carries both
//! bulk endpoints; interfaces 2-3 are the UAC1 audio function. USB
//! D+/D- run at Full Speed (12 Mbps).
//!
//! USB uses 115200 baud (CDC ACM ignores line coding, per the Kenwood
//! Operating Tips §5.13, "configuring the baud rate is unnecessary,
//! selecting randomly will suffice" since it's a virtual COM port).
//! USB also provides audio output (48 kHz, 16-bit, monaural, per §5.13.2).
//!
//! Bluetooth SPP runs through BT/GPS IC2044 → level-shift IC2046 →
//! MAIN MPU UART2. Requires 9600 baud with RTS/CTS hardware flow
//! control. The D75 supports Bluetooth 3.0 Class 2 with HSP + SPP
//! profiles only (no BLE, no HFP). Per §5.12, "configuration of the
//! baud rate is not necessary" for BT serial either, but we set 9600
//! explicitly for compatibility.
//!
//! The same VID/PID (2166:9023) is used in both normal operation and
//! firmware update mode (PTT+1 at power-on), though update mode uses
//! the bootloader's simpler USB implementation rather than `MatrixQuest`.
//!
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
    /// Present while the endpoint is open. `None` is a deliberate
    /// disconnected state used while reopening: the old descriptor must be
    /// dropped before an exclusive replacement can be opened.
    port: Option<SerialStream>,
    /// Whether the underlying path is a Bluetooth SPP port (detected
    /// at open time). BT ports must not be shut down explicitly.
    is_bluetooth: bool,
    /// The path this transport was opened at, the reopen fallback
    /// when USB discovery finds nothing.
    path: String,
    /// The effective baud currently configured. Tracked so a reopen
    /// mid-session (e.g. during MCP programming at 9600) comes back
    /// at the speed the radio is actually speaking.
    baud: u32,
    /// Stable USB identity captured from enumeration. Reopen uses this
    /// instead of selecting the first radio with the same VID/PID.
    usb_identity: Option<UsbReopenIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UsbReopenIdentity {
    serial_number: Option<String>,
    alias_key: String,
    serial_unique_at_open: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UsbCandidate {
    path: String,
    serial_number: Option<String>,
}

fn usb_candidates(discovered: &[tokio_serial::SerialPortInfo]) -> Vec<UsbCandidate> {
    discovered
        .iter()
        .filter_map(|port| {
            if let tokio_serial::SerialPortType::UsbPort(info) = &port.port_type {
                Some(UsbCandidate {
                    path: port.port_name.clone(),
                    serial_number: info.serial_number.clone(),
                })
            } else {
                None
            }
        })
        .collect()
}

fn usb_alias_key(path: &str) -> String {
    path.strip_prefix("/dev/cu.")
        .or_else(|| path.strip_prefix("/dev/tty."))
        .unwrap_or(path)
        .to_owned()
}

fn physical_usb_key(candidate: &UsbCandidate) -> (Option<&str>, String) {
    (
        candidate.serial_number.as_deref(),
        usb_alias_key(&candidate.path),
    )
}

fn capture_usb_identity(
    candidates: &[UsbCandidate],
    selected_path: &str,
) -> Option<UsbReopenIdentity> {
    let selected = candidates
        .iter()
        .find(|candidate| candidate.path == selected_path)?;
    let serial_unique_at_open = selected
        .serial_number
        .as_deref()
        .is_none_or(|serial_number| {
            let mut alias_keys = candidates
                .iter()
                .filter(|candidate| candidate.serial_number.as_deref() == Some(serial_number))
                .map(|candidate| usb_alias_key(&candidate.path))
                .collect::<Vec<_>>();
            alias_keys.sort_unstable();
            alias_keys.dedup();
            alias_keys.len() == 1
        });
    Some(UsbReopenIdentity {
        serial_number: selected.serial_number.clone(),
        alias_key: usb_alias_key(selected_path),
        serial_unique_at_open,
    })
}

fn choose_preferred_alias(candidates: &[&UsbCandidate], stored_path: &str) -> Option<String> {
    let callout_required = stored_path.starts_with("/dev/cu.");
    if callout_required {
        return candidates
            .iter()
            .filter(|candidate| candidate.path.starts_with("/dev/cu."))
            .min_by(|left, right| left.path.cmp(&right.path))
            .map(|candidate| candidate.path.clone());
    }
    candidates
        .iter()
        .find(|candidate| candidate.path == stored_path)
        .or_else(|| {
            candidates
                .iter()
                .find(|candidate| candidate.path.starts_with("/dev/cu."))
        })
        .or_else(|| {
            candidates
                .iter()
                .min_by(|left, right| left.path.cmp(&right.path))
        })
        .map(|candidate| candidate.path.clone())
}

fn select_usb_reopen_path(
    candidates: &[UsbCandidate],
    identity: &UsbReopenIdentity,
    stored_path: &str,
) -> Result<String, String> {
    if identity.serial_number.is_some() && !identity.serial_unique_at_open {
        return Err("the selected USB serial number is shared by multiple devices".to_string());
    }

    let matching = candidates
        .iter()
        .filter(|candidate| {
            identity.serial_number.as_deref().map_or_else(
                || usb_alias_key(&candidate.path) == identity.alias_key,
                |serial_number| candidate.serial_number.as_deref() == Some(serial_number),
            )
        })
        .collect::<Vec<_>>();

    if identity.serial_number.is_some() {
        let mut matching_keys = matching
            .iter()
            .map(|candidate| physical_usb_key(candidate))
            .collect::<Vec<_>>();
        matching_keys.sort_unstable();
        matching_keys.dedup();
        let matching_physical_count = matching_keys.len();
        if matching_physical_count > 1 {
            return Err(
                "multiple USB devices reported the selected radio's serial number".to_string(),
            );
        }
    }

    if let Some(path) = choose_preferred_alias(&matching, stored_path) {
        return Ok(path);
    }

    Err("the selected USB radio could not be identified after re-enumeration".to_string())
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
        #[cfg(unix)]
        let builder = builder.exclusive(true);
        let port = SerialStream::open(&builder).map_err(|e| TransportError::Open {
            path: path.to_owned(),
            source: e.into(),
        })?;
        let usb_identity = if is_bt {
            None
        } else {
            Self::discover_usb()
                .ok()
                .and_then(|ports| capture_usb_identity(&usb_candidates(&ports), path))
        };
        tracing::info!(path = %path, "serial port opened successfully");
        Ok(Self {
            port: Some(port),
            is_bluetooth: is_bt,
            path: path.to_owned(),
            baud: actual_baud,
            usb_identity,
        })
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
            .as_mut()
            .ok_or_else(|| {
                TransportError::Disconnected(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "serial endpoint is closed",
                ))
            })?
            .set_baud_rate(baud)
            .map_err(|e| TransportError::Open {
                path: String::new(),
                source: std::io::Error::other(e.to_string()),
            })?;
        self.baud = baud;
        Ok(())
    }

    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        tracing::debug!(bytes = data.len(), "writing to transport");
        tracing::trace!(raw = ?data, "raw bytes sent");
        let port = self.port.as_mut().ok_or_else(|| {
            TransportError::Write(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "serial endpoint is closed",
            ))
        })?;
        port.write_all(data).await.map_err(TransportError::Write)?;
        port.flush().await.map_err(TransportError::Write)?;
        Ok(())
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let port = self.port.as_mut().ok_or_else(|| {
            TransportError::Read(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "serial endpoint is closed",
            ))
        })?;
        let n = port.read(buf).await.map_err(TransportError::Read)?;
        tracing::debug!(bytes = n, "read from transport");
        if let Some(chunk) = buf.get(..n) {
            tracing::trace!(raw = ?chunk, "raw bytes received");
        }
        Ok(n)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        tracing::info!("closing serial transport");
        let Some(mut port) = self.port.take() else {
            return Ok(());
        };
        // Bluetooth SPP ports must NOT be shut down: on macOS the
        // stale RFCOMM channel outlives the shutdown and blocks the
        // next open. Dropping the FD (when this transport is dropped)
        // is the correct release for BT; only real serial devices get
        // an explicit shutdown.
        if self.is_bluetooth {
            tracing::debug!("Bluetooth SPP port: skipping shutdown, dropping FD instead");
            return Ok(());
        }
        port.shutdown()
            .await
            .map_err(TransportError::Disconnected)?;
        Ok(())
    }

    async fn reopen(&mut self) -> Result<(), TransportError> {
        tracing::info!(path = %self.path, baud = self.baud, "reopening serial transport");
        let original_identity = self.usb_identity.clone();
        // Best-effort close: a re-enumerated or unplugged port may
        // already be gone, and BT SPP ports skip shutdown by design.
        drop(self.close().await);
        let path = if self.is_bluetooth {
            // RFCOMM/SPP device nodes are stable across drops and are
            // not USB devices, so discovery by VID/PID cannot find them.
            self.path.clone()
        } else if let Some(identity) = &self.usb_identity {
            let discovered = Self::discover_usb()?;
            select_usb_reopen_path(&usb_candidates(&discovered), identity, &self.path).map_err(
                |detail| TransportError::Open {
                    path: self.path.clone(),
                    source: std::io::Error::new(std::io::ErrorKind::NotFound, detail),
                },
            )?
        } else {
            return Err(TransportError::Open {
                path: self.path.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "serial endpoint has no captured TH-D75 USB identity; refusing an \
                     identity-less reopen",
                ),
            });
        };
        // Bluetooth SPP paths are stable device nodes. USB paths were
        // selected above against the identity captured at the initial open;
        // an unqualified non-Bluetooth endpoint is deliberately not reopened.
        let mut fresh = Self::open(&path, self.baud)?;
        if let Some(identity) = original_identity {
            if identity.serial_number.is_some()
                && fresh
                    .usb_identity
                    .as_ref()
                    .and_then(|fresh_identity| fresh_identity.serial_number.as_ref())
                    != identity.serial_number.as_ref()
            {
                return Err(TransportError::Open {
                    path,
                    source: std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "USB identity changed between discovery and reopen",
                    ),
                });
            }
            fresh.usb_identity = Some(identity);
        }
        *self = fresh;
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

    fn usb_candidate(path: &str, serial_number: Option<&str>) -> UsbCandidate {
        UsbCandidate {
            path: path.to_owned(),
            serial_number: serial_number.map(str::to_owned),
        }
    }

    #[test]
    fn macos_callout_and_dialin_aliases_count_as_one_device() {
        let [callout, dialin] = [
            usb_candidate("/dev/cu.usbmodem101", Some("radio-a")),
            usb_candidate("/dev/tty.usbmodem101", Some("radio-a")),
        ];
        assert_eq!(physical_usb_key(&callout), physical_usb_key(&dialin));
    }

    #[test]
    fn reopen_follows_selected_serial_and_prefers_callout_alias() {
        let opened = vec![usb_candidate("/dev/cu.usbmodem101", Some("radio-a"))];
        let identity = capture_usb_identity(&opened, "/dev/cu.usbmodem101");
        assert!(identity.is_some(), "selected USB identity was not captured");
        let Some(identity) = identity else {
            return;
        };
        let reenumerated = vec![
            usb_candidate("/dev/cu.usbmodem202", Some("radio-b")),
            usb_candidate("/dev/tty.usbmodem303", Some("radio-a")),
            usb_candidate("/dev/cu.usbmodem303", Some("radio-a")),
        ];
        assert_eq!(
            select_usb_reopen_path(&reenumerated, &identity, "/dev/cu.usbmodem101"),
            Ok("/dev/cu.usbmodem303".to_string())
        );
    }

    #[test]
    fn reopen_refuses_a_different_radio_when_selected_target_is_missing() {
        let opened = vec![
            usb_candidate("/dev/cu.usbmodem101", Some("radio-a")),
            usb_candidate("/dev/cu.usbmodem202", Some("radio-b")),
        ];
        let identity = capture_usb_identity(&opened, "/dev/cu.usbmodem101");
        assert!(identity.is_some(), "selected USB identity was not captured");
        let Some(identity) = identity else {
            return;
        };
        let only_other = vec![usb_candidate("/dev/cu.usbmodem202", Some("radio-b"))];
        assert!(
            select_usb_reopen_path(&only_other, &identity, "/dev/cu.usbmodem101").is_err(),
            "reopen selected a different physical radio"
        );
    }

    #[test]
    fn sole_serialized_radio_does_not_fall_back_to_a_different_sole_radio() {
        let opened = vec![usb_candidate("/dev/cu.usbmodem101", Some("radio-a"))];
        let identity = capture_usb_identity(&opened, "/dev/cu.usbmodem101");
        assert!(identity.is_some(), "selected USB identity was not captured");
        let Some(identity) = identity else {
            return;
        };
        let replacement = vec![usb_candidate("/dev/cu.usbmodem202", Some("radio-b"))];
        assert!(
            select_usb_reopen_path(&replacement, &identity, "/dev/cu.usbmodem101").is_err(),
            "unique-at-open fallback selected a different serialized radio"
        );
    }

    #[test]
    fn no_serial_reopen_keeps_the_selected_node_among_multiple_radios() {
        let opened = vec![
            usb_candidate("/dev/cu.usbmodem101", None),
            usb_candidate("/dev/cu.usbmodem202", None),
        ];
        let identity = capture_usb_identity(&opened, "/dev/cu.usbmodem101");
        assert!(identity.is_some(), "selected USB identity was not captured");
        let Some(identity) = identity else {
            return;
        };
        let reenumerated = vec![
            usb_candidate("/dev/cu.usbmodem101", None),
            usb_candidate("/dev/cu.usbmodem202", None),
        ];
        assert_eq!(
            select_usb_reopen_path(&reenumerated, &identity, "/dev/cu.usbmodem101"),
            Ok("/dev/cu.usbmodem101".to_string())
        );
    }

    #[test]
    fn no_serial_radio_refuses_a_renamed_node() {
        let opened = vec![usb_candidate("/dev/cu.usbmodem101", None)];
        let identity = capture_usb_identity(&opened, "/dev/cu.usbmodem101");
        assert!(identity.is_some(), "selected USB identity was not captured");
        let Some(identity) = identity else {
            return;
        };
        let reenumerated = vec![usb_candidate("/dev/cu.usbmodem303", None)];
        assert!(
            select_usb_reopen_path(&reenumerated, &identity, "/dev/cu.usbmodem101").is_err(),
            "a sole no-serial replacement was mistaken for the opened radio"
        );
    }

    #[test]
    fn no_serial_radio_reopens_only_on_the_same_node_identity() {
        let opened = vec![usb_candidate("/dev/cu.usbmodem101", None)];
        let identity = capture_usb_identity(&opened, "/dev/cu.usbmodem101");
        assert!(identity.is_some(), "selected USB identity was not captured");
        let Some(identity) = identity else {
            return;
        };
        let reenumerated = vec![
            usb_candidate("/dev/tty.usbmodem101", None),
            usb_candidate("/dev/cu.usbmodem101", None),
            usb_candidate("/dev/cu.usbmodem303", None),
        ];
        assert_eq!(
            select_usb_reopen_path(&reenumerated, &identity, "/dev/cu.usbmodem101"),
            Ok("/dev/cu.usbmodem101".to_string())
        );
    }

    #[test]
    fn callout_reopen_refuses_a_dialin_only_alias() {
        let opened = vec![usb_candidate("/dev/cu.usbmodem101", Some("radio-a"))];
        let identity = capture_usb_identity(&opened, "/dev/cu.usbmodem101");
        assert!(identity.is_some(), "selected USB identity was not captured");
        let Some(identity) = identity else {
            return;
        };
        let dialin_only = vec![usb_candidate("/dev/tty.usbmodem303", Some("radio-a"))];
        assert!(
            select_usb_reopen_path(&dialin_only, &identity, "/dev/cu.usbmodem101").is_err(),
            "a dial-in alias replaced the required callout endpoint"
        );
    }

    #[test]
    fn duplicate_serial_numbers_are_ambiguous() {
        let opened = vec![
            usb_candidate("/dev/cu.usbmodem101", Some("duplicate")),
            usb_candidate("/dev/cu.usbmodem202", Some("duplicate")),
        ];
        let identity = capture_usb_identity(&opened, "/dev/cu.usbmodem101");
        assert!(identity.is_some(), "selected USB identity was not captured");
        let Some(identity) = identity else {
            return;
        };
        assert!(
            !identity.serial_unique_at_open,
            "duplicate serial was incorrectly qualified at initial open"
        );
        let duplicates = vec![
            usb_candidate("/dev/cu.usbmodem202", Some("duplicate")),
            usb_candidate("/dev/cu.usbmodem303", Some("duplicate")),
        ];
        assert!(
            select_usb_reopen_path(&duplicates, &identity, "/dev/cu.usbmodem101").is_err(),
            "duplicate serial numbers were treated as a stable identity"
        );
    }
}
