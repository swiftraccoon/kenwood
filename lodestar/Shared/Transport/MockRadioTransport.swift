// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// In-memory `RadioTransport` for tests and previews.
///
/// Scriptable: register exact-match request→response pairs with
/// `script(response:for:)`, inject unsolicited bytes with `push(_:)`,
/// inspect captured writes with `writtenBytes()`, and simulate a
/// Bluetooth drop with `simulateUnexpectedClose()` or a terminal helper
/// failure with `simulateUnexpectedFailure(message:)`.
///
/// Unscripted writes are captured but produce no reply (no echo). The
/// built-in conveniences are that `ID\r` answers `ID TH-D75\r` and
/// `FV\r` answers `FV 1.03\r`, so exact MCP target qualification works
/// without scripting.
public actor MockRadioTransport: RadioTransport {
    public let device: BluetoothDevice
    public nonisolated let pcOutputInterface: PcOutputInterface
    private var _state: RadioTransportState = .disconnected
    private let stateContinuation: AsyncStream<RadioTransportState>.Continuation
    public nonisolated let stateStream: AsyncStream<RadioTransportState>

    private var pendingReads: [[UInt8]] = []
    private var readContinuations:
        [(id: UInt64, maxBytes: Int, continuation: CheckedContinuation<[UInt8], Error>)] = []
    private var nextReadID: UInt64 = 0
    private var scripted: [[UInt8]: [[UInt8]]] = [:]
    private var writes: [[UInt8]] = []
    private let openDelayNanoseconds: UInt64
    private let closeDelayNanoseconds: UInt64

    public init(
        device: BluetoothDevice = .mockTHD75,
        pcOutputInterface: PcOutputInterface = .bluetooth,
        openDelayNanoseconds: UInt64 = 10_000_000,
        closeDelayNanoseconds: UInt64 = 0
    ) {
        self.device = device
        self.pcOutputInterface = pcOutputInterface
        self.openDelayNanoseconds = openDelayNanoseconds
        self.closeDelayNanoseconds = closeDelayNanoseconds
        var cont: AsyncStream<RadioTransportState>.Continuation!
        self.stateStream = AsyncStream { c in cont = c }
        self.stateContinuation = cont
    }

    public var state: RadioTransportState { _state }

    /// Register an exact-match canned response. Script `[]` to mean
    /// "radio stays silent for this request".
    public func script(response: [UInt8], for request: [UInt8]) {
        scripted[request] = [response]
    }

    /// Register a response sequence for repeated identical writes.
    public func scriptSequence(responses: [[UInt8]], for request: [UInt8]) {
        scripted[request] = responses
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
        for reader in readContinuations {
            reader.continuation.resume(returning: [])
        }
        readContinuations.removeAll()
    }

    /// Fail the link the way the disposable Bluetooth helper does when it
    /// exits or a write outcome becomes ambiguous. Pending reads resume empty,
    /// but the state stream remains alive so coordinator recovery is exercised.
    public func simulateUnexpectedFailure(message: String = "Bluetooth helper exited") {
        updateState(.failed(message: message))
        for reader in readContinuations {
            reader.continuation.resume(returning: [])
        }
        readContinuations.removeAll()
    }

    public func open() async throws {
        updateState(.connecting)
        try await Task.sleep(nanoseconds: openDelayNanoseconds)
        updateState(.connected)
    }

    public func close() async {
        if closeDelayNanoseconds > 0 {
            try? await Task.sleep(nanoseconds: closeDelayNanoseconds)
        }
        updateState(.disconnected)
        for reader in readContinuations {
            reader.continuation.resume(returning: [])
        }
        readContinuations.removeAll()
        stateContinuation.finish()
    }

    public func write(_ bytes: [UInt8]) async throws {
        guard case .connected = _state else {
            throw RadioTransportError.notConnected
        }
        writes.append(bytes)
        if var responses = scripted[bytes], !responses.isEmpty {
            let response = responses.removeFirst()
            if responses.isEmpty {
                scripted.removeValue(forKey: bytes)
            } else {
                scripted[bytes] = responses
            }
            if !response.isEmpty { enqueueRead(response) }
            return
        }
        // Built-in conveniences: exact target qualification answers.
        if bytes == Array("ID\r".utf8) {
            enqueueRead(Array("ID TH-D75\r".utf8))
        } else if bytes == Array("FV\r".utf8) {
            enqueueRead(Array("FV 1.03\r".utf8))
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
        let id = nextReadID
        nextReadID += 1
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { c in
                readContinuations.append(
                    (id: id, maxBytes: maxBytes, continuation: c)
                )
            }
        } onCancel: {
            Task { await self.cancelParkedRead(id: id) }
        }
    }

    private func cancelParkedRead(id: UInt64) {
        guard let index = readContinuations.firstIndex(
            where: { $0.id == id }
        ) else {
            return
        }
        let reader = readContinuations.remove(at: index)
        reader.continuation.resume(returning: [])
    }

    private func enqueueRead(_ bytes: [UInt8]) {
        if !readContinuations.isEmpty {
            let reader = readContinuations.removeFirst()
            let slice = Array(bytes.prefix(reader.maxBytes))
            if slice.count < bytes.count {
                pendingReads.insert(Array(bytes.dropFirst(slice.count)), at: 0)
            }
            reader.continuation.resume(returning: slice)
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
