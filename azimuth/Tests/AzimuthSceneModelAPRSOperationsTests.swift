// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import XCTest
@testable import Azimuth

@MainActor
final class AzimuthSceneModelAPRSOperationsTests: XCTestCase {
    func testActivatePropagatesLiveAPRSUpdates() async {
        let aprs = SceneModelAPRSFakeController(initialState: makeAPRSState())
        let model = makeModel(aprsController: aprs)
        model.activate()

        let activity = makeActivity(
            sequence: 18,
            direction: .rx,
            kind: .position,
            summary: "N0CALL-7 reported a position"
        )
        let updatedState = makeAPRSState(
            phase: .active,
            sessionID: 4,
            activities: [activity],
            stations: [
                APRSStation(
                    callsign: "N0CALL-7",
                    lastHeard: activity.timestamp,
                    packetCount: 1,
                    latitude: 42.3601,
                    longitude: -71.0589,
                    speedKnots: nil,
                    courseDegrees: nil,
                    path: ["WIDE1-1"],
                    latestSummary: activity.summary
                ),
            ]
        )

        aprs.publish(updatedState)

        await assertEventually(model.aprsState == updatedState) {
            model.aprsState == updatedState
        }
    }

    func testStartAndStopDelegateConfigurationAndRefreshObservableState() async {
        let aprs = SceneModelAPRSFakeController(initialState: makeAPRSState())
        let model = makeModel(aprsController: aprs)
        let configuration = APRSSessionConfiguration(
            stationCallsign: "W1AW-9",
            path: "WIDE1-1,WIDE2-1",
            dataRate: .bps9600,
            symbolTable: "/",
            symbolCode: ">",
            txDelay10ms: 35,
            persistence: 96,
            slotTime10ms: 8,
            txTail10ms: 4,
            fullDuplex: false
        )

        await model.startAPRS(configuration)

        XCTAssertEqual(aprs.startConfigurations, [configuration])
        XCTAssertEqual(model.aprsState.status.phase, .active)
        XCTAssertEqual(model.aprsState.status.configuration, configuration)
        XCTAssertNil(model.operationError)

        await model.stopAPRS()

        XCTAssertEqual(aprs.stopCallCount, 1)
        XCTAssertEqual(model.aprsState.status.phase, .inactive)
        XCTAssertNil(model.operationError)
    }

    func testCurrentModeRefusalOffersOneExplicitRecoveryWithoutRawCommand() async {
        let aprs = SceneModelAPRSFakeController(initialState: makeAPRSState())
        aprs.startError = RadioControllerError.aprsDVGatewayRecoveryRequired
        aprs.recoveryAvailable = true
        let model = makeModel(
            radioController: SceneModelAPRSFakeRadioController(connected: true),
            aprsController: aprs
        )

        await model.startAPRS(.receiveOnly)

        XCTAssertEqual(aprs.startConfigurations, [.receiveOnly])
        XCTAssertNil(model.operationError)
        guard case .offer(let connectionName, let available) =
                model.aprsDVGatewayRecoveryAlert else {
            return XCTFail("Expected an explicit APRS recovery offer")
        }
        XCTAssertEqual(connectionName, "USB-C")
        XCTAssertTrue(available)
        let message = model.aprsDVGatewayRecoveryAlert?.message ?? ""
        XCTAssertTrue(message.contains("Menu 983"))
        XCTAssertTrue(message.contains("Menu 506"))
        XCTAssertTrue(message.contains("Menu 650"))
        XCTAssertTrue(message.contains("freshly verified band"))
        XCTAssertFalse(message.contains("TN command"))
        XCTAssertFalse(message.contains("TN 2,"))
        XCTAssertEqual(aprs.recoveryCallCount, 0)

        model.dismissAPRSDVGatewayRecoveryAlert()

        XCTAssertNil(model.aprsDVGatewayRecoveryAlert)
        XCTAssertEqual(aprs.discardRecoveryCallCount, 1)
        XCTAssertEqual(aprs.recoveryCallCount, 0)
    }

