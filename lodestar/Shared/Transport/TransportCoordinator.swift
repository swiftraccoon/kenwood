// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import Observation
import OSLog

private let log = Logger(subsystem: "me.dhawkins.lodestar", category: "transport")

/// UI-facing state store for the active `RadioTransport`.
///
/// Owns the currently selected device, the transport instance, and the
/// last received CAT response text. SwiftUI views observe this via
/// `@Observable`.
@Observable
@MainActor
public final class TransportCoordinator {
    public private(set) var availableDevices: [BluetoothDevice] = []
    public private(set) var selectedDevice: BluetoothDevice?
    public private(set) var state: RadioTransportState = .disconnected
    public private(set) var lastResponseText: String = ""
    public private(set) var isBusy: Bool = false

    private var transport: RadioTransport?
    private var stateObserver: Task<Void, Never>?

    public init() {}

    public func refreshPairedDevices() {
        availableDevices = IOBluetoothTransport.pairedDevices()
    }

    public func select(_ device: BluetoothDevice) {
        selectedDevice = device
    }

    public func connect() async {
        guard let device = selectedDevice else { return }
        isBusy = true
        defer { isBusy = false }

        let t = IOBluetoothTransport(device: device)
        transport = t
        observeState(of: t)
        do {
            try await t.open()
        } catch {
            state = .failed(message: String(describing: error))
        }
    }

    public func disconnect() async {
        await transport?.close()
        transport = nil
        stateObserver?.cancel()
        stateObserver = nil
        state = .disconnected
    }

    public func sendIdentify() async {
        guard let t = transport else { return }
        isBusy = true
        defer { isBusy = false }
        do {
            let cmd = encodeCat(command: .identify)
            log.info("Send ID: writing \(cmd.count) bytes: \(Self.hexDump(cmd))")
            try await t.write(cmd)
            log.info("Send ID: write complete, waiting for response")

            // Race the reads against a 2s deadline. `readChunk` returns
            // nil when the timeout fires without the radio sending data,
            // which lets us exit the loop deterministically.
            var buffer: [UInt8] = []
            let totalDeadline = ContinuousClock.now.advanced(by: .seconds(2))
            while !buffer.contains(0x0D), ContinuousClock.now < totalDeadline {
                let chunk = try await readChunkWithTimeout(
                    transport: t, maxBytes: 256, deadline: totalDeadline
                )
                guard let chunk else {
                    log.warning("Send ID: read timed out after 2s; buffer=\(Self.hexDump(buffer))")
                    break
                }
                if chunk.isEmpty {
                    log.warning("Send ID: transport returned empty chunk (closed?)")
                    break
                }
                log.info("Send ID: got \(chunk.count) bytes: \(Self.hexDump(chunk))")
                buffer.append(contentsOf: chunk)
            }
            let crIndex = buffer.firstIndex(of: 0x0D) ?? buffer.endIndex
            let line = Array(buffer[..<crIndex])
            let response = parseCatLine(line: line)
            log.info("Send ID: parsed response=\(String(describing: response))")
            if buffer.isEmpty {
                lastResponseText = "No response in 2s. Check Menu 983 (must be USB)."
            } else {
                lastResponseText = Self.displayText(for: response)
            }
        } catch {
            log.error("Send ID failed: \(error)")
            lastResponseText = "Error: \(error)"
        }
    }

    /// Race `transport.read` against an absolute deadline. Returns `nil` if
    /// the deadline fires first.
    private func readChunkWithTimeout(
        transport: RadioTransport,
        maxBytes: Int,
        deadline: ContinuousClock.Instant
    ) async throws -> [UInt8]? {
        try await withThrowingTaskGroup(of: [UInt8]?.self) { group in
            group.addTask {
                try await transport.read(maxBytes: maxBytes)
            }
            group.addTask {
                try? await Task.sleep(until: deadline, clock: .continuous)
                return nil
            }
            defer { group.cancelAll() }
            return try await group.next() ?? nil
        }
    }

    private static func hexDump(_ bytes: [UInt8]) -> String {
        bytes.map { String(format: "%02x", $0) }.joined(separator: " ")
    }

    private func observeState(of transport: RadioTransport) {
        stateObserver?.cancel()
        let stream = transport.stateStream
        stateObserver = Task { [weak self] in
            for await s in stream {
                await MainActor.run {
                    self?.state = s
                }
            }
        }
    }

    private static func displayText(for resp: CatResponse) -> String {
        switch resp {
        case .identify(let model):
            return "Identify: \(model)"
        case .unknown:
            return "? (unknown command)"
        case .notAvailableInMode:
            return "N (not available in current mode)"
        case .raw(let line):
            return "Raw: \(line)"
        }
    }
}
