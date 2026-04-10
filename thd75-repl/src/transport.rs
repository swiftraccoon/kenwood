//! Transport discovery and connection.
//!
//! Auto-discovers the TH-D75 via USB or Bluetooth. On macOS, always
//! uses native `IOBluetooth` RFCOMM for Bluetooth — the macOS serial
//! driver (`/dev/cu.TH-D75`) drops bytes and is documented as broken.
//! On Linux/Windows, serial BT SPP ports are used normally.

use kenwood_thd75::transport::{EitherTransport, SerialTransport};

/// Discover and open a transport to the radio.
///
/// Priority order:
/// 1. Explicit `--port` if provided
/// 2. USB CDC-ACM auto-discovery
/// 3. Native Bluetooth (macOS: `IOBluetooth` RFCOMM, with one retry)
/// 4. Serial BT SPP ports (Linux/Windows only — skipped on macOS)
pub(crate) fn discover_and_open(
    port: Option<&str>,
    baud: u32,
) -> Result<(String, EitherTransport), Box<dyn std::error::Error>> {
    // Explicit port.
    if let Some(path) = port
        && path != "auto"
    {
        return open_explicit(path, baud);
    }

    // Auto-discover: USB first.
    let usb_ports = SerialTransport::discover_usb()?;
    if let Some(info) = usb_ports.first() {
        let path = info.port_name.clone();
        let transport = SerialTransport::open(&path, baud)?;
        return Ok((path, EitherTransport::Serial(transport)));
    }

    // Bluetooth.
    open_bluetooth(baud)
}

/// Open an explicitly specified port.
fn open_explicit(
    path: &str,
    baud: u32,
) -> Result<(String, EitherTransport), Box<dyn std::error::Error>> {
    // On macOS, BT serial paths must use native IOBluetooth.
    #[cfg(target_os = "macos")]
    if SerialTransport::is_bluetooth_port(path) {
        let bt = kenwood_thd75::BluetoothTransport::open(None)?;
        return Ok((path.to_string(), EitherTransport::Bluetooth(bt)));
    }
    let transport = SerialTransport::open(path, baud)?;
    Ok((path.to_string(), EitherTransport::Serial(transport)))
}

/// Open a Bluetooth connection with platform-appropriate handling.
///
/// macOS: native `IOBluetooth` RFCOMM with one retry (covers stale
/// RFCOMM channel from a prior session that didn't call `disconnect()`).
///
/// Linux/Windows: serial BT SPP port discovery.
#[allow(clippy::needless_return, unused_variables)]
fn open_bluetooth(baud: u32) -> Result<(String, EitherTransport), Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    {
        // First attempt.
        if let Ok(bt) = kenwood_thd75::BluetoothTransport::open(None) {
            return Ok(("bluetooth:TH-D75".into(), EitherTransport::Bluetooth(bt)));
        }

        // The previous session may not have released the RFCOMM channel.
        // Wait and retry once.
        println!("Bluetooth connection failed, retrying in 3 seconds.");
        std::thread::sleep(std::time::Duration::from_secs(3));

        let bt = kenwood_thd75::BluetoothTransport::open(None).map_err(|e| {
            format!(
                "Error: Bluetooth connection failed: {e}. \
                 If the previous session did not exit cleanly, \
                 wait a few seconds or run: sudo pkill bluetoothd"
            )
        })?;
        return Ok(("bluetooth:TH-D75".into(), EitherTransport::Bluetooth(bt)));
    }

    #[cfg(not(target_os = "macos"))]
    {
        let bt_ports = SerialTransport::discover_bluetooth()?;
        if let Some(info) = bt_ports.first() {
            let path = info.port_name.clone();
            let transport = SerialTransport::open(&path, baud)?;
            return Ok((path, EitherTransport::Serial(transport)));
        }
        Err("Error: no TH-D75 found on USB or Bluetooth. Use --port to specify.".into())
    }
}
