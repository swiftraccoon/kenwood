// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

#if os(iOS)

import Foundation
import ObjectiveC
import OSLog
import Security
import CoreBluetooth

private let log = Logger(subsystem: "org.swiftraccoon.lodestar", category: "private-bt")

/// iOS transport that attempts to reach a Classic Bluetooth SPP device
/// through Apple's **private** `BluetoothManager.framework`.
///
/// ## Why this exists
///
/// The public iOS SDK offers no Classic BT RFCOMM (SPP) surface —
/// `CoreBluetooth` is BLE-only, `ExternalAccessory` is MFi-gated, and
/// `IOBluetooth.framework` doesn't ship on iOS at all. Apple's own
/// Settings app and CarPlay daemon *do* open RFCOMM channels though,
/// via the private `BluetoothManager.framework` that lives at
/// `/System/Library/PrivateFrameworks/BluetoothManager.framework`.
///
/// ## Compatibility caveats
///
/// This code is sideload-only. App Store submission will reject it on
/// sight because the symbols it uses aren't part of the public SDK.
/// The symbols themselves change between iOS major versions — Apple
/// renamed or removed several classes around iOS 16/17 as part of
/// `bluetoothd` hardening. On any version where the probe fails we
/// fall back cleanly so the app still launches and the rest of the
/// reflector-client UI works.
///
/// ## Strategy
///
/// 1. `dlopen` the framework at its absolute path.
/// 2. Walk a list of candidate class names (`BluetoothManager`,
///    `BTManager`, etc.) via `objc_getClass` and keep the first hit.
/// 3. Message the singleton with `sharedInstance` and enumerate
///    devices via `pairedDevices` / `connectedDevices` / similar.
/// 4. For a chosen device, open an RFCOMM channel via the first
///    selector we find that matches `openRFCOMMChannel*`.
/// 5. Read / write bytes through the channel's delegate callbacks.
///
/// Every step is logged so on failure you can see exactly which
/// symbol was missing and on which iOS version.
public actor PrivateBluetoothTransport: RadioTransport {
    public let device: BluetoothDevice
    private var _state: RadioTransportState = .disconnected
    private let stateContinuation: AsyncStream<RadioTransportState>.Continuation
    public nonisolated let stateStream: AsyncStream<RadioTransportState>

    private var channelHandle: AnyObject?
    private var pendingReads: [[UInt8]] = []
    private var readContinuations: [CheckedContinuation<[UInt8], Error>] = []

    public init(device: BluetoothDevice) {
        self.device = device
        var cont: AsyncStream<RadioTransportState>.Continuation!
        self.stateStream = AsyncStream { c in cont = c }
        self.stateContinuation = cont
    }

    public var state: RadioTransportState { _state }

    // MARK: - RadioTransport

    public nonisolated static func pairedDevices() -> [BluetoothDevice] {
        log.info("PrivateBluetoothTransport.pairedDevices: starting probe")
        guard PrivateBluetoothBridge.loadFramework() else {
            log.warning("Framework did not load; returning empty device list")
            return []
        }
        return PrivateBluetoothBridge.enumeratePairedDevices()
    }

    public func open() async throws {
        updateState(.connecting)
        guard PrivateBluetoothBridge.loadFramework() else {
            let reason = "BluetoothManager.framework could not be dlopen'd (is this a sideloaded build on a device that allows private-framework loading?)"
            log.error("\(reason)")
            updateState(.failed(message: reason))
            throw RadioTransportError.notAvailableOnPlatform(reason: reason)
        }

        do {
            let chan = try PrivateBluetoothBridge.openRFCOMM(
                address: device.address,
                onData: { [weak self] bytes in
                    Task { await self?.deliverRead(bytes) }
                },
                onClose: { [weak self] in
                    Task { await self?.handleUnexpectedClose() }
                }
            )
            self.channelHandle = chan
            updateState(.connected)
        } catch {
            let reason = (error as? RadioTransportError)?.displayMessage ?? "\(error)"
            updateState(.failed(message: reason))
            throw error
        }
    }

    public func close() async {
        if let chan = channelHandle {
            PrivateBluetoothBridge.closeChannel(chan)
        }
        channelHandle = nil
        for c in readContinuations {
            c.resume(returning: [])
        }
        readContinuations.removeAll()
        pendingReads.removeAll()
        updateState(.disconnected)
        stateContinuation.finish()
    }

    public func write(_ bytes: [UInt8]) async throws {
        guard case .connected = _state, let chan = channelHandle else {
            throw RadioTransportError.notConnected
        }
        try PrivateBluetoothBridge.writeBytes(bytes, to: chan)
    }

    public func read(maxBytes: Int) async throws -> [UInt8] {
        if !pendingReads.isEmpty {
            let chunk = pendingReads.removeFirst()
            let slice = Array(chunk.prefix(maxBytes))
            if slice.count < chunk.count {
                pendingReads.insert(Array(chunk.dropFirst(slice.count)), at: 0)
            }
            return slice
        }
        if case .disconnected = _state { return [] }
        return try await withCheckedThrowingContinuation { c in
            readContinuations.append(c)
        }
    }

    // MARK: - Internal

    private func deliverRead(_ bytes: [UInt8]) {
        if let c = readContinuations.first {
            readContinuations.removeFirst()
            c.resume(returning: bytes)
            return
        }
        pendingReads.append(bytes)
    }

    private func handleUnexpectedClose() {
        channelHandle = nil
        updateState(.disconnected)
        for c in readContinuations {
            c.resume(returning: [])
        }
        readContinuations.removeAll()
    }

    private func updateState(_ new: RadioTransportState) {
        _state = new
        stateContinuation.yield(new)
    }
}

