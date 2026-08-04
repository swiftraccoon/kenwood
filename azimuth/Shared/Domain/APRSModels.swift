// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

enum APRSPacketDataRate: String, CaseIterable, Identifiable, Equatable, Sendable {
    case bps1200
    case bps9600

    var id: String { rawValue }

    var title: String {
        switch self {
        case .bps1200: return "1200 bps AFSK"
        case .bps9600: return "9600 bps"
        }
    }
}

/// Host-owned KISS configuration. Persistent radio-menu changes remain in the
/// settings editor; these values govern only the current operational session.
struct APRSSessionConfiguration: Equatable, Sendable {
    var stationCallsign: String
    var path: String
    var dataRate: APRSPacketDataRate
    var symbolTable: String
    var symbolCode: String
    var txDelay10ms: UInt8
    var persistence: UInt8
    var slotTime10ms: UInt8
    var txTail10ms: UInt8
    var fullDuplex: Bool

    static let receiveOnly = APRSSessionConfiguration(
        stationCallsign: "",
        path: "WIDE1-1,WIDE2-1",
        dataRate: .bps1200,
        symbolTable: "/",
        symbolCode: ">",
        txDelay10ms: 50,
        persistence: 128,
        slotTime10ms: 10,
        txTail10ms: 3,
        fullDuplex: false
    )

    var isReceiveOnly: Bool {
        stationCallsign.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
}

enum APRSSessionPhase: Equatable, Sendable {
    case unavailable(reason: String)
    case inactive
    case starting
    case active
    case restoring
    case failed

    var isActive: Bool { self == .active }

    /// The TH-D75 exposes automation CAT and host KISS on the same serial session.
    /// These phases therefore block screen, setting, and front-panel work.
    var ownsSerialLink: Bool {
        switch self {
        case .starting, .active, .restoring: return true
        case .unavailable, .inactive, .failed: return false
        }
    }
}

struct APRSSessionStatus: Equatable, Sendable {
    var phase: APRSSessionPhase
    var sessionID: UInt64
    var startedAt: Date?
    var configuration: APRSSessionConfiguration?
    var receivedPackets: UInt64
    var transmittedPackets: UInt64
    var decodeFailures: UInt64
    var droppedActivities: UInt64
    var lastError: String?

    static func unavailable(_ reason: String) -> APRSSessionStatus {
        APRSSessionStatus(
            phase: .unavailable(reason: reason),
            sessionID: 0,
            startedAt: nil,
            configuration: nil,
            receivedPackets: 0,
            transmittedPackets: 0,
            decodeFailures: 0,
            droppedActivities: 0,
            lastError: nil
        )
    }
}

enum APRSActivityDirection: String, CaseIterable, Identifiable, Equatable, Sendable {
    case rx
    case tx
    case system

    var id: String { rawValue }
}

enum APRSActivityKind: String, CaseIterable, Identifiable, Equatable, Sendable {
    case session
    case position
    case message
    case status
    case object
    case item
    case weather
    case telemetry
    case query
    case thirdParty
    case grid
    case rawGPS
    case capabilities
    case directionFinding
    case userDefined
    case test
    case rawWeather
    case ax25
    case kissControl
    case decodeError
    case error

    var id: String { rawValue }
}

struct APRSActivity: Identifiable, Equatable, Sendable {
    let sequence: UInt64
    let sessionID: UInt64
    let timestamp: Date
    let direction: APRSActivityDirection
    let kind: APRSActivityKind
    let source: String?
    let destination: String?
    let path: [String]
    let summary: String
    let rawPacket: String
    let rawAX25: Data
    let latitude: Double?
    let longitude: Double?
    let speedKnots: UInt16?
    let courseDegrees: UInt16?

    var id: UInt64 { sequence }
}

struct APRSStation: Identifiable, Equatable, Sendable {
    let callsign: String
    let lastHeard: Date
    let packetCount: UInt64
    let latitude: Double?
    let longitude: Double?
    let speedKnots: UInt16?
    let courseDegrees: UInt16?
    let path: [String]
    let latestSummary: String

    var id: String { callsign }
}

struct APRSOperationalState: Equatable, Sendable {
    var status: APRSSessionStatus
    var activities: [APRSActivity]
    var stations: [APRSStation]
    var latestSequence: UInt64
    var historyTruncated: Bool

    static func unavailable(_ reason: String) -> APRSOperationalState {
        APRSOperationalState(
            status: .unavailable(reason),
            activities: [],
            stations: [],
            latestSequence: 0,
            historyTruncated: false
        )
    }
}

/// Optional capability boundary for real packet operation. Keeping this
/// separate from `RadioControlling` prevents screen/settings-only adapters and
/// their tests from silently implying APRS packet access.
@MainActor
protocol APRSControlling: AnyObject {
    var currentAPRSState: APRSOperationalState { get }
    var aprsUpdates: AsyncStream<APRSOperationalState> { get }

    func startAPRS(_ configuration: APRSSessionConfiguration) async throws
    func stopAPRS() async throws
    func sendAPRSMessage(
        addressee: String,
        text: String,
        messageID: String?
    ) async throws -> APRSActivity
    func sendAPRSPosition(
        latitude: Double,
        longitude: Double,
        comment: String
    ) async throws -> APRSActivity
}

@MainActor
final class UnavailableAPRSController: APRSControlling {
    let currentAPRSState: APRSOperationalState

    init(reason: String = "This radio adapter does not expose a KISS packet stream.") {
        currentAPRSState = .unavailable(reason)
    }

    var aprsUpdates: AsyncStream<APRSOperationalState> {
        let state = currentAPRSState
        return AsyncStream { continuation in
            continuation.yield(state)
            continuation.finish()
        }
    }

    func startAPRS(_ configuration: APRSSessionConfiguration) async throws {
        throw RadioControllerError.capabilityUnavailable(unavailableReason)
    }

    func stopAPRS() async throws {
        throw RadioControllerError.capabilityUnavailable(unavailableReason)
    }

    func sendAPRSMessage(
        addressee: String,
        text: String,
        messageID: String?
    ) async throws -> APRSActivity {
        throw RadioControllerError.capabilityUnavailable(unavailableReason)
    }

    func sendAPRSPosition(
        latitude: Double,
        longitude: Double,
        comment: String
    ) async throws -> APRSActivity {
        throw RadioControllerError.capabilityUnavailable(unavailableReason)
    }

    private var unavailableReason: String {
        if case .unavailable(let reason) = currentAPRSState.status.phase { return reason }
        return "APRS packet operation is unavailable."
    }
}
