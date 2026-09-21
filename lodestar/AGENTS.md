# Lodestar (iPadOS + native macOS app: not iPhone, not Mac Catalyst)

**Never run window-hosting GUI experiments unattended** (NSWindow-in-XCTest probes,
launching the app binary from a background shell, AppleScript window manipulation):
a window-hosting test host SIGKILLed mid-layout trips the userspace watchdog and
takes down the WindowServer with the whole GUI session. Layout behavior that needs a
live window is verified by a human running the app from Xcode; pure logic goes in
unit tests (`RailState`-style), and that is the only automated layout-test layer.

## Dev loop

1. Rust-side change: `./lodestar-core/scripts/build-xcframework.sh` (regenerates `LodestarKit.xcframework` and `lodestar/Generated/LodestarCore.swift`).
2. Swift-side change or new source file: `cd lodestar && xcodegen generate`.
3. Run the `LodestarMac` scheme for Bluetooth over IOBluetooth; `LodestarIPad` for iPadOS (direct radio via the embedded DriverKit dext, on-device only).
4. Tests: `xcodebuild test -project Lodestar.xcodeproj -scheme LodestarMac -destination "platform=macOS"`.
5. iPad build check: `xcodebuild -project Lodestar.xcodeproj -scheme LodestarIPad -destination 'generic/platform=iOS' CODE_SIGNING_ALLOWED=NO build`.

- `xcodebuild -quiet` suppresses the test summary in current Xcode; run unquieted and grep for `Executed N tests` / `** TEST SUCCEEDED **`.
- Use `-jobs 2` for interactive builds; a full-parallel build pushes the machine into memory pressure.
- SourceKit reports phantom "Cannot find type" errors for cross-file Swift types here. xcodebuild is the only authority; do not chase them.

## Xcode project

- Managed by XcodeGen 2.42+. **Never hand-edit `Lodestar.xcodeproj`**; regenerate with `xcodegen generate` after any `project.yml` change.
- **`xcodegen generate` rewrites tracked files**: `iPad/Info.plist` comes from `project.yml`'s `info.properties`, so any commit touching `project.yml` must include the regenerated plists.
- `Generated/LodestarCore.swift` is UniFFI-emitted, gitignored, and compiled into both app targets, so its functions are module-scoped globals.
- `NSObject.version` shadows the generated `version()`, so `XCTestCase` subclasses must qualify: `Lodestar.version()`.
- The xcframework wraps a **static** library: set `embed: false` in XcodeGen `dependencies`. No `MODULEMAP_FILE` setting is needed; the build script stages the modulemap for auto-discovery.
- Both app targets set `PRODUCT_MODULE_NAME: Lodestar` so `@testable import Lodestar` resolves whichever host the test bundle binds to; product names stay per-target so `TEST_HOST` auto-derivation works.
- Without `SUPPORTS_MAC_DESIGNED_FOR_IPHONE_IPAD: NO` (and `SUPPORTS_MACCATALYST: NO`), "My Mac" launches the iPad binary as "Designed for iPad", where `os(macOS)` is false and the not-available UI shows. Verify the scheme when "My Mac" is the destination.
- Deployment targets iPadOS 26+ / macOS 26+; modern layout, toolbar and Map APIs are used unconditionally with no `#available` gates.

## Platform scope (do not relitigate without a new Apple document)

- iPhone has no **control** path to the TH-D75: Bluetooth Classic SPP is MFi-gated (Core Bluetooth is BLE/GATT only, never RFCOMM), USB-C CDC matching succeeds but every `IOServiceOpen` returns `kIOReturnNotPermitted`, and the radio has no MFi chip. `IORegistryEntryGetPath` still works, so presence detection is possible where communication is not.
- The iPhone USB **audio** path is open and hardware-verified: the radio enumerates as USB audio input "ADC stream IN" and iOS routes recording from it. Audio-only RX features would work; CAT and relay cannot.
- Mac Catalyst is dropped: `IOBluetoothDevice` is unavailable there, so RFCOMM cannot run.
- Pre-M iPads have the same restriction as iPhone and degrade to reflector-only.
- **Never query audio formats through a temporary `AVAudioEngine`**: `AVAudioEngine().inputNode.inputFormat(forBus: 0)` is a use-after-free (ARC frees the engine once `.inputNode` returns; the back-pointer is unretained) and crashes in `AVAudioIONodeImpl::AUI()`. Derive formats from the activated `AVAudioSession` instead. A long-lived dormant engine is also wrong: its inputNode format does not track route changes and dealloc races config-change callbacks.

