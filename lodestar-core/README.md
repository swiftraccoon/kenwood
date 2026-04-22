# lodestar-core

Rust core for [Lodestar](../lodestar) — the native macOS and iOS/iPadOS
D-STAR gateway app for the Kenwood TH-D75.

Wraps `dstar-gateway-core`, `dstar-gateway`, and `kenwood-thd75` and exposes
them to Swift via [UniFFI](https://mozilla.github.io/uniffi-rs/). The Swift
bindings ship as `LodestarKit.xcframework`, produced by
`scripts/build-xcframework.sh`.

## What's exposed

- `version()` — crate semver.
- CAT: `encode_cat`, `parse_cat_line` covering the `ID` identify command.
- MCP: page read/write primitives for flipping menu 650 (DV Gateway) into
  Reflector Terminal Mode.
- MMDVM: frame codec and `mmdvm_get_version_probe` for radio-mode detection.
- Reflector sessions: async `connect_reflector` + `ReflectorSession` with
  `send_header` / `send_voice` / `send_eot` over DPlus / DExtra / DCS, plus a
  `ReflectorObserver` callback protocol Swift implements to receive voice
  events and slow-data updates (TX text + DPRS/NMEA GPS).
- `decode_radio_header` — parse the radio's 41-byte on-wire D-STAR header
  into typed fields so Swift can synthesise a local "recently heard" entry
  for operator-originated transmissions (reflectors don't echo the sender
  back).

## License

GPL-2.0-or-later OR GPL-3.0-or-later.
