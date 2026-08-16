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
    case usbMmdvmMode
    case cdcUnresponsive

    var errorDescription: String? {
        switch self {
        case .transportClosed(let stage):
            return "The TH-D75 USB-C connection closed during \(stage)."
        case .usbMmdvmMode:
            return "The TH-D75 USB-C interface returned a valid MMDVM response, so CAT control is unavailable on that interface."
        case .cdcUnresponsive:
            return "Azimuth reopened the TH-D75 USB-C control session once and retried after a CDC control-line reset, but the radio did not answer. Power-cycle the radio, confirm Menu 980 is COM + AF/IF Output, then reconnect USB-C."
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

        var observation = WireObservationAccumulator()
        var probeByteCount = 0
        let probeDeadline = ContinuousClock.now.advanced(by: probeTimeout)

        probeLoop: while ContinuousClock.now < probeDeadline {
            guard let chunk = try await readChunk(maxBytes: 64, deadline: probeDeadline) else {
                break
            }
            guard !chunk.isEmpty else {
                throw AzimuthRadioModePreflightError.transportClosed(
                    stage: "radio mode detection"
                )
            }
            probeByteCount += chunk.count

            switch observation.ingest(chunk) {
            case .mmdvm(let frameByteCount):
                logMMDVM(frameByteCount: frameByteCount)
                return .mmdvm
            case .catLine:
                // This line may be the binary probe's rejection or stale CAT
                // output. It is useful only as a reason to reset promptly.
                // CAT is not proved until a complete line arrives after that
                // reset write.
                break probeLoop
            case .none:
                continue
            }
        }
        try Task.checkCancellation()

        if probeByteCount == 0 {
            azimuthRadioModeLog.info(
                "[Azimuth Radio] Mode probe was silent; resetting CAT parser"
            )
        } else {
            azimuthRadioModeLog.info(
                "[Azimuth Radio] Mode probe received \(probeByteCount, privacy: .public) unclassified bytes; resetting CAT parser"
            )
        }

        // The binary probe has no CR. On a CAT link, a bare CR terminates that
        // partial command and the TH-D75 answers with a complete CAT error
        // line. Start CAT framing at this write, but retain the MMDVM scanner:
        // a delayed GET_VERSION frame can arrive during the reset window.
        observation.resetCATLineFraming()
        try await transport.write(Self.carriageReturn)

        let resetDeadline = ContinuousClock.now.advanced(by: parserResetTimeout)
        var resetByteCount = 0
        var sawCompleteCATLine = false
        while ContinuousClock.now < resetDeadline {
            guard let chunk = try await readChunk(maxBytes: 64, deadline: resetDeadline) else {
                break
            }
            guard !chunk.isEmpty else {
                throw AzimuthRadioModePreflightError.transportClosed(stage: "CAT parser reset")
            }
            resetByteCount += chunk.count

            switch observation.ingest(chunk) {
            case .mmdvm(let frameByteCount):
                logMMDVM(frameByteCount: frameByteCount)
                return .mmdvm
            case .catLine:
                sawCompleteCATLine = true
            case .none:
                break
            }
        }
        try Task.checkCancellation()

        azimuthRadioModeLog.info(
            "[Azimuth Radio] CAT parser reset drained \(resetByteCount, privacy: .public) bytes"
        )

        // An E0 sync without a complete, validated GET_VERSION frame is an
        // unresolved binary boundary. Do not reinterpret a later printable
        // suffix as CAT.
        guard sawCompleteCATLine, !observation.sawMMDVMSync else {
            azimuthRadioModeLog.error(
                "[Azimuth Radio] Mode probe did not produce a complete, unambiguous CAT or MMDVM response"
            )
            return .unresponsive
        }

        azimuthRadioModeLog.notice(
            "[Azimuth Radio] Mode probe classified CAT; parser reset complete"
        )
        return .cat
    }

    private func logMMDVM(frameByteCount: Int) {
        azimuthRadioModeLog.notice(
            "[Azimuth Radio] Mode probe classified MMDVM (\(frameByteCount, privacy: .public) validated response bytes)"
        )
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

private enum WireObservation {
    case none
    case catLine
    case mmdvm(frameByteCount: Int)
}

/// Carries framing state across the GET_VERSION probe and CAT reset.
///
/// MMDVM recognition scans every possible sync position, so stale bytes and
/// an invalid earlier candidate cannot hide a later valid GET_VERSION frame.
/// CAT framing can be reset independently, while an unresolved MMDVM sync is
/// retained to keep partial binary input from being reinterpreted as ASCII.
private struct WireObservationAccumulator {
    private static let mmdvmSync: UInt8 = 0xE0
    private static let getVersionCommand: UInt8 = 0x00
    private static let maximumMMDVMFrameLength = 510
    private static let thD75DescriptionPrefix = Array("TH-D75 ".utf8)

    private var mmdvmBytes: [UInt8] = []
    private var catLines = CATLineScanner()
    private(set) var sawMMDVMSync = false

    mutating func resetCATLineFraming() {
        catLines = CATLineScanner()
    }

    mutating func ingest(_ bytes: [UInt8]) -> WireObservation {
        if bytes.contains(Self.mmdvmSync) {
            sawMMDVMSync = true
        }

        mmdvmBytes.append(contentsOf: bytes)
        if let frameByteCount = Self.validMMDVMVersionFrameLength(in: mmdvmBytes) {
            return .mmdvm(frameByteCount: frameByteCount)
        }

        // A complete MMDVM frame is at most 510 bytes. Scan before trimming,
        // then retain exactly enough suffix for a frame split across reads.
        if mmdvmBytes.count > Self.maximumMMDVMFrameLength {
            mmdvmBytes.removeFirst(mmdvmBytes.count - Self.maximumMMDVMFrameLength)
        }

        return catLines.ingest(bytes) ? .catLine : .none
    }

    private static func validMMDVMVersionFrameLength(in bytes: [UInt8]) -> Int? {
        for startIndex in bytes.indices where bytes[startIndex] == mmdvmSync {
            let lengthIndex = startIndex + 1
            guard lengthIndex < bytes.endIndex else { continue }

            let lengthField = bytes[lengthIndex]
            let frameLength: Int
            let commandOffset: Int
            if lengthField == 0 {
                let extendedLengthIndex = startIndex + 2
                guard extendedLengthIndex < bytes.endIndex else { continue }
                frameLength = Int(bytes[extendedLengthIndex]) + 255
                commandOffset = 3
            } else {
                guard lengthField >= 3 else { continue }
                frameLength = Int(lengthField)
                commandOffset = 2
            }

            guard frameLength <= maximumMMDVMFrameLength else { continue }
            let endIndex = startIndex + frameLength
            guard endIndex <= bytes.endIndex else { continue }

            let commandIndex = startIndex + commandOffset
            guard commandIndex < endIndex,
                  bytes[commandIndex] == getVersionCommand else {
                continue
            }

            let payloadStart = commandIndex + 1
            guard payloadStart < endIndex else { continue }
            let protocolVersion = bytes[payloadStart]
            guard protocolVersion == 1 || protocolVersion == 2 else { continue }

            let descriptionOffset = protocolVersion == 2 ? 20 : 1
            let descriptionStart = payloadStart + descriptionOffset
            guard descriptionStart < endIndex else { continue }

            var description = Array(bytes[descriptionStart ..< endIndex])
            while let last = description.last,
                  last == 0 || last == 0x20 || (0x09 ... 0x0D).contains(last) {
                description.removeLast()
            }
            guard !description.isEmpty,
                  description.allSatisfy({ (0x20 ... 0x7E).contains($0) }),
                  description.starts(with: thD75DescriptionPrefix) else {
                continue
            }
            return frameLength
        }
        return nil
    }
}

/// Minimal CAT line framing and lexical validation for mode proof.
///
/// CAT frames are printable ASCII terminated by CR. The two-character
/// mnemonic form and the protocol's one-byte `?` and `N` responses are
/// accepted. NMEA sentences, arbitrary binary bytes, partial lines, and an
/// empty CR are not CAT proof.
private struct CATLineScanner {
    private static let maximumLineLength = 64 * 1024

    private var line: [UInt8] = []
    private var overflowed = false

    mutating func ingest(_ bytes: [UInt8]) -> Bool {
        var foundValidLine = false
        for byte in bytes {
            if byte == 0x0D {
                if !overflowed, Self.isValidCATLine(line) {
                    foundValidLine = true
                }
                line.removeAll(keepingCapacity: true)
                overflowed = false
                continue
            }

            // NMEA uses CRLF. The shared CAT codec skips the LF residue at a
            // frame boundary, so mirror that behavior here.
            if byte == 0x0A, line.isEmpty {
                continue
            }
            guard !overflowed else { continue }
            guard line.count < Self.maximumLineLength else {
                line.removeAll(keepingCapacity: true)
                overflowed = true
                continue
            }
            line.append(byte)
        }
        return foundValidLine
    }

    private static func isValidCATLine(_ bytes: [UInt8]) -> Bool {
        if bytes == [UInt8(ascii: "?")] || bytes == [UInt8(ascii: "N")] {
            return true
        }
        guard bytes.count >= 2,
              bytes.allSatisfy({ (0x20 ... 0x7E).contains($0) }),
              isMnemonicByte(bytes[0]),
              isMnemonicByte(bytes[1]),
              bytes.count == 2 || bytes[2] == UInt8(ascii: " ") else {
            return false
        }
        return true
    }

    private static func isMnemonicByte(_ byte: UInt8) -> Bool {
        (UInt8(ascii: "A") ... UInt8(ascii: "Z")).contains(byte)
            || (UInt8(ascii: "0") ... UInt8(ascii: "9")).contains(byte)
    }
}
