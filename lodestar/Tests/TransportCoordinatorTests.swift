// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Lodestar

@MainActor
final class TransportCoordinatorTests: XCTestCase {
    private func pageFrame(_ page: UInt16, data: [UInt8]) throws -> [UInt8] {
        Array(try buildWritePageCmd(page: page, data: Data(data)))
    }

    private func mmdvmVersionResponse() -> [UInt8] {
        [0xE0, 0x0E, 0x00, 0x01] + Array("MMDVM 2018".utf8)
    }

    func testRefreshShowsEveryPairedDeviceWithoutNameFiltering() {
        let radio = BluetoothDevice(
            id: "00-11-22-33-44-55",
            name: "Field Radio",
            address: "00-11-22-33-44-55"
        )
        let headset = BluetoothDevice(
            id: "20-11-22-33-44-55",
            name: "Headphones",
            address: "20-11-22-33-44-55"
        )
        let coordinator = TransportCoordinator()
        coordinator.bluetoothPairedDevicesProvider = { [radio, headset] }

        coordinator.refreshPairedDevices()
        XCTAssertEqual(coordinator.availableDevices, [radio, headset])
    }

    func testConnectOpensOnlySelectedAddressAndRequiresExactCatIdentity() async throws {
        let selected = BluetoothDevice(
            id: "30-11-22-33-44-55",
            name: "Field Radio",
            address: "30-11-22-33-44-55"
        )
        let coordinator = TransportCoordinator()
        let mock = MockRadioTransport(device: selected)
        var openedDevices: [BluetoothDevice] = []
        await mock.script(
            response: Array("?\r".utf8),
            for: Array(mmdvmGetVersionProbe())
        )
        coordinator.transportFactory = { device in
            openedDevices.append(device)
            return mock
        }
        coordinator.select(selected)

        await coordinator.connect()

        XCTAssertEqual(openedDevices, [selected])
        XCTAssertEqual(coordinator.state, .connected)
        XCTAssertEqual(coordinator.radioMode, .cat)
        XCTAssertNotNil(coordinator.relayTransport)
        let writes = await mock.writtenBytes()
        XCTAssertEqual(
            writes,
            [Array(mmdvmGetVersionProbe()), Array("ID\r".utf8)]
        )
    }

    func testConnectRejectsWrongCatModelWithoutPublishingTransport() async throws {
        let candidate = BluetoothDevice(
            id: "30-11-22-33-44-55",
            name: "Serial Device",
            address: "30-11-22-33-44-55"
        )
        let coordinator = TransportCoordinator()
        let mock = MockRadioTransport(device: candidate)
        await mock.script(
            response: Array("?\r".utf8),
            for: Array(mmdvmGetVersionProbe())
        )
        await mock.script(
            response: Array("ID NOT-A-TH-D75\r".utf8),
            for: Array("ID\r".utf8)
        )
        coordinator.transportFactory = { _ in mock }
        coordinator.select(candidate)

        await coordinator.connect()
        let temporaryState = await mock.state

        XCTAssertEqual(temporaryState, .disconnected)
        XCTAssertNil(coordinator.relayTransport)
        guard case .failed(let message) = coordinator.state else {
            return XCTFail("wrong model must fail connection")
        }
        XCTAssertTrue(message.contains("not exact model TH-D75"), message)
    }

    func testPickerCancellationPreventsStaleOpenFromPublishingConnection() async throws {
        let coordinator = TransportCoordinator()
        let mock = MockRadioTransport(openDelayNanoseconds: 500_000_000)
        coordinator.transportFactory = { _ in mock }
        coordinator.select(.mockTHD75)

        let connection = Task { @MainActor in
            await coordinator.connect()
        }
        try await Task.sleep(nanoseconds: 30_000_000)
        coordinator.cancelConnectionAttempt()
        connection.cancel()
        await connection.value

        XCTAssertEqual(coordinator.state, .disconnected)
        XCTAssertNil(coordinator.relayTransport)
        let mockState = await mock.state
        XCTAssertEqual(mockState, .disconnected)
    }

