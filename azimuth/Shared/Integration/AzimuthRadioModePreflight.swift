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
    case bluetoothMmdvmMode
    case cdcUnresponsive

    var errorDescription: String? {
        switch self {
        case .transportClosed(let stage):
            return "The TH-D75 connection closed during \(stage)."
        case .usbMmdvmMode:
            return "After Azimuth sent the TH-D75 packet-mode exit sequence, the USB-C interface returned a valid MMDVM response, so CAT control is unavailable on that interface after recovery."
        case .bluetoothMmdvmMode:
            return "After Azimuth sent the TH-D75 packet-mode exit sequence, the Bluetooth interface returned a validated TH-D75 MMDVM response instead of CAT. That proves CAT was unavailable during this probe, but does not by itself identify the persistent radio setting which selected MMDVM."
        case .cdcUnresponsive:
            return "Azimuth sent the TH-D75 packet-mode exit sequence, then retried after a CDC control-line reset and one USB-C control-session reopen, but the radio did not answer. Power-cycle the radio, confirm Menu 980 is COM + AF/IF Output, then reconnect USB-C."
        }
    }
}

struct AzimuthPacketModeRecoveryTiming: Sendable {
    let initialFlushDelay: Duration
    let kissReturnDelay: Duration
    let tncExitDelay: Duration
    let finalSettleDelay: Duration
    let quietWindow: Duration
    let residueDrainLimit: Duration

    static let radio = Self(
        initialFlushDelay: .milliseconds(300),
        kissReturnDelay: .milliseconds(100),
        tncExitDelay: .milliseconds(100),
        finalSettleDelay: .milliseconds(300),
        quietWindow: .milliseconds(500),
        residueDrainLimit: .seconds(5)
    )
}

struct AzimuthCATPreservationTiming: Sendable {
    let quietWindow: Duration
    let responseTimeout: Duration

    static let radio = Self(
        quietWindow: .milliseconds(100),
        responseTimeout: .milliseconds(500)
    )
}

/// Establishes a known wire mode and a clean CAT command boundary before the
/// strict automation core is allowed to read from the transport.
struct AzimuthRadioModePreflight: Sendable {
    private static let getVersionProbe: [UInt8] = [0xE0, 0x03, 0x00]
    private static let carriageReturn: [UInt8] = [0x0D]
    private static let endTransmission: [UInt8] = [0x03]
    private static let kissReturn: [UInt8] = [0xC0, 0xFF, 0xC0]
    private static let tncExit: [UInt8] = Array("\rTC 1\r".utf8)
    private static let packetModeExit: [UInt8] = Array("TN 0,0\r".utf8)
    private static let identifyProbe: [UInt8] = Array("ID\r".utf8)

    let transport: any AzimuthRadioTransport
    let probeTimeout: Duration
    let catIdentityTimeout: Duration
    let catPreservationTiming: AzimuthCATPreservationTiming
    let packetModeRecoveryTiming: AzimuthPacketModeRecoveryTiming

    init(
        transport: any AzimuthRadioTransport,
        probeTimeout: Duration = .seconds(2),
        catIdentityTimeout: Duration = .seconds(2),
        catPreservationTiming: AzimuthCATPreservationTiming = .radio,
        packetModeRecoveryTiming: AzimuthPacketModeRecoveryTiming = .radio
    ) {
        self.transport = transport
        self.probeTimeout = probeTimeout
        self.catIdentityTimeout = catIdentityTimeout
        self.catPreservationTiming = catPreservationTiming
        self.packetModeRecoveryTiming = packetModeRecoveryTiming
    }

