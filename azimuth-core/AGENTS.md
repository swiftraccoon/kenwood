# azimuth-core

Rust control engine for Azimuth, the iPadOS and macOS TH-D75 app, exposed to
Swift through UniFFI as `AzimuthCore.xcframework`. Unlike `lodestar-core` it
depends on `kenwood-thd75` directly, and it controls only the modified
firmware: CAT identity `1.03.AZM` with automation ABI 3, attested by
`qualify_automation()` before control starts.

## Commands

```bash
./azimuth-core/scripts/build-xcframework.sh   # azimuth/AzimuthCore.xcframework and azimuth/Generated; runs from anywhere
cargo nextest run -p azimuth-core
```

- The script needs the Apple targets: `rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios aarch64-apple-darwin x86_64-apple-darwin`.
- On macOS, `./lint.sh` (full, or `-p azimuth-core`) also builds and tests the app; the steps are in `azimuth/AGENTS.md`.
- `src/bin/uniffi-bindgen.rs` builds only with `--features bindgen-cli`; the script runs it so the generator matches the `uniffi` dependency.

## Rules

- Automation CAT, APRS KISS and IF-DSP radio setup are mutually exclusive owners of the serial stream. Stopping APRS requalifies automation; stopping IF-DSP restores the saved radio state, and automation resumes only after that restoration succeeds.
- Settings writes take the snapshot identifier from `read_setting_values`. `apply_setting_changes` validates the whole batch before I/O, and the TH-D75 compare-and-exchange page primitive compares every affected live page before the first write, so a setting changed since the snapshot causes zero writes. Menu 650, Menu 980, the power-on bitmap and RF transmission stay outside this generic planner.
- Values crossing the FFI are exact raw storage values; a `ScaledInteger` record converts through `decode_setting_display_value` and `encode_setting_display_value`.
- IF-DSP retuning steps with read-back-verified UP/DW; direct FO/FQ writes stay quarantined, as in `kenwood-thd75`.
- On macOS the native Bluetooth helper is the separately signed `AzimuthBluetoothHelper` beside the app executable (`bundled_bluetooth_helper_executable`), never `current_exe()`: a child of the sandboxed app may carry only the sandbox and inherit entitlements.
- Lint divergence, forced by UniFFI scaffolding: no `unsafe_code` rule (hand-written source has no unsafe), no `unused_qualifications`, and `missing_const_for_fn` allowed. UniFFI gotchas are owned by `lodestar-core/AGENTS.md`.
