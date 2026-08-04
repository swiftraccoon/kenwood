// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

#if os(macOS)

import Foundation
import XCTest
@testable import Azimuth

final class AzimuthPOSIXUSBDiscoveryTests: XCTestCase {
    func testDiscoveryIncludesOnlySortedVerifiedTHD75CalloutDevices() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: false
        )
        defer { try? FileManager.default.removeItem(at: directory) }
        for name in [
            "cu.usbmodemC",
            "cu.usbmodemB",
            "tty.usbmodemA",
            "cu.Bluetooth",
            "cu.usbmodemA",
        ] {
            XCTAssertTrue(FileManager.default.createFile(
                atPath: directory.appendingPathComponent(name).path,
                contents: Data()
            ))
        }

        let radioA = directory.appendingPathComponent("cu.usbmodemA").path
        let unrelatedB = directory.appendingPathComponent("cu.usbmodemB").path
        let radioC = directory.appendingPathComponent("cu.usbmodemC").path
        let verified = POSIXAzimuthUSBSerialLink.availableDevicePaths(
            directory: directory.path,
            identityProvider: { path in
                if path == radioA || path == radioC {
                    return .init(
                        vendorID: POSIXAzimuthUSBSerialLink.thD75VendorID,
                        productID: POSIXAzimuthUSBSerialLink.thD75ProductID
                    )
                }
                if path == unrelatedB {
                    return .init(vendorID: 0x2341, productID: 0x0043)
                }
                return nil
            }
        )

        XCTAssertEqual(verified, [radioA, radioC])
    }

    func testImplicitSelectionRejectsMultipleVerifiedRadios() throws {
        let paths = ["/dev/cu.usbmodemA", "/dev/cu.usbmodemB"]

        XCTAssertThrowsError(
            try POSIXAzimuthUSBSerialLink.selectDevicePath(
                requestedPath: nil,
                verifiedPaths: paths
            )
        ) { error in
            guard case AzimuthUSBLinkError.ambiguousDevices(let found) = error else {
                return XCTFail("unexpected error: \(error)")
            }
            XCTAssertEqual(found, paths)
        }
        XCTAssertEqual(
            try POSIXAzimuthUSBSerialLink.selectDevicePath(
                requestedPath: paths[1],
                verifiedPaths: paths
            ),
            paths[1]
        )
        XCTAssertThrowsError(
            try POSIXAzimuthUSBSerialLink.selectDevicePath(
                requestedPath: "/dev/cu.usbmodem-unverified",
                verifiedPaths: paths
            )
        ) { error in
            XCTAssertEqual(error as? AzimuthUSBLinkError, .serviceNotFound)
        }
        XCTAssertThrowsError(
            try POSIXAzimuthUSBSerialLink.selectDevicePath(
                requestedPath: nil,
                verifiedPaths: []
            )
        ) { error in
            XCTAssertEqual(error as? AzimuthUSBLinkError, .serviceNotFound)
        }
    }
}

#endif
