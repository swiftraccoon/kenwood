// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Lodestar

final class StationActionsTests: XCTestCase {
    private func entry(
        text: String? = nil,
        position: GpsPosition? = nil
    ) -> ReflectorCoordinator.HeardEntry {
        ReflectorCoordinator.HeardEntry(
            mycall: "W7YAA",
            suffix: "5100",
            urcall: "CQCQCQ",
            endedAt: Date(timeIntervalSince1970: 1_784_500_000),
            duration: 8,
            frames: 400,
            endReason: "EOT",
            text: text,
            position: position
        )
    }

    private func position() -> GpsPosition {
        GpsPosition(
            callsign: "W7YAA",
            latitude: 43.9048,
            longitude: -116.6838,
            symbol: "/-",
            comment: nil
        )
    }

    // Minimal entry: only callsign-based actions.
    func testMinimalEntryOffersOnlyCallsignActions() {
        let ref = StationRef(entry: entry())
        XCTAssertEqual(ref.availableActions, [.lookUpQrz, .copyCallsign])
    }

    // TX message adds copy-message; no coordinate actions without GPS.
    func testTextAddsCopyMessage() {
        let ref = StationRef(entry: entry(text: "kelly/5100"))
        XCTAssertEqual(ref.availableActions, [.lookUpQrz, .copyCallsign, .copyMessage])
    }

    // Empty-string TX message is treated as absent.
    func testEmptyTextDoesNotAddCopyMessage() {
        let ref = StationRef(entry: entry(text: ""))
        XCTAssertEqual(ref.availableActions, [.lookUpQrz, .copyCallsign])
    }

    // Full entry offers everything, in fixed display order.
    func testFullEntryOffersAllActions() {
        let ref = StationRef(entry: entry(text: "kelly/5100", position: position()))
        XCTAssertEqual(
            ref.availableActions,
            [.lookUpQrz, .copyCallsign, .copyMessage, .copyCoordinates, .openInMaps]
        )
    }

    // Live stream converts with isLive and current slow-data fields.
    func testStreamSnapshotConversion() {
        let snap = ReflectorCoordinator.StreamSnapshot(
            id: 0x1234,
            mycall: "KB6MAT",
            suffix: "",
            urcall: "CQCQCQ",
            rpt1: "KB6MAT B",
            rpt2: "REF030 C",
            framesReceived: 1346,
            startedAt: Date(timeIntervalSince1970: 1_784_500_000),
            latestText: "Mat in California,US",
            latestPosition: position()
        )
        let ref = StationRef(stream: snap)
        XCTAssertTrue(ref.isLive)
        XCTAssertEqual(ref.mycall, "KB6MAT")
        XCTAssertEqual(
            ref.availableActions,
            [.lookUpQrz, .copyCallsign, .copyMessage, .copyCoordinates, .openInMaps]
        )
    }

    // Display shows MYCALL/SUFFIX; bare MYCALL when the suffix is empty.
    func testDisplayCallsignJoinsSuffix() {
        let ref = StationRef(entry: entry())
        XCTAssertEqual(ref.displayCallsign, "W7YAA/5100")
        let bare = StationRef(
            entry: ReflectorCoordinator.HeardEntry(
                mycall: "W7YAA", suffix: "", urcall: "CQCQCQ",
                endedAt: Date(timeIntervalSince1970: 0), duration: 1,
                frames: 1, endReason: "EOT", text: nil, position: nil
            )
        )
        XCTAssertEqual(bare.displayCallsign, "W7YAA")
    }
}
