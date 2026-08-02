// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import XCTest
@testable import Azimuth

@MainActor
final class AzimuthSceneModelIFDSPOperationsTests: XCTestCase {
    func testStartPreparesRadioBeforeStartingPhysicalAudio() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)

        await model.startIFDSP()

        XCTAssertEqual(events.values, ["radio.prepare", "audio.start"])
        XCTAssertEqual(model.ifDSPModeState, .active(.testStatus))
        XCTAssertTrue(model.ifDSPState.isStreaming)
        XCTAssertNil(model.operationError)
    }

    func testMissingUSBAudioStopsCaptureAndRestoresRadioImmediately() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(
            events: events,
            startResult: .waitingForUSBAudio(availableInputs: ["iPad Microphone"])
        )
        let model = makeModel(radio: radio, mode: mode, stream: stream)

        await model.startIFDSP()

        XCTAssertEqual(
            events.values,
            ["radio.prepare", "audio.start", "audio.stop", "radio.restore"]
        )
        XCTAssertEqual(model.ifDSPModeState, .inactive)
        XCTAssertEqual(model.ifDSPState, .idle)
        XCTAssertTrue(model.operationError?.contains("TH-D75 USB audio input was not available") == true)
    }

    func testUserStopEndsAudioBeforeRestoringEveryRadioField() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        await model.startIFDSP()
        events.values.removeAll()

        await model.stopIFDSP()

        XCTAssertEqual(events.values, ["audio.stop", "radio.restore"])
        XCTAssertEqual(model.ifDSPModeState, .inactive)
        XCTAssertEqual(model.ifDSPState, .idle)
    }

    func testUnexpectedAudioRouteLossAutomaticallyRestoresRadio() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        model.activate()
        await model.startIFDSP()
        events.values.removeAll()

        stream.publish(
            .paused(reason: "The TH-D75 USB audio input disconnected.", lastFrame: nil)
        )

        await assertEventually {
            model.ifDSPModeState == .inactive && model.ifDSPState == .idle
        }
        XCTAssertEqual(events.values, ["audio.stop", "radio.restore"])
        XCTAssertTrue(model.operationError?.contains("USB audio input disconnected") == true)
    }

    func testRetuneFailureStopsAudioAndRestoresSavedRadioState() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        await model.startIFDSP()
        mode.retuneError = .retuneRejected
        events.values.removeAll()

        await model.retuneIFDSP(to: 433_925_000)

        XCTAssertEqual(
            events.values,
            ["radio.retune.433925000", "audio.stop"]
        )
        XCTAssertEqual(model.ifDSPModeState, .inactive)
        XCTAssertEqual(model.ifDSPState, .idle)
        XCTAssertTrue(model.operationError?.contains("retune was rejected") == true)
    }

    func testRetuneFailureWithDegradedReservationRetriesRestoration() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        await model.startIFDSP()
        mode.retuneError = .retuneRejected
        mode.retuneRestorationPending = true
        events.values.removeAll()

        await model.retuneIFDSP(to: 433_925_000)

        XCTAssertEqual(
            events.values,
            ["radio.retune.433925000", "audio.stop", "radio.restore"]
        )
        XCTAssertEqual(model.ifDSPModeState, .inactive)
        XCTAssertEqual(model.ifDSPState, .idle)
    }

    func testRouteLossDuringSuccessfulRetuneStopsIFAndRestoresRadio() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        model.activate()
        await model.startIFDSP()
        mode.retuneDelayNanoseconds = 75_000_000
        events.values.removeAll()

        let retune = Task { await model.retuneIFDSP(to: 433_925_000) }
        await assertEventually {
            events.values.contains("radio.retune.433925000")
        }
        stream.publish(
            .paused(reason: "The TH-D75 USB audio input disconnected.", lastFrame: nil)
        )
        await retune.value

        XCTAssertEqual(
            events.values,
            ["radio.retune.433925000", "audio.stop", "radio.restore"]
        )
        XCTAssertEqual(model.ifDSPModeState, .inactive)
        XCTAssertEqual(model.ifDSPState, .idle)
        XCTAssertTrue(model.operationError?.contains("USB audio input disconnected") == true)
    }

    func testFailedRestorationStaysReservedUntilExplicitRetrySucceeds() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        await model.startIFDSP()
        mode.restoreError = .restoreRejected
        events.values.removeAll()

        await model.stopIFDSP()

        XCTAssertEqual(events.values, ["audio.stop", "radio.restore"])
        XCTAssertTrue(model.ifDSPModeState.reservesRadioState)
        XCTAssertTrue(model.operationError?.contains("not fully restored") == true)

        mode.restoreError = nil
        events.values.removeAll()
        await model.stopIFDSP()

        XCTAssertEqual(events.values, ["audio.stop", "radio.restore"])
        XCTAssertEqual(model.ifDSPModeState, .inactive)
    }

    func testBackgroundRestoresBeforeDisconnectAndDoesNotRestartIFMode() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        model.activate()
        await model.startIFDSP()
        events.values.removeAll()

        await model.handleScenePhaseBackground().value

        XCTAssertEqual(
            events.values,
            ["audio.stop", "radio.restore", "connection.disconnect"]
        )
        XCTAssertEqual(model.ifDSPModeState, .inactive)

        await model.handleScenePhaseActive().value

        XCTAssertEqual(events.values.last, "connection.connect")
        XCTAssertEqual(events.values.filter { $0 == "radio.prepare" }.count, 0)
        XCTAssertTrue(model.radioState.connection.isConnected)
        XCTAssertEqual(model.ifDSPState, .idle)
    }

    func testBackgroundRestoreFailureDisconnectsButDoesNotReconnectOrHideWarning() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        model.activate()
        await model.startIFDSP()
        mode.restoreError = .restoreRejected
        events.values.removeAll()

        await model.handleScenePhaseBackground().value

        XCTAssertEqual(
            events.values,
            ["audio.stop", "radio.restore", "connection.disconnect"]
        )
        XCTAssertTrue(model.operationError?.contains("not fully restored") == true)

        await model.handleScenePhaseActive().value

        XCTAssertFalse(events.values.contains("connection.connect"))
        XCTAssertEqual(model.radioState.connection, .disconnected)
        XCTAssertTrue(model.operationError?.contains("not fully restored") == true)
    }

    func testExplicitDisconnectAbortsWhenRadioRestorationIsStillPending() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        await model.startIFDSP()
        mode.restoreError = .restoreRejected
        events.values.removeAll()

        await model.disconnectRadio()

        XCTAssertEqual(events.values, ["audio.stop", "radio.restore"])
        XCTAssertTrue(model.radioState.connection.isConnected)
        XCTAssertTrue(model.ifDSPModeState.reservesRadioState)
        XCTAssertTrue(model.operationError?.contains("not fully restored") == true)
    }

    func testRestoredRadioButWorkspaceRefreshFailureUsesAccurateMessage() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        await model.startIFDSP()
        mode.restoreError = .restoreRejected
        mode.restoreCompletesBeforeError = true
        events.values.removeAll()

        await model.stopIFDSP()

        XCTAssertEqual(events.values, ["audio.stop", "radio.restore"])
        XCTAssertEqual(model.ifDSPModeState, .inactive)
        XCTAssertTrue(model.operationError?.contains("radio state was restored") == true)
        XCTAssertTrue(model.operationError?.contains("workspace could not be refreshed") == true)
        XCTAssertFalse(model.operationError?.contains("not fully restored") == true)
    }

    private func makeModel(
        radio: IFDSPTestRadioController,
        mode: IFDSPTestModeController,
        stream: IFDSPTestStream
    ) -> AzimuthSceneModel {
        AzimuthSceneModel(
            radioController: radio,
            catalogProvider: PreviewRadioSettingCatalogProvider(),
            assistantPlanner: IFDSPTestPlanner(),
            ifDSPStream: stream,
            ifDSPModeController: mode,
            initialCatalog: .designPreview
        )
    }

    private func assertEventually(
        timeout: TimeInterval = 1,
        condition: @escaping @MainActor () -> Bool,
        file: StaticString = #filePath,
        line: UInt = #line
    ) async {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition(), Date() < deadline {
            try? await Task.sleep(nanoseconds: 5_000_000)
        }
        XCTAssertTrue(condition(), "Timed out waiting for IF-DSP cleanup", file: file, line: line)
    }
}

