// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Azimuth

@MainActor
final class AzimuthRadioEndpointSelectionTests: XCTestCase {
    private let usb = RadioEndpoint(
        id: "usb:radio-a",
        name: "TH-D75 C3C10368",
        transport: .usb,
        detail: "/dev/cu.usbmodem101"
    )
    private let bluetooth = RadioEndpoint(
        id: "bluetooth:00-11-22-33-44-55",
        name: "TH-D75",
        transport: .bluetooth,
        detail: "00-11-22-33-44-55"
    )

    private let unrelatedBluetooth = RadioEndpoint(
        id: "bluetooth:66-77-88-99-AA-BB",
        name: "42\" OLED",
        transport: .bluetooth,
        detail: "66-77-88-99-AA-BB"
    )

    func testBluetoothOnlyInitialSelectionPrefersTheStockRadioName() {
        let selector = EndpointTestSelector(
            initialEndpoints: [unrelatedBluetooth, bluetooth]
        )
        let model = makeModel(selector: selector)

        XCTAssertEqual(
            model.radioEndpoints,
            [unrelatedBluetooth, bluetooth]
        )
        XCTAssertEqual(model.selectedRadioEndpointID, bluetooth.id)
    }

    func testBluetoothOnlyRefreshPrefersTheStockRadioNameWithoutASelection() async {
        let lowercaseRadio = RadioEndpoint(
            id: bluetooth.id,
            name: "th-d75",
            transport: .bluetooth,
            detail: bluetooth.detail
        )
        let selector = EndpointTestSelector(initialEndpoints: [])
        selector.nextRefresh = .success([unrelatedBluetooth, lowercaseRadio])
        let model = makeModel(selector: selector)

        await model.refreshRadioEndpoints().value

        XCTAssertEqual(
            model.radioEndpoints,
            [unrelatedBluetooth, lowercaseRadio]
        )
        XCTAssertEqual(model.selectedRadioEndpointID, lowercaseRadio.id)
    }

    func testRefreshPreservesAnExplicitUnrelatedBluetoothSelection() async {
        let selector = EndpointTestSelector(
            initialEndpoints: [unrelatedBluetooth, bluetooth]
        )
        let model = makeModel(selector: selector)
        model.selectRadioEndpoint(id: unrelatedBluetooth.id)
        selector.nextRefresh = .success([unrelatedBluetooth, bluetooth])

        await model.refreshRadioEndpoints().value

        XCTAssertEqual(model.selectedRadioEndpointID, unrelatedBluetooth.id)
    }

    func testRefreshReplacesAnAutomaticUnrelatedSelectionWhenTheRadioAppears() async {
        let selector = EndpointTestSelector(
            initialEndpoints: [unrelatedBluetooth]
        )
        let model = makeModel(selector: selector)
        XCTAssertEqual(model.selectedRadioEndpointID, unrelatedBluetooth.id)
        selector.nextRefresh = .success([unrelatedBluetooth, bluetooth])

        await model.refreshRadioEndpoints().value

        XCTAssertEqual(model.selectedRadioEndpointID, bluetooth.id)
    }

    func testBluetoothOnlySelectionFallsBackToTheFirstDeviceWithoutStockName() {
        let second = RadioEndpoint(
            id: "bluetooth:CC-DD-EE-FF-00-11",
            name: "Custom Radio Name",
            transport: .bluetooth,
            detail: "CC-DD-EE-FF-00-11"
        )
        let selector = EndpointTestSelector(
            initialEndpoints: [unrelatedBluetooth, second]
        )
        let model = makeModel(selector: selector)

        XCTAssertEqual(model.selectedRadioEndpointID, unrelatedBluetooth.id)
    }

    func testRefreshPreservesASelectedStableEndpoint() async {
        let selector = EndpointTestSelector(initialEndpoints: [usb, bluetooth])
        let model = makeModel(selector: selector)
        model.selectRadioEndpoint(id: bluetooth.id)
        selector.nextRefresh = .success([usb, bluetooth])

        await model.refreshRadioEndpoints().value

        XCTAssertEqual(model.radioEndpointRefreshState, .ready)
        XCTAssertEqual(model.radioEndpoints, [usb, bluetooth])
        XCTAssertEqual(model.selectedRadioEndpointID, bluetooth.id)
        XCTAssertEqual(model.selectedRadioEndpoint, bluetooth)
    }

