# lodestar-core

Rust core for [Lodestar](../lodestar), the native macOS and iPadOS
D-STAR gateway app for the Kenwood TH-D75.

Wraps `dstar-gateway-core`, `dstar-gateway`, `mmdvm-core`, and `mbelib-rs` and
exposes them to Swift via [UniFFI](https://mozilla.github.io/uniffi-rs/). The Swift
bindings ship as `LodestarKit.xcframework`, produced by
`scripts/build-xcframework.sh`.

## What's exposed

- `version()`: crate semver.
- Audio: `RxAudioPipeline`, which AMBE-decodes reflector voice frames into 8 kHz
  mono PCM for on-device monitoring.
- CAT: `encode_cat`, `parse_cat_line` covering the `ID` identify command.
- MCP: page read/write primitives for flipping menu 650 (DV Gateway) into
  Reflector Terminal Mode.
- MMDVM: frame codec and `mmdvm_get_version_probe` for radio-mode detection.
- Reflector sessions: async `connect_reflector` + `ReflectorSession` with
  `send_header` / `send_voice` / `send_eot` over DPlus / DExtra / DCS, plus a
  `ReflectorObserver` callback protocol Swift implements to receive voice
  events and slow-data updates (TX text + DPRS/NMEA GPS).
- `decode_radio_header`: parse the radio's 41-byte on-wire D-STAR header
  into typed fields so Swift can synthesise a local "recently heard" entry
  for operator-originated transmissions (reflectors don't echo the sender
  back).
- Reflector directory: `default_reflectors()` plus live-directory support via
  `parse_hosts_text`, `merge_directories`, and the `DirectoryEntry` /
  `DirectorySource` types for provenance-aware merging of bundled and DPlus
  auth-server sources.
- Logging bridge: `init_tracing()` installs a `tracing` subscriber that
  forwards each event to a Swift-implemented `LogSink` (with `LogLevel`),
  routing Rust diagnostics into the app's Log Viewer and Apple's Unified Log.

## License

GPL-2.0-or-later OR GPL-3.0-or-later.
