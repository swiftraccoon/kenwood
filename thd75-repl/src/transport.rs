//! Transport discovery and connection.
//!
//! Auto-discovers the TH-D75 via USB or Bluetooth. On macOS, always
//! uses native `IOBluetooth` RFCOMM for Bluetooth, because the macOS serial
//! driver (`/dev/cu.TH-D75`) drops bytes and is documented as broken.
//! On Linux/Windows, serial BT SPP ports are used normally.

use kenwood_thd75::transport::{EitherTransport, SerialTransport};
use kenwood_thd75::types::PcOutputInterface;

/// Provenance of the physical endpoint used for Menu 985 routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndpointInterface {
    /// Auto-discovery or an explicit CLI value proved the interface.
    Known(PcOutputInterface),
    /// An explicit serial path such as a Windows `COM` port did not carry
    /// enough metadata to distinguish USB from Bluetooth.
    UnspecifiedExplicitPort,
}

/// One opened radio transport together with the physical PC interface it
/// represents in the TH-D75 menu model.
pub(crate) struct OpenedTransport {
    /// Operator-facing endpoint label.
    pub(crate) label: String,
    /// Open byte transport.
    pub(crate) transport: EitherTransport,
    /// Menu 985 value required to route DV Gateway to this endpoint.
    pub(crate) endpoint_interface: EndpointInterface,
}

/// Discover and open a transport to the radio.
///
/// Priority order:
/// 1. Explicit `--port` if provided
/// 2. USB CDC-ACM auto-discovery
/// 3. Native Bluetooth (macOS: `IOBluetooth` RFCOMM)
/// 4. Serial BT SPP ports (Linux/Windows only; skipped on macOS)
pub(crate) fn discover_and_open(
    port: Option<&str>,
    baud: u32,
    explicit_interface: Option<PcOutputInterface>,
) -> Result<OpenedTransport, Box<dyn std::error::Error>> {
    if explicit_interface.is_some() && port.is_none_or(|path| path == "auto") {
        return Err("--port-interface requires a concrete --port path, not auto-discovery".into());
    }
    // Explicit port.
    if let Some(path) = port
        && path != "auto"
    {
        return open_explicit(path, baud, explicit_interface);
    }

    // Auto-discover: USB first.
    let usb_ports = SerialTransport::discover_usb()?;
    if let Some(info) = usb_ports.first() {
        let path = info.port_name.clone();
        let transport = SerialTransport::open_with_baud(&path, baud)?;
        return Ok(OpenedTransport {
            label: path,
            transport: EitherTransport::Serial(transport),
            endpoint_interface: EndpointInterface::Known(PcOutputInterface::Usb),
        });
    }

    // Bluetooth.
    open_bluetooth(baud)
}

/// Open an explicitly specified port.
fn open_explicit(
    path: &str,
    baud: u32,
    explicit_interface: Option<PcOutputInterface>,
) -> Result<OpenedTransport, Box<dyn std::error::Error>> {
    let endpoint_interface = resolve_explicit_interface(path, explicit_interface)?;

    // On macOS, explicit Bluetooth selection must resolve to one exact paired
    // address before native IOBluetooth opens RFCOMM.
    #[cfg(target_os = "macos")]
    if endpoint_interface == EndpointInterface::Known(PcOutputInterface::Bluetooth) {
        return open_explicit_bluetooth(path);
    }
    let transport = if endpoint_interface == EndpointInterface::Known(PcOutputInterface::Bluetooth)
    {
        SerialTransport::open_bluetooth(path)?
    } else {
        SerialTransport::open_with_baud(path, baud)?
    };
    Ok(OpenedTransport {
        label: path.to_owned(),
        transport: EitherTransport::Serial(transport),
        endpoint_interface,
    })
}