    func testPickerCancellationDuringProtocolProofPreventsStaleConnection() async throws {
        let coordinator = TransportCoordinator()
        let mock = MockRadioTransport(openDelayNanoseconds: 0)
        coordinator.transportFactory = { _ in mock }
        coordinator.select(.mockTHD75)

        let connection = Task { @MainActor in
            await coordinator.connect()
        }
        for _ in 0..<100 {
            let writes = await mock.writtenBytes()
            if writes.contains(Array(mmdvmGetVersionProbe())) { break }
            try await Task.sleep(nanoseconds: 1_000_000)
        }
        coordinator.cancelConnectionAttempt()
        connection.cancel()
        await connection.value

        XCTAssertEqual(coordinator.state, .disconnected)
        XCTAssertEqual(coordinator.radioMode, .unknown)
        XCTAssertNil(coordinator.relayTransport)
        let mockState = await mock.state
        XCTAssertEqual(mockState, .disconnected)
    }

    func testConnectUsesInjectedTransportFactory() async throws {
        let coordinator = TransportCoordinator()
        let mock = MockRadioTransport()
        // connect() runs the MMDVM GetVersion probe. The mock no longer
        // echoes writes, and the prober's timeout can't cancel a blocked
        // read (CheckedContinuation isn't cancellation-aware), so an
        // unanswered probe hangs connect(). Script any non-empty reply so
        // the probe read resumes and connect() finishes.
        await mock.script(response: mmdvmVersionResponse(), for: [0xE0, 0x03, 0x00])
        coordinator.transportFactory = { _ in mock }
        coordinator.select(.mockTHD75)

        await coordinator.connect()

        let state = await mock.state
        XCTAssertEqual(state, .connected, "connect() must open the injected transport")
        XCTAssertNotNil(coordinator.relayTransport, "transport handle must be retained")
    }

    func testUnexpectedDropResetsRadioMode() async throws {
        let coordinator = TransportCoordinator()
        let mock = MockRadioTransport()
        coordinator.transportFactory = { _ in mock }
        coordinator.select(.mockTHD75)
        // Probe gets an MMDVM version response → radioMode == .mmdvm.
        await mock.script(response: mmdvmVersionResponse(), for: [0xE0, 0x03, 0x00])
        await coordinator.connect()
        XCTAssertEqual(coordinator.radioMode, .mmdvm)

        await mock.simulateUnexpectedClose()
        // Let the state-observer task drain the yield.
        try await Task.sleep(nanoseconds: 200_000_000)

        XCTAssertEqual(coordinator.radioMode, .unknown,
                       "BT drop must clear radioMode so the relay reconciler stops wanting relay")
        XCTAssertNil(coordinator.relayTransport, "dead transport must be released")
        XCTAssertEqual(coordinator.state, .disconnected)
    }

    func testUnexpectedDropSchedulesReconnect() async throws {
        let coordinator = TransportCoordinator()
        let first = MockRadioTransport()
        let second = MockRadioTransport()
        var handedOut = 0
        coordinator.transportFactory = { _ in
            handedOut += 1
            return handedOut == 1 ? first : second
        }
        coordinator.reconnectDelaysNs = [50_000_000]   // 50 ms for tests
        coordinator.select(.mockTHD75)
        // Script BOTH probes so the initial connect and the reconnect
        // finish promptly instead of waiting out the 2 s probe timeout;
        // classification (.mmdvm here) is irrelevant to the asserts.
        await first.script(response: mmdvmVersionResponse(), for: [0xE0, 0x03, 0x00])
        await second.script(response: mmdvmVersionResponse(), for: [0xE0, 0x03, 0x00])
        await coordinator.connect()
        await first.simulateUnexpectedClose()
        try await Task.sleep(nanoseconds: 500_000_000)

        XCTAssertEqual(handedOut, 2, "drop must trigger a reconnect attempt")
        XCTAssertEqual(coordinator.state, .connected)
    }