    func prepareForAutomation() async throws -> AzimuthRadioWireMode {
        try Task.checkCancellation()
        azimuthRadioModeLog.notice(
            "[Azimuth Radio] CAT-preserving wire-mode preflight started"
        )

        if try await proveCATWithoutPacketModeRecovery() == .cat {
            return .cat
        }
        try Task.checkCancellation()

        azimuthRadioModeLog.notice("[Azimuth Radio] Packet-mode recovery started")

        // Recover the USB control channel before classifying it. KISS ignores
        // the ASCII commands, so its protocol-defined Return frame is
        // required. `TN 0,0` also exits a transient MMDVM mode selected by the
        // TNC command. Persistent DV Gateway mode ignores this sequence and is
        // identified by the fresh MMDVM probe after CAT identity fails. This
        // ordering matches the Rust radio connection path and prevents a
        // recoverable transient MMDVM session from being mistaken for Menu 650.
        try await writeRecoveryBytes(Self.carriageReturn)
        try await writeRecoveryBytes(Self.carriageReturn)
        try await sleepForRecovery(packetModeRecoveryTiming.initialFlushDelay)
        try await writeRecoveryBytes(Self.endTransmission)
        try await writeRecoveryBytes(Self.kissReturn)
        try await sleepForRecovery(packetModeRecoveryTiming.kissReturnDelay)
        try await writeRecoveryBytes(Self.tncExit)
        try await sleepForRecovery(packetModeRecoveryTiming.tncExitDelay)
        try await writeRecoveryBytes(Self.packetModeExit)
        try await sleepForRecovery(packetModeRecoveryTiming.finalSettleDelay)

        // Starting modes produce different residue. Drain every queued chunk
        // and require one complete quiet window, subject to a hard wall-clock
        // limit. No bytes from this drain can classify the recovered mode.
        guard try await drainRecoveryResidue() else {
            azimuthRadioModeLog.error(
                "[Azimuth Radio] Packet-mode recovery residue did not become quiet within the bounded drain"
            )
            return .unresponsive
        }

        // The residue drain established the pre-query quiet boundary. Accept
        // CAT only after one new exact identity response, followed by the
        // recovery timing's full quiet window.
        let postRecoveryIdentity = try await proveCATIdentity(
            responseTimeout: catIdentityTimeout,
            postIdentityQuietWindow: packetModeRecoveryTiming.quietWindow,
            stage: "post-recovery CAT identity probe"
        )
        try Task.checkCancellation()

        switch postRecoveryIdentity {
        case .proved(let byteCount):
            azimuthRadioModeLog.info(
                "[Azimuth Radio] Post-recovery CAT identity probe received \(byteCount, privacy: .public) bytes"
            )
            azimuthRadioModeLog.notice(
                "[Azimuth Radio] Mode probe classified CAT after packet-mode recovery; TH-D75 identity confirmed"
            )
            return .cat
        case .unavailable(let byteCount):
            azimuthRadioModeLog.info(
                "[Azimuth Radio] Post-recovery CAT identity proof unavailable after \(byteCount, privacy: .public) response bytes"
            )
        }

        // CAT did not answer after transient packet-mode recovery. Start a
        // new binary accumulator and require a response to a fresh probe, so
        // pre-recovery MMDVM residue cannot authorize the disruptive Menu 650
        // recovery prompt.
        var observation = WireObservationAccumulator()
        azimuthRadioModeLog.notice("[Azimuth Radio] MMDVM mode probe started")
        try await transport.write(Self.getVersionProbe)

        let probeDeadline = ContinuousClock.now.advanced(by: probeTimeout)
        var probeByteCount = 0
        while ContinuousClock.now < probeDeadline {
            guard let chunk = try await readChunk(maxBytes: 64, deadline: probeDeadline) else {
                break
            }
            guard !chunk.isEmpty else {
                throw AzimuthRadioModePreflightError.transportClosed(
                    stage: "MMDVM mode detection"
                )
            }
            probeByteCount += chunk.count

            if case .mmdvm(let frameByteCount) = observation.ingest(chunk) {
                logMMDVM(frameByteCount: frameByteCount)
                return .mmdvm
            }
        }
        try Task.checkCancellation()

        azimuthRadioModeLog.info(
            "[Azimuth Radio] MMDVM mode probe received \(probeByteCount, privacy: .public) bytes"
        )

        azimuthRadioModeLog.error(
            "[Azimuth Radio] Mode probe did not produce a complete, unambiguous CAT or MMDVM response"
        )
        return .unresponsive
    }