## macOS Bluetooth (IOBluetooth)

- The app process never owns an `IOBluetooth` object. `IOBluetoothTransport` re-executes the signed Lodestar binary with a private environment handshake; `IOBluetoothHelper.m` takes over in an Objective-C constructor before SwiftUI main. The child owns SDP, the RFCOMM channel, callbacks, synchronous writes, and its main-thread run loop.
- The picker opens only the exact selected address, and a connection is published only after exact CAT `ID TH-D75` or a complete MMDVM `GetVersion` frame proves the endpoint. TH-D75 channel 2 is Lodestar model policy, not a shared native default.
- The parent first reads an exact 16-byte `KENWBT-READY-v2!` prefix. Radio-open mode adds 18 endpoint bytes (17 ASCII address bytes plus one raw RFCOMM channel byte); read exactly 34 bytes so coalesced radio ingress stays untouched. Swift validates address and channel before handing stdout to the raw-stream reader. Inventory and echo modes carry the prefix alone.
- A separate inherited liveness FD makes parent death fatal to the helper even if its main thread wedges in IOBluetooth. Open has one 22-second parent deadline around the native 20-second SDP/baseband/channel deadline.
- Any cancelled, timed-out, partial or failed parent pipe write kills and reaps the helper and poisons that transport until an explicit fresh `open()`. Reads drain through one persistent dispatch source into actor-owned buffering; cancelling a parked caller never performs a native read or discards buffered bytes. Close gives EOF-driven channel-only cleanup 600 ms, then kills and reaps. The helper never closes the shared Bluetooth baseband.
- Persistent terminal-mode setup derives Menu 985 from the connected transport, updates Menu 985 and Menu 650 in one read-back-verified MCP session, then reopens only the same exact address for up to 90 seconds. Silence, malformed prefixes and early CAT are not success.
- **Native Bluetooth needs the binary signed** (`CODE_SIGN_IDENTITY: "-"`, `CODE_SIGNING_ALLOWED: YES`, `CODE_SIGNING_REQUIRED: YES`) and `NSBluetoothAlwaysUsageDescription` in the macOS Info.plist. Without both, Run either errors "must be code-signed" or the first BT call silently fails.

## iPad direct radio (DriverKit USB CDC)

- The radio (VID `0x2166`, PID `0x9023`) is a 4-interface composite device; **interface 1 is CDC Data class `0x0A`** (bulk IN + bulk OUT, the byte pipe). Interface 0 is CDC ACM control, 2 and 3 are USB audio and stay with the system stack.
- The personality matches interface 1 by `idVendor + idProduct + bConfigurationValue + bInterfaceNumber` with probe score 90000, outranking class-based system drivers. **Never add `IOMatchCategory` or `IOProbeScore`.** Endpoint addresses are discovered at `Start` by walking descriptors, never hardcoded.
- Wire bytes above the transport are identical to the macOS Bluetooth path: `MmdvmReader`, `MmdvmWriter`, `RadioModeProber` and `RelayCoordinator` reuse unchanged. Only the byte pump differs.
- Selector contract is duplicated in `Shared/Transport/USBSerialLink.swift` and `Driver/LodestarUserClient.iig`; **keep the two tables in sync**: 0 write (struct in <= 4096, `kIOReturnNoResources` means backpressure), 1 read (struct out <= 4096, 0 bytes means empty, never blocks), 2 armDoorbell (async completion, one-shot), 3 status, 4 copyLog.
- The dext fires the doorbell only on the RX ring's empty-to-non-empty edge (or immediately if armed while non-empty); the app re-arms FIRST, then drains until empty. That ordering closes every race and makes completion throttling harmless.
- iPadOS has no SystemExtensions framework: dexts inside an iPad app bundle are auto-discovered at install and loaded on USB match once the user enables the driver in Settings. There is no activation API to call. A new dext version only takes effect after the radio is unplugged and replugged. A missing Drivers toggle in Settings has been an iPadOS bug; the fix is updating iPadOS, not the app.
- Dext bundle ID must use the app bundle ID as a strict prefix; Apple enforces this at signing. The dext is embedded at `<App>.app/SystemExtensions/<dext-bundle-id>.dext`.
- Entitlements: the app carries `com.apple.developer.driverkit.communicates-with-drivers` (the iPadOS user-client key; `userclient-access` is macOS-only). The dext carries `com.apple.developer.driverkit` plus `com.apple.developer.driverkit.transport.usb`, wildcard `idVendor = "*"` **as a String** for development (any paid account, no Apple approval), swapped for the granted Integer form only for distribution. Same-team apps need no `userclient-access` or `allow-third-party-userclients` on the dext.
- The iOS SDK ships the IOKit user-client C API but no Swift module, so `import IOKit` fails; `iPad/IOKitShim.h` bridges it in. Function-like macros such as `kIOReturnNoResources` do not cross the bridge; redeclare needed values as Swift constants.
- DriverKit on iPadOS runs on M1+ only, and the Simulator does not support DriverKit at all. Charge-only USB-C cables enumerate nothing.
- The `LodestarDriver` target builds only as an embed dependency of `LodestarIPad`; an iOS scheme cannot target `driverkit` directly.

