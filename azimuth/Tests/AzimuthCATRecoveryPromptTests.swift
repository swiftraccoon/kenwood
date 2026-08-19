// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Azimuth

@MainActor
final class AzimuthCATRecoveryPromptTests: XCTestCase {
    func testMMDVMDetectionOffersRecoveryWithoutWritingBeforeConsent() async {
        let controller = CATRecoveryTestController()
        let model = makeModel(controller: controller)

        await model.connectRadio()

        XCTAssertEqual(controller.connectCallCount, 1)
        XCTAssertEqual(controller.recoveryCallCount, 0)
        XCTAssertNil(model.operationError)
        XCTAssertEqual(
            model.catRecoveryAlert,
            .usbMmdvmMode(
                automaticRecoveryAvailable: true,
                bluetoothFallbackAvailable: false
            )
        )
        XCTAssertTrue(model.catRecoveryAlert?.message.contains("radio restarts") == true)
        XCTAssertTrue(
            model.catRecoveryAlert?.message.contains(
                "already sent the transient packet-mode exit sequence"
            ) == true
        )

        model.dismissCATRecoveryAlert()

        XCTAssertNil(model.catRecoveryAlert)
        XCTAssertEqual(controller.recoveryCallCount, 0)
    }

    func testUseBluetoothInvokesOnlyTheNonDestructiveFallback() async {
        let controller = CATRecoveryTestController(
            bluetoothFallbackAvailable: true
        )
        let model = makeModel(controller: controller)
        await model.connectRadio()

        XCTAssertEqual(
            model.catRecoveryAlert,
            .usbMmdvmMode(
                automaticRecoveryAvailable: true,
                bluetoothFallbackAvailable: true
            )
        )

        await model.connectViaBluetoothFromUSBMMDVM()

        XCTAssertEqual(controller.bluetoothFallbackCallCount, 1)
        XCTAssertEqual(controller.recoveryCallCount, 0)
        XCTAssertNil(model.catRecoveryAlert)
        XCTAssertNil(model.operationError)
        XCTAssertEqual(
            model.radioState.connection,
            .connected(device: "Kenwood TH-D75", transport: "Bluetooth")
        )
    }

    func testApprovedRecoveryRunsOnceAndEndsConnected() async {
        let controller = CATRecoveryTestController(
            bluetoothFallbackAvailable: true
        )
        let model = makeModel(controller: controller)
        await model.connectRadio()

        await model.restoreCATFromUSBMMDVM()

        XCTAssertEqual(controller.recoveryCallCount, 1)
        XCTAssertEqual(controller.bluetoothFallbackCallCount, 0)
        XCTAssertNil(model.catRecoveryAlert)
        XCTAssertNil(model.operationError)
        XCTAssertTrue(model.radioState.connection.isConnected)
    }

    func testUnavailableBluetoothFallbackDoesNotOfferUseBluetooth() async {
        let controller = CATRecoveryTestController(
            bluetoothFallbackAvailable: false
        )
        let model = makeModel(controller: controller)

        await model.connectRadio()

        XCTAssertEqual(
            model.catRecoveryAlert,
            .usbMmdvmMode(
                automaticRecoveryAvailable: true,
                bluetoothFallbackAvailable: false
            )
        )
        XCTAssertFalse(model.catRecoveryAlert?.bluetoothFallbackAvailable == true)
        XCTAssertEqual(controller.bluetoothFallbackCallCount, 0)
    }

    func testRecoveryFailureStaysFailedAndBecomesActionableError() async {
        let controller = CATRecoveryTestController(
            recoveryFailure: "The paired Bluetooth link could not be opened."
        )
        let model = makeModel(controller: controller)
        await model.connectRadio()

        await model.restoreCATFromUSBMMDVM()

        XCTAssertEqual(controller.recoveryCallCount, 1)
        XCTAssertEqual(
            model.catRecoveryAlert,
            .recoveryFailed(
                message: "The paired Bluetooth link could not be opened.",
                automaticRecoveryAvailable: true,
                bluetoothFallbackAvailable: false
            )
        )
        XCTAssertTrue(model.catRecoveryAlert?.automaticRecoveryAvailable == true)
        XCTAssertNil(model.operationError)
        XCTAssertFalse(model.radioState.connection.isConnected)
    }