    func testUnexpectedFailureDetachesPoisonedTransportAndAllowsManualReconnect() async throws {
        let coordinator = TransportCoordinator()
        let first = MockRadioTransport()
        let second = MockRadioTransport()
        var handedOut = 0
        coordinator.transportFactory = { _ in
            handedOut += 1
            return handedOut == 1 ? first : second
        }
        // Keep automatic recovery inert so this test exercises the Connect
        // affordance shown by the failed state.
        coordinator.reconnectDelaysNs = []
        coordinator.select(.mockTHD75)
        await first.script(
            response: mmdvmVersionResponse(),
            for: Array(mmdvmGetVersionProbe())
        )
        await second.script(
            response: mmdvmVersionResponse(),
            for: Array(mmdvmGetVersionProbe())
        )
        await coordinator.connect()

        await first.simulateUnexpectedFailure()
        for _ in 0..<100 where coordinator.relayTransport != nil {
            try await Task.sleep(nanoseconds: 1_000_000)
        }

        guard case .failed(let message) = coordinator.state else {
            return XCTFail("helper failure must remain visible, got \(coordinator.state)")
        }
        XCTAssertEqual(message, "Bluetooth helper exited")
        XCTAssertNil(
            coordinator.relayTransport,
            "the poisoned transport must be detached before Connect is shown"
        )

        await coordinator.connect()

        XCTAssertEqual(handedOut, 2)
        XCTAssertEqual(coordinator.state, .connected)
        XCTAssertNotNil(coordinator.relayTransport)
    }

    func testUnexpectedFailureSchedulesReconnect() async throws {
        let coordinator = TransportCoordinator()
        let first = MockRadioTransport()
        let second = MockRadioTransport()
        var handedOut = 0
        coordinator.transportFactory = { _ in
            handedOut += 1
            return handedOut == 1 ? first : second
        }
        coordinator.reconnectDelaysNs = [50_000_000]
        coordinator.select(.mockTHD75)
        await first.script(
            response: mmdvmVersionResponse(),
            for: Array(mmdvmGetVersionProbe())
        )
        await second.script(
            response: mmdvmVersionResponse(),
            for: Array(mmdvmGetVersionProbe())
        )
        await coordinator.connect()

        await first.simulateUnexpectedFailure()
        try await Task.sleep(nanoseconds: 500_000_000)

        XCTAssertEqual(handedOut, 2, "terminal failure must trigger a fresh transport")
        XCTAssertEqual(coordinator.state, .connected)
        XCTAssertNotNil(coordinator.relayTransport)
    }

    func testUserDisconnectDoesNotTripUnexpectedPath() async throws {
        let coordinator = TransportCoordinator()
        let mock = MockRadioTransport()
        var handedOut = 0
        coordinator.transportFactory = { _ in
            handedOut += 1
            return mock
        }
        coordinator.reconnectDelaysNs = [50_000_000]
        coordinator.select(.mockTHD75)
        await mock.script(response: mmdvmVersionResponse(), for: [0xE0, 0x03, 0x00])
        await coordinator.connect()
        await coordinator.disconnect()
        // Give any (wrongly) scheduled reconnect ample time to fire.
        try await Task.sleep(nanoseconds: 300_000_000)
        XCTAssertEqual(handedOut, 1,
                       "user disconnect must not schedule a reconnect")
        XCTAssertEqual(coordinator.state, .disconnected)
        XCTAssertNil(coordinator.relayTransport)
    }

