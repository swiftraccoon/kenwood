// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
#if os(macOS)
import IOBluetooth
#endif

/// macOS transport using `IOBluetooth` RFCOMM (Serial Port Profile).
///
/// On iOS / iPadOS this type still compiles but every operation throws
/// `RadioTransportError.notAvailableOnPlatform`.
///
/// `IOBluetoothDevice` is a macOS-only framework — Mac Catalyst cannot
/// reach it (classes are marked unavailable on Catalyst). Lodestar ships
/// a separate native macOS target rather than a Catalyst build for
/// exactly this reason.
public actor IOBluetoothTransport: RadioTransport {
    public let device: BluetoothDevice
    private var _state: RadioTransportState = .disconnected
    private let stateContinuation: AsyncStream<RadioTransportState>.Continuation
    public nonisolated let stateStream: AsyncStream<RadioTransportState>

    #if os(macOS)
    private var rfcomm: RFCOMMBridge?
    private var pendingReads: [[UInt8]] = []
    private var readContinuations: [CheckedContinuation<[UInt8], Error>] = []
    #endif

    public init(device: BluetoothDevice) {
        self.device = device
        var cont: AsyncStream<RadioTransportState>.Continuation!
        self.stateStream = AsyncStream { c in cont = c }
        self.stateContinuation = cont
    }

    public var state: RadioTransportState { _state }

    /// Enumerate paired Bluetooth devices. On iOS returns an empty list.
    public nonisolated static func pairedDevices() -> [BluetoothDevice] {
        #if os(macOS)
        guard let devices = IOBluetoothDevice.pairedDevices() as? [IOBluetoothDevice] else {
            return []
        }
        return devices.compactMap { dev -> BluetoothDevice? in
            guard let address = dev.addressString else { return nil }
            let name = dev.name ?? address
            return BluetoothDevice(id: address, name: name, address: address)
        }
        #else
        return []
        #endif
    }

    public func open() async throws {
        #if os(macOS)
        updateState(.connecting)
        let bridge = await RFCOMMBridge(address: device.address)
        rfcomm = bridge
        await bridge.setHandlers(
            onData: { [weak self] bytes in
                Task { await self?.deliverRead(bytes) }
            },
            onClose: { [weak self] in
                Task { await self?.handleUnexpectedClose() }
            }
        )
        do {
            try await bridge.open()
            updateState(.connected)
        } catch {
            rfcomm = nil
            let reason = (error as? RadioTransportError)?.displayMessage ?? "\(error)"
            updateState(.failed(message: reason))
            throw error
        }
        #else
        throw RadioTransportError.notAvailableOnPlatform(
            reason: "Bluetooth Classic SPP is not available on iOS/iPadOS. Use the macOS build."
        )
        #endif
    }

    public func close() async {
        #if os(macOS)
        if let bridge = rfcomm {
            await bridge.close()
        }
        rfcomm = nil
        for c in readContinuations {
            c.resume(returning: [])
        }
        readContinuations.removeAll()
        pendingReads.removeAll()
        #endif
        updateState(.disconnected)
        stateContinuation.finish()
    }

    public func write(_ bytes: [UInt8]) async throws {
        #if os(macOS)
        guard case .connected = _state, let bridge = rfcomm else {
            throw RadioTransportError.notConnected
        }
        try await bridge.write(bytes)
        #else
        throw RadioTransportError.notAvailableOnPlatform(reason: "No IOBluetooth on iOS.")
        #endif
    }

    public func read(maxBytes: Int) async throws -> [UInt8] {
        #if os(macOS)
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
        #else
        throw RadioTransportError.notAvailableOnPlatform(reason: "No IOBluetooth on iOS.")
        #endif
    }

    #if os(macOS)
    private func deliverRead(_ bytes: [UInt8]) {
        if let c = readContinuations.first {
            readContinuations.removeFirst()
            c.resume(returning: bytes)
            return
        }
        pendingReads.append(bytes)
    }

    private func handleUnexpectedClose() {
        rfcomm = nil
        updateState(.disconnected)
        for c in readContinuations {
            c.resume(returning: [])
        }
        readContinuations.removeAll()
    }
    #endif

    private func updateState(_ new: RadioTransportState) {
        _state = new
        stateContinuation.yield(new)
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

#if os(macOS)

/// Main-actor-isolated wrapper around an `IOBluetoothRFCOMMChannel` so the
/// `IOBluetoothTransport` actor can hand off I/O without crossing thread
/// isolation boundaries. `IOBluetooth` runs its delegate callbacks on the
/// main thread, so the bridge lives on `@MainActor`.
@MainActor
final class RFCOMMBridge: NSObject, IOBluetoothRFCOMMChannelDelegate {
    private let address: String
    private var channel: IOBluetoothRFCOMMChannel?
    private var openContinuation: CheckedContinuation<Void, Error>?
    private var onData: (@Sendable ([UInt8]) -> Void)?
    private var onClose: (@Sendable () -> Void)?

    init(address: String) {
        self.address = address
    }

    func setHandlers(
        onData: @escaping @Sendable ([UInt8]) -> Void,
        onClose: @escaping @Sendable () -> Void
    ) {
        self.onData = onData
        self.onClose = onClose
    }

    func open() async throws {
        guard let dev = IOBluetoothDevice(addressString: address) else {
            throw RadioTransportError.deviceNotFound(address: address)
        }

        // Mirror thd75's bluetooth_mac.m: if the device already holds an
        // active baseband connection (e.g. a stale one from the broken
        // serial-port driver or a prior Lodestar session that didn't tear
        // down cleanly), close it and wait for it to actually drop before
        // we try to open anything new. Up to 3 seconds in 50ms slices.
        if dev.isConnected() {
            _ = dev.closeConnection()
            for _ in 0..<60 {
                if !dev.isConnected() { break }
                try? await Task.sleep(nanoseconds: 50_000_000)
            }
        }

        // SDP query triggers a fresh baseband connection.
        let queryResult = dev.performSDPQuery(nil)
        if queryResult != kIOReturnSuccess {
            throw RadioTransportError.openFailed(
                reason: "SDP query failed: \(String(format: "0x%08x", queryResult))"
            )
        }

        // Wait up to 5 seconds for the baseband to actually come up.
        for _ in 0..<100 {
            if dev.isConnected() { break }
            try? await Task.sleep(nanoseconds: 50_000_000)
        }
        if !dev.isConnected() {
            throw RadioTransportError.openFailed(
                reason: "Baseband did not connect within 5s"
            )
        }

        // Now look up the SPP service record.
        let sppUUID = IOBluetoothSDPUUID(
            uuid16: BluetoothSDPUUID16(kBluetoothSDPUUID16ServiceClassSerialPort.rawValue)
        )
        guard let serviceRecord = dev.getServiceRecord(for: sppUUID) else {
            throw RadioTransportError.openFailed(
                reason: "No SPP service record on \(address)"
            )
        }

        var rfcommID: BluetoothRFCOMMChannelID = 0
        let getResult = serviceRecord.getRFCOMMChannelID(&rfcommID)
        if getResult != kIOReturnSuccess || rfcommID == 0 {
            throw RadioTransportError.openFailed(
                reason: "No RFCOMM channel ID in SDP record"
            )
        }

        var ch: IOBluetoothRFCOMMChannel?
        let openResult = dev.openRFCOMMChannelAsync(
            &ch, withChannelID: rfcommID, delegate: self
        )
        if openResult != kIOReturnSuccess {
            throw RadioTransportError.openFailed(
                reason: "openRFCOMMChannelAsync failed: \(String(format: "0x%08x", openResult))"
            )
        }
        self.channel = ch

        // Wait for the delegate to confirm open.
        try await withCheckedThrowingContinuation { (c: CheckedContinuation<Void, Error>) in
            openContinuation = c
        }
    }

    func close() {
        // Mirror thd75's bluetooth_mac.m: nil the delegate FIRST so late
        // IOBluetooth callbacks on the main run loop can't call into this
        // bridge after we've torn it down. Then close the channel, then
        // close the baseband too so the next open() starts from a clean
        // state (the TH-D75 is sensitive to half-released channels).
        onData = nil
        onClose = nil
        if let ch = channel {
            ch.setDelegate(nil)
            _ = ch.close()
        }
        channel = nil
    }

    func write(_ bytes: [UInt8]) throws {
        guard let ch = channel else {
            throw RadioTransportError.notConnected
        }
        try bytes.withUnsafeBufferPointer { ptr -> Void in
            guard let base = ptr.baseAddress else { return }
            // IOBluetooth needs a mutable pointer; the call copies so the cast is safe.
            let mutablePtr = UnsafeMutableRawPointer(mutating: UnsafeRawPointer(base))
            let result = ch.writeSync(mutablePtr, length: UInt16(bytes.count))
            if result != kIOReturnSuccess {
                throw RadioTransportError.writeFailed(
                    reason: "writeSync failed: \(String(format: "0x%08x", result))"
                )
            }
        }
    }

    // MARK: - IOBluetoothRFCOMMChannelDelegate

    nonisolated func rfcommChannelOpenComplete(
        _ rfcommChannel: IOBluetoothRFCOMMChannel!,
        status error: IOReturn
    ) {
        Task { @MainActor [weak self] in
            guard let self else { return }
            if error == kIOReturnSuccess {
                self.openContinuation?.resume(returning: ())
            } else {
                self.openContinuation?.resume(throwing: RadioTransportError.openFailed(
                    reason: "delegate open failed: \(String(format: "0x%08x", error))"
                ))
            }
            self.openContinuation = nil
        }
    }

    nonisolated func rfcommChannelData(
        _ rfcommChannel: IOBluetoothRFCOMMChannel!,
        data dataPointer: UnsafeMutableRawPointer!,
        length dataLength: Int
    ) {
        let buf = UnsafeBufferPointer<UInt8>(
            start: dataPointer.assumingMemoryBound(to: UInt8.self),
            count: dataLength
        )
        let bytes = Array(buf)
        Task { @MainActor [weak self] in
            self?.onData?(bytes)
        }
    }

    nonisolated func rfcommChannelClosed(
        _ rfcommChannel: IOBluetoothRFCOMMChannel!
    ) {
        Task { @MainActor [weak self] in
            guard let self else { return }
            self.channel = nil
            self.onClose?()
        }
    }
}

#endif
