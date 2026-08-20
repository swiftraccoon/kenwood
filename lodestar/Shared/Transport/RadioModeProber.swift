// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import OSLog

private let log = Logger(subsystem: "org.swiftraccoon.lodestar", category: "radio-mode")

/// What framing the radio is currently speaking.
public enum RadioMode: Equatable, Sendable {
    /// Haven't probed yet, or probe is in flight.
    case unknown
    /// The attached interface responds to CAT ASCII. Menu 650 may be `Off`,
    /// or Menu 985 may route an enabled DV Gateway to the other interface.
    case cat
    /// Radio returned a complete MMDVM `GetVersion` frame.
    /// Menu 650 is `Reflector Terminal` (or `Access Point`), and Menu 985
    /// routes DV Gateway to this attached interface instead of the other one.
    case mmdvm
    /// The probe got a response we can't classify.
    case unrecognized(firstByte: UInt8)
}

/// Determines whether the attached radio is currently in MMDVM or CAT
/// mode by sending the MMDVM `GetVersion` probe and inspecting the
/// response framing.
///
/// MMDVM firmware responds to `GetVersion` with a complete binary frame,
/// while CAT mode either ignores the probe or returns `?` / `N`. A prefix,
/// silence, or unrelated valid command is not MMDVM proof.
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

        try Task.checkCancellation()
        try await transport.write(probe)
        try Task.checkCancellation()

        let deadline = ContinuousClock.now.advanced(by: timeout)
        let firstChunk = try await readChunkWithTimeout(
            transport: transport,
            maxBytes: 1,
            deadline: deadline
        )
        try Task.checkCancellation()
        guard let firstChunk, let first = firstChunk.first else {
            // No response at all. Radio either isn't listening, is
            // asleep, or is in CAT mode and simply ignored our probe.
            // We classify this as CAT because MMDVM firmware always
            // responds to GetVersion.
            //
            // Flush the radio's CAT line parser before returning: the
            // probe bytes carry no CR, so they linger in the radio's
            // line buffer and corrupt the NEXT CAT command ("ID\r"
            // right after a probe answers "?\r"; hardware-verified
            // 2026-07-19). A bare CR terminates the junk line; the
            // radio's "?\r" reply to it is drained and discarded here
            // so it can't poison the next reader either.
            log.info("radio-mode probe: no response → classifying as .cat (flushing line buffer)")
            try? await transport.write([0x0D])
            let flushDeadline = ContinuousClock.now.advanced(by: .milliseconds(300))
            while ContinuousClock.now < flushDeadline {
                // `try?` flattens the timeout's nil and an error into
                // one nil; either way there's nothing left to flush.
                guard let chunk = try? await readChunkWithTimeout(
                    transport: transport, maxBytes: 64, deadline: flushDeadline
                ), !chunk.isEmpty else { break }
                log.info("radio-mode probe: flushed \(Self.hex(chunk))")
            }
            return .cat
        }

        if first == UInt8(ascii: "?") || first == UInt8(ascii: "N") {
            await drainCatLine(until: deadline)
            return .cat
        }
        guard first == 0xE0 else {
            return .unrecognized(firstByte: first)
        }

        // Read exactly the advertised MMDVM frame length. This both prevents
        // a prefix or echoed request from proving terminal mode and leaves a
        // coalesced following frame for the relay reader.
        var frameBytes = [first]
        guard let length = try await readOneByte(until: deadline) else {
            return .unrecognized(firstByte: first)
        }
        frameBytes.append(length)
        let frameLength: Int
        if length == 0 {
            guard let extended = try await readOneByte(until: deadline) else {
                return .unrecognized(firstByte: first)
            }
            frameBytes.append(extended)
            frameLength = Int(extended) + 255
        } else {
            frameLength = Int(length)
        }
        guard frameLength >= 3 else {
            return .unrecognized(firstByte: first)
        }
        while frameBytes.count < frameLength {
            let chunk = try await readChunkWithTimeout(
                transport: transport,
                maxBytes: frameLength - frameBytes.count,
                deadline: deadline
            )
            try Task.checkCancellation()
            guard let chunk, !chunk.isEmpty else {
                return .unrecognized(firstByte: first)
            }
            frameBytes.append(contentsOf: chunk)
        }

        do {
            let decoded = try decodeMmdvmBytes(bytes: Data(frameBytes))
            guard decoded.bytesConsumed == frameBytes.count,
                  let frame = decoded.frame,
                  frame.command == 0x00 else {
                return .unrecognized(firstByte: first)
            }
            let version = try parseMmdvmVersionPayload(payload: frame.payload)
            log.info(
                "radio-mode probe: proved MMDVM protocol \(version.protocol), \(version.description, privacy: .public)"
            )
            return .mmdvm
        } catch {
            log.warning("radio-mode probe: invalid GetVersion response: \(error)")
            return .unrecognized(firstByte: first)
        }
    }

    private func readOneByte(
        until deadline: ContinuousClock.Instant
    ) async throws -> UInt8? {
        let chunk = try await readChunkWithTimeout(
            transport: transport,
            maxBytes: 1,
            deadline: deadline
        )
        try Task.checkCancellation()
        guard let chunk else {
            return nil
        }
        return chunk.first
    }

    private func drainCatLine(until deadline: ContinuousClock.Instant) async {
        while ContinuousClock.now < deadline {
            guard let chunk = try? await readChunkWithTimeout(
                transport: transport,
                maxBytes: 64,
                deadline: deadline
            ), !chunk.isEmpty else {
                return
            }
            if chunk.contains(0x0D) { return }
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