    func testApprovedRecoveryRetriesRetainedConfigurationOnce() async {
        let aprs = SceneModelAPRSFakeController(initialState: makeAPRSState())
        aprs.startError = RadioControllerError.aprsDVGatewayRecoveryRequired
        aprs.recoveryAvailable = true
        let model = makeModel(
            radioController: SceneModelAPRSFakeRadioController(connected: true),
            aprsController: aprs
        )

        await model.startAPRS(.receiveOnly)
        aprs.startError = nil
        await model.inspectDVGatewayAndRetryAPRS()

        XCTAssertEqual(aprs.recoveryCallCount, 1)
        XCTAssertEqual(model.aprsState.status.phase, .active)
        XCTAssertEqual(model.aprsState.status.configuration, .receiveOnly)
        XCTAssertNil(model.aprsDVGatewayRecoveryAlert)
        XCTAssertNil(model.operationError)
    }

    func testAutomaticAlertDismissalDoesNotDiscardApprovedRecoveryProof() async {
        let aprs = SceneModelAPRSFakeController(initialState: makeAPRSState())
        aprs.startError = RadioControllerError.aprsDVGatewayRecoveryRequired
        aprs.recoveryAvailable = true
        let model = makeModel(
            radioController: SceneModelAPRSFakeRadioController(connected: true),
            aprsController: aprs
        )

        await model.startAPRS(.receiveOnly)
        model.hideAPRSDVGatewayRecoveryAlertPresentation()

        XCTAssertNil(model.aprsDVGatewayRecoveryAlert)
        XCTAssertEqual(aprs.discardRecoveryCallCount, 0)
        XCTAssertTrue(aprs.automaticAPRSDVGatewayRecoveryAvailable)

        aprs.startError = nil
        await model.inspectDVGatewayAndRetryAPRS()

        XCTAssertEqual(aprs.recoveryCallCount, 1)
        XCTAssertEqual(aprs.discardRecoveryCallCount, 0)
        XCTAssertEqual(model.aprsState.status.phase, .active)
        XCTAssertNil(model.operationError)
    }

    func testApprovedRecoveryFailureIsDismissOnlyAndDoesNotLoopOffer() async {
        let aprs = SceneModelAPRSFakeController(initialState: makeAPRSState())
        aprs.startError = RadioControllerError.aprsDVGatewayRecoveryRequired
        aprs.recoveryAvailable = true
        aprs.recoveryError = RadioControllerError.operationFailed(
            "The TH-D75 still refused KISS after the one approved retry."
        )
        let model = makeModel(
            radioController: SceneModelAPRSFakeRadioController(connected: true),
            aprsController: aprs
        )

        await model.startAPRS(.receiveOnly)
        await model.inspectDVGatewayAndRetryAPRS()

        XCTAssertEqual(aprs.recoveryCallCount, 1)
        guard case .failed(let message) = model.aprsDVGatewayRecoveryAlert else {
            return XCTFail("Expected a dismiss-only recovery failure")
        }
        XCTAssertTrue(message.contains("one approved retry"))
        XCTAssertFalse(message.contains("TN"))
        XCTAssertFalse(
            model.aprsDVGatewayRecoveryAlert?.automaticRecoveryAvailable ?? true
        )
        XCTAssertNil(model.operationError)
    }

