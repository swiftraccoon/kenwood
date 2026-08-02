// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import XCTest
@testable import Azimuth

@MainActor
final class AssistantWorkflowTests: XCTestCase {
    func testDeclineDiscardsWithoutControllerCall() async throws {
        let fixture = try makeFixture()
        await fixture.model.proposeAssistantPlan(request: "Turn off beeps")
        fixture.model.declineAssistantPlan()

        XCTAssertEqual(fixture.controller.applyCallCount, 0)
        XCTAssertEqual(fixture.model.assistantWorkflow, .idle)
    }

    func testAcceptSendsOneValidatedBatch() async throws {
        let fixture = try makeFixture()
        await fixture.model.proposeAssistantPlan(request: "Turn off beeps")
        XCTAssertTrue(fixture.model.assistantCanAccept)

        await fixture.model.acceptAssistantPlan()

        XCTAssertEqual(fixture.controller.applyCallCount, 1)
        XCTAssertEqual(fixture.controller.batches.count, 1)
        XCTAssertEqual(fixture.controller.batches.first?.count, 1)
        guard case .applied = fixture.model.assistantWorkflow else {
            return XCTFail("Expected applied workflow state")
        }
    }

    func testAcceptPreservesAnOrderedListAndExecutesItAsOneBatch() async throws {
        let catalog = RadioSettingCatalog.designPreview
        let beep = try XCTUnwrap(catalog.definitions.first { $0.title == "Key Beep" })
        let gps = try XCTUnwrap(catalog.definitions.first { $0.title == "Built-in GPS" })
        let controller = AssistantMockRadioController(definition: beep, connected: true)
        controller.seedLiveValue(.boolean(false), for: gps.id)
        let planner = AssistantListMockPlanner(
            changes: [
                .init(settingID: beep.id, proposedValue: "Off", rationale: "Silence key tones."),
                .init(settingID: gps.id, proposedValue: "On", rationale: "Enable positioning."),
            ]
        )
        let model = AzimuthSceneModel(
            radioController: controller,
            catalogProvider: PreviewRadioSettingCatalogProvider(),
            assistantPlanner: planner
        )

        await model.proposeAssistantPlan(request: "Make the radio quiet and enable GPS")
        XCTAssertTrue(model.assistantCanAccept)
        await model.acceptAssistantPlan()

        XCTAssertEqual(controller.applyCallCount, 1)
        XCTAssertEqual(controller.batches.count, 1)
        XCTAssertEqual(controller.batches[0].map(\.settingID), [beep.id, gps.id])
        XCTAssertEqual(controller.batches[0].map(\.targetValue), [.boolean(false), .boolean(true)])
    }

    func testClarificationNeverCallsController() async throws {
        let fixture = try makeFixture(needsClarification: true)
        await fixture.model.proposeAssistantPlan(request: "Maybe change something")
        XCTAssertFalse(fixture.model.assistantCanAccept)

        await fixture.model.acceptAssistantPlan()

        XCTAssertEqual(fixture.controller.applyCallCount, 0)
    }

    func testDisconnectedNeverCallsController() async throws {
        let fixture = try makeFixture(connected: false)
        await fixture.model.proposeAssistantPlan(request: "Turn off beeps")
        XCTAssertFalse(fixture.model.assistantCanAccept)

        await fixture.model.acceptAssistantPlan()

        XCTAssertEqual(fixture.controller.applyCallCount, 0)
    }

    func testInvalidPlanNeverCallsController() async throws {
        let fixture = try makeFixture(proposedValue: "Definitely not a boolean")
        await fixture.model.proposeAssistantPlan(request: "Turn off beeps")
        XCTAssertFalse(fixture.model.assistantCanAccept)

        await fixture.model.acceptAssistantPlan()

        XCTAssertEqual(fixture.controller.applyCallCount, 0)
    }

    func testStaleBeforeValueNeverCallsController() async throws {
        let fixture = try makeFixture()
        await fixture.model.proposeAssistantPlan(request: "Turn off beeps")
        fixture.controller.changeLiveValue(to: .boolean(false))

        await fixture.model.acceptAssistantPlan()

        XCTAssertEqual(fixture.controller.applyCallCount, 0)
        guard case .failed(_, _, let message) = fixture.model.assistantWorkflow else {
            return XCTFail("Expected stale-plan failure")
        }
        XCTAssertTrue(message.contains("changed on the radio"))
    }

    func testBackgroundDisconnectsAndActiveRestoresPreviouslyLiveRadio() async throws {
        let fixture = try makeFixture()

        let background = fixture.model.handleScenePhaseBackground()
        await background.value
        XCTAssertEqual(fixture.controller.disconnectCallCount, 1)
        XCTAssertEqual(fixture.model.radioState.connection, .disconnected)

        let active = fixture.model.handleScenePhaseActive()
        await active.value
        XCTAssertEqual(fixture.controller.connectCallCount, 1)
        XCTAssertTrue(fixture.model.radioState.connection.isConnected)

        let repeatedActive = fixture.model.handleScenePhaseActive()
        await repeatedActive.value
        XCTAssertEqual(
            fixture.controller.connectCallCount,
            1,
            "Only a matching background teardown may trigger a reconnect"
        )
    }

