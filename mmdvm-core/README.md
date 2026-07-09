# mmdvm-core

Sans-io codec for the MMDVM ham-radio digital-voice modem protocol. Runtime-agnostic, I/O-free.

## Scope

- `MmdvmFrame` — the `[0xE0, len, cmd, payload]` wire-frame type and its codec.
- `command` module — every MMDVM protocol byte constant (GET_STATUS, GET_VERSION, SET_CONFIG, SET_MODE, SET_FREQ, SEND_CWID, per-mode TX/RX commands, ACKs / NAKs, debug frames).
- `ModemMode` — operating-mode enum. D-STAR is the primary exercise target; DMR / YSF / P25 / NXDN / POCSAG / FM command bytes are present but less tested.
- `Capabilities` — a two-byte bitfield struct (`cap1` / `cap2`) exposing the capability bits embedded in the protocol-v2 `GET_VERSION` response (there is no standalone capabilities command or frame).
- `ModemConfig` — a provisional 6-field `SET_CONFIG` parameter struct (invert flags, mode-enable flags, TX delay, mode, RX level, TX level). Stub only: it has no wire encoder yet, just an `idle()` constructor.
- A single generic `MmdvmFrame` (command byte + raw payload) carries every mode — there are no per-mode typed variants. D-STAR / DMR / YSF / P25 / NXDN / POCSAG / FM are distinguished by the command-byte constant, not by the frame type.

## Reference implementation

Tracks the C++ `MMDVMHost` codec ([`Modem.cpp`](https://github.com/g4klx/MMDVMHost/blob/master/Modem.cpp) / [`Modem.h`](https://github.com/g4klx/MMDVMHost/blob/master/Modem.h), GPL-2.0-or-later) byte for byte. Divergence is a bug.

Portions of this code are derived from MMDVMHost by Jonathan Naylor G4KLX, 2015–2026, GPL-2.0-or-later.

## Scope boundaries

No transport, no async, no session state. The tokio async shell is [`mmdvm`](../mmdvm/).

## Status

Pre-release. Public API is unstable. Command bytes and `ModemMode` support for non-D-STAR protocols are present but under-exercised.

## Reference

MMDVMHost: <https://github.com/g4klx/MMDVMHost>

Part of the [kenwood](..) workspace. License: GPL-2.0-or-later.