    func testMessageAndPositionSendsDelegateExactArgumentsAndReturnActivity() async throws {
        let aprs = SceneModelAPRSFakeController(
            initialState: makeAPRSState(phase: .active, sessionID: 7)
        )
        let model = makeModel(aprsController: aprs)
        let expectedMessage = makeActivity(
            sequence: 31,
            sessionID: 7,
            direction: .tx,
            kind: .message,
            summary: "Message to W1AW"
        )
        let expectedPosition = makeActivity(
            sequence: 32,
            sessionID: 7,
            direction: .tx,
            kind: .position,
            summary: "Position transmitted",
            latitude: 42.3601,
            longitude: -71.0589
        )
        aprs.nextMessageActivity = expectedMessage
        aprs.nextPositionActivity = expectedPosition

        let message = try await model.sendAPRSMessage(
            addressee: "W1AW",
            text: "Testing from Azimuth",
            messageID: "42"
        )
        let position = try await model.sendAPRSPosition(
            latitude: 42.3601,
            longitude: -71.0589,
            comment: "Portable"
        )

        XCTAssertEqual(
            aprs.messageRequests,
            [.init(addressee: "W1AW", text: "Testing from Azimuth", messageID: "42")]
        )
        XCTAssertEqual(
            aprs.positionRequests,
            [.init(latitude: 42.3601, longitude: -71.0589, comment: "Portable")]
        )
        XCTAssertEqual(message, expectedMessage)
        XCTAssertEqual(position, expectedPosition)
        XCTAssertEqual(model.aprsState.activities, [expectedMessage, expectedPosition])
        XCTAssertEqual(model.aprsState.status.transmittedPackets, 2)
        XCTAssertNil(model.operationError)
    }

    func testSendFailureIsRethrownAndPublishedAsOperationError() async {
        let aprs = SceneModelAPRSFakeController(
            initialState: makeAPRSState(phase: .active, sessionID: 9)
        )
        aprs.messageError = SceneModelAPRSTestError.transmitRejected
        let model = makeModel(aprsController: aprs)

        do {
            _ = try await model.sendAPRSMessage(
                addressee: "W1AW",
                text: "This should fail",
                messageID: nil
            )
            XCTFail("Expected the APRS controller error to be rethrown")
        } catch {
            XCTAssertEqual(error as? SceneModelAPRSTestError, .transmitRejected)
        }

        XCTAssertEqual(aprs.messageRequests.count, 1)
        XCTAssertEqual(model.operationError, "The packet transmitter rejected the frame.")
        XCTAssertFalse(model.isAPRSOperationInFlight)
    }

    func testBackgroundDisconnectInvalidatesAPRSAndForegroundDoesNotResumeIt() async {
        let aprs = SceneModelAPRSFakeController(initialState: makeAPRSState())
        let radio = SceneModelAPRSFakeRadioController(connected: true)
        radio.onDisconnect = { aprs.transportDisconnected() }
        let model = makeModel(radioController: radio, aprsController: aprs)
        model.activate()

        let configuration = APRSSessionConfiguration(
            stationCallsign: "W1AW-9",
            path: "WIDE1-1,WIDE2-1",
            dataRate: .bps1200,
            symbolTable: "/",
            symbolCode: ">",
            txDelay10ms: 50,
            persistence: 128,
            slotTime10ms: 10,
            txTail10ms: 3,
            fullDuplex: false
        )
        await model.startAPRS(configuration)
        XCTAssertEqual(aprs.startConfigurations.count, 1)
        XCTAssertEqual(model.aprsState.status.phase, .active)

        await model.handleScenePhaseBackground().value
        await assertEventually(model.aprsState.status.phase == .inactive) {
            model.aprsState.status.phase == .inactive
        }

        XCTAssertEqual(radio.disconnectCallCount, 1)
        XCTAssertEqual(model.radioState.connection, .disconnected)

        await model.handleScenePhaseActive().value

        XCTAssertEqual(radio.connectCallCount, 1)
        XCTAssertTrue(model.radioState.connection.isConnected)
        XCTAssertEqual(
            aprs.startConfigurations.count,
            1,
            "Foreground restoration may reconnect CAT, but must not silently restart KISS/APRS"
        )
        XCTAssertEqual(model.aprsState.status.phase, .inactive)
    }

    private func makeModel(
        radioController: (any RadioControlling)? = nil,
        aprsController: any APRSControlling
    ) -> AzimuthSceneModel {
        AzimuthSceneModel(
            radioController: radioController ?? SceneModelAPRSFakeRadioController(connected: false),
            catalogProvider: PreviewRadioSettingCatalogProvider(),
            assistantPlanner: SceneModelAPRSUnusedPlanner(),
            aprsController: aprsController,
            initialCatalog: .designPreview
        )
    }