fn resolve_explicit_interface(
    path: &str,
    explicit_interface: Option<PcOutputInterface>,
) -> Result<EndpointInterface, String> {
    let inferred_interface = infer_explicit_serial_interface(path);
    match (inferred_interface, explicit_interface) {
        (EndpointInterface::Known(inferred), Some(explicit)) if inferred != explicit => {
            Err(format!(
                "--port-interface {explicit} conflicts with the enumerated {inferred} endpoint \
                 {path}"
            ))
        }
        (EndpointInterface::Known(inferred), _) => Ok(EndpointInterface::Known(inferred)),
        (EndpointInterface::UnspecifiedExplicitPort, Some(explicit)) => {
            Ok(EndpointInterface::Known(explicit))
        }
        (EndpointInterface::UnspecifiedExplicitPort, None) => {
            Ok(EndpointInterface::UnspecifiedExplicitPort)
        }
    }
}

/// Infer only from enumerator-backed identity or an unambiguous platform path.
/// A generic explicit path stays unknown instead of silently becoming USB.
fn infer_explicit_serial_interface(path: &str) -> EndpointInterface {
    #[cfg(target_os = "macos")]
    if canonical_bluetooth_address(path).is_some() {
        return EndpointInterface::Known(PcOutputInterface::Bluetooth);
    }
    if SerialTransport::is_bluetooth_port(path) {
        return EndpointInterface::Known(PcOutputInterface::Bluetooth);
    }
    if SerialTransport::discover_usb()
        .ok()
        .is_some_and(|ports| ports.iter().any(|port| port.port_name == path))
    {
        return EndpointInterface::Known(PcOutputInterface::Usb);
    }
    if SerialTransport::discover_bluetooth()
        .ok()
        .is_some_and(|ports| ports.iter().any(|port| port.port_name == path))
    {
        return EndpointInterface::Known(PcOutputInterface::Bluetooth);
    }
    EndpointInterface::UnspecifiedExplicitPort
}

/// Open one explicitly selected macOS Bluetooth device by exact address.
///
/// A `/dev/cu.*` Bluetooth node is not an identity that native `IOBluetooth`
/// can preserve: opening the default display name could silently select a
/// different paired radio. Resolve the caller's address against the bounded
/// paired-device inventory, then hand that exact record to the native helper.
#[cfg(target_os = "macos")]
fn open_explicit_bluetooth(selector: &str) -> Result<OpenedTransport, Box<dyn std::error::Error>> {
    if canonical_bluetooth_address(selector).is_none() {
        return Err(exact_bluetooth_selector_guidance(selector).into());
    }
    let devices = kenwood_thd75::BluetoothTransport::paired_devices()?;
    let identities: Vec<_> = devices
        .iter()
        .map(|device| (device.address(), device.display_name()))
        .collect();
    let selected_index = resolve_exact_paired_device_index(selector, &identities)?;
    let selected = devices
        .get(selected_index)
        .ok_or("paired Bluetooth selection changed during resolution")?;
    let helper_executable = std::env::current_exe()?;
    let transport = kenwood_thd75::BluetoothTransport::open_paired_device_with_helper_executable(
        selected,
        helper_executable,
    )?;
    Ok(OpenedTransport {
        label: format!(
            "bluetooth:{} ({})",
            selected.display_name(),
            selected.address()
        ),
        transport: EitherTransport::Bluetooth(transport),
        endpoint_interface: EndpointInterface::Known(PcOutputInterface::Bluetooth),
    })
}

#[cfg(any(target_os = "macos", test))]
fn exact_bluetooth_selector_guidance(selector: &str) -> String {
    format!(
        "macOS native Bluetooth cannot preserve the physical identity of selector {selector:?}. \
         Pass an exact paired Bluetooth address, for example \
         --port 00-11-22-33-44-55 --port-interface bluetooth"
    )
}

