// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// Lossless projection of the Rust-owned KISS journal into app-domain models.
/// Sequence numbers remain the sole merge key so polling cannot duplicate a
/// transmit row that was returned directly by an explicit send operation.
enum AzimuthCoreAPRSAdapter {
    static let activityLimit = 1_000

    static func coreConfiguration(
        _ configuration: APRSSessionConfiguration
    ) -> AprsSessionConfig {
        AprsSessionConfig(
            stationCallsign: configuration.stationCallsign,
            path: configuration.path,
            dataRate: coreDataRate(configuration.dataRate),
            symbolTable: configuration.symbolTable,
            symbolCode: configuration.symbolCode,
            txDelay10ms: configuration.txDelay10ms,
            persistence: configuration.persistence,
            slotTime10ms: configuration.slotTime10ms,
            txTail10ms: configuration.txTail10ms,
            fullDuplex: configuration.fullDuplex
        )
    }

    static func operationalState(
        _ snapshot: AprsOperationalSnapshot,
        retaining previous: APRSOperationalState? = nil
    ) -> APRSOperationalState {
        let incoming = snapshot.activities.map(activity)
        let existing = previous?.activities ?? []
        var rowsBySequence = Dictionary(
            uniqueKeysWithValues: existing.map { ($0.sequence, $0) }
        )
        for row in incoming {
            rowsBySequence[row.sequence] = row
        }
        let allRows = rowsBySequence.values.sorted { $0.sequence < $1.sequence }
        let localHistoryWasTrimmed = allRows.count > activityLimit
        let retainedRows = Array(allRows.suffix(activityLimit))

        return APRSOperationalState(
            status: status(snapshot.status),
            activities: retainedRows,
            stations: snapshot.stations.map(station),
            latestSequence: max(snapshot.latestSequence, previous?.latestSequence ?? 0),
            historyTruncated: snapshot.historyTruncated
                || snapshot.status.droppedActivities > 0
                || localHistoryWasTrimmed
                || (previous?.historyTruncated ?? false)
        )
    }

    static func activity(_ record: AprsActivityRecord) -> APRSActivity {
        APRSActivity(
            sequence: record.sequence,
            sessionID: record.sessionId,
            timestamp: date(millisecondsSince1970: record.timestampUnixMs),
            direction: direction(record.direction),
            kind: kind(record.kind),
            source: record.source,
            destination: record.destination,
            path: record.path,
            summary: record.summary,
            rawPacket: record.rawPacket,
            rawAX25: record.rawAx25,
            latitude: record.latitude,
            longitude: record.longitude,
            speedKnots: record.speedKnots,
            courseDegrees: record.courseDegrees
        )
    }

    private static func status(_ record: AprsSessionStatus) -> APRSSessionStatus {
        APRSSessionStatus(
            phase: phase(record.phase),
            sessionID: record.sessionId,
            startedAt: record.startedAtUnixMs.map(date(millisecondsSince1970:)),
            configuration: record.configuration.map(configuration),
            receivedPackets: record.receivedPackets,
            transmittedPackets: record.transmittedPackets,
            decodeFailures: record.decodeFailures,
            droppedActivities: record.droppedActivities,
            lastError: record.lastError
        )
    }

    private static func station(_ record: AprsStationRecord) -> APRSStation {
        APRSStation(
            callsign: record.callsign,
            lastHeard: date(millisecondsSince1970: record.lastHeardUnixMs),
            packetCount: record.packetCount,
            latitude: record.latitude,
            longitude: record.longitude,
            speedKnots: record.speedKnots,
            courseDegrees: record.courseDegrees,
            path: record.path,
            latestSummary: record.latestSummary
        )
    }

    private static func configuration(
        _ record: AprsSessionConfig
    ) -> APRSSessionConfiguration {
        APRSSessionConfiguration(
            stationCallsign: record.stationCallsign,
            path: record.path,
            dataRate: dataRate(record.dataRate),
            symbolTable: record.symbolTable,
            symbolCode: record.symbolCode,
            txDelay10ms: record.txDelay10ms,
            persistence: record.persistence,
            slotTime10ms: record.slotTime10ms,
            txTail10ms: record.txTail10ms,
            fullDuplex: record.fullDuplex
        )
    }

    private static func coreDataRate(_ dataRate: APRSPacketDataRate) -> AprsPacketDataRate {
        switch dataRate {
        case .bps1200: return .bps1200
        case .bps9600: return .bps9600
        }
    }

    private static func dataRate(_ dataRate: AprsPacketDataRate) -> APRSPacketDataRate {
        switch dataRate {
        case .bps1200: return .bps1200
        case .bps9600: return .bps9600
        }
    }

    private static func phase(_ phase: AprsSessionPhase) -> APRSSessionPhase {
        switch phase {
        case .inactive: return .inactive
        case .starting: return .starting
        case .active: return .active
        case .restoring: return .restoring
        case .failed: return .failed
        }
    }

    private static func direction(
        _ direction: AprsActivityDirection
    ) -> APRSActivityDirection {
        switch direction {
        case .rx: return .rx
        case .tx: return .tx
        case .system: return .system
        }
    }

    private static func kind(_ kind: AprsActivityKind) -> APRSActivityKind {
        switch kind {
        case .session: return .session
        case .position: return .position
        case .message: return .message
        case .status: return .status
        case .object: return .object
        case .item: return .item
        case .weather: return .weather
        case .telemetry: return .telemetry
        case .query: return .query
        case .thirdParty: return .thirdParty
        case .grid: return .grid
        case .rawGps: return .rawGPS
        case .capabilities: return .capabilities
        case .directionFinding: return .directionFinding
        case .userDefined: return .userDefined
        case .test: return .test
        case .rawWeather: return .rawWeather
        case .ax25: return .ax25
        case .kissControl: return .kissControl
        case .decodeError: return .decodeError
        case .error: return .error
        }
    }

    private static func date(millisecondsSince1970: UInt64) -> Date {
        Date(timeIntervalSince1970: TimeInterval(millisecondsSince1970) / 1_000)
    }
}