/// Nearby Classic BT device seen during inquiry. Reported to the UI
/// as a candidate for the user to pair with.
public struct DiscoveredBluetoothDevice: Identifiable, Hashable, Sendable {
    public let id: String     // BT address
    public let name: String
    public let address: String
    /// RSSI at time of discovery (dBm). `nil` if the private API
    /// didn't report it.
    public let rssi: Int?
}

/// State of an attempted pairing with a discovered device.
public enum BluetoothPairingResult: Sendable {
    case success
    case denied(reason: String)
    case cancelled
    case timeout
}

/// Thin bridge that hides every `dlopen` + Obj-C runtime call. Kept
/// outside the `actor` so the call sites can stay synchronous where
/// Apple's private API is synchronous anyway.
enum PrivateBluetoothBridge {
    /// Candidate class names in order of historical preference. The
    /// first one that resolves via `objc_getClass` wins.
    private static let managerClassCandidates = [
        "BluetoothManager",  // iOS 3–15 lineage
        "BTManager",         // seen on some iOS 16+ dumps
        "BKManager",         // hypothetical
    ]
    private static let deviceClassCandidates = [
        "BluetoothDevice",
        "BTDevice",
        "BKDevice",
    ]

    private static let frameworkPath =
        "/System/Library/PrivateFrameworks/BluetoothManager.framework/BluetoothManager"

    private static var handle: UnsafeMutableRawPointer?

    /// `dlopen` the private framework. Idempotent. Returns `true` if
    /// the library is loaded (either just now or on a prior call).
    @discardableResult
    static func loadFramework() -> Bool {
        if handle != nil { return true }
        let h = dlopen(frameworkPath, RTLD_NOW)
        if h == nil, let err = dlerror() {
            let errStr = String(cString: err)
            log.error("dlopen(\(frameworkPath, privacy: .public)) failed: \(errStr, privacy: .public)")
            return false
        }
        handle = h
        log.info("dlopen succeeded: \(frameworkPath, privacy: .public)")
        return true
    }

    /// Resolve the first candidate class name via the Objective-C
    /// runtime, or `nil` if none of them exist on this iOS version.
    private static func resolveClass(candidates: [String]) -> AnyClass? {
        for name in candidates {
            if let cls = NSClassFromString(name) {
                log.info("resolved class \(name, privacy: .public)")
                return cls
            }
        }
        log.warning("no candidate class matched: \(candidates, privacy: .public)")
        return nil
    }

    /// Probe: dump what's available. Useful both as a diagnostic and
    /// as the entry point for `pairedDevices`.
    static func enumeratePairedDevices() -> [BluetoothDevice] {
        guard let managerClass = resolveClass(candidates: managerClassCandidates) as? NSObject.Type else {
            log.error("BluetoothManager class not found; returning []")
            return []
        }

        // `+sharedInstance` is the universal singleton entry point.
        let sharedSel = NSSelectorFromString("sharedInstance")
        guard managerClass.responds(to: sharedSel) else {
            log.error("manager class does not respond to sharedInstance")
            return []
        }
        guard let shared = managerClass.perform(sharedSel)?.takeUnretainedValue() as? NSObject else {
            log.error("sharedInstance returned nil / non-NSObject")
            return []
        }
        log.info("manager sharedInstance: \(shared, privacy: .public)")

        // Try a sequence of likely selectors to enumerate devices.
        let listSelectors = ["pairedDevices", "connectedDevices", "devices"]
        var deviceList: [NSObject] = []
        for selName in listSelectors {
            let sel = NSSelectorFromString(selName)
            guard shared.responds(to: sel) else { continue }
            guard let raw = shared.perform(sel)?.takeUnretainedValue() as? [NSObject] else {
                log.warning("\(selName, privacy: .public) returned non-array")
                continue
            }
            log.info("\(selName, privacy: .public) returned \(raw.count) devices")
            deviceList = raw
            break
        }
        if deviceList.isEmpty {
            log.warning("no device-list selector returned anything")
            return []
        }

        return deviceList.compactMap(translateDevice)
    }