    func testUserDisconnectCancelsPendingReconnect() async throws {
        let coordinator = TransportCoordinator()
        let first = MockRadioTransport()
        var handedOut = 0
        coordinator.transportFactory = { _ in
            handedOut += 1
            return first
        }
        // Long enough that the reconnect is still sleeping when we
        // disconnect, short enough that a wrongly-surviving reconnect
        // fires well inside the assertion grace period.
        coordinator.reconnectDelaysNs = [150_000_000]
        coordinator.select(.mockTHD75)
        await first.script(response: mmdvmVersionResponse(), for: [0xE0, 0x03, 0x00])
        await coordinator.connect()

        // Unexpected drop schedules the reconnect...
        await first.simulateUnexpectedClose()
        try await Task.sleep(nanoseconds: 50_000_000)
        // ...and the user disconnects while it is still pending.
        await coordinator.disconnect()
        try await Task.sleep(nanoseconds: 400_000_000)

        XCTAssertEqual(handedOut, 1,
                       "user disconnect must cancel a pending post-drop reconnect")
        XCTAssertEqual(coordinator.state, .disconnected)
    }

    func testManualConnectCannotReplaceAnOpeningReconnect() async throws {
        let coordinator = TransportCoordinator()
        let first = MockRadioTransport()
        let second = MockRadioTransport()
        let forbiddenThird = MockRadioTransport()
        var handedOut = 0
        coordinator.transportFactory = { _ in
            handedOut += 1
            switch handedOut {
            case 1: return first
            case 2: return second
            default: return forbiddenThird
            }
        }
        coordinator.reconnectDelaysNs = [0]
        coordinator.select(.mockTHD75)
        await first.script(
            response: mmdvmVersionResponse(),
            for: Array(mmdvmGetVersionProbe())
        )
        await second.script(
            response: mmdvmVersionResponse(),
            for: Array(mmdvmGetVersionProbe())
        )
        await coordinator.connect()

        await first.simulateUnexpectedClose()
        for _ in 0..<100 where handedOut < 2 {
            try await Task.sleep(nanoseconds: 1_000_000)
        }
        XCTAssertEqual(handedOut, 2, "precondition: reconnect must have started opening")

        // Cancelling a reconnect is cooperative. A manual connect must not
        // create a third transport while the cancelled open still owns the
        // coordinator transaction lease.
        await coordinator.connect()
        try await Task.sleep(nanoseconds: 50_000_000)

        XCTAssertEqual(
            handedOut,
            2,
            "manual connect must not replace or be clobbered by an in-flight reconnect"
        )
        XCTAssertNil(coordinator.relayTransport)
        let secondState = await second.state
        XCTAssertEqual(secondState, .disconnected)
    }

    func testBackgroundDisconnectsAndForegroundReconnects() async throws {
        let coordinator = TransportCoordinator()
        let first = MockRadioTransport()
        let second = MockRadioTransport()
        var handedOut = 0
        coordinator.transportFactory = { _ in
            handedOut += 1
            return handedOut == 1 ? first : second
        }
        coordinator.select(.mockTHD75)
        await first.script(response: mmdvmVersionResponse(), for: [0xE0, 0x03, 0x00])
        await second.script(response: mmdvmVersionResponse(), for: [0xE0, 0x03, 0x00])
        await coordinator.connect()
        guard case .connected = coordinator.state else {
            return XCTFail("precondition: connect failed, state \(coordinator.state)")
        }

        await coordinator.handleScenePhaseBackground()
        XCTAssertEqual(coordinator.state, .disconnected,
                       "backgrounding must tear down the radio connection")
        XCTAssertNil(coordinator.relayTransport)

        await coordinator.handleScenePhaseActive()
        try await Task.sleep(nanoseconds: 200_000_000)
        XCTAssertEqual(handedOut, 2, "foreground must reconnect after a background teardown")
        let secondState = await second.state
        XCTAssertEqual(secondState, .connected)
        XCTAssertNotNil(coordinator.relayTransport)
    }