    func testForegroundDoesNotConnectWithoutAPreviouslyLiveRadio() async throws {
        let fixture = try makeFixture(connected: false)

        let background = fixture.model.handleScenePhaseBackground()
        await background.value
        let active = fixture.model.handleScenePhaseActive()
        await active.value

        XCTAssertEqual(fixture.controller.disconnectCallCount, 0)
        XCTAssertEqual(fixture.controller.connectCallCount, 0)
        XCTAssertEqual(fixture.model.radioState.connection, .disconnected)
    }

    private func makeFixture(
        connected: Bool = true,
        proposedValue: String = "Off",
        needsClarification: Bool = false
    ) throws -> Fixture {
        let catalog = RadioSettingCatalog.designPreview
        let definition = try XCTUnwrap(catalog.definitions.first { $0.title == "Key Beep" })
        let controller = AssistantMockRadioController(
            definition: definition,
            connected: connected
        )
        let planner = AssistantMockPlanner(
            definition: definition,
            proposedValue: proposedValue,
            needsClarification: needsClarification
        )
        let model = AzimuthSceneModel(
            radioController: controller,
            catalogProvider: PreviewRadioSettingCatalogProvider(),
            assistantPlanner: planner
        )
        return Fixture(model: model, controller: controller)
    }

    private struct Fixture {
        let model: AzimuthSceneModel
        let controller: AssistantMockRadioController
    }
}

@MainActor
private final class AssistantMockPlanner: AssistantPlanning {
    let availability: AssistantAvailability = .available
    let definition: RadioSettingDefinition
    let proposedValue: String
    let needsClarification: Bool

    init(
        definition: RadioSettingDefinition,
        proposedValue: String,
        needsClarification: Bool
    ) {
        self.definition = definition
        self.proposedValue = proposedValue
        self.needsClarification = needsClarification
    }

    func propose(
        request: String,
        catalog: RadioSettingCatalog,
        currentValues: [String: ProposedSettingValue]
    ) async throws -> AssistantPlan {
        AssistantPlanValidator.validate(
            request: request,
            draft: AssistantPlanDraft(
                summary: "Change the key beep.",
                needsClarification: needsClarification,
                changes: [
                    .init(
                        settingID: definition.id,
                        proposedValue: proposedValue,
                        rationale: "Matches the request."
                    ),
                ]
            ),
            catalog: catalog,
            currentValues: currentValues
        )
    }
}

@MainActor
private final class AssistantListMockPlanner: AssistantPlanning {
    let availability: AssistantAvailability = .available
    let changes: [AssistantPlanDraft.Change]

    init(changes: [AssistantPlanDraft.Change]) {
        self.changes = changes
    }

    func propose(
        request: String,
        catalog: RadioSettingCatalog,
        currentValues: [String: ProposedSettingValue]
    ) async throws -> AssistantPlan {
        AssistantPlanValidator.validate(
            request: request,
            draft: AssistantPlanDraft(
                summary: "Apply the requested radio configuration.",
                needsClarification: false,
                changes: changes
            ),
            catalog: catalog,
            currentValues: currentValues
        )
    }
}

@MainActor
private final class AssistantMockRadioController: RadioControlling {
    private(set) var currentState: RadioWorkspaceState
    private(set) var connectCallCount = 0
    private(set) var disconnectCallCount = 0
    private(set) var applyCallCount = 0
    private(set) var batches: [[ValidatedRadioSettingChange]] = []
    private let definition: RadioSettingDefinition

    init(definition: RadioSettingDefinition, connected: Bool) {
        self.definition = definition
        currentState = RadioWorkspaceState(
            connection: connected
                ? .connected(device: "TH-D75", transport: "USB-C")
                : .disconnected,
            capabilities: connected
                ? RadioCapabilities(
                    screenStreaming: .available,
                    frontPanelControl: .available,
                    settingRead: .available,
                    settingWrite: .available
                )
                : .disconnected,
            screenFrame: nil,
            telemetry: .unavailable,
            settingValues: [definition.id: .boolean(true)],
            lastScreenError: nil
        )
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
        currentState.connection = .connected(device: "TH-D75", transport: "USB-C")
        currentState.capabilities = RadioCapabilities(
            screenStreaming: .available,
            frontPanelControl: .available,
            settingRead: .available,
            settingWrite: .available
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
        applyCallCount += 1
        batches.append(changes)
        progress(.init(completedCount: 0, totalCount: changes.count, currentSettingID: changes.first?.settingID))
        let results = changes.map { change in
            currentState.settingValues[change.settingID] = change.targetValue
            return RadioSettingApplyResult(
                settingID: change.settingID,
                previousValue: change.previousValue,
                targetValue: change.targetValue,
                outcome: .applied
            )
        }
        progress(.init(completedCount: changes.count, totalCount: changes.count, currentSettingID: nil))
        return RadioSettingApplyReport(results: results)
    }

    func changeLiveValue(to value: ProposedSettingValue) {
        currentState.settingValues[definition.id] = value
    }

    func seedLiveValue(_ value: ProposedSettingValue, for settingID: String) {
        currentState.settingValues[settingID] = value
    }
}
