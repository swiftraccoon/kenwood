// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import OSLog

private let azimuthRadioModeLog = Logger(
    subsystem: "org.swiftraccoon.azimuth",
    category: "radio-mode"
)

enum AzimuthRadioWireMode: Sendable, Equatable {
    case cat
    case mmdvm
    case unresponsive
}

enum AzimuthRadioModePreflightError: LocalizedError, Sendable, Equatable {
    case transportClosed(stage: String)
    case dvGatewayMode
    case cdcUnresponsive

    var errorDescription: String? {
        switch self {
        case .transportClosed(let stage):
            return "The TH-D75 USB-C connection closed during \(stage)."
        case .dvGatewayMode:
            return "USB-C is carrying DV Gateway/MMDVM data instead of Kenwood CAT commands. Set TH-D75 Menu 650 (DV Gateway) to Off, then reconnect in Azimuth."
        case .cdcUnresponsive:
            return "Azimuth opened both TH-D75 USB interfaces and retried after a CDC control-line reset, but the radio did not answer. Power-cycle the radio, confirm Menu 980 is COM + AF/IF Output, then reconnect USB-C."
        }
    }
}

/// Establishes a known wire mode and a clean CAT command boundary before the
/// strict automation core is allowed to read from the transport.
struct AzimuthRadioModePreflight: Sendable {
    private static let getVersionProbe: [UInt8] = [0xE0, 0x03, 0x00]
    private static let carriageReturn: [UInt8] = [0x0D]

    let transport: any AzimuthRadioTransport
    let probeTimeout: Duration
    let parserResetTimeout: Duration

    init(
        transport: any AzimuthRadioTransport,
        probeTimeout: Duration = .seconds(2),
        parserResetTimeout: Duration = .milliseconds(300)
    ) {
        self.transport = transport
        self.probeTimeout = probeTimeout
        self.parserResetTimeout = parserResetTimeout
    }

    func prepareForAutomation() async throws -> AzimuthRadioWireMode {
        azimuthRadioModeLog.notice("[Azimuth Radio] Mode probe started")
        try await transport.write(Self.getVersionProbe)

        let deadline = ContinuousClock.now.advanced(by: probeTimeout)
        let response = try await readChunk(maxBytes: 64, deadline: deadline)
        try Task.checkCancellation()

        guard let response else {
            azimuthRadioModeLog.info(
                "[Azimuth Radio] Mode probe was silent; resetting CAT parser"
            )
            let drainedByteCount = try await resetCATParser()
            guard drainedByteCount > 0 else {
                azimuthRadioModeLog.error(
                    "[Azimuth Radio] Mode probe and CAT parser reset both received no response"
                )
                return .unresponsive
            }
            azimuthRadioModeLog.notice(
                "[Azimuth Radio] Mode probe classified CAT; parser reset complete"
            )
            return .cat
        }
        guard !response.isEmpty else {
            throw AzimuthRadioModePreflightError.transportClosed(stage: "radio mode detection")
        }

        if response[0] == 0xE0 {
            let frameBytes = try await drainMMDVMFrame(
                startingWith: response,
                deadline: deadline
            )
            azimuthRadioModeLog.notice(
                "[Azimuth Radio] Mode probe classified MMDVM (\(frameBytes, privacy: .public) response bytes)"
            )
            return .mmdvm
        }

        // Any non-MMDVM response is either CAT's rejection of the binary
        // probe or stale CAT output. Terminate the partial line and drain all
        // replies so connectAutomation receives a pristine byte stream.
        azimuthRadioModeLog.info(
            "[Azimuth Radio] Mode probe received a non-MMDVM response (\(response.count, privacy: .public) bytes); resetting CAT parser"
        )
        _ = try await resetCATParser()
        azimuthRadioModeLog.notice(
            "[Azimuth Radio] Mode probe classified CAT; parser reset complete"
        )
        return .cat
    }

    private func resetCATParser() async throws -> Int {
        try Task.checkCancellation()
        try await transport.write(Self.carriageReturn)

        let deadline = ContinuousClock.now.advanced(by: parserResetTimeout)
        var drainedByteCount = 0
        while ContinuousClock.now < deadline {
            guard let chunk = try await readChunk(maxBytes: 64, deadline: deadline) else {
                break
            }
            guard !chunk.isEmpty else {
                throw AzimuthRadioModePreflightError.transportClosed(stage: "CAT parser reset")
            }
            drainedByteCount += chunk.count
        }
        try Task.checkCancellation()
        azimuthRadioModeLog.info(
            "[Azimuth Radio] CAT parser reset drained \(drainedByteCount, privacy: .public) bytes"
        )
        return drainedByteCount
    }

    private func drainMMDVMFrame(
        startingWith initialBytes: [UInt8],
        deadline: ContinuousClock.Instant
    ) async throws -> Int {
        var bytes = initialBytes
        while bytes.count < 2, ContinuousClock.now < deadline {
            guard let chunk = try await readChunk(maxBytes: 1, deadline: deadline) else {
                return bytes.count
            }
            guard !chunk.isEmpty else {
                throw AzimuthRadioModePreflightError.transportClosed(stage: "MMDVM frame read")
            }
            bytes.append(contentsOf: chunk)
        }
        guard bytes.count >= 2 else { return bytes.count }

        let frameLength = Int(bytes[1])
        while bytes.count < frameLength, ContinuousClock.now < deadline {
            guard let chunk = try await readChunk(
                maxBytes: frameLength - bytes.count,
                deadline: deadline
            ) else {
                return bytes.count
            }
            guard !chunk.isEmpty else {
                throw AzimuthRadioModePreflightError.transportClosed(stage: "MMDVM frame read")
            }
            bytes.append(contentsOf: chunk)
        }
        return bytes.count
    }

    private func readChunk(
        maxBytes: Int,
        deadline: ContinuousClock.Instant
    ) async throws -> [UInt8]? {
        try Task.checkCancellation()
        return try await withThrowingTaskGroup(of: [UInt8]?.self) { group in
            group.addTask {
                try await transport.read(maxBytes: maxBytes)
            }
            group.addTask {
                try? await Task.sleep(until: deadline, clock: .continuous)
                return nil
            }
            defer { group.cancelAll() }
            let result = try await group.next() ?? nil
            try Task.checkCancellation()
            return result
        }
    }
}