@MainActor
private final class IFDSPTestEvents {
    var values: [String] = []
}

@MainActor
private final class IFDSPTestModeController: IFDSPModeControlling {
    private(set) var ifDSPModeState: IFDSPRadioModeState = .inactive
    private let events: IFDSPTestEvents
    var prepareError: IFDSPTestError?
    var retuneError: IFDSPTestError?
    var retuneRestorationPending = false
    var retuneDelayNanoseconds: UInt64 = 0
    var restoreError: IFDSPTestError?
    var restoreCompletesBeforeError = false

    init(events: IFDSPTestEvents) {
        self.events = events
    }

    func prepareIFDSPMode() async throws -> IFDSPRadioModeStatus {
        events.values.append("radio.prepare")
        if let prepareError {
            ifDSPModeState = .failed(message: prepareError.localizedDescription, restorationPending: false)
            throw prepareError
        }
        ifDSPModeState = .active(.testStatus)
        return .testStatus
    }

    func retuneIFDSP(to frequencyHz: UInt32) async throws -> IFDSPRadioModeStatus {
        events.values.append("radio.retune.\(frequencyHz)")
        if retuneDelayNanoseconds > 0 {
            try await Task.sleep(nanoseconds: retuneDelayNanoseconds)
        }
        if let retuneError {
            ifDSPModeState = retuneRestorationPending
                ? .failed(message: retuneError.localizedDescription, restorationPending: true)
                : .inactive
            throw retuneError
        }
        let status = IFDSPRadioModeStatus(
            bandBFrequencyHz: frequencyHz,
            ifCenterHz: 12_000
        )
        ifDSPModeState = .active(status)
        return status
    }

