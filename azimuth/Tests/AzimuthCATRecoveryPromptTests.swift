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
            .usbMmdvmMode(automaticRecoveryAvailable: true)
        )
        XCTAssertTrue(model.catRecoveryAlert?.message.contains("radio restarts") == true)

        model.dismissCATRecoveryAlert()

        XCTAssertNil(model.catRecoveryAlert)
        XCTAssertEqual(controller.recoveryCallCount, 0)
    }

    func testApprovedRecoveryRunsOnceAndEndsConnected() async {
        let controller = CATRecoveryTestController()
        let model = makeModel(controller: controller)
        await model.connectRadio()

        await model.restoreCATFromUSBMMDVM()

        XCTAssertEqual(controller.recoveryCallCount, 1)
        XCTAssertNil(model.catRecoveryAlert)
        XCTAssertNil(model.operationError)
        XCTAssertTrue(model.radioState.connection.isConnected)
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
            .recoveryFailed(message: "The paired Bluetooth link could not be opened.")
        )
        XCTAssertNil(model.operationError)
        XCTAssertFalse(model.radioState.connection.isConnected)
    }

    func testLifecycleCancellationDoesNotBecomeARecoveryFailureAlert() async {
        let controller = CATRecoveryTestController(cancelRecovery: true)
        let model = makeModel(controller: controller)
        await model.connectRadio()

        await model.restoreCATFromUSBMMDVM()

        XCTAssertEqual(controller.recoveryCallCount, 1)
        XCTAssertNil(model.catRecoveryAlert)
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

    func testUnavailableAutomaticRecoveryLeavesTheRadioUntouched() async {
        let controller = CATRecoveryTestController(
            automaticRecoveryAvailable: false
        )
        let model = makeModel(controller: controller)

        await model.connectRadio()

        XCTAssertEqual(
            model.catRecoveryAlert,
            .usbMmdvmMode(automaticRecoveryAvailable: false)
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

    private func makeModel(
        controller: CATRecoveryTestController
    ) -> AzimuthSceneModel {
        AzimuthSceneModel(
            radioController: controller,
            catalogProvider: PreviewRadioSettingCatalogProvider(),
            assistantPlanner: OnDeviceAssistantPlanner(),
            initialCatalog: .designPreview
        )
    }
}

@MainActor
private final class CATRecoveryTestController: RadioControlling {
    private(set) var currentState = RadioWorkspaceState.disconnected
    let automaticCATRecoveryAvailable: Bool
    private(set) var connectCallCount = 0
    private(set) var recoveryCallCount = 0
    private(set) var disconnectCallCount = 0
    private(set) var recoveryStarted = false
    private(set) var recoveryCancellationObserved = false
    private let recoveryFailure: String?
    private let cancelRecovery: Bool
    private let blockRecovery: Bool
    private let lateCancellationFailure: String?

    init(
        recoveryFailure: String? = nil,
        cancelRecovery: Bool = false,
        blockRecovery: Bool = false,
        lateCancellationFailure: String? = nil,
        automaticRecoveryAvailable: Bool = true
    ) {
        self.recoveryFailure = recoveryFailure
        self.cancelRecovery = cancelRecovery
        self.blockRecovery = blockRecovery
        self.lateCancellationFailure = lateCancellationFailure
        automaticCATRecoveryAvailable = automaticRecoveryAvailable
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
        recoveryStarted = true
        if cancelRecovery { throw CancellationError() }
        if blockRecovery {
            do {
                try await Task.sleep(for: .seconds(60))
            } catch is CancellationError {
                recoveryCancellationObserved = true
                if let lateCancellationFailure {
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
