# thd75-tui

Terminal dashboard for the Kenwood TH-D75, using
[`kenwood-thd75`](https://github.com/swiftraccoon/kenwood/tree/main/thd75),
[Ratatui](https://ratatui.rs/) and
[Crossterm](https://github.com/crossterm-rs/crossterm). This package is an
unpublished binary and exports no Rust library API.

## Running

```sh
cargo run -p thd75-tui -- --port /dev/cu.usbmodem123 --baud 115200
```

The baud rate must be nonzero. Omit `--port` to discover one USB serial port;
multiple matches require explicit selection. With no USB port, macOS selects
the paired Bluetooth display name `TH-D75` through the native transport, which
rejects ambiguous names. Other platforms discover one Bluetooth serial port.
An explicit `--port` always names a serial port.

Initial connection reads the CAT model and serial identity before enabling
auto-information. Reconnect closes the previous connection, searches the
original endpoint and available USB paths, and compares the original CAT
serial before sending state-changing commands. Native Bluetooth reconnect uses
the exact opened address. A failed connection close stops replacement attempts.
Unproved binary links are refused rather than receiving a mode-changing recovery
preamble. `--exit-terminal-mode` offers Menu 650 guidance after the corresponding
link diagnosis; EOF and input errors stop retries.

## Dashboard

- Band panels show live frequency, mode, power, squelch, step and attenuator
  observations. Busy and S-meter values come from auto-information, with meter
  reads gated by pushed busy changes. Failed or missing reads show
  `Unavailable`; relative toggles require a current observation.
- The memory browser inspects regular channel records and recalls populated
  channels. Direct frequency entry and persisted per-channel editing are
  unavailable; UP/DW steps remain available.
- The APRS panel receives decoded events and tracks up to 256 stations and 128
  outgoing delivery records. A configured live radio callsign is required to
  enter APRS mode. Starting APRS enables automatic RF acknowledgements for
  addressed messages and automatic position-query replies. Query replies and
  manual beacons require a validated live local position, which this dashboard
  does not currently obtain. Message IDs first mean queued; later events report
  delivery, rejection or expiry.
- D-STAR CAT controls prepare CQ, URCALL, reflector-link and unlink fields.
  They do not transmit those commands or open a reflector network connection.
  Modem monitoring is unavailable because the model's Kenwood wire profile
  returns `UnsupportedProfile` before Terminal entry or configuration writes.
- The MCP panel reads the complete model memory image. Settings are staged
  separately from the displayed snapshot, queued through a bounded command
  channel, and written only after an exact live page comparison. The displayed
  value changes after verified completion. A failed or stale write retains the
  previous image and requires a fresh read. Full-image writes are unavailable.

`Tab` changes focus, `?` shows controls, and `q` or Ctrl-C quits. Input buffers
retain at most 1,024 UTF-8 bytes. The command queue holds at most 32 pending
operations; a full or closed queue reports rejection. Shutdown discards queued
requests that have not started and finishes the current radio operation.
APRS shutdown attempts KISS exit and, after a successful exit, CAT restoration.
Every result then attempts connection close and joins the radio and keyboard
tasks before restoring the screen.
Operation, connection-release and terminal-restoration failures are reported
independently. Shutdown can wait for the model operation's transport deadline.

## Cache and diagnostics

The cache stores only validated, complete memory images. Reads are bounded to
the model image size plus one growth-detection byte. Replacement writes use an
exclusive temporary file, file sync and rename; containing directories are not
synced. Cache failures are displayed independently of completed radio work.

Cache locations are `~/Library/Caches/thd75-tui/mcp.bin` on macOS,
`$XDG_CACHE_HOME/thd75-tui/mcp.bin` or `~/.cache/thd75-tui/mcp.bin` on Linux,
and `%LOCALAPPDATA%/thd75-tui/mcp.bin` on Windows. If no platform cache directory
is available, disk caching is disabled. Cached data is a snapshot; live writes
still compare the complete page on the connected radio.

Setting `RUST_LOG` enables `thd75-tui.log` in the working directory when that
file can be created. Logging setup failure leaves the dashboard usable.

Part of the [Kenwood workspace](https://github.com/swiftraccoon/kenwood).
License: GPL-2.0-or-later.