    func testLifecycleCancellationDoesNotBecomeARecoveryFailureAlert() async {
        let controller = CATRecoveryTestController(cancelRecovery: true)
        let model = makeModel(controller: controller)
        await model.connectRadio()

        await model.restoreCATFromUSBMMDVM()

        XCTAssertEqual(controller.recoveryCallCount, 1)
        XCTAssertEqual(
            model.catRecoveryAlert,
            .usbMmdvmMode(
                automaticRecoveryAvailable: true,
                bluetoothFallbackAvailable: false
            )
        )
        XCTAssertNil(model.operationError)
        XCTAssertFalse(model.radioState.connection.isConnected)
    }

    func testUserCanCancelRecoveryAndWaitForSafeDisconnect() async {
        let controller = CATRecoveryTestController(blockRecovery: true)
        let model = makeModel(controller: controller)
        await model.connectRadio()

        let recoveryTask = Task { @MainActor in
            await model.restoreCATFromUSBMMDVM()
        }
        for _ in 0..<1_000 where !controller.recoveryStarted {
            await Task.yield()
        }
        XCTAssertTrue(controller.recoveryStarted)
        XCTAssertTrue(model.isCATRecoveryInFlight)
        XCTAssertTrue(model.isRadioOperationInFlight)

        await model.cancelCATRecovery()
        await recoveryTask.value

        XCTAssertEqual(controller.disconnectCallCount, 1)
        XCTAssertTrue(controller.recoveryCancellationObserved)
        XCTAssertEqual(model.radioState.connection, .disconnected)
        XCTAssertFalse(model.isCATRecoveryInFlight)
        XCTAssertFalse(model.isRadioOperationInFlight)
        XCTAssertNil(model.catRecoveryAlert)
        XCTAssertNil(model.operationError)
    }

    func testLateCancellationPreservesCompletedRadioOutcome() async {
        let completedMessage =
            "Menu 650 was changed to Off and the radio is rebooting. The USB-C CAT reconnect was stopped."
        let controller = CATRecoveryTestController(
            blockRecovery: true,
            lateCancellationFailure: completedMessage
        )
        let model = makeModel(controller: controller)
        await model.connectRadio()

        let recoveryTask = Task { @MainActor in
            await model.restoreCATFromUSBMMDVM()
        }
        for _ in 0..<1_000 where !controller.recoveryStarted {
            await Task.yield()
        }
        await model.cancelCATRecovery()
        await recoveryTask.value

        XCTAssertEqual(
            model.catRecoveryAlert,
            .recoveryFailed(message: completedMessage)
        )
        XCTAssertEqual(model.radioState.connection, .disconnected)
    }

    func testBluetoothHandoffHasDistinctNonDestructiveProgressAndStop() async {
        let controller = CATRecoveryTestController(
            blockBluetoothFallback: true,
            bluetoothFallbackAvailable: true
        )
        let model = makeModel(controller: controller)
        await model.connectRadio()

        let handoffTask = Task { @MainActor in
            await model.connectViaBluetoothFromUSBMMDVM()
        }
        for _ in 0..<1_000 where !controller.bluetoothFallbackStarted {
            await Task.yield()
        }

        XCTAssertTrue(controller.bluetoothFallbackStarted)
        XCTAssertEqual(model.radioConnectionActivity, .bluetoothHandoff)
        XCTAssertTrue(model.isBluetoothHandoffInFlight)
        XCTAssertFalse(model.isCATRecoveryInFlight)

        await model.cancelRadioConnection()
        await handoffTask.value

        XCTAssertTrue(controller.bluetoothFallbackCancellationObserved)
        XCTAssertEqual(model.radioState.connection, .disconnected)
        XCTAssertNil(model.radioConnectionActivity)
        XCTAssertNil(model.catRecoveryAlert)
    }

    func testBluetoothHandoffFailureKeepsBothRecoveryChoicesRetryable() async {
        let controller = CATRecoveryTestController(
            bluetoothFallbackFailure: "The paired radio did not answer.",
            bluetoothFallbackAvailable: true
        )
        let model = makeModel(controller: controller)
        await model.connectRadio()

        await model.connectViaBluetoothFromUSBMMDVM()

        XCTAssertEqual(
            model.catRecoveryAlert,
            .recoveryFailed(
                message: "The paired radio did not answer.",
                automaticRecoveryAvailable: true,
                bluetoothFallbackAvailable: true
            )
        )
        await model.connectViaBluetoothFromUSBMMDVM()
        XCTAssertEqual(
            controller.bluetoothFallbackCallCount,
            2,
            "A transient handoff failure must not require a USB reconnect before retry."
        )
    }