    func restoreIFDSPMode() async throws {
        events.values.append("radio.restore")
        if let restoreError {
            ifDSPModeState = restoreCompletesBeforeError
                ? .inactive
                : .failed(message: restoreError.localizedDescription, restorationPending: true)
            throw restoreError
        }
        ifDSPModeState = .inactive
    }
}

@MainActor
private final class IFDSPTestStream: IFDSPLiveStreaming {
    private(set) var currentState: IFDSPLiveStreamState = .idle
    private(set) var configuration: IFDSPConfiguration = .standard
    let monitoringState = IFDSPMonitoringState.unavailable(reason: "Test monitoring is off.")
    let updates: AsyncStream<IFDSPLiveStreamState>

    private let continuation: AsyncStream<IFDSPLiveStreamState>.Continuation
    private let events: IFDSPTestEvents
    private let startResult: IFDSPLiveStreamState

    init(events: IFDSPTestEvents, startResult: IFDSPLiveStreamState) {
        self.events = events
        self.startResult = startResult
        let stream = AsyncStream.makeStream(
            of: IFDSPLiveStreamState.self,
            bufferingPolicy: .bufferingNewest(16)
        )
        updates = stream.stream
        continuation = stream.continuation
        continuation.yield(currentState)
    }

    func start() async {
        events.values.append("audio.start")
        publish(startResult)
    }

    func stop() {
        events.values.append("audio.stop")
        publish(.idle)
    }

    func setConfiguration(_ configuration: IFDSPConfiguration) async {
        self.configuration = configuration
    }

    func publish(_ state: IFDSPLiveStreamState) {
        currentState = state
        continuation.yield(state)
    }
}

@MainActor
private final class IFDSPTestRadioController: RadioControlling {
    private(set) var currentState = IFDSPTestRadioController.connectedState
    let updates: AsyncStream<RadioWorkspaceState>

    private let continuation: AsyncStream<RadioWorkspaceState>.Continuation
    private let events: IFDSPTestEvents

    init(events: IFDSPTestEvents) {
        self.events = events
        let stream = AsyncStream.makeStream(
            of: RadioWorkspaceState.self,
            bufferingPolicy: .bufferingNewest(8)
        )
        updates = stream.stream
        continuation = stream.continuation
        continuation.yield(currentState)
    }

    func connect() async throws {
        events.values.append("connection.connect")
        currentState = Self.connectedState
        continuation.yield(currentState)
    }

    func disconnect() async {
        events.values.append("connection.disconnect")
        currentState = .disconnected
        continuation.yield(currentState)
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
        connection: .connected(device: "TH-D75", transport: "USB-C"),
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

@MainActor
private final class IFDSPTestPlanner: AssistantPlanning {
    let availability = AssistantAvailability.unavailable(reason: "Not used by IF-DSP tests.")

    func propose(
        request: String,
        catalog: RadioSettingCatalog,
        currentValues: [String: ProposedSettingValue]
    ) async throws -> AssistantPlan {
        throw IFDSPTestError.unexpectedAssistant
    }
}

private enum IFDSPTestError: LocalizedError {
    case retuneRejected
    case restoreRejected
    case unexpectedAssistant

    var errorDescription: String? {
        switch self {
        case .retuneRejected:
            "The retune was rejected."
        case .restoreRejected:
            "The saved radio state could not be restored."
        case .unexpectedAssistant:
            "The IF-DSP test invoked the Assistant unexpectedly."
        }
    }
}

private extension IFDSPRadioModeStatus {
    static let testStatus = IFDSPRadioModeStatus(
        bandBFrequencyHz: 145_500_000,
        ifCenterHz: 12_000
    )
}

private extension IFDSPLiveStreamState {
    static let testStreaming = IFDSPLiveStreamState.streaming(
        route: IFDSPInputRoute(
            name: "TH-D75 USB Audio",
            kind: .usbAudio,
            sourceSampleRate: 48_000,
            sourceChannelCount: 1
        ),
        frame: nil
    )
}
