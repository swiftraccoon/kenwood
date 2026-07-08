// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Lodestar

@MainActor
final class DirectoryStoreTests: XCTestCase {
    func testStartsWithBundledDirectory() throws {
        let store = ReflectorDirectoryStore(cacheUrl: nil)
        XCTAssertFalse(store.entries.isEmpty)
        XCTAssertTrue(store.entries.allSatisfy { $0.source == .bundled })
        XCTAssertTrue(store.reflectors.contains { $0.name == "REF030" })
    }

    func testCacheRoundTrip() async throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString)
        let cacheUrl = dir.appendingPathComponent("reflectors.json")
        let store = ReflectorDirectoryStore(cacheUrl: cacheUrl)
        let fetched = [Reflector(
            name: "XLX999", host: "xlx999.example", port: 30001,
            protocol: .dExtra, description: ""
        )]
        store.integrate(fetched: fetched, source: .xlxRegistry)
        XCTAssertTrue(store.entries.contains { $0.reflector.name == "XLX999" })

        let reloaded = ReflectorDirectoryStore(cacheUrl: cacheUrl)
        XCTAssertTrue(reloaded.entries.contains { $0.reflector.name == "XLX999" },
                      "fetched entries must survive relaunch via the JSON cache")
    }
}
