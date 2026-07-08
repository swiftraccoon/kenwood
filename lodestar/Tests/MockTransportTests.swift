// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Lodestar

final class MockTransportTests: XCTestCase {
    func testScriptedResponseAndWriteCapture() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        // MMDVM GetVersion probe: 0xE0 0x03 0x00 → version response frame.
        await mock.script(response: [0xE0, 0x04, 0x00, 0x01], for: [0xE0, 0x03, 0x00])
        try await mock.write([0xE0, 0x03, 0x00])
        let reply = try await mock.read(maxBytes: 16)
        XCTAssertEqual(reply, [0xE0, 0x04, 0x00, 0x01])
        let written = await mock.writtenBytes()
        XCTAssertEqual(written, [[0xE0, 0x03, 0x00]])
    }

    func testUnscriptedWriteDoesNotEcho() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        try await mock.write([0x01, 0x02])
        await mock.push([0xAA])
        let got = try await mock.read(maxBytes: 16)
        XCTAssertEqual(got, [0xAA], "unscripted write must not enqueue an echo")
    }

    func testSimulateUnexpectedCloseResumesReadsAndKeepsStream() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        async let pending = mock.read(maxBytes: 16)
        try await Task.sleep(nanoseconds: 50_000_000)
        await mock.simulateUnexpectedClose()
        let got = try await pending
        XCTAssertEqual(got, [], "pending read resumes empty on close")
        let state = await mock.state
        XCTAssertEqual(state, .disconnected)
    }
}
