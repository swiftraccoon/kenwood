// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Azimuth

final class APRSWorkspaceTests: XCTestCase {
    func testSnapshotDoesNotTreatUnreadOrPlaceholderCallsignAsConfigured() {
        let catalog = testCatalog()

        let unread = APRSConfigurationSnapshot(catalog: catalog, values: [:])
        XCTAssertEqual(unread.callsignStatus, .notRead)
        XCTAssertEqual(unread.callsignLabel, "NOT READ")

        let empty = APRSConfigurationSnapshot(
            catalog: catalog,
            values: [APRSSettingID.myCallsign: .text("   ")]
        )
        XCTAssertEqual(empty.callsignStatus, .missing)

        let placeholder = APRSConfigurationSnapshot(
            catalog: catalog,
            values: [APRSSettingID.myCallsign: .text("NOCALL")]
        )
        XCTAssertEqual(placeholder.callsignStatus, .missing)

        let placeholderWithSSID = APRSConfigurationSnapshot(
            catalog: catalog,
            values: [APRSSettingID.myCallsign: .text("nocall-7")]
        )
        XCTAssertEqual(placeholderWithSSID.callsignStatus, .missing)
    }

    func testSnapshotRecognizesConfiguredCallsignAndCatalogChoiceLabels() {
        let snapshot = APRSConfigurationSnapshot(
            catalog: testCatalog(),
            values: [
                APRSSettingID.myCallsign: .text(" K1ABC-7 "),
                APRSSettingID.beaconMethod: .choice(rawValue: 3),
                APRSSettingID.dataBand: .choice(rawValue: 1),
                APRSSettingID.dataSpeed: .choice(rawValue: 0),
                APRSSettingID.packetPath: .choice(rawValue: 0),
            ]
        )

        XCTAssertEqual(snapshot.callsignStatus, .configured("K1ABC-7"))
        XCTAssertEqual(snapshot.callsignLabel, "K1ABC-7")
        XCTAssertEqual(snapshot.beaconMethodLabel, "SmartBeaconing")
        XCTAssertEqual(snapshot.dataBandLabel, "B Band")
        XCTAssertEqual(snapshot.dataSpeedLabel, "1200 bps")
        XCTAssertEqual(snapshot.packetPathLabel, "New-N")
    }

    func testSnapshotResolvesTheSelectedStatusTextSlot() {
        let snapshot = APRSConfigurationSnapshot(
            catalog: testCatalog(),
            values: [
                APRSSettingID.statusTextSelect: .integer(2),
                "aprs.StatusTextList[2].StatusText": .text("Portable by the lake"),
            ]
        )

        XCTAssertEqual(snapshot.selectedStatusLabel, "Portable by the lake")
        XCTAssertEqual(snapshot.selectedStatusIndex, 2)
        XCTAssertEqual(snapshot.label(for: APRSSettingID.statusTextSelect), "Text 3")
    }

    func testCuratedLinksAreUniqueAndResolveAgainstReviewedSchema() async throws {
        XCTAssertEqual(Set(APRSSettingLinks.all.map(\.id)).count, APRSSettingLinks.all.count)

        let catalog = try await AzimuthCoreCatalogProvider().catalog()
        for link in APRSSettingLinks.all {
            let definition = try XCTUnwrap(
                catalog.definition(id: link.id),
                "Missing curated APRS setting \(link.id)"
            )
            XCTAssertEqual(definition.group, .aprs)
            XCTAssertNotNil(definition.menuNumberLabel)
        }
    }

    private func testCatalog() -> RadioSettingCatalog {
        RadioSettingCatalog(
            source: .reviewedSchema(version: "test"),
            definitions: [
                definition(
                    id: APRSSettingID.myCallsign,
                    title: "My Callsign",
                    domain: .text(maxLength: 9, encoding: .ascii)
                ),
                definition(
                    id: APRSSettingID.beaconMethod,
                    title: "Method",
                    domain: .choice([
                        .init(rawValue: 0, label: "Manual"),
                        .init(rawValue: 1, label: "PTT"),
                        .init(rawValue: 2, label: "Auto"),
                        .init(rawValue: 3, label: "SmartBeaconing"),
                    ])
                ),
                definition(
                    id: APRSSettingID.dataBand,
                    title: "Data Band",
                    domain: .choice([
                        .init(rawValue: 0, label: "A Band"),
                        .init(rawValue: 1, label: "B Band"),
                    ])
                ),
                definition(
                    id: APRSSettingID.dataSpeed,
                    title: "Data Speed",
                    domain: .choice([
                        .init(rawValue: 0, label: "1200 bps"),
                        .init(rawValue: 1, label: "9600 bps"),
                    ])
                ),
                definition(
                    id: APRSSettingID.packetPath,
                    title: "Packet Path",
                    domain: .choice([
                        .init(rawValue: 0, label: "New-N"),
                        .init(rawValue: 1, label: "Relay"),
                    ])
                ),
                definition(
                    id: APRSSettingID.statusTextSelect,
                    title: "Status Text",
                    domain: .integer(range: 0...4, step: 1, unit: nil)
                ),
            ]
        )
    }

    private func definition(
        id: String,
        title: String,
        domain: RadioSettingDomain
    ) -> RadioSettingDefinition {
        RadioSettingDefinition(
            id: id,
            group: .aprs,
            title: title,
            summary: "Test APRS setting.",
            domain: domain,
            menuNumbers: ["500"],
            schemaReference: nil,
            requiresRestart: false,
            requiresReconnect: false,
            isSpecializedEditor: false
        )
    }
}
