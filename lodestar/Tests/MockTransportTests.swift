// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Lodestar

final class MockTransportTests: XCTestCase {
    #if DEBUG && os(macOS)
    func testBluetoothHelperReadyEchoAndRawPipe() throws {
        let payload = Array("ID\r".utf8)
        XCTAssertEqual(
            try IOBluetoothTransport.helperEchoProbe(payload),
            payload
        )
    }

    func testBluetoothHelperDynamicLivenessDescriptorCannotAliasPipes() throws {
        let payload = Array("ID\r".utf8)
        XCTAssertEqual(
            try IOBluetoothTransport.helperHighDescriptorProbe(payload),
            payload
        )
    }

    func testBluetoothHelperRejectsConcurrentChildAndReleasesAfterReap() throws {
        try IOBluetoothTransport.helperReapProbe()
    }

    func testBluetoothHelperSeparatesProductionControlAndTestModes() {
        XCTAssertTrue(IOBluetoothTransport.helperEnvironmentProtocolProbe())
    }

    func testBluetoothHelperReturnsAllPairedDevicesWithoutNameFiltering() {
        let address = "00-11-22-33-44-55"
        let name = "Field Radio"
        let headsetAddress = "AA-BB-CC-DD-EE-FF"
        let headsetName = "Headset"
        var payload = Array("THD75BT-READY-v1".utf8)
        payload.append(contentsOf: UInt16(address.utf8.count).bigEndianBytes)
        payload.append(contentsOf: UInt16(name.utf8.count).bigEndianBytes)
        payload.append(contentsOf: address.utf8)
        payload.append(contentsOf: name.utf8)
        payload.append(contentsOf: UInt16(headsetAddress.utf8.count).bigEndianBytes)
        payload.append(contentsOf: UInt16(headsetName.utf8.count).bigEndianBytes)
        payload.append(contentsOf: headsetAddress.utf8)
        payload.append(contentsOf: headsetName.utf8)
        payload.append(contentsOf: [0, 0, 0, 0])

        XCTAssertEqual(
            IOBluetoothTransport.helperParsePairedDevicePayload(payload),
            [
                BluetoothDevice(id: address, name: name, address: address),
                BluetoothDevice(
                    id: headsetAddress,
                    name: headsetName,
                    address: headsetAddress
                ),
            ]
        )
    }

    func testBluetoothHelperRejectsMalformedAndTrailingPayloads() {
        let address = "00-11-22-33-44-55"
        let name = "TH-D75"
        var truncated = Array("THD75BT-READY-v1".utf8)
        truncated.append(contentsOf: UInt16(address.utf8.count).bigEndianBytes)
        truncated.append(contentsOf: UInt16(name.utf8.count).bigEndianBytes)
        truncated.append(contentsOf: address.utf8.dropLast())
        XCTAssertNil(IOBluetoothTransport.helperParsePairedDevicePayload(truncated))

        var trailing = Array("THD75BT-READY-v1".utf8)
        trailing.append(contentsOf: [0, 0, 0, 0, 0xAA])
        XCTAssertNil(IOBluetoothTransport.helperParsePairedDevicePayload(trailing))
    }

    func testLiveBluetoothHelperReturnsExpectedPairedRadio() throws {
        let environment = ProcessInfo.processInfo.environment
        guard environment["LODESTAR_HARDWARE_BLUETOOTH_ENUMERATION"] == "1" else {
            throw XCTSkip("Set LODESTAR_HARDWARE_BLUETOOTH_ENUMERATION=1 for live discovery")
        }
        guard let expectedAddress = environment["LODESTAR_HARDWARE_BLUETOOTH_ADDRESS"],
              !expectedAddress.isEmpty else {
            return XCTFail(
                "Set LODESTAR_HARDWARE_BLUETOOTH_ADDRESS to the paired radio address."
            )
        }
        guard let devices = IOBluetoothTransport.helperPairedDevicesForTesting() else {
            return XCTFail(
                "The signed paired-device helper did not return a complete payload."
            )
        }
        XCTAssertTrue(
            devices.contains {
                $0.address.caseInsensitiveCompare(expectedAddress) == .orderedSame
            },
            "The complete helper payload did not contain the expected paired radio."
        )
    }

