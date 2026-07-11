# kenwood-thd75

[![Rust 1.94+](https://img.shields.io/badge/rust-1.94%2B-blue.svg)](https://www.rust-lang.org)
[![License: GPL v2+](https://img.shields.io/badge/license-GPL--2.0--or--later-blue.svg)](https://github.com/swiftraccoon/kenwood/blob/main/LICENSE)

Async Rust library for full control of the Kenwood TH-D75 ham radio transceiver.

## Features

- **CAT protocol** — All 55 commands with strict type safety. Every parameter uses validated types that reject invalid values at construction time.
- **MCP programming** — Binary memory read/write via `0M PROGRAM` mode. Read and modify all 1,200 MCP channel entries (1,000 standard channels plus special channels), settings, and calibration data.
- **SD card parsing** — Read `.d75` configs, `.nme` GPS logs, `.tsv` repeater/callsign/QSO lists, `.wav` audio recordings, and `.bmp` screen captures.
- **APRS integration** — High-level `AprsClient` that owns `Radio<T>` + `KissSession` and threads `now: Instant` into the sans-io stack. Packet-radio protocol code (KISS framing, AX.25 codec, APRS parser/digipeater/SmartBeaconing/messaging/station-list, APRS-IS) lives in the sibling [`kiss-tnc`](https://github.com/swiftraccoon/kenwood/tree/main/kiss-tnc), [`ax25-codec`](https://github.com/swiftraccoon/kenwood/tree/main/ax25-codec), [`aprs`](https://github.com/swiftraccoon/kenwood/tree/main/aprs), [`aprs-is`](https://github.com/swiftraccoon/kenwood/tree/main/aprs-is) crates.
- **MCP bridge** — `From<McpSmartBeaconingConfig> for aprs::SmartBeaconingConfig` (mph → km/h) in `thd75/src/aprs/mcp_bridge.rs`.
- **Transport layer** — USB (CDC ACM) and Bluetooth SPP with auto-detection. Native `IOBluetooth` on macOS, serial RFCOMM on Linux/Windows.
- **Session resilience** — `Radio::reconnect()` re-establishes a dropped USB or Bluetooth link on the same transport identity (surviving USB re-enumeration and MCP programming-mode exits), and an opt-in `RadioSupervisor` retries with capped exponential backoff while broadcasting typed link events. MCP writes verify by read-back before reporting success.
- **Async** — Built on tokio. All radio operations are async.

## Quick start

```rust,no_run
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Auto-detect USB port
    let ports = SerialTransport::discover_usb()?;
    let port = ports.first().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no TH-D75 USB port found")
    })?;
    let transport = SerialTransport::open(&port.port_name, SerialTransport::DEFAULT_BAUD)?;

    let mut radio = Radio::connect(transport).await?;

    let version = radio.get_firmware_version().await?;
    println!("firmware {version}");

    let freq = radio.get_frequency(kenwood_thd75::types::Band::A).await?;
    println!("Band A: {}", freq.rx_frequency);

    Ok(())
}
```

## Examples

Runnable examples live in [`examples/`](https://github.com/swiftraccoon/kenwood/tree/main/thd75/examples). Run any of them with `cargo run -p kenwood-thd75 --example <name>`:

| Example | Description |
|---------|-------------|
| `identify` | Print the radio model ID, firmware version, region code, and power status. |
| `monitor` | Poll S-meter, frequency, mode, and busy state on both bands every 250 ms. |
| `tune` | Tune to a frequency or memory channel via the safe VFO/Memory-switching API. |
| `channel_dump` | Read memory channels 0-999 via CAT, optionally reading display names via MCP. |
| `config_backup` | Read the entire 500 KB radio memory via MCP and save it to a binary file. |
| `write_settings` | Temporarily change and restore squelch via CAT, then overwrite channel 0's display name via MCP. |
| `bluetooth` | Connect over native macOS Bluetooth or a Linux/Windows serial RFCOMM port (pair via Menu 934 first). |
| `bt_native` | Exercise the native `IOBluetooth` RFCOMM transport (macOS). |
| `pf_screen_capture` | Assign the front-panel PF1 key to Screen Capture via an MCP memory write. |
| `kiss_monitor` | Decode KISS frames, AX.25 packets, and APRS position reports from the TNC. |

## Supported connections

| Platform | USB | Bluetooth |
|----------|-----|-----------|
| macOS | `/dev/cu.usbmodem*` | Native `IOBluetooth` RFCOMM |
| Linux | `/dev/ttyACM*` | `/dev/rfcomm*` via `SerialTransport` |
| Windows | `COM*` | BT COM port via `SerialTransport` |

## Radio compatibility

Tested on TH-D75A firmware v1.03. The TH-D75E (European model) has different TX frequency ranges but uses the same protocol.

## License

GPL-2.0-or-later