/// Resolve only a syntactically exact Bluetooth address to one paired record.
/// Display names and serial-device paths are diagnostics, never selectors.
#[cfg(any(target_os = "macos", test))]
fn resolve_exact_paired_device_index(
    selector: &str,
    devices: &[(&str, &str)],
) -> Result<usize, String> {
    let options = paired_bluetooth_options(devices);
    let Some(address) = canonical_bluetooth_address(selector) else {
        return Err(exact_bluetooth_selector_guidance(selector));
    };
    devices
        .iter()
        .position(|(paired_address, _)| paired_address.eq_ignore_ascii_case(&address))
        .ok_or_else(|| {
            format!("Bluetooth address {address} is not in the paired-device inventory. {options}")
        })
}

#[cfg(any(target_os = "macos", test))]
fn paired_bluetooth_options(devices: &[(&str, &str)]) -> String {
    if devices.is_empty() {
        return "No paired Bluetooth devices were reported by macOS.".to_owned();
    }
    let choices: Vec<_> = devices
        .iter()
        .map(|(address, name)| format!("{name:?} at {address}"))
        .collect();
    format!("Paired devices: {}.", choices.join(", "))
}

#[cfg(any(target_os = "macos", test))]
fn canonical_bluetooth_address(address: &str) -> Option<String> {
    let bytes = address.as_bytes();
    if bytes.len() != 17 {
        return None;
    }
    let separator = *bytes.get(2)?;
    if !matches!(separator, b'-' | b':') {
        return None;
    }
    let mut canonical = String::with_capacity(17);
    for (index, byte) in bytes.iter().copied().enumerate() {
        if matches!(index, 2 | 5 | 8 | 11 | 14) {
            if byte != separator {
                return None;
            }
            canonical.push('-');
        } else if byte.is_ascii_hexdigit() {
            canonical.push(char::from(byte.to_ascii_uppercase()));
        } else {
            return None;
        }
    }
    Some(canonical)
}

/// Open a Bluetooth connection using native `IOBluetooth` RFCOMM.
///
/// `_baud` is ignored: the native macOS RFCOMM path negotiates its own
/// line parameters. A single attempt is made. The transport never tears down
/// an already-connected Bluetooth baseband as a cleanup strategy; an SPP
/// channel owned by another process simply makes this open fail.
#[cfg(target_os = "macos")]
fn open_bluetooth(_baud: u32) -> Result<OpenedTransport, Box<dyn std::error::Error>> {
    let bt = kenwood_thd75::BluetoothTransport::open(None).map_err(|e| {
        format!(
            "Error: Bluetooth connection failed: {e}. \
             Confirm that the radio is paired, Bluetooth is enabled, and no \
             other application currently owns its SPP channel."
        )
    })?;
    Ok(OpenedTransport {
        label: "bluetooth:TH-D75".into(),
        transport: EitherTransport::Bluetooth(bt),
        endpoint_interface: EndpointInterface::Known(PcOutputInterface::Bluetooth),
    })
}

