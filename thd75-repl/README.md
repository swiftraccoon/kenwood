# thd75-repl

Accessible command-line REPL for the Kenwood TH-D75 transceiver. Designed for screen-reader compatibility following WCAG 2.1 guidelines.

## Features

- CAT radio control (frequency, mode, squelch, power, VOX, etc.)
- D-STAR reflector gateway (DPlus/DExtra/DCS, REF/XRF/XLX/DCS reflectors)
- APRS KISS mode (packet radio)
- Auto-detect Reflector Terminal Mode on startup
- Auto-download Pi-Star host files
- Plain text output, one line at a time
- No box drawing, escape sequences, or cursor repositioning

## Usage

```
thd75-repl [--port /dev/cu.usbmodem1234] [--baud 115200]
```

## D-STAR Gateway

```
d75> dstar start KQ4NIT
dstar> link REF030C
dstar> monitor
dstar> unlink
dstar> dstar stop
```

## Logging

By default no log file is created and no tracing output is written —
the terminal only shows normal REPL output. File logging is opt-in
because trace-level capture during D-STAR voice flow generates large
files fast (~1 MB/s per active reflector link).

To enable a log file, pass `--log-level` or `--trace`:

```
thd75-repl --trace               # trace level — captures every packet
thd75-repl --log-level=debug     # state transitions + decoded events
thd75-repl --log-level=info      # high-level session flow only
```

File location (rotated daily):

- macOS: `~/Library/Logs/thd75-repl/thd75-repl.log.<date>`
- Linux: `~/.local/state/thd75-repl/thd75-repl.log.<date>`
- Windows: `%LOCALAPPDATA%\thd75-repl\logs\thd75-repl.log.<date>`

For live stderr output (power users, no file), set `RUST_LOG`:

```
RUST_LOG=dstar_gateway=debug thd75-repl
RUST_LOG=dstar_gateway=trace,kenwood_thd75::slow_data=debug thd75-repl
```

`RUST_LOG` and `--log-level` are independent — you can combine them
(live stderr stream + persistent file) or use either alone.

## Requirements

- Rust 1.89+
- Kenwood TH-D75 connected via USB or Bluetooth
