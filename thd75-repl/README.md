# thd75-repl

Accessible command-line REPL for the Kenwood TH-D75 transceiver. Designed for screen-reader compatibility following WCAG 2.1 guidelines.

## Features

- CAT radio control (frequency, mode, squelch, power, VOX, etc.)
- D-STAR reflector gateway (DPlus/DExtra, REF/XRF/XLX reflectors)
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

## Requirements

- Rust 1.89+
- Kenwood TH-D75 connected via USB or Bluetooth
