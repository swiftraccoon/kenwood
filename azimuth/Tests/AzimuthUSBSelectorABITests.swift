// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Azimuth

final class AzimuthUSBSelectorABITests: XCTestCase {
    func testV1SelectorsRemainStable() {
        XCTAssertEqual(AzimuthUSBSelectorV1.write.rawValue, 0)
        XCTAssertEqual(AzimuthUSBSelectorV1.read.rawValue, 1)
        XCTAssertEqual(AzimuthUSBSelectorV1.armDoorbell.rawValue, 2)
        XCTAssertEqual(AzimuthUSBSelectorV1.status.rawValue, 3)
        XCTAssertEqual(AzimuthUSBSelectorV1.copyLog.rawValue, 4)
        XCTAssertEqual(AzimuthUSBSelectorV1.allCases.count, 5)
    }

    func testV2BaudSelectorAppendsV1() {
        XCTAssertEqual(AzimuthUSBSelectorV2.setBaudRate.rawValue, 5)
        XCTAssertEqual(AzimuthUSBABIV1.version, 1)
        XCTAssertEqual(AzimuthUSBABIV2.version, 2)
        XCTAssertEqual(AzimuthUSBABIV2.supportedBaudRates, [9_600, 115_200])
    }

    func testBoundedWireConstants() {
        XCTAssertEqual(AzimuthUSBABIV1.maximumTransferBytes, 4096)
        XCTAssertEqual(AzimuthUSBABIV1.statusScalarCount, 4)
        XCTAssertEqual(AzimuthUSBABIV1.logEntryBytes, 32)
        XCTAssertEqual(AzimuthUSBDextLogEntry.wireSize, 32)
    }

    func testDiagnosticRecordDecodesLittleEndianWithoutAlignment() throws {
        var bytes = [UInt8](repeating: 0, count: 33)
        // Use an intentionally unaligned slice starting at index one.
        bytes[1...4] = [0x78, 0x56, 0x34, 0x12]
        bytes[5...8] = [15, 0, 0, 0]
        bytes[9...16] = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
        bytes[17...24] = [0x80, 0x25, 0, 0, 0, 0, 0, 0]
        let entry = try XCTUnwrap(AzimuthUSBDextLogEntry(bytes: bytes[1...32]))
        XCTAssertEqual(entry.sequence, 0x12345678)
        XCTAssertEqual(entry.event, 15)
        XCTAssertEqual(entry.code, -1)
        XCTAssertEqual(entry.a, 9_600)
    }

    func testTransmitCompletionDiagnosticIsHumanReadable() throws {
        let entry = try XCTUnwrap(Self.logEntry(
            event: 16,
            code: 0,
            a: 3,
            b: 3
        ))
        XCTAssertEqual(entry.text, "#42 TX complete OK bytes=3/3")
    }

    func testTransmitTimeoutUsesTheActualIOKitErrorCode() throws {
        let timeout = Int64(Int32(bitPattern: 0xE00002D6))
        let entry = try XCTUnwrap(Self.logEntry(
            event: 16,
            code: timeout,
            a: 0,
            b: 3
        ))

        XCTAssertEqual(
            entry.text,
            "#42 TX complete kIOReturnTimeout bytes=0/3"
        )
    }

    func testRelevantIOKitReturnCodesAreDecodedExactly() {
        XCTAssertEqual(
            azimuthKernReturnString(Int32(bitPattern: 0xE00002C0)),
            "kIOReturnNoDevice"
        )
        XCTAssertEqual(
            azimuthKernReturnString(Int32(bitPattern: 0xE00002D6)),
            "kIOReturnTimeout"
        )
        XCTAssertEqual(
            azimuthKernReturnString(Int32(bitPattern: 0xE00002D9)),
            "kIOReturnNotAttached"
        )
        XCTAssertEqual(
            azimuthKernReturnString(Int32(bitPattern: 0xE00002E2)),
            "kIOReturnNotPermitted"
        )
        XCTAssertEqual(
            azimuthKernReturnString(Int32(bitPattern: 0xE00002EB)),
            "kIOReturnAborted"
        )
    }

    func testSessionAndReceivePumpDiagnosticsAreHumanReadable() throws {
        XCTAssertEqual(
            try XCTUnwrap(Self.logEntry(event: 3, code: 0, a: 0, b: 0)).text,
            "#42 SET_CONTROL_LINE_STATE none -> OK"
        )
        XCTAssertEqual(
            try XCTUnwrap(Self.logEntry(event: 19, code: 0, a: 115_200, b: 0)).text,
            "#42 session SET_LINE_CODING baud=115200 -> OK"
        )
        XCTAssertEqual(
            try XCTUnwrap(Self.logEntry(event: 20, code: 0, a: 3, b: 0)).text,
            "#42 session SET_CONTROL_LINE_STATE DTR|RTS -> OK"
        )
        XCTAssertEqual(
            try XCTUnwrap(Self.logEntry(event: 23, code: 0, a: 0, b: 0)).text,
            "#42 session SET_CONTROL_LINE_STATE none -> OK"
        )
        XCTAssertEqual(
            try XCTUnwrap(Self.logEntry(event: 21, code: 0, a: 0, b: 1_024)).text,
            "#42 bulk-IN submit initial OK bytes=1024"
        )
        XCTAssertEqual(
            try XCTUnwrap(Self.logEntry(event: 22, code: 0, a: 2, b: 0)).text,
            "#42 bulk-IN complete OK bytes=2 priorStreak=0"
        )
    }

    func testClientResetDiagnosticExposesAbandonedTransmitState() throws {
        let entry = try XCTUnwrap(Self.logEntry(
            event: 17,
            code: 0,
            a: 3,
            b: 6
        ))
        XCTAssertEqual(
            entry.text,
            "#42 client attached; TX reset OK active=3 queued=6"
        )
    }

    private static func logEntry(
        event: UInt32,
        code: Int64,
        a: UInt64,
        b: UInt64
    ) -> AzimuthUSBDextLogEntry? {
        var bytes = [UInt8](repeating: 0, count: AzimuthUSBDextLogEntry.wireSize)
        func store<T: FixedWidthInteger>(_ value: T, at offset: Int) {
            for index in 0..<MemoryLayout<T>.size {
                bytes[offset + index] = UInt8(
                    truncatingIfNeeded: value >> (index * 8)
                )
            }
        }
        store(UInt32(42), at: 0)
        store(event, at: 4)
        store(UInt64(bitPattern: code), at: 8)
        store(a, at: 16)
        store(b, at: 24)
        return AzimuthUSBDextLogEntry(bytes: bytes[...])
    }
}
