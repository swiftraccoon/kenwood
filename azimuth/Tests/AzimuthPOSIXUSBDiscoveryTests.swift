// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

#if os(macOS)

import Foundation
import XCTest
@testable import Azimuth

final class AzimuthPOSIXUSBDiscoveryTests: XCTestCase {
    func testDiscoveryIncludesOnlySortedCalloutDevices() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: false
        )
        defer { try? FileManager.default.removeItem(at: directory) }
        for name in ["cu.usbmodemB", "tty.usbmodemA", "cu.Bluetooth", "cu.usbmodemA"] {
            XCTAssertTrue(FileManager.default.createFile(
                atPath: directory.appendingPathComponent(name).path,
                contents: Data()
            ))
        }

        XCTAssertEqual(
            POSIXAzimuthUSBSerialLink.availableDevicePaths(directory: directory.path),
            ["cu.usbmodemA", "cu.usbmodemB"].map {
                directory.appendingPathComponent($0).path
            }
        )
    }
}

#endif
