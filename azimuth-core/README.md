# azimuth-core

`azimuth-core` is the independent Rust control engine for Azimuth, the native
iPadOS and macOS TH-D75 app.

The Apple app owns USB discovery and implements the generated asynchronous
`ByteTransport` protocol. Rust owns the protocol stream and exposes:

- exact V1.03.AZM automation qualification and a refusal canary before control starts;
- authenticated 240x180 screen capture in RGB565LE and display-ready RGBA8888;
- one-use screen leases for guarded front-panel taps, followed by mandatory
  post-tap recapture;
- all 400 writable MCP-D75 setting records from the authoritative generated
  `kenwood-thd75` registry;
- typed live setting reads using one minimal sparse MCP session;
- pure plan validation for boolean, integer, text, byte, enum, and finite
  choice values;
- explicit specialized presentation for the bitmap and scaled GPS coordinate
  fields, with checked raw-storage and display-seconds conversion helpers;
- automatic user-approved batch execution with typed preconditions and an
  exact full-page compare before any verified write.

## Setting approval safety

`read_setting_values` retains the complete MCP pages behind an opaque snapshot
identifier. Every accepted `SettingChange` repeats that identifier, the value
shown during review, and the desired value. `apply_setting_changes` validates
the complete batch before I/O, checks the typed preconditions, and uses the
TH-D75 compare-and-exchange page primitive. That primitive reads every affected
live page and compares every byte before starting the first write. A changed
radio setting therefore invalidates the approval and causes zero writes.

All typed values read from or written to the controller are the exact raw
storage values. A catalog record with `ScaledInteger` presentation must use
`decode_setting_display_value` and `encode_setting_display_value` for its
user-facing seconds value. A `Blob` record requires its own editor.

The TH-D75 cannot make a multi-page write physically atomic. If USB fails after
one page has been written and verified, the returned apply error clearly says
which page writes may have started.

## Build

Run `scripts/build-xcframework.sh` on macOS with the listed Rust Apple targets
installed. It creates `azimuth/AzimuthCore.xcframework` with iPadOS device,
iPadOS simulator, and native macOS slices, then writes the generated Swift
binding source to `azimuth/Generated`.

## License

GPL-2.0-or-later.