    func testBluetoothAuthorizationDenialKeepsRecoveryActionsRetryable() async {
        let controller = CATRecoveryTestController(
            denyBluetoothAuthorization: true,
            bluetoothFallbackAvailable: true
        )
        let model = makeModel(controller: controller)
        await model.connectRadio()

        await model.restoreCATFromUSBMMDVM()

        XCTAssertEqual(controller.recoveryCallCount, 1)
        XCTAssertEqual(
            model.catRecoveryAlert,
            .recoveryFailed(
                message: AzimuthBluetoothAuthorizationError.denied.localizedDescription,
                automaticRecoveryAvailable: true,
                bluetoothFallbackAvailable: true
            )
        )
        XCTAssertTrue(
            model.radioEndpointDiscoveryWarning?.contains("System Settings") == true
        )

        await model.restoreCATFromUSBMMDVM()
        XCTAssertEqual(
            controller.recoveryCallCount,
            2,
            "Authorization failure must not require reconnecting USB before retry."
        )
    }

    func testUnavailableAutomaticRecoveryLeavesTheRadioUntouched() async {
        let controller = CATRecoveryTestController(
            automaticRecoveryAvailable: false
        )
        let model = makeModel(controller: controller)

        await model.connectRadio()

        XCTAssertEqual(
            model.catRecoveryAlert,
            .usbMmdvmMode(
                automaticRecoveryAvailable: false,
                bluetoothFallbackAvailable: false
            )
        )
        XCTAssertTrue(model.catRecoveryAlert?.message.contains("set Menu 650") == true)

        await model.restoreCATFromUSBMMDVM()

        XCTAssertEqual(controller.recoveryCallCount, 0)
        XCTAssertEqual(
            model.catRecoveryAlert,
            .recoveryFailed(
                message: "Automatic CAT recovery is unavailable for this radio connection."
            )
        )
        XCTAssertNil(model.operationError)
    }

    func testCapabilitiesDoNotOfferBluetoothWhenFreshInventoryHasZeroDevices() async {
        let controller = CATRecoveryTestController(
            automaticRecoveryAvailable: true,
            bluetoothFallbackAvailable: true
        )
        let model = makeModel(
            controller: controller,
            likelyBluetoothCandidate: false,
            totalPairedBluetoothDevices: 0
        )

        await model.connectRadio()

        XCTAssertEqual(
            model.catRecoveryAlert,
            .usbMmdvmMode(
                automaticRecoveryAvailable: false,
                bluetoothFallbackAvailable: false
            )
        )
        XCTAssertTrue(
            model.catRecoveryAlert?.message.contains("needs a configured, paired TH-D75") == true
        )
    }

    func testCustomNamedPairedDeviceOffersHonestSerialQualifiedTry() async {
        let controller = CATRecoveryTestController(
            automaticRecoveryAvailable: true,
            bluetoothFallbackAvailable: true
        )
        let model = makeModel(
            controller: controller,
            likelyBluetoothCandidate: false,
            totalPairedBluetoothDevices: 1
        )

        await model.connectRadio()

        XCTAssertEqual(model.radioEndpoints, [.defaultUSBC])
        XCTAssertEqual(model.pairedBluetoothDeviceCount, 1)
        XCTAssertEqual(
            model.catRecoveryAlert,
            .usbMmdvmMode(
                automaticRecoveryAvailable: true,
                bluetoothFallbackAvailable: true
            )
        )
        XCTAssertTrue(
            model.catRecoveryAlert?.message.contains("try to locate and verify") == true
        )
    }

    private func makeModel(
        controller: CATRecoveryTestController,
        likelyBluetoothCandidate: Bool = true,
        totalPairedBluetoothDevices: UInt32 = 1
    ) -> AzimuthSceneModel {
        let endpoints: [RadioEndpoint] = likelyBluetoothCandidate
            ? [.defaultUSBC, .catRecoveryBluetooth]
            : [.defaultUSBC]
        return AzimuthSceneModel(
            radioController: controller,
            catalogProvider: PreviewRadioSettingCatalogProvider(),
            assistantPlanner: OnDeviceAssistantPlanner(),
            radioEndpointSelector: CATRecoveryEndpointSelector(
                endpoints: endpoints,
                totalPairedBluetoothDevices: totalPairedBluetoothDevices
            ),
            initialCatalog: .designPreview
        )
    }
}

private extension RadioEndpoint {
    static let catRecoveryBluetooth = RadioEndpoint(
        id: "bluetooth:00:11:22:33:44:55",
        name: "Kenwood TH-D75",
        transport: .bluetooth,
        detail: "00:11:22:33:44:55"
    )
}

@MainActor
private final class CATRecoveryEndpointSelector: RadioEndpointSelecting {
    let initialEndpoints: [RadioEndpoint]
    let initialPairedBluetoothDeviceCount: UInt32?