    func testRefreshSelectsTheFirstEndpointWhenThePreviousOneDisappears() async {
        let selector = EndpointTestSelector(initialEndpoints: [usb, bluetooth])
        let model = makeModel(selector: selector)
        model.selectRadioEndpoint(id: bluetooth.id)
        selector.nextRefresh = .success([usb])

        await model.refreshRadioEndpoints().value

        XCTAssertEqual(model.radioEndpoints, [usb])
        XCTAssertEqual(model.selectedRadioEndpointID, usb.id)
    }

    func testFailedRefreshPreservesTheLastUsableListAndSelection() async {
        let selector = EndpointTestSelector(initialEndpoints: [usb, bluetooth])
        let model = makeModel(selector: selector)
        model.selectRadioEndpoint(id: bluetooth.id)
        selector.nextRefresh = .failure(EndpointTestError.discoveryFailed)

        await model.refreshRadioEndpoints().value

        XCTAssertEqual(
            model.radioEndpointRefreshState,
            .failed(message: "Bluetooth discovery failed.")
        )
        XCTAssertEqual(model.radioEndpoints, [usb, bluetooth])
        XCTAssertEqual(model.selectedRadioEndpointID, bluetooth.id)
    }

    func testPartialRefreshPublishesFreshUSBAndBluetoothWarning() async {
        let selector = EndpointTestSelector(initialEndpoints: [bluetooth])
        let model = makeModel(selector: selector)
        selector.nextRefresh = .success([usb])
        selector.nextWarning = "Bluetooth connections unavailable: permission denied."

        await model.refreshRadioEndpoints().value

        XCTAssertEqual(model.radioEndpointRefreshState, .ready)
        XCTAssertEqual(model.radioEndpoints, [usb])
        XCTAssertEqual(model.selectedRadioEndpointID, usb.id)
        XCTAssertEqual(
            model.radioEndpointDiscoveryWarning,
            "Bluetooth connections unavailable: permission denied."
        )
        XCTAssertNil(model.pairedBluetoothDeviceCount)
        XCTAssertNil(model.radioEndpointRefreshError)
    }

    func testDuplicateRefreshSnapshotFailsWithoutChangingSelection() async {
        let selector = EndpointTestSelector(initialEndpoints: [usb, bluetooth])
        let model = makeModel(selector: selector)
        model.selectRadioEndpoint(id: bluetooth.id)
        selector.nextRefresh = .success([usb, usb])

        await model.refreshRadioEndpoints().value

        guard case .failed(let message) = model.radioEndpointRefreshState else {
            return XCTFail("Duplicate endpoints must fail the refresh.")
        }
        XCTAssertTrue(message.contains(usb.id))
        XCTAssertEqual(model.radioEndpoints, [usb, bluetooth])
        XCTAssertEqual(model.selectedRadioEndpointID, bluetooth.id)
    }

    func testConnectRoutesTheSelectedEndpointBeforeOpeningTheController() async {
        let events = EndpointEventRecorder()
        let selector = EndpointTestSelector(
            initialEndpoints: [usb, bluetooth],
            events: events
        )
        let controller = EndpointTestRadioController(events: events)
        let model = makeModel(controller: controller, selector: selector)
        model.selectRadioEndpoint(id: bluetooth.id)

        await model.connectRadio()

        XCTAssertEqual(events.values, ["select:\(bluetooth.id)", "connect"])
        XCTAssertEqual(selector.selectedEndpointIDs, [bluetooth.id])
        XCTAssertEqual(controller.connectCallCount, 1)
        XCTAssertTrue(model.radioState.connection.isConnected)
        XCTAssertNil(model.operationError)
    }

    func testSelectionFailurePreventsAnyRadioOpen() async {
        let selector = EndpointTestSelector(initialEndpoints: [usb, bluetooth])
        selector.selectionError = EndpointTestError.selectionFailed
        let controller = EndpointTestRadioController()
        let model = makeModel(controller: controller, selector: selector)
        model.selectRadioEndpoint(id: bluetooth.id)

        await model.connectRadio()

        XCTAssertEqual(selector.selectedEndpointIDs, [bluetooth.id])
        XCTAssertEqual(controller.connectCallCount, 0)
        XCTAssertEqual(model.operationError, "The selected connection could not be prepared.")
        XCTAssertFalse(model.radioState.connection.isConnected)
    }

