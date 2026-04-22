// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Lodestar

final class LodestarCoreFFITests: XCTestCase {
    // `NSObject.version` shadows the module-scoped `version()` function
    // from Generated/LodestarCore.swift; qualify with the module name.

    func testVersionIsNonEmpty() {
        let v = Lodestar.version()
        XCTAssertFalse(v.isEmpty, "version() must not return an empty string")
    }

    func testVersionIsSemverShape() {
        let v = Lodestar.version()
        let parts = v.split(separator: ".")
        XCTAssertEqual(parts.count, 3, "expected three-part semver, got \(v)")
        for part in parts {
            XCTAssertTrue(
                part.allSatisfy({ $0.isASCII && $0.isNumber }),
                "semver part \(part) is not numeric"
            )
        }
    }

    func testVersionMatchesZeroYxMajor() {
        let v = Lodestar.version()
        XCTAssertTrue(v.hasPrefix("0."), "expected 0.x.y core version, got \(v)")
    }
}
