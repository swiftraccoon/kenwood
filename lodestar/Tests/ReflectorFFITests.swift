// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Lodestar

final class ReflectorFFITests: XCTestCase {
    func testDefaultReflectorListIsNonEmpty() {
        let list = defaultReflectors()
        XCTAssertFalse(list.isEmpty, "default reflector list must be populated")
    }

    func testDefaultReflectorListCoversAllProtocols() {
        let kinds = Set(defaultReflectors().map(\.protocol))
        XCTAssertTrue(kinds.contains(.dPlus), "needs at least one DPlus reflector")
        XCTAssertTrue(kinds.contains(.dExtra), "needs at least one DExtra reflector")
        XCTAssertTrue(kinds.contains(.dcs), "needs at least one DCS reflector")
    }

    func testEveryReflectorHasNonZeroPort() {
        for r in defaultReflectors() {
            XCTAssertNotEqual(r.port, 0, "\(r.name) has zero port")
            XCTAssertFalse(r.host.isEmpty, "\(r.name) has empty host")
            XCTAssertFalse(r.name.isEmpty, "reflector has empty name")
        }
    }

    func testReflectorNamesAreUppercase() {
        for r in defaultReflectors() {
            XCTAssertEqual(r.name, r.name.uppercased(), "\(r.name) should be uppercase")
        }
    }
}