    init(
        endpoints: [RadioEndpoint],
        totalPairedBluetoothDevices: UInt32
    ) {
        initialEndpoints = endpoints
        initialPairedBluetoothDeviceCount = totalPairedBluetoothDevices
    }

    func refreshEndpoints() async throws -> RadioEndpointDiscoverySnapshot {
        RadioEndpointDiscoverySnapshot(
            endpoints: initialEndpoints,
            pairedBluetoothDeviceCount: initialPairedBluetoothDeviceCount
        )
    }

    func selectEndpoint(id: String) async throws {
        guard initialEndpoints.contains(where: { $0.id == id }) else {
            throw RadioEndpointSelectionError.invalidEndpoint(id: id)
        }
    }
}

@MainActor
private final class CATRecoveryTestController: RadioControlling {
    private(set) var currentState = RadioWorkspaceState.disconnected
    private(set) var automaticCATRecoveryAvailable: Bool
    private(set) var bluetoothCATFallbackAvailable: Bool
    private(set) var connectCallCount = 0
    private(set) var recoveryCallCount = 0
    private(set) var bluetoothFallbackCallCount = 0
    private(set) var disconnectCallCount = 0
    private(set) var recoveryStarted = false
    private(set) var recoveryCancellationObserved = false
    private(set) var bluetoothFallbackStarted = false
    private(set) var bluetoothFallbackCancellationObserved = false
    private let recoveryFailure: String?
    private let denyBluetoothAuthorization: Bool
    private let cancelRecovery: Bool
    private let blockRecovery: Bool
    private let blockBluetoothFallback: Bool
    private let bluetoothFallbackFailure: String?
    private let lateCancellationFailure: String?

    init(
        recoveryFailure: String? = nil,
        denyBluetoothAuthorization: Bool = false,
        cancelRecovery: Bool = false,
        blockRecovery: Bool = false,
        blockBluetoothFallback: Bool = false,
        bluetoothFallbackFailure: String? = nil,
        lateCancellationFailure: String? = nil,
        automaticRecoveryAvailable: Bool = true,
        bluetoothFallbackAvailable: Bool = false
    ) {
        self.recoveryFailure = recoveryFailure
        self.denyBluetoothAuthorization = denyBluetoothAuthorization
        self.cancelRecovery = cancelRecovery
        self.blockRecovery = blockRecovery
        self.blockBluetoothFallback = blockBluetoothFallback
        self.bluetoothFallbackFailure = bluetoothFallbackFailure
        self.lateCancellationFailure = lateCancellationFailure
        automaticCATRecoveryAvailable = automaticRecoveryAvailable
        bluetoothCATFallbackAvailable = bluetoothFallbackAvailable
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
        var failed = RadioWorkspaceState.disconnected
        failed.connection = .failed(
            message: "The TH-D75 is using USB-C for DV Gateway/MMDVM data."
        )
        currentState = failed
        throw RadioControllerError.usbMmdvmMode
    }

    func restoreCATFromUSBMMDVM() async throws {
        recoveryCallCount += 1
        if denyBluetoothAuthorization {
            throw AzimuthBluetoothAuthorizationError.denied
        }
        recoveryStarted = true
        if cancelRecovery { throw CancellationError() }
        if blockRecovery {
            do {
                try await Task.sleep(for: .seconds(60))
            } catch is CancellationError {
                recoveryCancellationObserved = true
                if let lateCancellationFailure {
                    automaticCATRecoveryAvailable = false
                    bluetoothCATFallbackAvailable = false
                    throw RadioControllerError.operationFailed(lateCancellationFailure)
                }
                throw CancellationError()
            }
        }
        if let recoveryFailure {
            throw RadioControllerError.operationFailed(recoveryFailure)
        }
        currentState = RadioWorkspaceState(
            connection: .connected(device: "Kenwood TH-D75", transport: "USB-C"),
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

    func connectViaBluetoothFromUSBMMDVM() async throws {
        bluetoothFallbackCallCount += 1
        bluetoothFallbackStarted = true
        if blockBluetoothFallback {
            do {
                try await Task.sleep(for: .seconds(60))
            } catch is CancellationError {
                bluetoothFallbackCancellationObserved = true
                throw CancellationError()
            }
        }
        if let bluetoothFallbackFailure {
            throw RadioControllerError.operationFailed(bluetoothFallbackFailure)
        }
        currentState = RadioWorkspaceState(
            connection: .connected(device: "Kenwood TH-D75", transport: "Bluetooth"),
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
}