    func testVerifiedResolvedBluetoothEndpointIsAppendedAndSelected() async {
        let resolved = RadioEndpoint(
            id: "bluetooth:AA:BB:CC:DD:EE:FF",
            name: "Field TH-D75",
            transport: .bluetooth,
            detail: "AA-BB-CC-DD-EE-FF"
        )
        let selector = EndpointTestSelector(initialEndpoints: [usb, bluetooth])
        selector.resolvedEndpoint = resolved
        let controller = EndpointTestRadioController()
        let model = makeModel(controller: controller, selector: selector)

        await model.connectRadio()

        XCTAssertTrue(model.radioState.connection.isConnected)
        XCTAssertEqual(model.selectedRadioEndpointID, resolved.id)
        XCTAssertEqual(model.selectedRadioEndpoint, resolved)
        XCTAssertEqual(model.radioEndpoints, [usb, bluetooth, resolved])
    }

    func testResolvedUSBEndpointReplacesStalePathForTheSameStableRadio() async {
        let reenumerated = RadioEndpoint(
            id: usb.id,
            name: usb.name,
            transport: .usb,
            detail: "/dev/cu.usbmodem202"
        )
        let selector = EndpointTestSelector(initialEndpoints: [usb, bluetooth])
        selector.resolvedEndpoint = reenumerated
        let controller = EndpointTestRadioController()
        let model = makeModel(controller: controller, selector: selector)

        await model.connectRadio()

        XCTAssertTrue(model.radioState.connection.isConnected)
        XCTAssertEqual(model.selectedRadioEndpointID, usb.id)
        XCTAssertEqual(model.selectedRadioEndpoint, reenumerated)
        XCTAssertEqual(model.radioEndpoints, [reenumerated, bluetooth])
    }

    func testConnectedRadioPreventsEndpointChanges() {
        let selector = EndpointTestSelector(initialEndpoints: [usb, bluetooth])
        let controller = EndpointTestRadioController(initiallyConnected: true)
        let model = makeModel(controller: controller, selector: selector)

        model.selectRadioEndpoint(id: bluetooth.id)

        XCTAssertFalse(model.canSelectRadioEndpoint)
        XCTAssertEqual(model.selectedRadioEndpointID, usb.id)
    }

    func testConnectedRadioPreventsDiscoveryFromChangingTheSelectedEndpoint() async {
        let selector = EndpointTestSelector(initialEndpoints: [usb, bluetooth])
        selector.nextRefresh = .success([bluetooth])
        let controller = EndpointTestRadioController(initiallyConnected: true)
        let model = makeModel(controller: controller, selector: selector)

        await model.refreshRadioEndpoints().value

        XCTAssertEqual(selector.refreshCallCount, 0)
        XCTAssertEqual(model.radioEndpoints, [usb, bluetooth])
        XCTAssertEqual(model.selectedRadioEndpointID, usb.id)
        XCTAssertEqual(model.radioEndpointRefreshState, .ready)
    }

    func testCancelledRefreshRestoresAUsableReadyState() async {
        let selector = EndpointTestSelector(initialEndpoints: [usb, bluetooth])
        selector.refreshDelay = .seconds(60)
        let model = makeModel(selector: selector)

        let refresh = model.refreshRadioEndpoints()
        await Task.yield()
        XCTAssertEqual(model.radioEndpointRefreshState, .refreshing)

        refresh.cancel()
        await refresh.value

        XCTAssertEqual(selector.refreshCallCount, 1)
        XCTAssertEqual(model.radioEndpointRefreshState, .ready)
        XCTAssertEqual(model.radioEndpoints, [usb, bluetooth])
        XCTAssertEqual(model.selectedRadioEndpointID, usb.id)
    }

