// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import OSLog

private let log = Logger(subsystem: "org.swiftraccoon.lodestar", category: "radio-mode")

/// What framing the radio is currently speaking.
public enum RadioMode: Equatable, Sendable {
    /// Haven't probed yet, or probe is in flight.
    case unknown
    /// Radio responds to CAT ASCII. Menu 650 (DV Gateway) is `Off`.
    case cat
    /// Radio responds with MMDVM binary framing (first byte `0xE0`).
    /// Menu 650 is `Reflector Terminal` (or `Access Point`). The BT
    /// channel is no longer a CAT channel — it carries MMDVM frames.
    case mmdvm
    /// The probe got a response we can't classify.
    case unrecognized(firstByte: UInt8)
}

/// Determines whether the attached radio is currently in MMDVM or CAT
/// mode by sending the MMDVM `GetVersion` probe and inspecting the
/// first response byte.
///
/// Matches `thd75-repl::detect_mmdvm_mode`: MMDVM firmware responds in
/// ~20 ms with a `0xE0`-prefixed frame, while CAT mode either ignores
/// the probe (nothing arrives) or returns `?` / `N` (not 0xE0).
public struct RadioModeProber {
    public let transport: RadioTransport
    public let timeout: Duration

    public init(transport: RadioTransport, timeout: Duration = .seconds(2)) {
        self.transport = transport
        self.timeout = timeout
    }

    /// Send the probe and classify the response.
    public func probe() async throws -> RadioMode {
        let probe = Array(mmdvmGetVersionProbe())
        log.info("radio-mode probe: sending \(Self.hex(probe))")

        try await transport.write(probe)

        // Read up to 64 bytes or time out. MMDVM responds with a full
        // version frame (usually 40-60 bytes). Silence or a short
        // ASCII reply (`?\r`, `N\r`) means CAT.
        let deadline = ContinuousClock.now.advanced(by: timeout)
        var buffer: [UInt8] = []
        while buffer.count < 64, ContinuousClock.now < deadline {
            let remaining = 64 - buffer.count
            let chunk = try await readChunkWithTimeout(
                transport: transport, maxBytes: remaining, deadline: deadline
            )
            guard let chunk else { break } // timed out
            if chunk.isEmpty { break }     // transport closed
            buffer.append(contentsOf: chunk)
            // As soon as we have the first byte, we can classify — but
            // keep draining briefly to absorb the rest of the frame so
            // the next caller's read starts on a clean byte boundary.
            if buffer.count >= 1 {
                break
            }
        }

        log.info("radio-mode probe: received \(buffer.count) bytes: \(Self.hex(buffer))")

        switch buffer.first {
        case nil:
            // No response at all. Radio either isn't listening, is
            // asleep, or is in CAT mode and simply ignored our probe.
            // We classify this as CAT because MMDVM firmware always
            // responds to GetVersion.
            log.info("radio-mode probe: no response → classifying as .cat")
            return .cat
        case 0xE0:
            // Drain whatever's left of the frame (~50 bytes) so it
            // doesn't poison the next read.
            _ = try? await drainFrame(startBuffer: buffer, deadline: deadline)
            return .mmdvm
        case .some(let b):
            return .unrecognized(firstByte: b)
        }
    }

    /// Having seen `0xE0` as the first byte, keep reading until we've
    /// absorbed the whole MMDVM frame. Best-effort — if we time out or
    /// the frame is malformed, we move on; the next read will handle
    /// any leftover garbage.
    private func drainFrame(
        startBuffer: [UInt8], deadline: ContinuousClock.Instant
    ) async throws {
        var buffer = startBuffer
        // Length byte is `buffer[1]`, and the whole frame is that many bytes.
        while buffer.count < 2, ContinuousClock.now < deadline {
            let chunk = try await readChunkWithTimeout(
                transport: transport, maxBytes: 1, deadline: deadline
            )
            guard let chunk, !chunk.isEmpty else { return }
            buffer.append(contentsOf: chunk)
        }
        guard buffer.count >= 2 else { return }
        let want = Int(buffer[1])
        while buffer.count < want, ContinuousClock.now < deadline {
            let remaining = want - buffer.count
            let chunk = try await readChunkWithTimeout(
                transport: transport, maxBytes: remaining, deadline: deadline
            )
            guard let chunk, !chunk.isEmpty else { return }
            buffer.append(contentsOf: chunk)
        }
    }

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

    private static func hex(_ bytes: [UInt8]) -> String {
        bytes.map { String(format: "%02x", $0) }.joined(separator: " ")
    }
}
