// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Azimuth

final class AzimuthRouteTests: XCTestCase {
    func testOperatorDestinationsHaveStableOrderAndMetadata() {
        XCTAssertEqual(
            AzimuthRoute.allCases,
            [.radio, .aprs, .ifDSP, .settings, .assistant, .learn]
        )
        XCTAssertEqual(AzimuthRoute.allCases.map(\.rawValue), [
            "radio", "aprs", "if-dsp", "settings", "assistant", "learn",
        ])
        XCTAssertEqual(AzimuthRoute.allCases.map(\.title), [
            "Radio", "APRS", "IF-DSP", "Settings", "Assistant", "Learn",
        ])
        XCTAssertEqual(AzimuthRoute.aprs.symbol, "point.3.connected.trianglepath.dotted")
        XCTAssertEqual(AzimuthRoute.ifDSP.symbol, "waveform.path.ecg.rectangle")
    }

    func testRouteIdentityAndPresentationMetadataAreUnique() {
        let routes = AzimuthRoute.allCases
        XCTAssertEqual(Set(routes.map(\.id)).count, routes.count)
        XCTAssertEqual(Set(routes.map(\.title)).count, routes.count)
        XCTAssertEqual(Set(routes.map(\.symbol)).count, routes.count)
        XCTAssertTrue(routes.allSatisfy { !$0.title.isEmpty && !$0.symbol.isEmpty })
    }
}