    func testUserCanStopOrdinaryConnectionAttempt() async {
        let selector = EndpointTestSelector(initialEndpoints: [usb, bluetooth])
        let controller = EndpointTestRadioController(blockConnect: true)
        let model = makeModel(controller: controller, selector: selector)

        let connectionTask = Task { @MainActor in
            await model.connectRadio()
        }
        for _ in 0..<1_000 where !controller.connectStarted {
            await Task.yield()
        }

        XCTAssertTrue(controller.connectStarted)
        XCTAssertEqual(model.radioConnectionActivity, .connection)
        XCTAssertTrue(model.isRadioOperationInFlight)

        await model.cancelRadioConnection()
        await connectionTask.value

        XCTAssertTrue(controller.connectCancellationObserved)
        XCTAssertEqual(controller.disconnectCallCount, 1)
        XCTAssertEqual(model.radioState.connection, .disconnected)
        XCTAssertNil(model.radioConnectionActivity)
        XCTAssertFalse(model.isRadioOperationInFlight)
        XCTAssertNil(model.operationError)
    }

    func testValidatedUSBSelectionAndConnectRemainUsableDuringBluetoothRefresh() async {
        let secondUSB = RadioEndpoint(
            id: "usb:radio-b",
            name: "TH-D75 B3B00002",
            transport: .usb,
            detail: "/dev/cu.usbmodem201"
        )
        let selector = EndpointTestSelector(
            initialEndpoints: [usb, secondUSB, bluetooth]
        )
        selector.refreshDelay = .seconds(60)
        let controller = EndpointTestRadioController()
        let model = makeModel(controller: controller, selector: selector)

        let refresh = model.refreshRadioEndpoints()
        for _ in 0..<1_000 where selector.refreshCallCount == 0 {
            await Task.yield()
        }

        XCTAssertEqual(model.radioEndpointRefreshState, .refreshing)
        XCTAssertTrue(model.canSelectRadioEndpoint)
        XCTAssertTrue(model.canSelectRadioEndpoint(id: secondUSB.id))
        model.selectRadioEndpoint(id: secondUSB.id)
        XCTAssertEqual(model.selectedRadioEndpointID, secondUSB.id)
        XCTAssertFalse(model.canSelectRadioEndpoint(id: bluetooth.id))
        model.selectRadioEndpoint(id: bluetooth.id)
        XCTAssertEqual(model.selectedRadioEndpointID, secondUSB.id)
        XCTAssertTrue(model.canConnectSelectedRadioEndpoint)

        await model.connectRadio()

        XCTAssertEqual(controller.connectCallCount, 1)
        XCTAssertEqual(selector.selectedEndpointIDs, [secondUSB.id])
        XCTAssertTrue(model.radioState.connection.isConnected)
        XCTAssertNil(model.operationError)

        refresh.cancel()
        await refresh.value
    }

    func testRetainedBluetoothWaitsForBlockedDiscoveryBeforeConnecting() async {
        let selector = EndpointTestSelector(initialEndpoints: [usb, bluetooth])
        selector.refreshDelay = .seconds(60)
        let controller = EndpointTestRadioController()
        let model = makeModel(controller: controller, selector: selector)
        model.selectRadioEndpoint(id: bluetooth.id)

        let refresh = model.refreshRadioEndpoints()
        for _ in 0..<1_000 where selector.refreshCallCount == 0 {
            await Task.yield()
        }

        XCTAssertEqual(model.radioEndpointRefreshState, .refreshing)
        XCTAssertFalse(model.canConnectSelectedRadioEndpoint)

        await model.connectRadio()

        XCTAssertEqual(controller.connectCallCount, 0)
        XCTAssertEqual(
            model.operationError,
            RadioEndpointSelectionError.refreshInProgress.localizedDescription
        )

        refresh.cancel()
        await refresh.value
    }

    func testFixedDefaultSelectorKeepsExistingUSBConnectBehavior() async {
        let controller = EndpointTestRadioController()
        let model = AzimuthSceneModel(
            radioController: controller,
            catalogProvider: PreviewRadioSettingCatalogProvider(),
            assistantPlanner: OnDeviceAssistantPlanner(),
            initialCatalog: .designPreview
        )

        await model.connectRadio()

        XCTAssertEqual(model.radioEndpoints, [.defaultUSBC])
        XCTAssertEqual(model.selectedRadioEndpointID, RadioEndpoint.defaultUSBC.id)
        XCTAssertEqual(controller.connectCallCount, 1)
        XCTAssertNil(model.operationError)
    }

    private func makeModel(
        controller: EndpointTestRadioController = EndpointTestRadioController(),
        selector: EndpointTestSelector
    ) -> AzimuthSceneModel {
        AzimuthSceneModel(
            radioController: controller,
            catalogProvider: PreviewRadioSettingCatalogProvider(),
            assistantPlanner: OnDeviceAssistantPlanner(),
            radioEndpointSelector: selector,
            initialCatalog: .designPreview
        )
    }
}