    /// Translate a private `BluetoothDevice` into our UI-facing model.
    private static func translateDevice(_ obj: NSObject) -> BluetoothDevice? {
        let addrSel = NSSelectorFromString("address")
        let nameSel = NSSelectorFromString("name")
        guard obj.responds(to: addrSel) else {
            log.warning("device does not respond to -address")
            return nil
        }
        let address: String
        if let a = obj.perform(addrSel)?.takeUnretainedValue() as? String {
            address = a
        } else {
            log.warning("-address returned non-string")
            return nil
        }
        let name: String
        if obj.responds(to: nameSel),
           let n = obj.perform(nameSel)?.takeUnretainedValue() as? String {
            name = n
        } else {
            name = address
        }
        return BluetoothDevice(id: address, name: name, address: address)
    }

    /// Best-effort RFCOMM open. Returns an opaque channel handle if
    /// successful; throws a descriptive `RadioTransportError` otherwise.
    ///
    /// `onData` / `onClose` are called from whatever thread the
    /// framework uses — callers should hop onto an actor if they need
    /// isolation.
    static func openRFCOMM(
        address: String,
        onData: @escaping @Sendable ([UInt8]) -> Void,
        onClose: @escaping @Sendable () -> Void
    ) throws -> AnyObject {
        _ = onData
        _ = onClose
        // First attempt probes: find the device by address, then find
        // a selector that opens a channel. The actual wiring of the
        // delegate callbacks is iOS-version-specific and we'll wire
        // it once the probe tells us which selector exists.
        guard let deviceClass = resolveClass(candidates: deviceClassCandidates) as? NSObject.Type else {
            throw RadioTransportError.notAvailableOnPlatform(
                reason: "BluetoothDevice private class missing on this iOS version"
            )
        }

        // Many iOS versions expose `+[BluetoothDevice deviceWithAddress:]`.
        let devByAddrSel = NSSelectorFromString("deviceWithAddress:")
        guard deviceClass.responds(to: devByAddrSel) else {
            throw RadioTransportError.notAvailableOnPlatform(
                reason: "BluetoothDevice missing +deviceWithAddress:"
            )
        }
        let addrNS = address as NSString
        guard let device = deviceClass.perform(devByAddrSel, with: addrNS)?.takeUnretainedValue() as? NSObject else {
            throw RadioTransportError.deviceNotFound(address: address)
        }
        log.info("resolved device: \(device, privacy: .public)")

        // Probe the device for an RFCOMM-opening selector. On every
        // iOS version we've seen these names historically — we pick
        // the first that responds.
        let openCandidates = [
            "openChannel",
            "openRFCOMMChannel",
            "openRFCOMMChannelSync:",
            "openConnection",
            "connect",
        ]
        var which: String?
        for sel in openCandidates {
            if device.responds(to: NSSelectorFromString(sel)) {
                which = sel
                break
            }
        }
        guard let sel = which else {
            throw RadioTransportError.notAvailableOnPlatform(
                reason: "No known RFCOMM-open selector responds on BluetoothDevice; iOS \(iosVersionString()) likely renamed or removed them."
            )
        }
        log.info("attempting RFCOMM open via -[\(sel, privacy: .public)]")

        // At this point we'd call the selector. Without entitlements
        // `bluetoothd` almost certainly denies the connect — we'll
        // log the response but not actually wire delegate callbacks
        // until the probe tells us we can get this far. Return the
        // device object as the "handle" so the actor can close it.
        device.perform(NSSelectorFromString(sel))
        return device
    }

    /// Close whatever channel handle `openRFCOMM` returned.
    static func closeChannel(_ handle: AnyObject) {
        guard let obj = handle as? NSObject else { return }
        let closeCandidates = ["closeChannel", "disconnect", "closeConnection"]
        for name in closeCandidates {
            let sel = NSSelectorFromString(name)
            if obj.responds(to: sel) {
                obj.perform(sel)
                log.info("closed via -\(name, privacy: .public)")
                return
            }
        }
        log.warning("no close-channel selector found")
    }

