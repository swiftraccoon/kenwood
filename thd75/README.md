# kenwood-thd75

[![Rust 1.94+](https://img.shields.io/badge/rust-1.94%2B-blue.svg)](https://www.rust-lang.org)
[![License: GPL v2+](https://img.shields.io/badge/license-GPL--2.0--or--later-blue.svg)](LICENSE)

Async Rust library for full control of the Kenwood TH-D75 ham radio transceiver.

## Features

- **CAT protocol** — All 55 commands with strict type safety. Every parameter uses validated types that reject invalid values at construction time.
- **MCP programming** — Binary memory read/write via `0M PROGRAM` mode. Read and modify all 1,200 channels, settings, and calibration data.
- **SD card parsing** — Read `.d75` configs, `.nme` GPS logs, `.tsv` repeater/callsign/QSO lists, `.wav` audio recordings, and `.bmp` screen captures.
- **KISS TNC** — Full KISS frame encode/decode, AX.25 UI frame parsing, and APRS data parsing (position, message, status, object, item, weather, Mic-E, compressed).
- **Transport layer** — USB (CDC ACM) and Bluetooth SPP with auto-detection. Native IOBluetooth on macOS, serial RFCOMM on Linux/Windows.
- **Async** — Built on tokio. All radio operations are async.

## Quick start

```rust
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Auto-detect USB port
    let ports = SerialTransport::discover_usb()?;
    let port = &ports[0].port_name;
    let transport = SerialTransport::open(port, SerialTransport::DEFAULT_BAUD)?;

    let mut radio = Radio::connect(transport).await?;

    let (model, version) = radio.get_firmware_version().await?;
    println!("{model} firmware {version}");

    let freq = radio.get_frequency(kenwood_thd75::types::Band::A).await?;
    println!("Band A: {freq}");

    Ok(())
}
```

## Supported connections

| Platform | USB | Bluetooth |
|----------|-----|-----------|
| macOS | `/dev/cu.usbmodem*` | Native IOBluetooth RFCOMM |
| Linux | `/dev/ttyACM*` | `/dev/rfcomm*` via SerialTransport |
| Windows | `COM*` | BT COM port via SerialTransport |

## Radio compatibility

Tested on TH-D75A firmware v1.03. The TH-D75E (European model) has different TX frequency ranges but uses the same protocol.

## License

GPL-2.0-or-later