## UI rules

- The wide/narrow branch in `SessionCanvas` is driven by >= 700 pt of *actual available width* (never device idiom) and MUST stay synchronous through `GeometryReader`. An `onGeometryChange` state round-trip is a layout loop: the observer dies with the branch its own write replaces, freezing the initial branch forever.
- Map elevation stays `.flat`: `.realistic` 3D terrain trips a Metal API Validation abort under load in debug runs on device.
- One destination means no navigation chrome: both platforms use a plain `NavigationStack`. Reach for `NavigationSplitView` or `TabView` only when `AppRoute` grows a second case.
- All per-protocol colors and symbols come from `ProtocolStyle.swift`; never inline a protocol color. Empty states use `ContentUnavailableView`, never a blank screen.
- The relay is never a user-facing toggle. DPlus auth failure is surfaced verbatim from the core error, which already suggests XRF/DCS alternatives; do not rewrite it in the UI.

## Background execution (iPad)

- `UIBackgroundModes: audio`: a linked reflector session keeps `ReflectorAudioPlayer`'s engine rendering (zero volume while muted or relay-owned) so the process, the reflector UDP session and the USB user client survive backgrounding. Do not "optimize" the silent rendering away; it IS the keep-alive.
- The keep-alive begins on link, deliberately survives the unexpected-disconnect backoff, and is released only on user disconnect, terminal end, or backoff exhaustion.
- `.inactive` never tears down: it fires for Notification Center pulls and call UI, where teardown would kill a live session. `.background` tears down only when no reflector is linked.

## Swift and FFI

- `SWIFT_VERSION: "5.10"` with `SWIFT_STRICT_CONCURRENCY: complete`. Forced by UniFFI 0.31, whose callback-interface scaffolding fails under Swift 6 language mode regardless of strict-concurrency level. Hand-written Swift still uses Swift 6 idioms.
- The CAT, reflector and UniFFI contracts are owned by `lodestar-core/AGENTS.md`; do not restate them here. Swift-side consequence worth keeping: proc-macro exports map `Vec<u8>` to `Data` while UDL `sequence<u8>` maps to `[UInt8]`. Bridge at call sites with `Array(data)` / `Data(array)`.
- `ReflectorObserver` callbacks arrive on a background tokio task: hop to the main actor before touching `@Observable` state, and never block inside a callback.
- Switch over `ReflectorEvent` exhaustively; new variants are a Swift-side breaking change.
- `headerBytes` and `voiceBytes` are forwarded to the radio as MMDVM frames unchanged. Swift never re-frames or pads them.
- The reflector list comes from the host files bundled in `lodestar-core/data/`. Never hand-roll a list on the Swift side.
- Relay runs only when the radio mode is `.mmdvm` and a reflector session is live; both preconditions are checked in `RelayCoordinator.start()`. Outbound picks a fresh non-zero u16 stream ID per header and increments `seq` per frame; inbound uses the reflector's stream ID.
- `RadioModeProber` sends `[0xE0, 0x03, 0x00]` and classifies: `0xE0` first byte is `.mmdvm`; no response is `.cat` (MMDVM firmware always answers, so silence means CAT); `?` or `N` is `.cat` (CAT mode can reply); anything else is `.unrecognized`.
- `TransportCoordinator.probeRadioMode()` runs inside `connect()`. Gate it on `transport != nil`, never on `state == .connected`, which is set asynchronously by the state-observer task and races the probe.
