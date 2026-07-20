// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Lodestar

final class StationMapModelTests: XCTestCase {
    private func entry(
        mycall: String,
        endedAt: Date,
        lat: Double? = nil,
        lon: Double? = nil
    ) -> ReflectorCoordinator.HeardEntry {
        let position: GpsPosition?
        if let lat, let lon {
            position = GpsPosition(callsign: mycall, latitude: lat, longitude: lon, symbol: "/-", comment: nil)
        } else {
            position = nil
        }
        return ReflectorCoordinator.HeardEntry(
            mycall: mycall, suffix: "", urcall: "CQCQCQ",
            endedAt: endedAt, duration: 5, frames: 250,
            endReason: "EOT", text: nil, position: position
        )
    }

    // Entries without GPS produce no pins.
    func testEntriesWithoutPositionAreSkipped() {
        let anns = StationMapPane.annotations(
            heard: [entry(mycall: "W1AW", endedAt: Date(timeIntervalSince1970: 100))],
            liveStream: nil
        )
        XCTAssertTrue(anns.isEmpty)
    }

    // One pin per callsign — the most recent position wins.
    // recentlyHeard is newest-first, so the FIRST match per call is kept.
    func testDuplicateCallsignKeepsNewestPosition() {
        let newer = entry(mycall: "W7YAA", endedAt: Date(timeIntervalSince1970: 200), lat: 44.0, lon: -116.0)
        let older = entry(mycall: "W7YAA", endedAt: Date(timeIntervalSince1970: 100), lat: 43.0, lon: -117.0)
        let anns = StationMapPane.annotations(heard: [newer, older], liveStream: nil)
        XCTAssertEqual(anns.count, 1)
        XCTAssertEqual(anns[0].latitude, 44.0)
    }

    // A live stream with GPS becomes a live pin and supersedes the
    // same station's heard-history pin.
    func testLiveStreamSupersedesHeardPin() {
        let heard = entry(mycall: "KB6MAT", endedAt: Date(timeIntervalSince1970: 100), lat: 34.0, lon: -117.0)
        let snap = ReflectorCoordinator.StreamSnapshot(
            id: 1, mycall: "KB6MAT", suffix: "", urcall: "CQCQCQ",
            rpt1: "KB6MAT B", rpt2: "REF030 C", framesReceived: 10,
            startedAt: Date(timeIntervalSince1970: 300),
            latestText: nil,
            latestPosition: GpsPosition(callsign: "KB6MAT", latitude: 34.5347, longitude: -117.2015, symbol: "/-", comment: nil)
        )
        let anns = StationMapPane.annotations(heard: [heard], liveStream: snap)
        XCTAssertEqual(anns.count, 1)
        XCTAssertTrue(anns[0].isLive)
        XCTAssertEqual(anns[0].latitude, 34.5347)
    }

    // MARK: - Camera fitting region

    private func annotation(_ callsign: String, lat: Double, lon: Double) -> StationAnnotationModel {
        let e = entry(mycall: callsign, endedAt: Date(timeIntervalSince1970: 100), lat: lat, lon: lon)
        return StationMapPane.annotations(heard: [e], liveStream: nil)[0]
    }

    // Single station: centered on it, zoomed no tighter than the floor
    // (the ".automatic zooms to street level on one pin" bug).
    func testFittingRegionSingleStationUsesMinimumSpan() throws {
        let region = try XCTUnwrap(
            StationMapPane.fittingRegion(for: [annotation("W7YAA", lat: 43.9, lon: -116.7)])
        )
        XCTAssertEqual(region.center.latitude, 43.9, accuracy: 0.001)
        XCTAssertEqual(region.center.longitude, -116.7, accuracy: 0.001)
        XCTAssertEqual(region.span.latitudeDelta, 6.0, accuracy: 0.001)
        XCTAssertEqual(region.span.longitudeDelta, 6.0, accuracy: 0.001)
    }

    // Spread stations: fits all with margin; the tight axis still
    // respects the floor.
    func testFittingRegionSpreadStationsFitsWithMargin() throws {
        let region = try XCTUnwrap(StationMapPane.fittingRegion(for: [
            annotation("KB6MAT", lat: 34.0, lon: -117.0),
            annotation("W7YAA", lat: 44.0, lon: -116.0),
        ]))
        XCTAssertEqual(region.center.latitude, 39.0, accuracy: 0.001)
        XCTAssertEqual(region.center.longitude, -116.5, accuracy: 0.001)
        XCTAssertEqual(region.span.latitudeDelta, 14.0, accuracy: 0.001, "10° spread × 1.4 margin")
        XCTAssertEqual(region.span.longitudeDelta, 6.0, accuracy: 0.001, "1.4° fitted lon still floors at 6°")
    }

    func testFittingRegionEmptyIsNil() {
        XCTAssertNil(StationMapPane.fittingRegion(for: []))
    }

    // A live stream that hasn't reported GPS yet adds no pin but keeps
    // the station's historical pin (not superseded by nothing).
    func testLiveStreamWithoutPositionKeepsHeardPin() {
        let heard = entry(mycall: "KB6MAT", endedAt: Date(timeIntervalSince1970: 100), lat: 34.0, lon: -117.0)
        let snap = ReflectorCoordinator.StreamSnapshot(
            id: 1, mycall: "KB6MAT", suffix: "", urcall: "CQCQCQ",
            rpt1: "KB6MAT B", rpt2: "REF030 C", framesReceived: 10,
            startedAt: Date(timeIntervalSince1970: 300),
            latestText: nil, latestPosition: nil
        )
        let anns = StationMapPane.annotations(heard: [heard], liveStream: snap)
        XCTAssertEqual(anns.count, 1)
        XCTAssertFalse(anns[0].isLive)
    }
}