    static func writeBytes(_ bytes: [UInt8], to handle: AnyObject) throws {
        guard let obj = handle as? NSObject else {
            throw RadioTransportError.writeFailed(reason: "channel handle is not NSObject")
        }
        let writeCandidates = ["writeData:", "writeSync:", "write:"]
        let data = Data(bytes) as NSData
        for name in writeCandidates {
            let sel = NSSelectorFromString(name)
            if obj.responds(to: sel) {
                obj.perform(sel, with: data)
                return
            }
        }
        throw RadioTransportError.writeFailed(
            reason: "No known write selector responds on channel handle"
        )
    }

    private static func iosVersionString() -> String {
        let v = ProcessInfo.processInfo.operatingSystemVersion
        return "\(v.majorVersion).\(v.minorVersion).\(v.patchVersion)"
    }

    // MARK: - Classic BT inquiry + pairing (private API)
    //
    // iOS Settings doesn't surface Classic-BT pairing for non-MFi
    // peripherals, so the app has to drive the whole flow itself. All
    // of this lives inside `BluetoothManager.framework` and is
    // version-fragile — every call probes multiple selector names.

    /// Shared observer that maps the private framework's notifications
    /// into the app's `DiscoveredBluetoothDevice` / pairing callbacks.
    /// Kept as a singleton because notification registration is
    /// process-wide and re-registering on every scan call leaks.
    private static let notificationObserver: InquiryNotificationObserver = {
        InquiryNotificationObserver()
    }()

    /// Start scanning for nearby Classic BT devices. New discoveries
    /// arrive via the `onDevice` callback; call `stopInquiry()` when
    /// done. Returns `true` if the framework accepted the scan request.
    @discardableResult
    static func startInquiry(
        onDevice: @escaping @Sendable (DiscoveredBluetoothDevice) -> Void
    ) -> Bool {
        guard let shared = managerSharedInstance() else {
            log.error("startInquiry: no manager shared instance")
            return false
        }

        // Ensure BT is powered on first. Historical names vary across
        // iOS versions; we probe candidates and silently skip any the
        // daemon doesn't expose.
        for name in ["setPowerEnabled:", "setPower:", "enablePower", "powerOn"] {
            if invokeBool(shared, selector: name, argument: true) {
                log.info("power-on via -\(name, privacy: .public)")
                break
            }
        }

        notificationObserver.setDeviceCallback(onDevice)
        notificationObserver.attachIfNeeded()

        // Selectors, in order of historical preference. Newer iOS
        // versions may have renamed these — we just pick the first
        // that responds.
        let inquirySelectors = [
            "startScan",
            "setDeviceScanningEnabled:",
            "setScanning:",
            "performDeviceInquiry",
            "startInquiry",
        ]
        for name in inquirySelectors {
            let sel = NSSelectorFromString(name)
            guard shared.responds(to: sel) else { continue }
            if name.hasSuffix(":") {
                _ = invokeBool(shared, selector: name, argument: true)
            } else {
                shared.perform(sel)
            }
            log.info("inquiry started via -\(name, privacy: .public)")
            return true
        }
        log.warning("no inquiry selector matched on manager singleton")
        return false
    }

    /// Stop scanning.
    static func stopInquiry() {
        guard let shared = managerSharedInstance() else { return }
        let stopSelectors = [
            "stopScan",
            "setDeviceScanningEnabled:",
            "setScanning:",
            "stopInquiry",
        ]
        for name in stopSelectors {
            let sel = NSSelectorFromString(name)
            guard shared.responds(to: sel) else { continue }
            if name.hasSuffix(":") {
                _ = invokeBool(shared, selector: name, argument: false)
            } else {
                shared.perform(sel)
            }
            log.info("inquiry stopped via -\(name, privacy: .public)")
            return
        }
    }

    /// Issue a pair request for the given address. The private
    /// framework triggers a system pairing sheet (with PIN /
    /// numeric-comparison prompt if the radio asks for one).
    /// `onResult` is called once the OS reports success or failure.
    static func pair(
        address: String,
        onResult: @escaping @Sendable (BluetoothPairingResult) -> Void
    ) {
        guard let device = resolveDevice(address: address) else {
            onResult(.denied(reason: "could not resolve BluetoothDevice for \(address)"))
            return
        }

        notificationObserver.setPairingCallback(onResult)
        notificationObserver.attachIfNeeded()

        let pairSelectors = [
            "pair",
            "connect",
            "createPairing",
            "openConnection",
        ]
        for name in pairSelectors {
            let sel = NSSelectorFromString(name)
            guard device.responds(to: sel) else { continue }
            device.perform(sel)
            log.info("pair initiated via -\(name, privacy: .public)")
            return
        }
        onResult(.denied(reason: "no pair/connect selector responded on device"))
    }

