// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import XCTest
@testable import Azimuth

final class APRSOperationsWorkspaceTests: XCTestCase {
    func testActivityQueryCombinesDirectionAndTextWithoutAddingRows() {
        let received = activity(
            sequence: 1,
            direction: .rx,
            kind: .message,
            source: "K1ABC-7",
            summary: "K1ABC-7 → N0CALL: trailhead"
        )
        let transmitted = activity(
            sequence: 2,
            direction: .tx,
            kind: .position,
            source: "N0CALL",
            summary: "N0CALL position 42.1, -71.2"
        )
        let system = activity(
            sequence: 3,
            direction: .system,
            kind: .session,
            source: nil,
            summary: "KISS packet monitoring is active"
        )

        let result = APRSActivityQuery(filter: .received, text: "trailHEAD")
            .apply(to: [received, transmitted, system])

        XCTAssertEqual(result, [received])
        XCTAssertEqual(
            APRSActivityQuery(filter: .transmitted).apply(to: [received, transmitted, system]),
            [transmitted]
        )
        XCTAssertEqual(
            APRSActivityQuery(filter: .system).apply(to: [received, transmitted, system]),
            [system]
        )
        XCTAssertTrue(APRSActivityQuery(filter: .all, text: "missing").apply(to: [received]).isEmpty)
    }

    func testProblemAndWeatherFiltersCoverProtocolVariants() {
        let decode = activity(sequence: 1, direction: .rx, kind: .decodeError, source: nil, summary: "bad AX.25")
        let transport = activity(sequence: 2, direction: .system, kind: .error, source: nil, summary: "link down")
        let weather = activity(sequence: 3, direction: .rx, kind: .weather, source: "WX1", summary: "weather")
        let rawWeather = activity(sequence: 4, direction: .rx, kind: .rawWeather, source: "WX2", summary: "raw weather")
        let rows = [decode, transport, weather, rawWeather]

        XCTAssertEqual(APRSActivityQuery(filter: .problems).apply(to: rows), [decode, transport])
        XCTAssertEqual(APRSActivityQuery(filter: .weather).apply(to: rows), [weather, rawWeather])
    }

    func testSessionConfigurationValidationAllowsReceiveOnlyAndRejectsUnsafeFields() {
        XCTAssertNil(
            APRSSessionConfigurationValidator.firstError(in: .receiveOnly),
            "A blank source callsign is a deliberate receive-only session"
        )

        var configuration = APRSSessionConfiguration.receiveOnly
        configuration.stationCallsign = "TOOLONG-16"
        XCTAssertNotNil(APRSSessionConfigurationValidator.firstError(in: configuration))

        configuration.stationCallsign = "K1ABC-7"
        configuration.symbolTable = "//"
        XCTAssertEqual(
            APRSSessionConfigurationValidator.firstError(in: configuration),
            "Symbol table must contain exactly one printable ASCII character."
        )

        configuration.symbolTable = "/"
        configuration.txDelay10ms = 121
        XCTAssertEqual(
            APRSSessionConfigurationValidator.firstError(in: configuration),
            "TX delay must be between 0 and 120 (0–1200 ms)."
        )
    }

    func testTransmitValidationMatchesOneShotCoreLimits() {
        XCTAssertNil(
            APRSTransmitValidator.messageError(
                addressee: "K1ABC-7",
                text: "Meet at the trailhead",
                messageID: "A12"
            )
        )
        XCTAssertNotNil(
            APRSTransmitValidator.messageError(
                addressee: "K1ABC-7",
                text: String(repeating: "x", count: 68),
                messageID: nil
            )
        )
        XCTAssertNotNil(
            APRSTransmitValidator.messageError(
                addressee: "K1ABC-7",
                text: "hello",
                messageID: "bad-id"
            )
        )

        let position = APRSTransmitValidator.position(latitude: "42.3601", longitude: "-71.0589")
        XCTAssertEqual(position?.latitude, 42.3601)
        XCTAssertEqual(position?.longitude, -71.0589)
        XCTAssertNil(APRSTransmitValidator.position(latitude: "91", longitude: "0"))
        XCTAssertNil(APRSTransmitValidator.position(latitude: "0", longitude: "-181"))
    }

    func testSettingQueryIncludesOnlyAPRSAndUsesMenuOrder() {
        let menu590 = definition(id: "aprs.PcOutput", group: .aprs, menu: "590", title: "PC Output")
        let menu500 = definition(id: "aprs.MyCallsign", group: .aprs, menu: "500", title: "My Callsign")
        let radio = definition(id: "radio.BatterySaver", group: .radio, menu: "920", title: "Battery Saver")

        XCTAssertEqual(
            APRSSettingQuery().apply(to: [menu590, radio, menu500]).map(\.id),
            [menu500.id, menu590.id]
        )
        XCTAssertEqual(
            APRSSettingQuery(text: "590").apply(to: [menu590, menu500]).map(\.id),
            [menu590.id]
        )
    }

    private func activity(
        sequence: UInt64,
        direction: APRSActivityDirection,
        kind: APRSActivityKind,
        source: String?,
        summary: String
    ) -> APRSActivity {
        APRSActivity(
            sequence: sequence,
            sessionID: 9,
            timestamp: Date(timeIntervalSince1970: Double(sequence)),
            direction: direction,
            kind: kind,
            source: source,
            destination: nil,
            path: [],
            summary: summary,
            rawPacket: summary,
            rawAX25: Data(),
            latitude: nil,
            longitude: nil,
            speedKnots: nil,
            courseDegrees: nil
        )
    }

    private func definition(
        id: String,
        group: RadioSettingGroup,
        menu: String,
        title: String
    ) -> RadioSettingDefinition {
        RadioSettingDefinition(
            id: id,
            group: group,
            title: title,
            summary: "Summary for \(title)",
            domain: .boolean,
            menuNumbers: [menu],
            schemaReference: nil,
            requiresRestart: false,
            isSpecializedEditor: false
        )
    }
}
