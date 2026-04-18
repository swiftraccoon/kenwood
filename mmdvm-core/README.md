# mmdvm-core

Sans-io codec for the MMDVM ham-radio digital-voice modem protocol. Runtime-agnostic, I/O-free.

## Scope

- `MmdvmFrame` — the `[0xE0, len, cmd, payload]` wire-frame type and its codec.
- `command` module — every MMDVM protocol byte constant (GET_STATUS, GET_VERSION, GET_CAPABILITIES, SET_CONFIG, SET_MODE, SEND_CWID, per-mode TX/RX commands, ACKs / NAKs, debug frames).
- `ModemMode` — operating-mode enum. D-STAR is the primary exercise target; DMR / YSF / P25 / NXDN / POCSAG / FM command bytes are present but less tested.
- `Capabilities` — decoder for the `GET_CAPABILITIES` / `GET_VERSION` response frame.
- `Config` — encoder for the `SET_CONFIG` frame (all ~40 fields: audio gains, RF power, per-mode enables, TX delay, etc.).
- Per-mode frame types: D-STAR header / voice / voice-sync / EOT as typed variants; DMR / YSF / P25 / NXDN / POCSAG / FM as byte-level passthroughs.

## Reference implementation

Tracks the C++ `MMDVMHost` codec at `ref/MMDVMHost/Modem.cpp` / `Modem.h` (GPL-2.0-or-later) byte for byte. Divergence is a bug.

Portions of this code are derived from MMDVMHost by Jonathan Naylor G4KLX, 2015–2026, GPL-2.0-or-later.

## Scope boundaries

No transport, no async, no session state. The tokio async shell is [`mmdvm`](../mmdvm/).

## Status

Pre-release. Public API is unstable. Frame types for non-D-STAR modes are present but under-exercised.

## Reference

MMDVMHost: <https://github.com/g4klx/MMDVMHost>

Part of the [kenwood](..) workspace. License: GPL-2.0-or-later.
