// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

#if os(iOS)

import Foundation
import OSLog

private let log = Logger(subsystem: "org.swiftraccoon.lodestar", category: "usb-serial")

/// iPad direct-radio transport over USB-C CDC, mediated by a DriverKit
/// driver extension (`org.swiftraccoon.lodestar.ipad.driver`).
///
/// ## Architecture
///
/// ```
/// LodestarIPad.app  ──IOServiceOpen──▶  LodestarDriver.dext  ──USB-CDC──▶  TH-D75
///   (Swift)          + external           (C++ subclass of                  (over USB-C cable)
///                    method calls)         IOUserUSBSerial)
/// ```
///
/// **iPadOS does not use `OSSystemExtensionRequest`** — that API is
/// `API_UNAVAILABLE(ios)` (verified against the iPhoneOS 26.5 SDK
/// headers in `SystemExtensions.framework`). On iPadOS, dexts that ship
/// inside an app bundle are loaded **automatically** when the matching
/// USB device is plugged in, provided:
///
/// 1. The .dext is at `<App>.app/SystemExtensions/<dext-bundle-id>.dext`.
/// 2. The dext's `IOKitPersonalities` matches the connected USB device
///    by VID `0x2166` / PID `0x9023` (TH-D75 — verified via
///    `thd75/src/transport/serial.rs:106`).
/// 3. The user has approved this app to install drivers in
///    Settings → Privacy & Security → Driver Extensions.
///
/// Once loaded, the app opens the dext's user-client connection via
/// `IOServiceOpen`, then sends external method requests. The wire bytes
/// above this transport — CAT, MMDVM frames, KISS — are **identical**
/// to the macOS Bluetooth path: `MmdvmReader`, `MmdvmWriter`,
/// `RadioModeProber`, `RelayCoordinator`, and the whole CAT stack reuse
/// unchanged.
///
/// ## Status
///
/// This Swift side is scaffolded. The dext source (`lodestar/Driver/`)
/// is **not yet implemented** — `open()` throws clearly until it's
/// built, signed, and matched. See `lodestar/CLAUDE.md` for the
/// "iPad Direct-Radio (DriverKit USB-CDC)" plan and prerequisites
/// (Apple Developer Program, M-series iPad, transport entitlement
/// approval).
public actor USBSerialTransport: RadioTransport {
    public let device: BluetoothDevice
    private var _state: RadioTransportState = .disconnected
    private let stateContinuation: AsyncStream<RadioTransportState>.Continuation
    public nonisolated let stateStream: AsyncStream<RadioTransportState>

    /// Synthetic device descriptor — the iPad USB path doesn't need a
    /// device-picker like Bluetooth does (one cable, one radio), so we
    /// use a fixed descriptor matching the TH-D75 USB IDs.
    public static let synthetic: BluetoothDevice = BluetoothDevice(
        id: "usb:2166:9023",
        name: "TH-D75 (USB-C)",
        address: "USB-CDC"
    )

    public init(device: BluetoothDevice = USBSerialTransport.synthetic) {
        self.device = device
        var cont: AsyncStream<RadioTransportState>.Continuation!
        self.stateStream = AsyncStream { c in cont = c }
        self.stateContinuation = cont
    }

    public var state: RadioTransportState { _state }

    /// Probe for a connected TH-D75 over USB.
    ///
    /// Returns `[synthetic]` if the dext reports a matched device, else
    /// `[]`. Until the dext lands, always returns `[]`.
    public nonisolated static func availableDevices() -> [BluetoothDevice] {
        log.debug("USBSerialTransport.availableDevices: dext not implemented yet")
        return []
    }

    public func open() async throws {
        updateState(.connecting)
        let reason = "USB-C DriverKit transport is not implemented yet. " +
            "The dext (LodestarDriver) needs to be built, the " +
            "`com.apple.developer.driverkit.transport.usb` entitlement " +
            "approved by Apple, and activated via System Extensions."
        updateState(.failed(message: reason))
        throw RadioTransportError.notAvailableOnPlatform(reason: reason)
    }

    public func close() async {
        updateState(.disconnected)
        stateContinuation.finish()
    }

    public func write(_ bytes: [UInt8]) async throws {
        _ = bytes
        throw RadioTransportError.notConnected
    }

    public func read(maxBytes: Int) async throws -> [UInt8] {
        _ = maxBytes
        throw RadioTransportError.notConnected
    }

    private func updateState(_ new: RadioTransportState) {
        _state = new
        stateContinuation.yield(new)
    }
}

#endif