    /// Look up (or create, if allowed) a `BluetoothDevice` by address.
    private static func resolveDevice(address: String) -> NSObject? {
        guard let deviceClass = resolveClass(candidates: deviceClassCandidates) as? NSObject.Type else {
            return nil
        }
        let candidates = ["deviceWithAddress:", "deviceForAddress:"]
        for name in candidates {
            let sel = NSSelectorFromString(name)
            guard deviceClass.responds(to: sel) else { continue }
            let arg = address as NSString
            guard let obj = deviceClass.perform(sel, with: arg)?.takeUnretainedValue() as? NSObject else {
                continue
            }
            return obj
        }
        return nil
    }

    /// Cached manager singleton.
    private static func managerSharedInstance() -> NSObject? {
        guard loadFramework() else { return nil }
        guard let cls = resolveClass(candidates: managerClassCandidates) as? NSObject.Type else {
            return nil
        }
        let sel = NSSelectorFromString("sharedInstance")
        guard cls.responds(to: sel) else { return nil }
        return cls.perform(sel)?.takeUnretainedValue() as? NSObject
    }

    /// Call an Obj-C `BOOL` setter dynamically (e.g. `-setPowerEnabled:`).
    /// Swift's `perform(_:with:)` boxes the argument as an object, but
    /// private setters expect a raw BOOL — route through the C-fn
    /// pointer. Returns `false` and logs when the target doesn't
    /// implement the selector (prevents `unrecognized selector`
    /// crashes from typos / renamed selectors in newer iOS versions).
    private static func invokeBool(_ target: NSObject, selector: String, argument: Bool) -> Bool {
        let sel = NSSelectorFromString(selector)
        guard target.responds(to: sel) else {
            log.debug("invokeBool: target does not respond to -\(selector, privacy: .public)")
            return false
        }
        guard let method = target.method(for: sel) else {
            log.warning("invokeBool: method_for(\(selector, privacy: .public)) returned nil despite responds(to:)")
            return false
        }
        typealias BoolSetter = @convention(c) (AnyObject, Selector, Bool) -> Void
        let fn = unsafeBitCast(method, to: BoolSetter.self)
        fn(target, sel, argument)
        return true
    }

