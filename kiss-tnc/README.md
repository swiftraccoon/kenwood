# kiss-tnc

KISS TNC wire-framing codec per Chepponis/Karn (1987). `no_std` + `alloc`, sans-io.

## Scope

- `KissFrame` with `encode_kiss_frame` / `decode_kiss_frame` one-shot codec.
- `KissDecoder` streaming state machine for reassembly across serial-read boundaries.
- `KissCommand` (Data, TxDelay, Persistence, SlotTime, TxTail, FullDuplex, SetHardware, Return) and `KissPort` newtype with `TH_D75` constant.
- FEND / FESC / TFEND / TFESC byte-stuffing constants.

Non-goals: AX.25 parsing (see [`ax25-codec`](../ax25-codec/)), APRS parsing (see [`aprs`](../aprs/)), transport I/O, radio-specific firmware quirks (e.g. the TH-D75's CMD_RETURN nibble encoding, handled in [`kenwood-thd75`](../thd75/)).

## Status

Extracted from `kenwood-thd75` April 2026. Pre-release. Public API is unstable and will change without notice as additional consumers (TM-D750, standalone TNC host) are integrated. Pin to a specific git SHA if you need stability.

## Reference

KISS protocol: <http://www.ka9q.net/papers/kiss.html>

Part of the [kenwood](..) workspace. License: GPL-2.0-or-later.