    private func assertEventually(
        _ initialCondition: @autoclosure () -> Bool,
        timeout: TimeInterval = 1,
        condition: @escaping @MainActor () -> Bool,
        file: StaticString = #filePath,
        line: UInt = #line
    ) async {
        if initialCondition() { return }
        let deadline = Date().addingTimeInterval(timeout)
        while !condition(), Date() < deadline {
            try? await Task.sleep(nanoseconds: 5_000_000)
        }
        XCTAssertTrue(condition(), "Timed out waiting for the APRS state update", file: file, line: line)
    }
}

@MainActor
private final class SceneModelAPRSFakeController: APRSControlling {
    struct MessageRequest: Equatable {
        let addressee: String
        let text: String
        let messageID: String?
    }

    struct PositionRequest: Equatable {
        let latitude: Double
        let longitude: Double
        let comment: String
    }

    private(set) var currentAPRSState: APRSOperationalState
    let aprsUpdates: AsyncStream<APRSOperationalState>

    private let continuation: AsyncStream<APRSOperationalState>.Continuation
    private(set) var startConfigurations: [APRSSessionConfiguration] = []
    private(set) var stopCallCount = 0
    private(set) var messageRequests: [MessageRequest] = []
    private(set) var positionRequests: [PositionRequest] = []
    private(set) var recoveryCallCount = 0
    private(set) var discardRecoveryCallCount = 0
    var nextMessageActivity: APRSActivity?
    var nextPositionActivity: APRSActivity?
    var messageError: (any Error)?
    var positionError: (any Error)?
    var startError: (any Error)?
    var recoveryError: (any Error)?
    var recoveryAvailable = false

    var automaticAPRSDVGatewayRecoveryAvailable: Bool {
        recoveryAvailable
    }

    init(initialState: APRSOperationalState) {
        currentAPRSState = initialState
        let stream = AsyncStream.makeStream(
            of: APRSOperationalState.self,
            bufferingPolicy: .bufferingNewest(16)
        )
        aprsUpdates = stream.stream
        continuation = stream.continuation
    }

    func publish(_ state: APRSOperationalState) {
        currentAPRSState = state
        continuation.yield(state)
    }

    func startAPRS(_ configuration: APRSSessionConfiguration) async throws {
        startConfigurations.append(configuration)
        if let startError { throw startError }
        activate(configuration)
    }

    func recoverDVGatewayAndRetryAPRS() async throws {
        recoveryCallCount += 1
        recoveryAvailable = false
        if let recoveryError { throw recoveryError }
        guard let configuration = startConfigurations.last else {
            throw SceneModelAPRSTestError.missingStubbedActivity
        }
        activate(configuration)
    }

    func discardAPRSDVGatewayRecovery() {
        discardRecoveryCallCount += 1
        recoveryAvailable = false
    }

    private func activate(_ configuration: APRSSessionConfiguration) {
        var state = currentAPRSState
        state.status.phase = .active
        state.status.sessionID &+= 1
        state.status.startedAt = Date(timeIntervalSince1970: 1_753_984_800)
        state.status.configuration = configuration
        state.status.lastError = nil
        publish(state)
    }

    func stopAPRS() async throws {
        stopCallCount += 1
        var state = currentAPRSState
        state.status.phase = .inactive
        state.status.configuration = nil
        state.status.lastError = nil
        publish(state)
    }

    func sendAPRSMessage(
        addressee: String,
        text: String,
        messageID: String?
    ) async throws -> APRSActivity {
        messageRequests.append(.init(addressee: addressee, text: text, messageID: messageID))
        if let messageError { throw messageError }
        guard let activity = nextMessageActivity else {
            throw SceneModelAPRSTestError.missingStubbedActivity
        }
        recordTransmission(activity)
        return activity
    }

    func sendAPRSPosition(
        latitude: Double,
        longitude: Double,
        comment: String
    ) async throws -> APRSActivity {
        positionRequests.append(.init(latitude: latitude, longitude: longitude, comment: comment))
        if let positionError { throw positionError }
        guard let activity = nextPositionActivity else {
            throw SceneModelAPRSTestError.missingStubbedActivity
        }
        recordTransmission(activity)
        return activity
    }