    /// Human-readable diagnostic string for the iOS `DevicePickerSheet`.
    /// Walks every layer of the stack so we can see exactly where
    /// things break on the current iOS / signing combination.
    public static func diagnostic() -> String {
        var lines: [String] = []
        lines.append("=== Lodestar BT Private-API Probe ===")
        lines.append("iOS \(iosVersionString())")
        lines.append("")

        // ---- 1. dlopen ----
        lines.append("[1] dlopen BluetoothManager.framework")
        if loadFramework() {
            lines.append("    ✓ loaded")
        } else {
            if let err = dlerror() {
                lines.append("    ✗ \(String(cString: err))")
            } else {
                lines.append("    ✗ returned nil (dlopen restricted)")
            }
            return lines.joined(separator: "\n")
        }
        lines.append("")

        // ---- 2. Entitlements actually granted to our process ----
        lines.append("[2] Entitlements granted to this process")
        let found = dumpEntitlements()
        if found.isEmpty {
            lines.append("    (no BT-related entitlements found on this binary)")
        } else {
            for (k, v) in found.sorted(by: { $0.key < $1.key }) {
                lines.append("    \(k) = \(v)")
            }
        }
        lines.append("")

        // ---- 3. Resolve manager class ----
        lines.append("[3] Resolve manager class")
        var managerClass: AnyClass?
        for name in managerClassCandidates {
            if let cls = NSClassFromString(name) {
                lines.append("    ✓ \(name)")
                managerClass = cls
                break
            } else {
                lines.append("    ✗ \(name) not found")
            }
        }
        guard let cls = managerClass as? NSObject.Type else {
            return lines.joined(separator: "\n")
        }
        lines.append("")

        // ---- 4. +sharedInstance ----
        lines.append("[4] +sharedInstance")
        let sharedSel = NSSelectorFromString("sharedInstance")
        guard cls.responds(to: sharedSel),
              let shared = cls.perform(sharedSel)?.takeUnretainedValue() as? NSObject
        else {
            lines.append("    ✗ unavailable")
            return lines.joined(separator: "\n")
        }
        lines.append("    ✓ \(shared)")
        lines.append("")

        // ---- 5. Enumerate devices (daemon-backed) ----
        lines.append("[5] Daemon-backed device queries")
        for name in ["pairedDevices", "connectedDevices", "devices",
                     "allDevices", "knownDevices"] {
            let sel = NSSelectorFromString(name)
            if shared.responds(to: sel) {
                let raw = shared.perform(sel)?.takeUnretainedValue()
                if let arr = raw as? [NSObject] {
                    lines.append("    ✓ -\(name) → \(arr.count) device(s)")
                } else if raw == nil {
                    lines.append("    ? -\(name) → nil (XPC probably denied)")
                } else {
                    lines.append("    ? -\(name) → non-array")
                }
            } else {
                lines.append("    ✗ -\(name) not implemented")
            }
        }
        lines.append("")

        // ---- 6. Method enumeration on the singleton ----
        let allMethods = enumerateMethods(ofClass: type(of: shared)).sorted()
        lines.append("[6] BluetoothManager selectors (\(allMethods.count) total)")
        for n in allMethods {
            lines.append("    -\(n)")
        }
        lines.append("")

        // ---- 7. (XPC direct probing skipped: iOS Swift import hides
        //        xpc_connection_create_mach_service; we already have
        //        definitive XPC-denied evidence from BluetoothManager's
        //        own MBFXPC log line.) ----

        // ---- 8. CoreBluetooth sanity check (public API) ----
        lines.append("[8] CoreBluetooth state (public-API reference point)")
        lines.append("    \(probeCoreBluetooth())")
        lines.append("    (if CoreBluetooth is poweredOn, the daemon is reachable")
        lines.append("     to entitled callers — our private-API rejection is")
        lines.append("     specifically because we lack `com.apple.private.bluetooth.*`.)")
        lines.append("")

        // ---- 9. Resolve device class ----
        lines.append("[9] BluetoothDevice class candidates")
        for name in deviceClassCandidates {
            if NSClassFromString(name) != nil {
                lines.append("    ✓ \(name)")
            } else {
                lines.append("    ✗ \(name)")
            }
        }
        lines.append("")

        // ---- 10. Call every state-reader selector we saw ----
        lines.append("[10] Client-side state readers (no XPC required in theory)")
        let zeroArgReaders = [
            "bluetoothState", "bluetoothStateAction",
            "available", "connectable", "connected", "audioConnected",
            "connectedDeviceNamesThatMayBeDenylisted",
            "connectingDevices",
            "_discoveryState",
            "_advertisingState",
            "lastInitError",
        ]
        for name in zeroArgReaders {
            let sel = NSSelectorFromString(name)
            if shared.responds(to: sel) {
                let raw = shared.perform(sel)?.takeUnretainedValue()
                lines.append("    -\(name) → \(describeReturnValue(raw))")
            } else {
                lines.append("    ✗ -\(name) not implemented")
            }
        }
        lines.append("")

        // ---- 11. Explicit _attach call + re-read ----
        lines.append("[11] Force -_attach, then re-read state")
        let attachSel = NSSelectorFromString("_attach")
        if shared.responds(to: attachSel) {
            shared.perform(attachSel)
            Thread.sleep(forTimeInterval: 0.25)
            lines.append("    -_attach called; waited 250 ms")
            let sel = NSSelectorFromString("pairedDevices")
            if let arr = shared.perform(sel)?.takeUnretainedValue() as? [NSObject] {
                lines.append("    -pairedDevices post-attach → \(arr.count)")
            }
        } else {
            lines.append("    ✗ -_attach not implemented")
        }
        lines.append("")

        // ---- 12. (block-completion probe skipped: `perform(_:with:)`
        //        can't safely bridge a Swift @convention(block) closure
        //        to an Obj-C NSBlock arg — the framework crashes when
        //        it invokes the cast pointer.)

        // ---- 13. Known error-code decoding ----
        lines.append("    1301 = BTResult 'authorization required'")
        lines.append("           (daemon checks entitlements on XPC connect; we lack")
        lines.append("           `com.apple.private.bluetooth.*` and are rejected.)")

        return lines.joined(separator: "\n")
    }

    /// Properly await CoreBluetooth state.
    ///
    /// Subtle: `CBCentralManager.state` is published asynchronously on
    /// the queue we pass in. The diagnostic is usually called from
    /// the main thread (button tap); if we block main while waiting,
    /// the callback can never run — classic deadlock. So we dispatch
    /// CB to a private background queue and only block the calling
    /// thread, which is safe regardless of what thread we're on.
    ///
    /// Also reports `CBCentralManager.authorization` (iOS 13.1+) so
    /// we can tell "unknown" (never asked) from "denied" (user said
    /// no) from "allowed but daemon unreachable".
    private static func probeCoreBluetooth() -> String {
        let auth = CBCentralManager.authorization
        let authStr: String
        switch auth {
        case .notDetermined:  authStr = "notDetermined (user has not been asked)"
        case .restricted:     authStr = "restricted"
        case .denied:         authStr = "denied (user refused BT permission)"
        case .allowedAlways:  authStr = "allowedAlways"
        @unknown default:     authStr = "unhandled (\(auth.rawValue))"
        }

        let delegate = CBProbeDelegate()
        let queue = DispatchQueue(label: "org.swiftraccoon.lodestar.cb-probe")
        let manager = CBCentralManager(
            delegate: delegate,
            queue: queue,
            options: [CBCentralManagerOptionShowPowerAlertKey: false]
        )
        delegate.manager = manager
        _ = delegate.semaphore.wait(timeout: .now() + .seconds(3))
        let stateStr = describe(manager.state)
        return "authorization = \(authStr)\n    state = \(stateStr)"
    }

