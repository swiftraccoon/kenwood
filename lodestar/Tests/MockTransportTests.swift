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

    func testParkedReadHonorsMaxBytesAndQueuesRemainder() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        let first = Task { try await mock.read(maxBytes: 1) }
        try await Task.sleep(nanoseconds: 20_000_000)
        await mock.push([0x10, 0x20, 0x30])

        let firstChunk = try await first.value
        let remainder = try await mock.read(maxBytes: 8)
        XCTAssertEqual(firstChunk, [0x10])
        XCTAssertEqual(remainder, [0x20, 0x30])
    }

    func testCancellingOneParkedReadDoesNotResumeItsSibling() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        let first = Task { try await mock.read(maxBytes: 8) }
        try await Task.sleep(nanoseconds: 20_000_000)
        let second = Task { try await mock.read(maxBytes: 8) }
        try await Task.sleep(nanoseconds: 20_000_000)

        first.cancel()
        let cancelledResult = try await first.value
        XCTAssertEqual(cancelledResult, [])
        await mock.push([0xAA])
        let siblingResult = try await second.value
        XCTAssertEqual(siblingResult, [0xAA])
    }
}

final class McpSessionTests: XCTestCase {
    private func enter(
        _ session: McpSession,
        over mock: MockRadioTransport
    ) async throws {
        await mock.script(
            response: Array("0M\r".utf8),
            for: Array(buildEnterCmd())
        )
        try await session.enterProgramming()
    }

    private func pageFrame(_ page: UInt16, data: [UInt8]) throws -> [UInt8] {
        Array(try buildWritePageCmd(page: page, data: Data(data)))
    }

    func testEntryRequiresExactTypedIdAndFirmwareImmediatelyBeforeMcp() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        let session = McpSession(transport: mock)
        try await enter(session, over: mock)
        await mock.script(response: [0x06], for: [UInt8(ascii: "E")])
        try await session.exitProgramming()