    func testBackgroundDuringIdentifyDefersForegroundReconnectUntilLeaseRelease() async throws {
        let coordinator = TransportCoordinator()
        let first = MockRadioTransport(closeDelayNanoseconds: 100_000_000)
        let second = MockRadioTransport()
        var handedOut = 0
        coordinator.transportFactory = { _ in
            handedOut += 1
            return handedOut == 1 ? first : second
        }
        coordinator.select(.mockTHD75)
        await first.script(
            response: Array("?\r".utf8),
            for: Array(mmdvmGetVersionProbe())
        )
        await second.script(
            response: Array("?\r".utf8),
            for: Array(mmdvmGetVersionProbe())
        )
        await coordinator.connect()
        await first.script(response: [], for: Array("ID\r".utf8))

        let identify = Task { @MainActor in
            await coordinator.sendIdentify()
        }
        try await Task.sleep(nanoseconds: 20_000_000)
        XCTAssertTrue(coordinator.isBusy, "precondition: identify must own the I/O lease")

        let background = Task { @MainActor in
            await coordinator.handleScenePhaseBackground()
        }
        try await Task.sleep(nanoseconds: 20_000_000)
        XCTAssertNil(
            coordinator.relayTransport,
            "backgrounding must detach before awaiting a potentially slow close"
        )

        // Foreground arrives while close/read completion still owns the old
        // lease. Reconnect must be remembered rather than silently dropped.
        await coordinator.handleScenePhaseActive()
        await background.value
        await identify.value

        for _ in 0..<200 where handedOut < 2 {
            try await Task.sleep(nanoseconds: 1_000_000)
        }
        XCTAssertEqual(handedOut, 2)
        for _ in 0..<200 where coordinator.state != .connected {
            try await Task.sleep(nanoseconds: 1_000_000)
        }
        XCTAssertEqual(coordinator.state, .connected)
        XCTAssertNotNil(coordinator.relayTransport)
    }

    func testForegroundWithoutPriorConnectionDoesNotConnect() async throws {
        let coordinator = TransportCoordinator()
        var handedOut = 0
        coordinator.transportFactory = { _ in
            handedOut += 1
            return MockRadioTransport()
        }
        await coordinator.handleScenePhaseActive()
        try await Task.sleep(nanoseconds: 100_000_000)
        XCTAssertEqual(handedOut, 0, "no prior connection → no spurious connect")
        XCTAssertEqual(coordinator.state, .disconnected)
    }

    func testBackgroundWhileDisconnectedIsANoOp() async throws {
        let coordinator = TransportCoordinator()
        coordinator.transportFactory = { _ in MockRadioTransport() }
        await coordinator.handleScenePhaseBackground()
        XCTAssertEqual(coordinator.state, .disconnected)
        // A later foreground must not "restore" a connection that never was.
        await coordinator.handleScenePhaseActive()
        try await Task.sleep(nanoseconds: 100_000_000)
        XCTAssertNil(coordinator.relayTransport)
    }

    func testMcpQualificationFailureKeepsProvenCatTransportConnected() async throws {
        let coordinator = TransportCoordinator()
        let mock = MockRadioTransport()
        coordinator.transportFactory = { _ in mock }
        coordinator.select(.mockTHD75)
        await mock.script(
            response: [UInt8(ascii: "?")],
            for: Array(mmdvmGetVersionProbe())
        )
        await coordinator.connect()
        await mock.script(
            response: Array("FV 1.04\r".utf8),
            for: Array("FV\r".utf8)
        )

        await coordinator.enableReflectorTerminalMode()

        XCTAssertEqual(coordinator.state, .connected)
        XCTAssertNotNil(coordinator.relayTransport)
        guard case .failed(let message) = coordinator.mcpStatus else {
            return XCTFail("expected failed MCP status, got \(coordinator.mcpStatus)")
        }
        XCTAssertTrue(message.contains("firmware 1.04"))
        let writes = await mock.writtenBytes()
        XCTAssertFalse(writes.contains(Array(buildEnterCmd())))
    }