@MainActor
private final class EndpointEventRecorder {
    private(set) var values: [String] = []

    func append(_ value: String) {
        values.append(value)
    }
}

private enum EndpointTestError: LocalizedError {
    case discoveryFailed
    case selectionFailed

    var errorDescription: String? {
        switch self {
        case .discoveryFailed: return "Bluetooth discovery failed."
        case .selectionFailed: return "The selected connection could not be prepared."
        }
    }
}

@MainActor
private final class EndpointTestSelector: RadioEndpointSelecting {
    let initialEndpoints: [RadioEndpoint]
    var nextRefresh: Result<[RadioEndpoint], Error>?
    var nextWarning: String?
    var nextPairedBluetoothDeviceCount: UInt32?
    var refreshDelay: Duration?
    var selectionError: Error?
    var resolvedEndpoint: RadioEndpoint?
    private(set) var refreshCallCount = 0
    private(set) var selectedEndpointIDs: [String] = []
    private let events: EndpointEventRecorder?

    init(
        initialEndpoints: [RadioEndpoint],
        events: EndpointEventRecorder? = nil
    ) {
        self.initialEndpoints = initialEndpoints
        self.events = events
    }

    func refreshEndpoints() async throws -> RadioEndpointDiscoverySnapshot {
        refreshCallCount += 1
        if let refreshDelay {
            try await Task.sleep(for: refreshDelay)
        }
        return RadioEndpointDiscoverySnapshot(
            endpoints: try nextRefresh?.get() ?? initialEndpoints,
            warning: nextWarning,
            pairedBluetoothDeviceCount: nextPairedBluetoothDeviceCount
        )
    }

    func selectEndpoint(id: String) async throws {
        selectedEndpointIDs.append(id)
        events?.append("select:\(id)")
        if let selectionError { throw selectionError }
    }

    func selectedEndpoint() async -> RadioEndpoint? {
        if let resolvedEndpoint { return resolvedEndpoint }
        guard let selected = selectedEndpointIDs.last else { return nil }
        return initialEndpoints.first { $0.id == selected }
    }
}

@MainActor
private final class EndpointTestRadioController: RadioControlling {
    private(set) var currentState: RadioWorkspaceState
    private(set) var connectCallCount = 0
    private(set) var disconnectCallCount = 0
    private(set) var connectStarted = false
    private(set) var connectCancellationObserved = false
    private let events: EndpointEventRecorder?
    private let blockConnect: Bool

    init(
        events: EndpointEventRecorder? = nil,
        initiallyConnected: Bool = false,
        blockConnect: Bool = false
    ) {
        self.events = events
        self.blockConnect = blockConnect
        currentState = initiallyConnected
            ? Self.connectedState
            : .disconnected
    }

    var updates: AsyncStream<RadioWorkspaceState> {
        let state = currentState
        return AsyncStream { continuation in
            continuation.yield(state)
            continuation.finish()
        }
    }

    func connect() async throws {
        connectCallCount += 1
        connectStarted = true
        events?.append("connect")
        if blockConnect {
            do {
                try await Task.sleep(for: .seconds(60))
            } catch is CancellationError {
                connectCancellationObserved = true
                throw CancellationError()
            }
        }
        currentState = Self.connectedState
    }

    func disconnect() async {
        disconnectCallCount += 1
        currentState = .disconnected
    }

    func refreshScreen() async throws {}
    func refreshSettings() async throws {}
    func press(_ key: RadioFrontPanelKey) async throws {}

    func applySettings(
        _ changes: [ValidatedRadioSettingChange],
        progress: @escaping @MainActor @Sendable (RadioSettingApplyProgress) -> Void
    ) async throws -> RadioSettingApplyReport {
        RadioSettingApplyReport(results: [])
    }

    private static let connectedState = RadioWorkspaceState(
        connection: .connected(device: "Kenwood TH-D75", transport: "Selected endpoint"),
        capabilities: RadioCapabilities(
            screenStreaming: .available,
            frontPanelControl: .available,
            settingRead: .available,
            settingWrite: .available
        ),
        screenFrame: nil,
        telemetry: .unavailable,
        settingValues: [:],
        lastScreenError: nil
    )
}