    /// Convert an arbitrary Obj-C return value into a log-friendly
    /// string. Numbers come back boxed as `NSNumber`, strings and
    /// arrays pass through.
    private static func describeReturnValue(_ raw: Any?) -> String {
        guard let raw = raw else { return "nil" }
        if let b = raw as? NSNumber {
            return "NSNumber(\(b))"
        }
        if let s = raw as? String {
            return "\"\(s)\""
        }
        if let a = raw as? [Any] {
            return "array(\(a.count))"
        }
        return "\(raw)"
    }

    // MARK: - Entitlement probing

    // `SecTask*` APIs are callable on iOS but Swift's module map hides
    // the `SecTask` type. Bind by symbol name using `OpaquePointer` to
    // dodge the import-layer gate.
    @_silgen_name("SecTaskCreateFromSelf")
    private static func SecTaskCreateFromSelf_iOS(
        _ allocator: UnsafeRawPointer?
    ) -> OpaquePointer?

    @_silgen_name("SecTaskCopyValueForEntitlement")
    private static func SecTaskCopyValueForEntitlement_iOS(
        _ task: OpaquePointer,
        _ entitlement: CFString,
        _ error: UnsafeMutablePointer<Unmanaged<CFError>?>?
    ) -> Unmanaged<CFTypeRef>?

    /// Query `SecTaskCopyValueForEntitlement` for every plausible BT-
    /// related entitlement name. Returns the ones the code signature
    /// actually carries.
    static func dumpEntitlements() -> [String: Any] {
        let task = Self.SecTaskCreateFromSelf_iOS(nil)
        let candidates: [String] = [
            // Public capabilities (ADP-grantable)
            "com.apple.developer.bluetooth",
            "com.apple.security.device.bluetooth",
            "com.apple.security.bluetooth",
            // Bluetooth-specific private entitlements (Apple-only)
            "com.apple.bluetooth.allow-public-interface",
            "com.apple.bluetoothd",
            "com.apple.private.bluetooth",
            "com.apple.private.bluetooth.allow-connecting-to-external-connect",
            "com.apple.private.bluetooth.system.paired-devices",
            "com.apple.private.bluetoothd",
            "com.apple.private.bluetoothd.xpc",
            "com.apple.private.MobileBluetooth",
            // Daemon-adjacent
            "com.apple.mobile.bluetooth.internal",
            "com.apple.private.accessory-daemon.classic",
            // MFi-ish
            "com.apple.external-accessory.wireless-configuration",
            "com.apple.accessorysetupkit",
            // Generic debug / dev
            "com.apple.security.get-task-allow",
            "get-task-allow",
            "aps-environment",
        ]
        var found: [String: Any] = [:]
        guard let task = task else { return found }
        for key in candidates {
            if let raw = Self.SecTaskCopyValueForEntitlement_iOS(task, key as CFString, nil) {
                found[key] = raw.takeRetainedValue()
            }
        }
        return found
    }

    // MARK: - Method enumeration

    /// Walk both instance and class methods of the manager singleton's
    /// class via the Obj-C runtime. Returns selector names.
    static func enumerateMethods(ofClass cls: AnyClass) -> [String] {
        var names: Set<String> = []
        var count: UInt32 = 0
        // Instance methods
        if let methods = class_copyMethodList(cls, &count) {
            defer { free(methods) }
            for i in 0..<Int(count) {
                let m = methods[i]
                let selector = method_getName(m)
                names.insert(String(cString: sel_getName(selector)))
            }
        }
        // Class methods (by walking the metaclass)
        let meta: AnyClass = object_getClass(cls) ?? cls
        var classCount: UInt32 = 0
        if let methods = class_copyMethodList(meta, &classCount) {
            defer { free(methods) }
            for i in 0..<Int(classCount) {
                let m = methods[i]
                let selector = method_getName(m)
                names.insert("+" + String(cString: sel_getName(selector)))
            }
        }
        return Array(names)
    }

    private static func describe(_ state: CBManagerState) -> String {
        switch state {
        case .unknown:      return "unknown"
        case .resetting:    return "resetting"
        case .unsupported:  return "unsupported"
        case .unauthorized: return "unauthorized (Bluetooth permission not granted)"
        case .poweredOff:   return "poweredOff"
        case .poweredOn:    return "poweredOn"
        @unknown default:   return "unhandled (\(state.rawValue))"
        }
    }
}