    func testMcpRefusesToStartWhileIdentifyOwnsParkedRead() async throws {
        let coordinator = TransportCoordinator()
        let mock = MockRadioTransport()
        coordinator.transportFactory = { _ in mock }
        coordinator.select(.mockTHD75)
        await mock.script(
            response: [UInt8(ascii: "?")],
            for: Array(mmdvmGetVersionProbe())
        )
        await coordinator.connect()
        await mock.script(response: [], for: Array("ID\r".utf8))

        let identify = Task { @MainActor in
            await coordinator.sendIdentify()
        }
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertTrue(coordinator.isBusy, "precondition: identify must own the CAT read")

        await coordinator.enableReflectorTerminalMode()
        guard case .failed(let message) = coordinator.mcpStatus else {
            await mock.push(Array("ID TH-D75\r".utf8))
            await identify.value
            return XCTFail("expected MCP refusal while CAT I/O is active")
        }
        XCTAssertTrue(message.contains("still in progress"), message)

        await mock.push(Array("ID TH-D75\r".utf8))
        await identify.value
        let writes = await mock.writtenBytes()
        XCTAssertFalse(writes.contains(Array("FV\r".utf8)))
        XCTAssertFalse(writes.contains(Array(buildEnterCmd())))
    }

    func testMmdvmModeNeverReceivesCatOrMcpTraffic() async throws {
        let coordinator = TransportCoordinator()
        let mock = MockRadioTransport()
        coordinator.transportFactory = { _ in mock }
        coordinator.select(.mockTHD75)
        await mock.script(
            response: mmdvmVersionResponse(),
            for: Array(mmdvmGetVersionProbe())
        )
        await coordinator.connect()
        XCTAssertEqual(coordinator.radioMode, .mmdvm)

        await coordinator.enableReflectorTerminalMode()

        let writes = await mock.writtenBytes()
        XCTAssertEqual(writes, [Array(mmdvmGetVersionProbe())])
        XCTAssertNotNil(coordinator.relayTransport)
        XCTAssertEqual(coordinator.state, .connected)
    }

    func testOverlappingCoordinatorMcpFlowsCannotCreateTwoSessions() async throws {
        let coordinator = TransportCoordinator()
        let cat = MockRadioTransport()
        let terminal = MockRadioTransport()
        var handedOut = 0
        coordinator.transportFactory = { _ in
            handedOut += 1
            return handedOut == 1 ? cat : terminal
        }
        coordinator.reconnectDelaysNs = []
        coordinator.terminalModePollDelayNs = 0
        coordinator.terminalModeTransitionWindow = .seconds(1)
        coordinator.select(.mockTHD75)
        await cat.script(
            response: [UInt8(ascii: "?")],
            for: Array(mmdvmGetVersionProbe())
        )
        await terminal.script(
            response: mmdvmVersionResponse(),
            for: Array(mmdvmGetVersionProbe())
        )
        await coordinator.connect()

        var interfacePage = [UInt8](repeating: 0, count: 256)
        interfacePage[0x93] = reflectorTerminalSettings(interface: .bluetooth).interfaceValue
        var modePage = [UInt8](repeating: 0, count: 256)
        modePage[0xA0] = 1
        await cat.script(
            response: try pageFrame(0x0010, data: interfacePage),
            for: Array(buildReadPageCmd(page: 0x0010))
        )
        await cat.script(
            response: try pageFrame(0x001C, data: modePage),
            for: Array(buildReadPageCmd(page: 0x001C))
        )
        await cat.scriptSequence(responses: [[0x06], [0x06]], for: [0x06])
        await cat.script(response: [0x06], for: [UInt8(ascii: "E")])

        let first = Task { @MainActor in
            await coordinator.enableReflectorTerminalMode()
        }
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertNil(
            coordinator.relayTransport,
            "MCP must quarantine the transport from the relay"
        )
        await coordinator.enableReflectorTerminalMode()
        await coordinator.probeRadioMode()
        await coordinator.disconnect()
        let stateDuringMcp = await cat.state
        XCTAssertEqual(
            stateDuringMcp,
            .connected,
            "public disconnect must not close a transport owned by MCP cleanup"
        )
        await cat.push(Array("0M\r".utf8))
        await first.value

        let writes = await cat.writtenBytes()
        XCTAssertEqual(
            writes.filter { $0 == Array(buildEnterCmd()) }.count,
            1,
            "coordinator latch must prevent a second McpSession"
        )
        XCTAssertEqual(
            writes.filter { $0 == Array(mmdvmGetVersionProbe()) }.count,
            1,
            "concurrent public probe must not inject bytes during MCP"
        )
        XCTAssertEqual(
            writes.filter { $0 == [UInt8(ascii: "E")] }.count,
            1
        )
        XCTAssertEqual(handedOut, 2)
        XCTAssertEqual(coordinator.state, .connected)
        XCTAssertEqual(coordinator.radioMode, .mmdvm)
        XCTAssertEqual(coordinator.mcpStatus, .succeeded)
    }