    func testBluetoothTransportRejectsNameInAddressFieldBeforeSpawningHelper() async {
        let named = BluetoothDevice(
            id: "TH-D75",
            name: "TH-D75",
            address: "TH-D75"
        )
        let transport = IOBluetoothTransport(device: named)

        do {
            try await transport.open()
            XCTFail("a display name must never reach the exact-address open path")
        } catch let error as RadioTransportError {
            guard case .openFailed(let reason) = error else {
                return XCTFail("expected openFailed, got \(error)")
            }
            XCTAssertTrue(reason.contains("exact address"), reason)
        } catch {
            XCTFail("unexpected error: \(error)")
        }
    }

    func testBluetoothCancelledReadPreservesFollowingBytes() async throws {
        let transport = IOBluetoothTransport.helperTestTransport()
        try await transport.open()
        let cancelledRead = Task {
            try await transport.read(maxBytes: 64)
        }
        try await Task.sleep(for: .milliseconds(20))
        cancelledRead.cancel()
        let cancelledResult = try await cancelledRead.value
        XCTAssertEqual(cancelledResult, [])

        let payload = Array("FV\r".utf8)
        try await transport.write(payload)
        let preserved = try await transport.read(maxBytes: 64)
        XCTAssertEqual(preserved, payload)
        await transport.close()
    }

    func testBluetoothCancelledBackpressuredWritePoisonsHelper() async throws {
        let transport = IOBluetoothTransport.helperTestTransport(wedged: true)
        try await transport.open()
        let write = Task {
            try await transport.write([UInt8](repeating: 0x41, count: 1_048_576))
        }
        try await Task.sleep(for: .milliseconds(20))
        write.cancel()
        do {
            try await write.value
            XCTFail("cancelled partial helper write must fail")
        } catch {
            // Expected: cancellation destroys the uncertain byte stream.
        }

        let state = await transport.state
        guard case .failed = state else {
            return XCTFail("cancelled write must poison the transport")
        }
        do {
            try await transport.write([0x42])
            XCTFail("a poisoned helper must not be reusable")
        } catch let error as RadioTransportError {
            XCTAssertEqual(error, .notConnected)
        }
        await transport.close()
    }
    #endif

    func testScriptedResponseAndWriteCapture() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        // MMDVM GetVersion probe: 0xE0 0x03 0x00 to a typed v1 response.
        let version = [0xE0, 0x0E, 0x00, 0x01] + Array("MMDVM 2018".utf8)
        await mock.script(response: version, for: [0xE0, 0x03, 0x00])
        try await mock.write([0xE0, 0x03, 0x00])
        let reply = try await mock.read(maxBytes: 16)
        XCTAssertEqual(reply, version)
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

    func testModeProbeRejectsTruncatedMmdvmPrefix() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        await mock.script(
            response: [0xE0],
            for: Array(mmdvmGetVersionProbe())
        )

        let mode = try await RadioModeProber(
            transport: mock,
            timeout: .milliseconds(50)
        ).probe()

        XCTAssertEqual(mode, .unrecognized(firstByte: 0xE0))
    }

    func testModeProbeRequiresGetVersionCommandInCompleteMmdvmFrame() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        await mock.script(
            response: [0xE0, 0x03, 0x01],
            for: Array(mmdvmGetVersionProbe())
        )

        let mode = try await RadioModeProber(transport: mock).probe()

        XCTAssertEqual(mode, .unrecognized(firstByte: 0xE0))
    }

    func testModeProbeRejectsEchoedGetVersionRequest() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        let probe = Array(mmdvmGetVersionProbe())
        await mock.script(response: probe, for: probe)

        let mode = try await RadioModeProber(transport: mock).probe()

        XCTAssertEqual(mode, .unrecognized(firstByte: 0xE0))
    }

    func testModeProbeLeavesCoalescedFollowingFrameUnread() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        let version = [0xE0, 0x0E, 0x00, 0x01] + Array("MMDVM 2018".utf8)
        let following: [UInt8] = [0xE0, 0x03, 0x01]
        await mock.script(
            response: version + following,
            for: Array(mmdvmGetVersionProbe())
        )

        let mode = try await RadioModeProber(transport: mock).probe()
        let unread = try await mock.read(maxBytes: following.count)

        XCTAssertEqual(mode, .mmdvm)
        XCTAssertEqual(unread, following)
    }
}