    /// Proves an already-reopened endpoint is ordinary TH-D75 CAT without
    /// sending any packet-mode recovery or MMDVM bytes. This is the only safe
    /// preflight while a rebooting recovery is expected to preserve the TNC
    /// data band established by its authenticated settings transaction.
    func proveCATWithoutPacketModeRecovery() async throws -> AzimuthRadioWireMode {
        try Task.checkCancellation()

        // A normal CAT connection must not have its saved TNC data band
        // overwritten by a blind `TN 0,0`. Require a quiet input boundary and
        // one fresh, isolated TH-D75 identity before sending any packet-mode
        // recovery bytes.
        guard try await requireQuiet(
            for: catPreservationTiming.quietWindow,
            stage: "pre-recovery CAT quiet check"
        ) else {
            azimuthRadioModeLog.info(
                "[Azimuth Radio] Pre-recovery CAT boundary contained residue"
            )
            return .unresponsive
        }
        switch try await proveCATIdentity(
            responseTimeout: catPreservationTiming.responseTimeout,
            postIdentityQuietWindow: catPreservationTiming.quietWindow,
            stage: "pre-recovery CAT identity probe"
        ) {
        case .proved(let byteCount):
            azimuthRadioModeLog.info(
                "[Azimuth Radio] Pre-recovery CAT identity probe received \(byteCount, privacy: .public) bytes"
            )
            azimuthRadioModeLog.notice(
                "[Azimuth Radio] Mode probe classified CAT without packet-mode recovery; TH-D75 identity confirmed"
            )
            return .cat
        case .unavailable(let byteCount):
            azimuthRadioModeLog.info(
                "[Azimuth Radio] Pre-recovery CAT identity proof unavailable after \(byteCount, privacy: .public) response bytes"
            )
            return .unresponsive
        }
    }

    private func logMMDVM(frameByteCount: Int) {
        azimuthRadioModeLog.notice(
            "[Azimuth Radio] Mode probe classified MMDVM (\(frameByteCount, privacy: .public) validated response bytes)"
        )
    }

    private func sleepForRecovery(_ duration: Duration) async throws {
        try Task.checkCancellation()
        guard duration != .zero else { return }
        try await Task.sleep(for: duration)
    }

    private func writeRecoveryBytes(_ bytes: [UInt8]) async throws {
        try Task.checkCancellation()
        try await transport.write(bytes)
    }

    /// Returns true only after one complete quiet window. Each received chunk
    /// restarts that window, while the independent hard limit bounds an active
    /// or hostile stream.
    private func drainRecoveryResidue() async throws -> Bool {
        let quietWindow = packetModeRecoveryTiming.quietWindow
        guard quietWindow != .zero else { return true }

        let drainDeadline = ContinuousClock.now.advanced(
            by: packetModeRecoveryTiming.residueDrainLimit
        )
        while ContinuousClock.now < drainDeadline {
            let quietDeadline = ContinuousClock.now.advanced(by: quietWindow)
            let canWaitForFullQuietWindow = quietDeadline <= drainDeadline
            let readDeadline = canWaitForFullQuietWindow ? quietDeadline : drainDeadline
            guard let chunk = try await readChunk(maxBytes: 4096, deadline: readDeadline) else {
                return canWaitForFullQuietWindow
            }
            guard !chunk.isEmpty else {
                throw AzimuthRadioModePreflightError.transportClosed(
                    stage: "packet-mode recovery"
                )
            }
        }
        return false
    }

    private func requireQuiet(
        for quietWindow: Duration,
        stage: String
    ) async throws -> Bool {
        try Task.checkCancellation()
        guard quietWindow != .zero else { return true }
        let deadline = ContinuousClock.now.advanced(by: quietWindow)
        guard let chunk = try await readChunk(maxBytes: 1, deadline: deadline) else {
            return true
        }
        guard !chunk.isEmpty else {
            throw AzimuthRadioModePreflightError.transportClosed(
                stage: stage
            )
        }
        return false
    }

