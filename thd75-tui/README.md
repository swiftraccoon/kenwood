# thd75-tui

![thd75-tui](../thd75_tui.png)

Terminal UI for the Kenwood TH-D75. Built on [`kenwood-thd75`](../thd75/), [ratatui](https://ratatui.rs/), and [crossterm](https://github.com/crossterm-rs/crossterm).

## Scope

- Real-time CAT display: frequency, mode, squelch, RSSI, battery level, power step, tones, shift direction, step size.
- Memory channel browser: 1000 channels with name / frequency / mode / group columns; edit and write back via CAT or MCP.
- APRS monitor panel: decoded position, message, status, weather, telemetry, Mic-E, object, item reports.
- D-STAR gateway reflector monitor (via [`dstar-gateway-core`](../dstar-gateway-core/)): link status, heard stations, voice-transmission events.
- MCP programming: full memory dump (~55 s at 9600 baud), channel read/write, settings patches. Cached at `~/Library/Caches/thd75-tui/mcp.bin` on macOS for offline correlation.

## Running

```
cargo run -p thd75-tui -- [--port /dev/cu.usbmodem*] [--baud 115200] [--mcp-speed safe|fast]
```

Default port auto-discovers USB (VID `0x2166` / PID `0x9023`) or the paired Bluetooth SPP channel (macOS IOBluetooth RFCOMM; Linux/Windows via the serial emulator).

## Status

Pre-release. The TUI follows library-side API churn closely; layout, keybindings, and panel organization change without notice between commits.

Part of the [kenwood](..) workspace. License: GPL-2.0-or-later.