private extension UInt16 {
    var bigEndianBytes: [UInt8] {
        [UInt8(self >> 8), UInt8(self & 0xFF)]
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

    private func assertReflectorTerminalModeUpdate(
        selecting interface: PcOutputInterface,
        from storedInterface: UInt8,
        file: StaticString = #filePath,
        line: UInt = #line
    ) async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        let session = McpSession(transport: mock)

        let interfacePage: UInt16 = 0x0010
        let modePage: UInt16 = 0x001C
        let settings = reflectorTerminalSettings(interface: interface)
        var currentInterface = [UInt8](repeating: 0, count: 256)
        currentInterface[0x93] = storedInterface
        var expectedInterface = currentInterface
        expectedInterface[0x93] = settings.interfaceValue
        let currentMode = [UInt8](repeating: 0, count: 256)
        var expectedMode = currentMode
        expectedMode[0xA0] = 1

        let interfaceRead = Array(buildReadPageCmd(page: interfacePage))
        let modeRead = Array(buildReadPageCmd(page: modePage))
        let interfaceWrite = try pageFrame(interfacePage, data: expectedInterface)
        let modeWrite = try pageFrame(modePage, data: expectedMode)

        await mock.script(
            response: Array("0M\r".utf8),
            for: Array(buildEnterCmd())
        )
        await mock.scriptSequence(
            responses: [
                try pageFrame(interfacePage, data: currentInterface),
                try pageFrame(interfacePage, data: expectedInterface),
            ],
            for: interfaceRead
        )
        await mock.scriptSequence(
            responses: [
                try pageFrame(modePage, data: currentMode),
                try pageFrame(modePage, data: expectedMode),
            ],
            for: modeRead
        )
        await mock.script(response: [0x06], for: interfaceWrite)
        await mock.script(response: [0x06], for: modeWrite)
        await mock.scriptSequence(
            responses: [[0x06], [0x06], [0x06], [0x06]],
            for: [0x06]
        )
        await mock.script(response: [0x06], for: [UInt8(ascii: "E")])

        try await session.enableReflectorTerminalMode(on: interface)

        let written = await mock.writtenBytes()
        XCTAssertEqual(
            written,
            [
                Array("ID\r".utf8),
                Array("FV\r".utf8),
                Array(buildEnterCmd()),
                interfaceRead,
                [0x06],
                modeRead,
                [0x06],
                interfaceWrite,
                interfaceRead,
                [0x06],
                modeWrite,
                modeRead,
                [0x06],
                [UInt8(ascii: "E")],
            ],
            "both settings must be read and read-back verified in one MCP session",
            file: file,
            line: line
        )
        XCTAssertEqual(
            written.filter { $0 == Array(buildEnterCmd()) }.count,
            1,
            file: file,
            line: line
        )
        XCTAssertEqual(
            written.filter { $0 == [UInt8(ascii: "E")] }.count,
            1,
            file: file,
            line: line
        )
    }

    func testReflectorTerminalModeSelectsBluetoothBeforeEnablingMode() async throws {
        let bluetooth = reflectorTerminalSettings(interface: .bluetooth)
        let usb = reflectorTerminalSettings(interface: .usb)
        XCTAssertEqual(bluetooth.interfaceValue, 1)
        try await assertReflectorTerminalModeUpdate(
            selecting: .bluetooth,
            from: usb.interfaceValue
        )
    }

    func testReflectorTerminalModeSelectsUsbBeforeEnablingMode() async throws {
        let bluetooth = reflectorTerminalSettings(interface: .bluetooth)
        let usb = reflectorTerminalSettings(interface: .usb)
        XCTAssertEqual(usb.interfaceValue, 0)
        try await assertReflectorTerminalModeUpdate(
            selecting: .usb,
            from: bluetooth.interfaceValue
        )
    }

    func testReflectorTerminalModeRouteOnlyWritesOnlyMenu985Page() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        let session = McpSession(transport: mock)
        let settings = reflectorTerminalSettings(interface: .bluetooth)
        let interfacePage = pageOf(offset: settings.interfaceOffset)
        let modePage = pageOf(offset: settings.modeOffset)
        let interfaceRead = Array(buildReadPageCmd(page: interfacePage))
        let modeRead = Array(buildReadPageCmd(page: modePage))

        var currentInterface = [UInt8](repeating: 0, count: 256)
        var expectedInterface = currentInterface
        expectedInterface[Int(byteOf(offset: settings.interfaceOffset))] = settings.interfaceValue
        var currentMode = [UInt8](repeating: 0, count: 256)
        currentMode[Int(byteOf(offset: settings.modeOffset))] = settings.modeValue
        let interfaceWrite = try pageFrame(interfacePage, data: expectedInterface)