    /// Proves CAT only when the complete exchange is exactly `ID TH-D75\r` and
    /// no trailing byte arrives during the supplied quiet window. A malformed,
    /// incomplete, silent, or noisy exchange is unavailable proof, not
    /// permission to accept a later identity from the same exchange.
    private func proveCATIdentity(
        responseTimeout: Duration,
        postIdentityQuietWindow: Duration,
        stage: String
    ) async throws -> CATIdentityProof {
        try Task.checkCancellation()
        try await transport.write(Self.identifyProbe)

        var identity = StrictCATIdentityAccumulator()
        let deadline = ContinuousClock.now.advanced(by: responseTimeout)
        var byteCount = 0
        while ContinuousClock.now < deadline {
            guard let chunk = try await readChunk(maxBytes: 64, deadline: deadline) else {
                break
            }
            guard !chunk.isEmpty else {
                throw AzimuthRadioModePreflightError.transportClosed(stage: stage)
            }
            byteCount += chunk.count

            switch identity.ingest(chunk) {
            case .identity:
                guard try await requireQuiet(
                    for: postIdentityQuietWindow,
                    stage: "post-identity quiet check"
                ) else {
                    azimuthRadioModeLog.error(
                        "[Azimuth Radio] CAT identity response was followed by unexpected bytes"
                    )
                    return .unavailable(byteCount: byteCount)
                }
                return .proved(byteCount: byteCount)
            case .invalid:
                return .unavailable(byteCount: byteCount)
            case .incomplete:
                break
            }
        }
        try Task.checkCancellation()
        return .unavailable(byteCount: byteCount)
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

private enum CATIdentityProof {
    case proved(byteCount: Int)
    case unavailable(byteCount: Int)
}

private enum WireObservation {
    case none
    case mmdvm(frameByteCount: Int)
}

/// Carries framing state across reads from one fresh MMDVM probe.
///
/// MMDVM recognition scans every possible sync position, so stale bytes and
/// an invalid earlier candidate cannot hide a later valid GET_VERSION frame.
private struct WireObservationAccumulator {
    private static let mmdvmSync: UInt8 = 0xE0
    private static let getVersionCommand: UInt8 = 0x00
    private static let maximumMMDVMFrameLength = 510
    private static let thD75DescriptionPrefix = Array("TH-D75 ".utf8)

    private var mmdvmBytes: [UInt8] = []

    mutating func ingest(_ bytes: [UInt8]) -> WireObservation {
        mmdvmBytes.append(contentsOf: bytes)
        if let frameByteCount = Self.validMMDVMVersionFrameLength(in: mmdvmBytes) {
            return .mmdvm(frameByteCount: frameByteCount)
        }

        // A complete MMDVM frame is at most 510 bytes. Scan before trimming,
        // then retain exactly enough suffix for a frame split across reads.
        if mmdvmBytes.count > Self.maximumMMDVMFrameLength {
            mmdvmBytes.removeFirst(mmdvmBytes.count - Self.maximumMMDVMFrameLength)
        }
        return .none
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

/// Requires the complete response stream to be exactly `ID TH-D75\r`.
/// A valid identity after any leading or trailing byte is not fresh isolated
/// proof and must not authorize the strict automation core.
private struct StrictCATIdentityAccumulator {
    private static let expected = Array("ID TH-D75\r".utf8)

    private var bytes: [UInt8] = []
    private(set) var isInvalid = false

    mutating func ingest(_ chunk: [UInt8]) -> StrictCATIdentityObservation {
        guard !isInvalid else { return .invalid }
        bytes.append(contentsOf: chunk)
        guard bytes.count <= Self.expected.count,
              Self.expected.starts(with: bytes) else {
            isInvalid = true
            return .invalid
        }
        return bytes == Self.expected ? .identity : .incomplete
    }
}

private enum StrictCATIdentityObservation {
    case incomplete
    case invalid
    case identity
}