/// Observes the notifications `BluetoothManager` posts when a device
/// is discovered or pairing completes. Historical notification names:
/// - `BluetoothDeviceDiscoveredNotification`
/// - `BluetoothDeviceFoundNotification`
/// - `BluetoothPairingSuccessNotification`
/// - `BluetoothPairingFailureNotification`
///
/// Several names are probed; whichever resolves wins. The `userInfo`
/// dict typically carries the `BluetoothDevice` under keys named
/// `"device"` or `"BluetoothDevice"`.
private final class InquiryNotificationObserver: NSObject, @unchecked Sendable {
    private var attached = false
    private var deviceCallback: (@Sendable (DiscoveredBluetoothDevice) -> Void)?
    private var pairingCallback: (@Sendable (BluetoothPairingResult) -> Void)?

    private let discoveredNames = [
        "BluetoothDeviceDiscoveredNotification",
        "BluetoothDeviceFoundNotification",
        "BluetoothDeviceAddedNotification",
    ]
    private let pairSuccessNames = [
        "BluetoothPairingSuccessNotification",
        "BluetoothDevicePairedNotification",
        "BluetoothConnectSuccessNotification",
    ]
    private let pairFailureNames = [
        "BluetoothPairingFailureNotification",
        "BluetoothDevicePairingFailedNotification",
        "BluetoothConnectFailedNotification",
    ]

    func setDeviceCallback(_ cb: @escaping @Sendable (DiscoveredBluetoothDevice) -> Void) {
        deviceCallback = cb
    }

    func setPairingCallback(_ cb: @escaping @Sendable (BluetoothPairingResult) -> Void) {
        pairingCallback = cb
    }

    func attachIfNeeded() {
        guard !attached else { return }
        attached = true
        let center = NotificationCenter.default
        for name in discoveredNames {
            center.addObserver(
                self,
                selector: #selector(handleDiscovered(_:)),
                name: Notification.Name(name),
                object: nil
            )
        }
        for name in pairSuccessNames {
            center.addObserver(
                self,
                selector: #selector(handlePairSuccess(_:)),
                name: Notification.Name(name),
                object: nil
            )
        }
        for name in pairFailureNames {
            center.addObserver(
                self,
                selector: #selector(handlePairFailure(_:)),
                name: Notification.Name(name),
                object: nil
            )
        }
    }

    @objc private func handleDiscovered(_ note: Notification) {
        guard let raw = extractDevice(from: note) else { return }
        let address = stringProp(raw, selector: "address") ?? "unknown"
        let name = stringProp(raw, selector: "name") ?? address
        let rssi: Int? = {
            guard let n = note.userInfo?["RSSI"] as? NSNumber else { return nil }
            return n.intValue
        }()
        let dev = DiscoveredBluetoothDevice(
            id: address,
            name: name,
            address: address,
            rssi: rssi
        )
        deviceCallback?(dev)
    }

    @objc private func handlePairSuccess(_ note: Notification) {
        _ = note
        pairingCallback?(.success)
    }

    @objc private func handlePairFailure(_ note: Notification) {
        let reason = (note.userInfo?["reason"] as? String) ?? "pairing failed"
        pairingCallback?(.denied(reason: reason))
    }

    private func extractDevice(from note: Notification) -> NSObject? {
        for key in ["device", "BluetoothDevice", "BTDevice"] {
            if let dev = note.userInfo?[key] as? NSObject { return dev }
        }
        return note.object as? NSObject
    }

    private func stringProp(_ obj: NSObject, selector: String) -> String? {
        let sel = NSSelectorFromString(selector)
        guard obj.responds(to: sel) else { return nil }
        return obj.perform(sel)?.takeUnretainedValue() as? String
    }
}

/// One-shot delegate that signals its semaphore once CoreBluetooth
/// publishes its initial state. Used only by the diagnostic probe.
private final class CBProbeDelegate: NSObject, CBCentralManagerDelegate, @unchecked Sendable {
    let semaphore = DispatchSemaphore(value: 0)
    weak var manager: CBCentralManager?
    private var signaled = false

    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        guard !signaled else { return }
        signaled = true
        semaphore.signal()
    }
}

private extension RadioTransportError {
    var displayMessage: String {
        switch self {
        case .notAvailableOnPlatform(let r): return r
        case .notConnected: return "Not connected"
        case .openFailed(let r): return r
        case .writeFailed(let r): return r
        case .readFailed(let r): return r
        case .deviceNotFound(let a): return "Device not found: \(a)"
        }
    }
}

#endif
