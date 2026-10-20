# Azimuth (iPadOS + native macOS app for the TH-D75)

Never run window-hosting GUI experiments unattended; the reason is in
`lodestar/AGENTS.md` and applies to this app too.

## Dev loop

1. Rust-side change: `./azimuth-core/scripts/build-xcframework.sh` (writes the gitignored `AzimuthCore.xcframework` and `Generated/`).
2. `project.yml` change or new source file: `cd azimuth && xcodegen generate`. `Azimuth.xcodeproj` is generated and gitignored; `iPad/Info.plist` and `macOS/Info.plist` are generated and tracked, and `./lint.sh` regenerates them with `--only-plists` and fails on a stale committed copy.
3. Tests, from `azimuth/`: `xcodebuild test -project Azimuth.xcodeproj -scheme AzimuthMac -destination 'platform=macOS'`. iPad build check: `xcodebuild build -project Azimuth.xcodeproj -scheme AzimuthIPad -destination 'generic/platform=iOS' CODE_SIGNING_ALLOWED=NO`.
4. A physical iPad build needs `project-local.yml` (copy `project-local.example.yml`; gitignored) with the same paid team on `AzimuthIPad` and `AzimuthDriver`. USBDriverKit needs an M-series iPad and the radio; the Simulator covers UI, assistant, catalog and recorded-transport tests only.

## Rules

- macOS is sandboxed. The RFCOMM helper `AzimuthBluetoothHelper` (`macOS/BluetoothHelper/main.c`, which only calls `bt_helper_link_anchor()`) is copied beside the app executable and re-signed by a post-build script with exactly the sandbox and inherit entitlements before Xcode signs the app; any other entitlement on the helper breaks sandbox inheritance. `AzimuthBluetoothHelperPackagingTests` (in `Tests/AzimuthCoreIntegrationTests.swift`) checks the embedded signature and runs a no-radio echo through it.
- The DriverKit user-client selector ABI is append-only: v1 is 0 write, 1 read, 2 armDoorbell, 3 status, 4 copyLog; v2 appends 5 setBaudRate (9600 or 115200). `Driver/AzimuthUserClient.iig` and `AzimuthUSBSelectorV1`/`AzimuthUSBSelectorV2` in `Shared/Transport/USBSerialLink.swift` must agree; `AzimuthUSBSelectorABITests` pins them.
- Radio identity is CAT `AE` on an accepted session, never a USB descriptor serial or a `/dev/cu.usbmodem*` name; USB discovery records the interface's nonzero IORegistry USB-device ancestor. IF capture opens only the CoreAudio input whose ancestor matches the CAT-qualified CDC interface, never the default input, a name match or a sole USB audio device.
- One CAT/MCP owner is active at a time. Connection probing never changes Menu 650: an MMDVM answer leads to a same-radio transport handoff or a separately confirmed recovery, never an automatic Menu 650 write.
- The on-device model proposes catalog setting identifiers and typed values only, never raw CAT bytes or memory offsets, and nothing reaches the radio until the operator accepts the validated diff.
- Hardware tests are opt-in by environment variable: `AZIMUTH_HARDWARE_IPAD_MMDVM_PROMPT=1`, `AZIMUTH_HARDWARE_BLUETOOTH_PRIMARY=1` and the destructive `AZIMUTH_HARDWARE_BLUETOOTH_RECOVERY=1`. `AZIMUTH_VERBOSE_USB_TRACE=1` enables packet-level tracing.
- `SWIFT_VERSION` is 5 with complete strict concurrency; read the UniFFI 0.31 Swift 6 limitation in `lodestar/AGENTS.md` before changing it.