        await mock.script(response: Array("0M\r".utf8), for: Array(buildEnterCmd()))
        await mock.scriptSequence(
            responses: [
                try pageFrame(interfacePage, data: currentInterface),
                try pageFrame(interfacePage, data: expectedInterface),
            ],
            for: interfaceRead
        )
        await mock.script(
            response: try pageFrame(modePage, data: currentMode),
            for: modeRead
        )
        await mock.script(response: [0x06], for: interfaceWrite)
        await mock.scriptSequence(responses: [[0x06], [0x06], [0x06]], for: [0x06])
        await mock.script(response: [0x06], for: [UInt8(ascii: "E")])

        try await session.enableReflectorTerminalMode(on: .bluetooth)

        let writes = await mock.writtenBytes()
        XCTAssertEqual(
            writes,
            [
                Array("ID\r".utf8), Array("FV\r".utf8), Array(buildEnterCmd()),
                interfaceRead, [0x06], modeRead, [0x06], interfaceWrite,
                interfaceRead, [0x06], [UInt8(ascii: "E")],
            ]
        )
    }

    func testReflectorTerminalModeModeOnlyWritesOnlyMenu650Page() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        let session = McpSession(transport: mock)
        let settings = reflectorTerminalSettings(interface: .usb)
        let interfacePage = pageOf(offset: settings.interfaceOffset)
        let modePage = pageOf(offset: settings.modeOffset)
        let interfaceRead = Array(buildReadPageCmd(page: interfacePage))
        let modeRead = Array(buildReadPageCmd(page: modePage))

        var currentInterface = [UInt8](repeating: 0, count: 256)
        currentInterface[Int(byteOf(offset: settings.interfaceOffset))] = settings.interfaceValue
        let currentMode = [UInt8](repeating: 0, count: 256)
        var expectedMode = currentMode
        expectedMode[Int(byteOf(offset: settings.modeOffset))] = settings.modeValue
        let modeWrite = try pageFrame(modePage, data: expectedMode)

        await mock.script(response: Array("0M\r".utf8), for: Array(buildEnterCmd()))
        await mock.script(
            response: try pageFrame(interfacePage, data: currentInterface),
            for: interfaceRead
        )
        await mock.scriptSequence(
            responses: [
                try pageFrame(modePage, data: currentMode),
                try pageFrame(modePage, data: expectedMode),
            ],
            for: modeRead
        )
        await mock.script(response: [0x06], for: modeWrite)
        await mock.scriptSequence(responses: [[0x06], [0x06], [0x06]], for: [0x06])
        await mock.script(response: [0x06], for: [UInt8(ascii: "E")])

        try await session.enableReflectorTerminalMode(on: .usb)

        let writes = await mock.writtenBytes()
        XCTAssertEqual(
            writes,
            [
                Array("ID\r".utf8), Array("FV\r".utf8), Array(buildEnterCmd()),
                interfaceRead, [0x06], modeRead, [0x06], modeWrite,
                modeRead, [0x06], [UInt8(ascii: "E")],
            ]
        )
    }

    func testReflectorTerminalModeUnchangedWritesNoPage() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        let session = McpSession(transport: mock)
        let settings = reflectorTerminalSettings(interface: .bluetooth)
        let interfacePage = pageOf(offset: settings.interfaceOffset)
        let modePage = pageOf(offset: settings.modeOffset)
        let interfaceRead = Array(buildReadPageCmd(page: interfacePage))
        let modeRead = Array(buildReadPageCmd(page: modePage))

        var currentInterface = [UInt8](repeating: 0, count: 256)
        currentInterface[Int(byteOf(offset: settings.interfaceOffset))] = settings.interfaceValue
        var currentMode = [UInt8](repeating: 0, count: 256)
        currentMode[Int(byteOf(offset: settings.modeOffset))] = settings.modeValue

        await mock.script(response: Array("0M\r".utf8), for: Array(buildEnterCmd()))
        await mock.script(
            response: try pageFrame(interfacePage, data: currentInterface),
            for: interfaceRead
        )
        await mock.script(
            response: try pageFrame(modePage, data: currentMode),
            for: modeRead
        )
        await mock.scriptSequence(responses: [[0x06], [0x06]], for: [0x06])
        await mock.script(response: [0x06], for: [UInt8(ascii: "E")])

        try await session.enableReflectorTerminalMode(on: .bluetooth)

        let writes = await mock.writtenBytes()
        XCTAssertEqual(
            writes,
            [
                Array("ID\r".utf8), Array("FV\r".utf8), Array(buildEnterCmd()),
                interfaceRead, [0x06], modeRead, [0x06], [UInt8(ascii: "E")],
            ]
        )
        XCTAssertFalse(writes.contains { $0.first == UInt8(ascii: "W") })
    }

    func testReflectorTerminalModePartialFailureReportsPossiblyAndVerifiedPages() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        let session = McpSession(transport: mock)
        let settings = reflectorTerminalSettings(interface: .bluetooth)
        let interfacePage = pageOf(offset: settings.interfaceOffset)
        let modePage = pageOf(offset: settings.modeOffset)
        let interfaceRead = Array(buildReadPageCmd(page: interfacePage))
        let modeRead = Array(buildReadPageCmd(page: modePage))

        let currentInterface = [UInt8](repeating: 0, count: 256)
        var expectedInterface = currentInterface
        expectedInterface[Int(byteOf(offset: settings.interfaceOffset))] = settings.interfaceValue
        let currentMode = [UInt8](repeating: 0, count: 256)
        var expectedMode = currentMode
        expectedMode[Int(byteOf(offset: settings.modeOffset))] = settings.modeValue
        let interfaceWrite = try pageFrame(interfacePage, data: expectedInterface)
        let modeWrite = try pageFrame(modePage, data: expectedMode)

        await mock.script(response: Array("0M\r".utf8), for: Array(buildEnterCmd()))
        await mock.scriptSequence(
            responses: [
                try pageFrame(interfacePage, data: currentInterface),
                try pageFrame(interfacePage, data: expectedInterface),
            ],
            for: interfaceRead
        )
        await mock.script(
            response: try pageFrame(modePage, data: currentMode),
            for: modeRead
        )
        await mock.script(response: [0x06], for: interfaceWrite)
        await mock.script(response: [0x15], for: modeWrite)
        await mock.scriptSequence(responses: [[0x06], [0x06], [0x06]], for: [0x06])
        await mock.script(response: [0x06], for: [UInt8(ascii: "E")])

        do {
            try await session.enableReflectorTerminalMode(on: .bluetooth)
            XCTFail("second page failure must not report success")
        } catch let error as McpOrchestratorError {
            guard case .terminalSettingsUpdateFailed(
                _, let possiblyWrittenPages, let verifiedWrittenPages
            ) = error else {
                return XCTFail("expected structured terminal update error, got \(error)")
            }
            XCTAssertEqual(possiblyWrittenPages, [interfacePage, modePage])
            XCTAssertEqual(verifiedWrittenPages, [interfacePage])
            XCTAssertTrue(error.localizedDescription.contains("0x10, 0x1C"))
        }
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
            XCTAssertEqual(
                error,
                .unsupportedFirmware(
                    actual: "1.04",
                    accepted: ["1.03", "1.03.000", "1.03.AZM"]
                )
            )
        }

        let written = await mock.writtenBytes()
        XCTAssertEqual(written, [Array("ID\r".utf8), Array("FV\r".utf8)])
        let mustDetach = await session.requiresTransportDetach()
        XCTAssertFalse(mustDetach, "no MCP entry byte was sent")
        let state = await mock.state
        XCTAssertEqual(state, .connected)
    }

    func testQualificationTimeoutClosesAndTerminalizesSession() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        await mock.script(response: [], for: Array("ID\r".utf8))
        let session = McpSession(
            transport: mock,
            pageReadTimeoutSeconds: 5,
            catTimeoutSeconds: 0.03
        )

        do {
            try await session.enterProgramming()
            XCTFail("an unproved CAT exchange must fail")
        } catch let error as McpOrchestratorError {
            guard case .catResponseTimeout(let command, _) = error else {
                return XCTFail("expected CAT timeout, got \(error)")
            }
            XCTAssertEqual(command, "identify")
        }

        let requiresDetach = await session.requiresTransportDetach()
        let transportState = await mock.state
        XCTAssertTrue(requiresDetach)
        XCTAssertEqual(transportState, .disconnected)
        do {
            try await session.enterProgramming()
            XCTFail("an ambiguous qualification must make the session terminal")
        } catch let error as McpOrchestratorError {
            guard case .invalidPhase(_, _, let actual) = error else {
                return XCTFail("expected terminal phase, got \(error)")
            }
            XCTAssertEqual(actual, "terminal")
        }
        let written = await mock.writtenBytes()
        XCTAssertEqual(
            written.filter { $0 == Array("ID\r".utf8) }.count,
            1
        )
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

    func testEntryAcceptsAzimuthFirmwareIdentity() async throws {
        let mock = MockRadioTransport()
        try await mock.open()
        await mock.script(
            response: Array("FV 1.03.AZM\r".utf8),
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
