//! Transport discovery and connection.
//!
//! Auto-discovers the TH-D75 via USB or Bluetooth, matching the same
//! logic used by the TUI.

use kenwood_thd75::transport::{EitherTransport, SerialTransport};

/// Discover and open a transport to the radio.
///
/// Tries the explicit port if provided, otherwise auto-discovers USB
/// then Bluetooth.
pub(crate) fn discover_and_open(
    port: Option<&str>,
    baud: u32,
) -> Result<(String, EitherTransport), Box<dyn std::error::Error>> {
    // Explicit port.
    if let Some(path) = port
        && path != "auto"
    {
        #[cfg(target_os = "macos")]
        if SerialTransport::is_bluetooth_port(path) {
            let bt = kenwood_thd75::BluetoothTransport::open(None)?;
            return Ok((path.to_string(), EitherTransport::Bluetooth(bt)));
        }
        let transport = SerialTransport::open(path, baud)?;
        return Ok((path.to_string(), EitherTransport::Serial(transport)));
    }

    // Auto-discover: USB first.
    let usb_ports = SerialTransport::discover_usb()?;
    if let Some(info) = usb_ports.first() {
        let path = info.port_name.clone();
        let transport = SerialTransport::open(&path, baud)?;
        return Ok((path, EitherTransport::Serial(transport)));
    }

    // Bluetooth fallback.
    #[cfg(target_os = "macos")]
    {
        if let Ok(bt) = kenwood_thd75::BluetoothTransport::open(None) {
            return Ok(("bluetooth:TH-D75".into(), EitherTransport::Bluetooth(bt)));
        }
    }

    let bt_ports = SerialTransport::discover_bluetooth()?;
    if let Some(info) = bt_ports.first() {
        let path = info.port_name.clone();
        let transport = SerialTransport::open(&path, baud)?;
        return Ok((path, EitherTransport::Serial(transport)));
    }

    Err("No TH-D75 found on USB or Bluetooth. Use --port to specify.".into())
}