    func transportDisconnected() {
        var state = currentAPRSState
        state.status.phase = .inactive
        state.status.configuration = nil
        state.status.lastError = nil
        publish(state)
    }

    private func recordTransmission(_ activity: APRSActivity) {
        var state = currentAPRSState
        state.activities.append(activity)
        state.latestSequence = activity.sequence
        state.status.transmittedPackets &+= 1
        publish(state)
    }
}

@MainActor
private final class SceneModelAPRSFakeRadioController: RadioControlling {
    private(set) var currentState: RadioWorkspaceState
    let updates: AsyncStream<RadioWorkspaceState>

    private let continuation: AsyncStream<RadioWorkspaceState>.Continuation
    private(set) var connectCallCount = 0
    private(set) var disconnectCallCount = 0
    var onDisconnect: (@MainActor () -> Void)?

    init(connected: Bool) {
        currentState = connected ? Self.connectedState : .disconnected
        let stream = AsyncStream.makeStream(
            of: RadioWorkspaceState.self,
            bufferingPolicy: .bufferingNewest(8)
        )
        updates = stream.stream
        continuation = stream.continuation
    }

    func connect() async throws {
        connectCallCount += 1
        currentState = Self.connectedState
        continuation.yield(currentState)
    }

    func disconnect() async {
        disconnectCallCount += 1
        currentState = .disconnected
        continuation.yield(currentState)
        onDisconnect?()
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
private final class SceneModelAPRSUnusedPlanner: AssistantPlanning {
    let availability = AssistantAvailability.unavailable(reason: "Not used by APRS lifecycle tests.")

    func propose(
        request: String,
        catalog: RadioSettingCatalog,
        currentValues: [String: ProposedSettingValue]
    ) async throws -> AssistantPlan {
        throw RadioControllerError.operationFailed("The APRS lifecycle test invoked the Assistant unexpectedly.")
    }
}

private enum SceneModelAPRSTestError: LocalizedError, Equatable {
    case transmitRejected
    case missingStubbedActivity

    var errorDescription: String? {
        switch self {
        case .transmitRejected:
            "The packet transmitter rejected the frame."
        case .missingStubbedActivity:
            "The test did not provide a packet activity result."
        }
    }
}

private func makeAPRSState(
    phase: APRSSessionPhase = .inactive,
    sessionID: UInt64 = 0,
    activities: [APRSActivity] = [],
    stations: [APRSStation] = []
) -> APRSOperationalState {
    APRSOperationalState(
        status: APRSSessionStatus(
            phase: phase,
            sessionID: sessionID,
            startedAt: phase == .active ? Date(timeIntervalSince1970: 1_753_984_800) : nil,
            configuration: nil,
            receivedPackets: UInt64(activities.filter { $0.direction == .rx }.count),
            transmittedPackets: UInt64(activities.filter { $0.direction == .tx }.count),
            decodeFailures: 0,
            droppedActivities: 0,
            lastError: nil
        ),
        activities: activities,
        stations: stations,
        latestSequence: activities.last?.sequence ?? 0,
        historyTruncated: false
    )
}

private func makeActivity(
    sequence: UInt64,
    sessionID: UInt64 = 1,
    direction: APRSActivityDirection,
    kind: APRSActivityKind,
    summary: String,
    latitude: Double? = nil,
    longitude: Double? = nil
) -> APRSActivity {
    APRSActivity(
        sequence: sequence,
        sessionID: sessionID,
        timestamp: Date(timeIntervalSince1970: 1_753_984_800 + TimeInterval(sequence)),
        direction: direction,
        kind: kind,
        source: direction == .rx ? "N0CALL-7" : "W1AW-9",
        destination: direction == .tx ? "W1AW" : "APRS",
        path: ["WIDE1-1"],
        summary: summary,
        rawPacket: "N0CALL-7>APRS,WIDE1-1:>Azimuth test",
        rawAX25: Data([0x82, 0xA0, 0xA4, 0xA6]),
        latitude: latitude,
        longitude: longitude,
        speedKnots: nil,
        courseDegrees: nil
    )
}