        let written = await mock.writtenBytes()
        XCTAssertEqual(
            Array(written.prefix(3)),
            [
                Array("ID\r".utf8),
                Array("FV\r".utf8),
                Array(buildEnterCmd()),
            ]
        )
    }

    func testEntryRejectsWrongFirmwareBeforeMcpWireTraffic() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        await mock.script(
            response: Array("FV 1.04\r".utf8),
            for: Array("FV\r".utf8)
        )

        let session = McpSession(transport: mock)
        do {
            try await session.enterProgramming()
            XCTFail("unqualified firmware must be rejected")
        } catch let error as McpOrchestratorError {
            XCTAssertEqual(error, .unsupportedFirmware(actual: "1.04"))
        }

        let written = await mock.writtenBytes()
        XCTAssertEqual(written, [Array("ID\r".utf8), Array("FV\r".utf8)])
        let mustDetach = await session.requiresTransportDetach()
        XCTAssertFalse(mustDetach, "no MCP entry byte was sent")
        let state = await mock.state
        XCTAssertEqual(state, .connected)
    }

    func testEntryAcceptsExactFullFirmwareWireForm() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        await mock.script(
            response: Array("FV 1.03.000\r".utf8),
            for: Array("FV\r".utf8)
        )
        let session = McpSession(transport: mock)
        try await enter(session, over: mock)
        await mock.script(response: [0x06], for: [UInt8(ascii: "E")])
        try await session.exitProgramming()
    }

    func testConcurrentEntryCannotPassQualificationReentrancyWindow() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        let session = McpSession(transport: mock)

        let first = Task {
            try await session.enterProgramming()
        }
        try await Task.sleep(nanoseconds: 50_000_000)

        do {
            try await session.enterProgramming()
            XCTFail("overlapping entry must be rejected")
        } catch let error as McpOrchestratorError {
            guard case .invalidPhase(_, _, let actual) = error else {
                await mock.push(Array("0M\r".utf8))
                _ = try? await first.value
                return XCTFail("expected invalid phase, got \(error)")
            }
            XCTAssertTrue(
                ["qualifying", "entering"].contains(actual),
                "unexpected phase \(actual)"
            )
        }

        await mock.push(Array("0M\r".utf8))
        try await first.value
        await mock.script(response: [0x06], for: [UInt8(ascii: "E")])
        try await session.exitProgramming()

        let writes = await mock.writtenBytes()
        XCTAssertEqual(writes.filter { $0 == Array(buildEnterCmd()) }.count, 1)
    }

    func testExitProgrammingAcceptsAck() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        let session = McpSession(transport: mock)
        try await enter(session, over: mock)
        await mock.script(response: [0x06], for: [UInt8(ascii: "E")])
        try await session.exitProgramming()

        let written = await mock.writtenBytes()
        XCTAssertEqual(written.last, [UInt8(ascii: "E")])
        let proved = await session.exitWasProved()
        XCTAssertTrue(proved)
        let state = await mock.state
        XCTAssertEqual(state, .disconnected)
    }

    func testExitProgrammingRejectsWrongAckAndNeverSendsSecondExit() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        let session = McpSession(transport: mock)
        try await enter(session, over: mock)
        await mock.script(response: [0x15], for: [UInt8(ascii: "E")])
        do {
            try await session.exitProgramming()
            XCTFail("wrong MCP exit ACK must fail")
        } catch let error as McpOrchestratorError {
            XCTAssertEqual(error, .badExitAck(actual: 0x15))
        }

        do {
            try await session.exitProgramming()
            XCTFail("terminal session must reject a second exit")
        } catch let error as McpOrchestratorError {
            guard case .invalidPhase(_, _, let actual) = error else {
                return XCTFail("expected invalid phase, got \(error)")
            }
            XCTAssertEqual(actual, "terminal")
        }

        let written = await mock.writtenBytes()
        XCTAssertEqual(
            written.filter { $0 == [UInt8(ascii: "E")] }.count,
            1,
            "an ambiguous or rejected E must never be resent"
        )
        XCTAssertTrue(
            McpOrchestratorError.badExitAck(actual: 0x15)
                .localizedDescription.contains("power-cycle")
        )
    }

    func testReadPageRejectsStaleEchoThenDrainsAndRetriesOnce() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        let session = McpSession(transport: mock)
        try await enter(session, over: mock)

        let requested: UInt16 = 0x001C
        let request = Array(buildReadPageCmd(page: requested))
        let expected = [UInt8](repeating: 0xA5, count: 256)
        await mock.scriptSequence(
            responses: [
                try pageFrame(0x001B, data: [UInt8](repeating: 0x11, count: 256)),
                try pageFrame(requested, data: expected),
            ],
            for: request
        )
        await mock.scriptSequence(
            responses: [[0x06], [0x06]],
            for: [0x06]
        )

        let actual = try await session.readPage(requested)
        XCTAssertEqual([UInt8](actual), expected)

        await mock.script(response: [0x06], for: [UInt8(ascii: "E")])
        try await session.exitProgramming()
        let writes = await mock.writtenBytes()
        XCTAssertEqual(writes.filter { $0 == request }.count, 2)
        XCTAssertEqual(writes.filter { $0 == [0x06] }.count, 2)
    }

    func testConcurrentPageOperationCannotInterleaveTransportExchange() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        let session = McpSession(transport: mock)
        try await enter(session, over: mock)

        let page: UInt16 = 0x001C
        let expected = [UInt8](repeating: 0x66, count: 256)
        await mock.script(response: [0x06], for: [0x06])
        let first = Task {
            try await session.readPage(page)
        }
        try await Task.sleep(nanoseconds: 50_000_000)

        do {
            _ = try await session.readPage(page)
            XCTFail("overlapping page operation must be rejected")
        } catch let error as McpOrchestratorError {
            XCTAssertEqual(
                error,
                .operationInProgress(operation: "read MCP page")
            )
        }

        await mock.push(try pageFrame(page, data: expected))
        let actual = try await first.value
        XCTAssertEqual([UInt8](actual), expected)

        await mock.script(response: [0x06], for: [UInt8(ascii: "E")])
        try await session.exitProgramming()
        let writes = await mock.writtenBytes()
        XCTAssertEqual(
            writes.filter { $0 == Array(buildReadPageCmd(page: page)) }.count,
            1
        )
    }

    func testPartialPageTimeoutIsNeverRetriedAndExitAckIsNotTrusted() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        let session = McpSession(
            transport: mock,
            pageReadTimeoutSeconds: 0.05
        )
        try await enter(session, over: mock)

        let page: UInt16 = 0x001C
        let request = Array(buildReadPageCmd(page: page))
        await mock.script(
            response: Array(try pageFrame(
                page, data: [UInt8](repeating: 0x55, count: 256)
            ).prefix(32)),
            for: request
        )
        await mock.script(response: [0x06], for: [UInt8(ascii: "E")])

        do {
            _ = try await session.readPage(page)
            XCTFail("partial W frame must fail")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("power-cycle"))
        }

        let writes = await mock.writtenBytes()
        XCTAssertEqual(writes.filter { $0 == request }.count, 1)
        XCTAssertEqual(writes.filter { $0 == [UInt8(ascii: "E")] }.count, 1)
        let proved = await session.exitWasProved()
        XCTAssertFalse(proved, "an ACK after a desynchronized frame may be stale")
    }

    func testDelayedPageAckCannotBeMistakenForExitProof() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        let session = McpSession(transport: mock)
        try await enter(session, over: mock)

        let page: UInt16 = 0x001C
        var malformed = try pageFrame(
            page, data: [UInt8](repeating: 0x77, count: 256)
        )
        malformed[0] = UInt8(ascii: "X")
        malformed.append(0x06)
        await mock.script(
            response: malformed,
            for: Array(buildReadPageCmd(page: page))
        )

        do {
            _ = try await session.readPage(page)
            XCTFail("malformed W frame must fail")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("power-cycle"))
        }

        let writes = await mock.writtenBytes()
        XCTAssertEqual(
            writes.filter { $0 == [UInt8(ascii: "E")] }.count,
            1
        )
        let proved = await session.exitWasProved()
        XCTAssertFalse(
            proved,
            "a stale page ACK queued ahead of E must not prove MCP exit"
        )
    }

    func testWritePageReadsBackAndRejectsAnyByteMismatch() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        let session = McpSession(transport: mock)
        try await enter(session, over: mock)

        let page: UInt16 = 0x001C
        var expected = [UInt8](repeating: 0x22, count: 256)
        expected[173] = 0x33
        var actual = expected
        actual[173] = 0x44

        let write = Array(try buildWritePageCmd(page: page, data: Data(expected)))
        await mock.script(response: [0x06], for: write)
        await mock.script(
            response: try pageFrame(page, data: actual),
            for: Array(buildReadPageCmd(page: page))
        )
        await mock.script(response: [0x06], for: [0x06])
        await mock.script(response: [0x06], for: [UInt8(ascii: "E")])

        do {
            try await session.writePage(page, data: Data(expected))
            XCTFail("a one-byte read-back mismatch must fail")
        } catch let error as McpOrchestratorError {
            XCTAssertEqual(
                error,
                .writeVerificationMismatch(
                    page: page,
                    offset: 173,
                    expected: 0x33,
                    actual: 0x44
                )
            )
        }

        let writes = await mock.writtenBytes()
        XCTAssertTrue(writes.contains(write))
        XCTAssertTrue(writes.contains(Array(buildReadPageCmd(page: page))))
        XCTAssertEqual(
            writes.filter { $0 == [UInt8(ascii: "E")] }.count,
            1
        )
        let proved = await session.exitWasProved()
        XCTAssertTrue(proved, "failed verification must still prove one-shot exit")
        let state = await mock.state
        XCTAssertEqual(state, .disconnected)
    }
}
