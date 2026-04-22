# Lodestar — native macOS + iOS / iPadOS app

D-STAR gateway app for the Kenwood TH-D75. Runs on iPhone (iOS 17+),
iPad (iPadOS 17+), and Mac (native macOS 14+). Bluetooth Classic SPP
to the radio is macOS-only right now because `IOBluetoothDevice` is
unavailable on Mac Catalyst and iOS Core Bluetooth Classic is GATT-only.

## Build

```bash
# One-time: build the Rust xcframework
../lodestar-core/scripts/build-xcframework.sh

# Generate the Xcode project
xcodegen generate

# Open in Xcode
open Lodestar.xcodeproj
```

## License

GPL-2.0-or-later OR GPL-3.0-or-later.
