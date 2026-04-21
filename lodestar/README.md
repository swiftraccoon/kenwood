# Lodestar — iOS / iPadOS / Mac Catalyst app

D-STAR gateway app for the Kenwood TH-D75. Runs on iPhone (iOS 17+),
iPad (iPadOS 17+), and Mac (Mac Catalyst, macOS 14+).

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
