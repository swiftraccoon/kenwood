// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// In-memory `RadioTransport` for tests and previews.
///
/// Responds to `ID\r` with `"ID TH-D75A\r"`, otherwise echoes the write
/// back verbatim. Replace with a fixture-driven version if more scenarios
/// become useful.
public actor MockRadioTransport: RadioTransport {
    public let device: BluetoothDevice
    private var _state: RadioTransportState = .disconnected
    private let stateContinuation: AsyncStream<RadioTransportState>.Continuation
    public nonisolated let stateStream: AsyncStream<RadioTransportState>

    private var pendingReads: [[UInt8]] = []
    private var readContinuations: [CheckedContinuation<[UInt8], Error>] = []

    public init(device: BluetoothDevice = .mockTHD75) {
        self.device = device
        var cont: AsyncStream<RadioTransportState>.Continuation!
        self.stateStream = AsyncStream { c in cont = c }
        self.stateContinuation = cont
    }

    public var state: RadioTransportState { _state }

    public func open() async throws {
        updateState(.connecting)
        try await Task.sleep(nanoseconds: 50_000_000) // 50ms
        updateState(.connected)
    }

    public func close() async {
        updateState(.disconnected)
        for c in readContinuations {
            c.resume(returning: [])
        }
        readContinuations.removeAll()
        stateContinuation.finish()
    }

    public func write(_ bytes: [UInt8]) async throws {
        guard case .connected = _state else {
            throw RadioTransportError.notConnected
        }
        let response = Self.mockResponse(for: bytes)
        enqueueRead(response)
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
        return try await withCheckedThrowingContinuation { c in
            readContinuations.append(c)
        }
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

    private static func mockResponse(for bytes: [UInt8]) -> [UInt8] {
        // `ID\r` → `ID TH-D75A\r`. Otherwise echo.
        if bytes == Array("ID\r".utf8) {
            return Array("ID TH-D75A\r".utf8)
        }
        return bytes
    }
}

public extension BluetoothDevice {
    static let mockTHD75 = BluetoothDevice(
        id: "mock-th-d75",
        name: "TH-D75 (Mock)",
        address: "00-00-00-00-00-01"
    )
}