/// Open a Bluetooth connection via serial BT SPP port discovery.
///
/// On Linux/Windows there is no native `IOBluetooth` equivalent, so we
/// enumerate serial ports that look like Bluetooth TH-D75 pairings and
/// open the first one at the requested baud rate.
#[cfg(not(target_os = "macos"))]
fn open_bluetooth(_baud: u32) -> Result<OpenedTransport, Box<dyn std::error::Error>> {
    let bt_ports = SerialTransport::discover_bluetooth()?;
    if let Some(info) = bt_ports.first() {
        let path = info.port_name.clone();
        let transport = SerialTransport::open_bluetooth(&path)?;
        return Ok(OpenedTransport {
            label: path,
            transport: EitherTransport::Serial(transport),
            endpoint_interface: EndpointInterface::Known(PcOutputInterface::Bluetooth),
        });
    }
    Err("Error: no TH-D75 found on USB or Bluetooth. Use --port to specify.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAIRED_DEVICES: &[(&str, &str)] = &[
        ("00-11-22-33-44-55", "TH-D75 Shack"),
        ("AA-BB-CC-DD-EE-FF", "TH-D75 Mobile"),
    ];

    #[test]
    fn exact_bluetooth_address_resolves_one_paired_identity() -> Result<(), String> {
        let selected = resolve_exact_paired_device_index("aa:bb:cc:dd:ee:ff", PAIRED_DEVICES)?;
        assert_eq!(
            selected, 1,
            "a colon-delimited lower-case address must resolve to its canonical paired identity"
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn exact_bluetooth_address_proves_the_interface_without_an_override() -> Result<(), String> {
        let interface = resolve_explicit_interface("AA-BB-CC-DD-EE-FF", None)?;
        assert_eq!(
            interface,
            EndpointInterface::Known(PcOutputInterface::Bluetooth),
            "an exact Bluetooth address is self-identifying on macOS"
        );
        Ok(())
    }

    #[test]
    fn bluetooth_display_name_and_serial_path_are_not_identity_selectors() {
        for selector in ["TH-D75 Shack", "/dev/cu.TH-D75-Shack"] {
            let result = resolve_exact_paired_device_index(selector, PAIRED_DEVICES);
            assert!(
                matches!(result, Err(ref error) if error.contains("exact paired Bluetooth address")
                    && error.contains("00-11-22-33-44-55")),
                "non-address selector did not receive exact-address guidance: {result:?}"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_explicit_serial_node_is_rejected_before_native_open() -> Result<(), String> {
        let error = open_explicit("/dev/cu.TH-D75", 115_200, None)
            .err()
            .ok_or("a macOS Bluetooth serial node was silently redirected")?
            .to_string();
        assert!(
            error.contains("cannot preserve the physical identity")
                && error.contains("--port 00-11-22-33-44-55")
                && error.contains("--port-interface bluetooth"),
            "serial-node rejection lost exact-address instructions: {error}"
        );
        Ok(())
    }

    #[test]
    fn unpaired_exact_bluetooth_address_lists_available_identities() {
        let result = resolve_exact_paired_device_index("12-34-56-78-9A-BC", PAIRED_DEVICES);
        assert!(
            matches!(result, Err(ref error) if error.contains("is not in the paired-device inventory")
                && error.contains("TH-D75 Shack")
                && error.contains("AA-BB-CC-DD-EE-FF")),
            "unpaired exact address did not list actionable paired identities: {result:?}"
        );
    }

    #[test]
    fn malformed_bluetooth_addresses_are_never_partially_normalized() {
        for selector in ["00-11:22-33-44-55", "00-11-22-33-44", "GG-11-22-33-44-55"] {
            assert!(
                canonical_bluetooth_address(selector).is_none(),
                "malformed selector was accepted: {selector}"
            );
        }
    }

    #[test]
    fn explicit_interface_rejects_known_path_conflicts() -> Result<(), String> {
        let Err(error) = resolve_explicit_interface("/dev/rfcomm0", Some(PcOutputInterface::Usb))
        else {
            return Err("a known Bluetooth path accepted a USB override".to_owned());
        };
        assert!(
            error.contains("conflicts"),
            "conflicting override error lost its reason: {error}"
        );
        Ok(())
    }

    #[test]
    fn explicit_interface_qualifies_an_opaque_path() -> Result<(), String> {
        let interface = resolve_explicit_interface(
            "opaque-port-that-cannot-exist",
            Some(PcOutputInterface::Bluetooth),
        )?;
        assert_eq!(
            interface,
            EndpointInterface::Known(PcOutputInterface::Bluetooth),
            "the explicit override must qualify an otherwise opaque path"
        );
        Ok(())
    }

    #[test]
    fn opaque_explicit_path_remains_unspecified_without_override() -> Result<(), String> {
        let interface = resolve_explicit_interface("opaque-port-that-cannot-exist", None)?;
        assert_eq!(
            interface,
            EndpointInterface::UnspecifiedExplicitPort,
            "an opaque path without an override must remain unresolved"
        );
        Ok(())
    }
}
