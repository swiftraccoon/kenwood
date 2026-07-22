// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// In-memory `RadioTransport` for tests and previews.
///
/// Scriptable: register exact-match request→response pairs with
/// `script(response:for:)`, inject unsolicited bytes with `push(_:)`,
/// inspect captured writes with `writtenBytes()`, and simulate a
/// Bluetooth drop with `simulateUnexpectedClose()`.
///
/// Unscripted writes are captured but produce no reply (no echo). The
/// one built-in convenience is that `ID\r` still answers `ID TH-D75A\r`
/// so the CAT identify round-trip works without scripting.
public actor MockRadioTransport: RadioTransport {
    public let device: BluetoothDevice
    private var _state: RadioTransportState = .disconnected
    private let stateContinuation: AsyncStream<RadioTransportState>.Continuation
    public nonisolated let stateStream: AsyncStream<RadioTransportState>

    private var pendingReads: [[UInt8]] = []
    private var readContinuations: [CheckedContinuation<[UInt8], Error>] = []
    private var scripted: [[UInt8]: [UInt8]] = [:]
    private var writes: [[UInt8]] = []

    public init(device: BluetoothDevice = .mockTHD75) {
        self.device = device
        var cont: AsyncStream<RadioTransportState>.Continuation!
        self.stateStream = AsyncStream { c in cont = c }
        self.stateContinuation = cont
    }

    public var state: RadioTransportState { _state }

    /// Register an exact-match canned response. Script `[]` to mean
    /// "radio stays silent for this request".
    public func script(response: [UInt8], for request: [UInt8]) {
        scripted[request] = response
    }

    /// Inject unsolicited bytes, as if the radio sent them.
    public func push(_ bytes: [UInt8]) {
        enqueueRead(bytes)
    }

    /// Every payload passed to `write()`, in call order.
    public func writtenBytes() -> [[UInt8]] { writes }

    /// Drop the link the way a Bluetooth blip does: state flips to
    /// `.disconnected` and pending reads resume empty, but the state
    /// stream stays open (the transport object is still alive).
    public func simulateUnexpectedClose() {
        updateState(.disconnected)
        for c in readContinuations { c.resume(returning: []) }
        readContinuations.removeAll()
    }

    public func open() async throws {
        updateState(.connecting)
        try await Task.sleep(nanoseconds: 10_000_000)
        updateState(.connected)
    }

    public func close() async {
        updateState(.disconnected)
        for c in readContinuations { c.resume(returning: []) }
        readContinuations.removeAll()
        stateContinuation.finish()
    }

    public func write(_ bytes: [UInt8]) async throws {
        guard case .connected = _state else {
            throw RadioTransportError.notConnected
        }
        writes.append(bytes)
        if let response = scripted[bytes] {
            if !response.isEmpty { enqueueRead(response) }
            return
        }
        // Built-in convenience: CAT identify still answers.
        if bytes == Array("ID\r".utf8) {
            enqueueRead(Array("ID TH-D75A\r".utf8))
        }
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
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { c in
                readContinuations.append(c)
            }
        } onCancel: {
            Task { await self.resumeParkedReadsEmpty() }
        }
    }

    /// Cancellation support: resume every parked read with an empty
    /// chunk, the same signal a closed transport produces.
    private func resumeParkedReadsEmpty() {
        for c in readContinuations { c.resume(returning: []) }
        readContinuations.removeAll()
    }

    private func enqueueRead(_ bytes: [UInt8]) {
        if let c = readContinuations.first {
            readContinuations.removeFirst()
            c.resume(returning: bytes)
            return
        }
        pendingReads.append(bytes)
    }

    private func updateState(_ new: RadioTransportState) {
        _state = new
        stateContinuation.yield(new)
    }
}

public extension BluetoothDevice {
    static let mockTHD75 = BluetoothDevice(
        id: "mock-th-d75",
        name: "TH-D75 (Mock)",
        address: "00-00-00-00-00-01"
    )
}
