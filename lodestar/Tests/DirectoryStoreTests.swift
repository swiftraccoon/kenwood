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

    func testDPlusCacheRoundTripAndRefreshReplacement() throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: dir) }
        let cacheUrl = dir.appendingPathComponent("reflectors.json")
        let store = ReflectorDirectoryStore(cacheUrl: cacheUrl)
        let bundled = try XCTUnwrap(store.entries.first {
            $0.source == .bundled && $0.reflector.protocol == .dPlus
        })
        store.integrateDPlus(fetched: [Reflector(
            name: bundled.reflector.name, host: "auth-shadow.example", port: 20001,
            protocol: .dPlus, description: ""
        ), Reflector(
            name: "REF888", host: "removed.example", port: 20001,
            protocol: .dPlus, description: ""
        ), Reflector(
            name: "XLX999", host: "wrong-protocol.example", port: 30001,
            protocol: .dExtra, description: ""
        )])
        store.integrateDPlus(fetched: [Reflector(
            name: "REF999", host: "fresh.example", port: 20001,
            protocol: .dPlus, description: ""
        )])
        XCTAssertFalse(store.entries.contains { $0.reflector.name == "REF888" })
        XCTAssertFalse(store.entries.contains { $0.reflector.name == "XLX999" })
        XCTAssertTrue(store.entries.contains { entry in
            entry.reflector.name == bundled.reflector.name
                && entry.reflector.host == bundled.reflector.host
                && entry.reflector.port == bundled.reflector.port
                && entry.source == .bundled
        }, "a removed auth override must reveal its bundled fallback")
        XCTAssertTrue(store.entries.contains {
            $0.reflector.name == "REF999" && $0.reflector.host == "fresh.example"
        })

        let reloaded = ReflectorDirectoryStore(cacheUrl: cacheUrl)
        XCTAssertTrue(reloaded.entries.contains { entry in
            entry.reflector.name == "REF999"
                && entry.reflector.host == "fresh.example"
                && entry.source == .dPlusAuth
        }, "DPlus-auth entries must survive relaunch via the JSON cache")

        store.integrateDPlus(fetched: [])
        XCTAssertFalse(store.entries.contains { $0.source == .dPlusAuth })
        let reloadedEmptySnapshot = ReflectorDirectoryStore(cacheUrl: cacheUrl)
        XCTAssertTrue(
            reloadedEmptySnapshot.entries.allSatisfy { $0.source == .bundled },
            "an empty successful refresh must remove the prior auth snapshot"
        )
    }

    func testLegacyXlxCacheRowsAreIgnored() throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: dir) }
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let cacheUrl = dir.appendingPathComponent("reflectors.json")
        let legacyCache: [String: Any] = [
            "fetchedAt": 0.0,
            "entries": [
                [
                    "name": "XLX999",
                    "host": "attacker-controlled.example",
                    "port": 30001,
                    "protocolName": "dextra",
                    "sourceName": "xlx",
                ],
                [
                    "name": "MISLABELLED-AUTH-ROW",
                    "host": "mislabelled-auth.example",
                    "port": 30001,
                    "protocolName": "dextra",
                    "sourceName": "auth",
                ],
            ],
        ]
        let data = try JSONSerialization.data(withJSONObject: legacyCache)
        try data.write(to: cacheUrl)

        let store = ReflectorDirectoryStore(cacheUrl: cacheUrl)
        XCTAssertFalse(store.entries.contains { $0.reflector.name == "XLX999" })
        XCTAssertFalse(store.entries.contains {
            $0.reflector.name == "MISLABELLED-AUTH-ROW"
        })
        XCTAssertTrue(store.entries.allSatisfy { $0.source == .bundled })
    }
}