    func testTerminalTransitionRejectsEarlyCatAndDerivesUsbRouteFromTransport() async throws {
        let device = BluetoothDevice(
            id: "40-11-22-33-44-55",
            name: "Selected Radio",
            address: "40-11-22-33-44-55"
        )
        let cat = MockRadioTransport(device: device, pcOutputInterface: .usb)
        let earlyCat = MockRadioTransport(device: device, pcOutputInterface: .usb)
        let terminal = MockRadioTransport(device: device, pcOutputInterface: .usb)
        let coordinator = TransportCoordinator()
        var openedDevices: [BluetoothDevice] = []
        var handedOut = 0
        coordinator.transportFactory = { selected in
            openedDevices.append(selected)
            handedOut += 1
            switch handedOut {
            case 1: return cat
            case 2: return earlyCat
            default: return terminal
            }
        }
        coordinator.terminalModePollDelayNs = 0
        coordinator.terminalModeTransitionWindow = .seconds(1)
        coordinator.select(device)

        await cat.script(
            response: Array("?\r".utf8),
            for: Array(mmdvmGetVersionProbe())
        )
        await coordinator.connect()

        let settings = reflectorTerminalSettings(interface: .usb)
        let interfacePage = pageOf(offset: settings.interfaceOffset)
        let modePage = pageOf(offset: settings.modeOffset)
        let interfaceRead = Array(buildReadPageCmd(page: interfacePage))
        let modeRead = Array(buildReadPageCmd(page: modePage))
        var currentInterface = [UInt8](repeating: 0, count: 256)
        currentInterface[Int(byteOf(offset: settings.interfaceOffset))] =
            reflectorTerminalSettings(interface: .bluetooth).interfaceValue
        var expectedInterface = currentInterface
        expectedInterface[Int(byteOf(offset: settings.interfaceOffset))] =
            settings.interfaceValue
        var currentMode = [UInt8](repeating: 0, count: 256)
        currentMode[Int(byteOf(offset: settings.modeOffset))] = settings.modeValue
        let interfaceWrite = try pageFrame(interfacePage, data: expectedInterface)

        await cat.script(response: Array("0M\r".utf8), for: Array(buildEnterCmd()))
        await cat.scriptSequence(
            responses: [
                try pageFrame(interfacePage, data: currentInterface),
                try pageFrame(interfacePage, data: expectedInterface),
            ],
            for: interfaceRead
        )
        await cat.script(
            response: try pageFrame(modePage, data: currentMode),
            for: modeRead
        )
        await cat.script(response: [0x06], for: interfaceWrite)
        await cat.scriptSequence(responses: [[0x06], [0x06], [0x06]], for: [0x06])
        await cat.script(response: [0x06], for: [UInt8(ascii: "E")])
        await earlyCat.script(
            response: Array("?\r".utf8),
            for: Array(mmdvmGetVersionProbe())
        )
        await terminal.script(
            response: mmdvmVersionResponse(),
            for: Array(mmdvmGetVersionProbe())
        )

        await coordinator.enableReflectorTerminalMode()

        XCTAssertEqual(openedDevices, [device, device, device])
        XCTAssertEqual(handedOut, 3)
        XCTAssertEqual(coordinator.state, .connected)
        XCTAssertEqual(coordinator.radioMode, .mmdvm)
        XCTAssertEqual(coordinator.mcpStatus, .succeeded)
        XCTAssertNotNil(coordinator.relayTransport)
        let initialState = await cat.state
        let earlyState = await earlyCat.state
        let terminalState = await terminal.state
        let earlyWrites = await earlyCat.writtenBytes()
        let terminalWrites = await terminal.writtenBytes()
        XCTAssertEqual(initialState, .disconnected)
        XCTAssertEqual(earlyState, .disconnected)
        XCTAssertEqual(terminalState, .connected)

        let catWrites = await cat.writtenBytes()
        XCTAssertTrue(catWrites.contains(interfaceWrite))
        XCTAssertEqual(catWrites.filter { $0.first == UInt8(ascii: "W") }.count, 1)
        XCTAssertEqual(
            earlyWrites,
            [Array(mmdvmGetVersionProbe())],
            "early CAT is retryable and must not be mistaken for transition success"
        )
        XCTAssertEqual(
            terminalWrites,
            [Array(mmdvmGetVersionProbe())]
        )
    }

