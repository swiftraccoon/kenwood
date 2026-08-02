// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Azimuth

final class LearningContentTests: XCTestCase {
    private let chapters = AzimuthLearningLibrary.chapters

    func testLibraryIsACompleteStructuredCurriculum() {
        XCTAssertEqual(chapters.count, 18)
        XCTAssertEqual(Set(chapters.map(\.id)).count, chapters.count, "chapter IDs must be stable and unique")
        XCTAssertTrue(chapters.allSatisfy { !$0.title.isEmpty && !$0.summary.isEmpty })
        XCTAssertTrue(chapters.allSatisfy { !$0.sections.isEmpty })
        XCTAssertTrue(chapters.allSatisfy { chapter in
            chapter.sections.allSatisfy { !$0.heading.isEmpty && !$0.body.isEmpty }
        })
        XCTAssertTrue(chapters.allSatisfy { !$0.relatedGroups.isEmpty })

        let collections = Dictionary(grouping: chapters, by: \.collection)
        for collection in LearningCollection.allCases {
            XCTAssertGreaterThanOrEqual(
                collections[collection, default: []].count,
                2,
                "every Learn collection should offer more than one useful path"
            )
        }
    }

    func testLibraryCoversEveryRequestedOperatorWorkflow() {
        let expectedIDs: Set<String> = [
            "analog-basics",
            "bands-and-modes",
            "scan-and-resume",
            "memory-channels",
            "repeaters-and-tones",
            "aprs-identity-and-beacons",
            "aprs-messages",
            "gps-and-position",
            "dstar-callsigns-and-routing",
            "dstar-gateway-modes",
            "usb-audio-and-control",
            "battery-and-power",
            "display-and-accessibility",
            "settings-backup-and-write-safety",
            "assistant-approval",
        ]
        XCTAssertTrue(expectedIDs.isSubset(of: Set(chapters.map(\.id))))
    }

    func testTaskLanguageIsSearchable() throws {
        XCTAssertTrue(try chapter("analog-basics").matches("squelch"))
        XCTAssertTrue(try chapter("bands-and-modes").matches("narrow FM"))
        XCTAssertTrue(try chapter("scan-and-resume").matches("Carrier resume"))
        XCTAssertTrue(try chapter("memory-channels").matches("VFO"))
        XCTAssertTrue(try chapter("repeaters-and-tones").matches("CTCSS"))
        XCTAssertTrue(try chapter("aprs-identity-and-beacons").matches("SmartBeaconing"))
        XCTAssertTrue(try chapter("aprs-messages").matches("acknowledgement"))
        XCTAssertTrue(try chapter("gps-and-position").matches("NMEA"))
        XCTAssertTrue(try chapter("dstar-callsigns-and-routing").matches("URCALL"))
        XCTAssertTrue(try chapter("usb-audio-and-control").matches("detect output"))
        XCTAssertTrue(try chapter("battery-and-power").matches("Auto power off"))
        XCTAssertTrue(try chapter("display-and-accessibility").matches("accessibility"))
        XCTAssertTrue(try chapter("settings-backup-and-write-safety").matches("recovery point"))
        XCTAssertTrue(try chapter("assistant-approval").matches("Accept"))
    }

    func testCrossLinksReachEverySettingsGroup() {
        let linkedGroups = Set(chapters.flatMap(\.relatedGroups))
        XCTAssertEqual(linkedGroups, Set(RadioSettingGroup.allCases))
    }

    func testSpecializedChaptersLinkToRelevantSettings() throws {
        XCTAssertEqual(
            Set(try chapter("aprs-identity-and-beacons").relatedGroups),
            Set([.aprs, .gps, .connectivity])
        )
        XCTAssertTrue(
            try chapter("dstar-callsigns-and-routing").relatedGroups.contains(.digitalVoice)
        )
        XCTAssertTrue(
            try chapter("usb-audio-and-control").relatedGroups.contains(.audio)
        )
        XCTAssertEqual(
            Set(try chapter("assistant-approval").relatedGroups),
            Set(RadioSettingGroup.allCases)
        )
    }

    private func chapter(_ id: String) throws -> LearningChapter {
        try XCTUnwrap(chapters.first { $0.id == id })
    }
}