    func testAmbiguousMcpExitDetachesTransportAndRequiresPowerCycle() async throws {
        let coordinator = TransportCoordinator()
        let mock = MockRadioTransport()
        coordinator.transportFactory = { _ in mock }
        coordinator.reconnectDelaysNs = []
        coordinator.select(.mockTHD75)
        await mock.script(
            response: [UInt8(ascii: "?")],
            for: Array(mmdvmGetVersionProbe())
        )
        await coordinator.connect()

        await mock.script(
            response: Array("0M\r".utf8),
            for: Array(buildEnterCmd())
        )
        var interfacePage = [UInt8](repeating: 0, count: 256)
        interfacePage[0x93] = reflectorTerminalSettings(interface: .bluetooth).interfaceValue
        var modePage = [UInt8](repeating: 0, count: 256)
        modePage[0xA0] = 1
        await mock.script(
            response: try pageFrame(0x0010, data: interfacePage),
            for: Array(buildReadPageCmd(page: 0x0010))
        )
        await mock.script(
            response: try pageFrame(0x001C, data: modePage),
            for: Array(buildReadPageCmd(page: 0x001C))
        )
        await mock.scriptSequence(responses: [[0x06], [0x06]], for: [0x06])
        await mock.script(response: [0x15], for: [UInt8(ascii: "E")])

        await coordinator.enableReflectorTerminalMode()

        XCTAssertEqual(coordinator.state, .disconnected)
        XCTAssertNil(coordinator.relayTransport)
        guard case .failed(let message) = coordinator.mcpStatus else {
            return XCTFail("expected failed MCP status, got \(coordinator.mcpStatus)")
        }
        XCTAssertTrue(message.contains("power-cycle"), message)
        let writes = await mock.writtenBytes()
        XCTAssertEqual(
            writes.filter { $0 == [UInt8(ascii: "E")] }.count,
            1,
            "coordinator must not retry an ambiguous exit"
        )
    }

    /// Mock whose open() always throws, for failure-path tests.
    private struct FailingTransport: RadioTransport {
        let device: BluetoothDevice = .mockTHD75
        let pcOutputInterface: PcOutputInterface = .bluetooth
        var state: RadioTransportState { get async { .failed(message: "boom") } }
        var stateStream: AsyncStream<RadioTransportState> { AsyncStream { $0.finish() } }
        func open() async throws { throw RadioTransportError.openFailed(reason: "boom") }
        func close() async {}
        func write(_ bytes: [UInt8]) async throws { throw RadioTransportError.notConnected }
        func read(maxBytes: Int) async throws -> [UInt8] { throw RadioTransportError.notConnected }
    }

    func testConnectFailureReleasesTransport() async throws {
        let coordinator = TransportCoordinator()
        coordinator.transportFactory = { _ in FailingTransport() }
        coordinator.select(.mockTHD75)
        await coordinator.connect()
        guard case .failed = coordinator.state else {
            return XCTFail("expected .failed, got \(coordinator.state)")
        }
        XCTAssertNil(coordinator.relayTransport,
                     "failed connect must not leave a dangling transport")
    }
}
