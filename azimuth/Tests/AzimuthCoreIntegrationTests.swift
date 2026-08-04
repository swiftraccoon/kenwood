// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import XCTest
@testable import Azimuth

final class AzimuthCoreCatalogTests: XCTestCase {
    func testGeneratedCatalogProjectsAllFourHundredAuthoritativeSettings() async throws {
        let catalog = try await AzimuthCoreCatalogProvider().catalog()

        XCTAssertEqual(catalog.definitions.count, 400)
        XCTAssertFalse(catalog.definitions.contains { $0.id.hasPrefix("preview.") })
        XCTAssertEqual(Set(catalog.definitions.map(\.id)).count, 400)
        XCTAssertNotNil(catalog.definition(id: "radio.Beep"))
        XCTAssertNotNil(catalog.definition(id: "gps.BuiltInGps"))
        XCTAssertNotNil(catalog.definition(id: "aprs.MessageGroup"))
        if case .reviewedSchema(let version) = catalog.source {
            XCTAssertEqual(version, "MCP-D75 schema 3")
        } else {
            XCTFail("Shipping catalog must identify the reviewed schema")
        }
    }

    func testHandheldMenuNumbersAreCompleteConservativeAndSearchable() async throws {
        let catalog = try await AzimuthCoreCatalogProvider().catalog()

        XCTAssertEqual(catalog.definition(id: "radio.Beep")?.menuNumbers, ["914"])
        XCTAssertEqual(catalog.definition(id: "radio.UsbAudioOutLevel")?.menuNumbers, ["91A"])
        XCTAssertEqual(catalog.definition(id: "radio.UsbFunction")?.menuNumbers, ["980"])
        XCTAssertEqual(
            catalog.definition(id: "gps.MyPositionList[4].Name")?.menuNumbers,
            ["401"]
        )
        XCTAssertEqual(
            catalog.definition(id: "aprs.ObjectList[2].ObjectComment")?.menuNumbers,
            ["516"]
        )
        XCTAssertEqual(
            catalog.definition(id: "dv.MyCallsignDvGatewayList[5].MyCallsignDvGateway")?.menuNumbers,
            ["651"]
        )

        let intentionallyUnnumbered = Set(
            [
                "radio.PoweronBitmap",
                "radio.EarphoneAntenna",
                "aprs.CursorControl",
                "aprs.NavitraGroupModeOnOff",
                "aprs.NavitraGroupCode",
                "aprs.NavitraMessageSelect",
                "aprs.BeaconType",
            ] + (0..<5).map { "aprs.NavitraMessageList[\($0)].NavitraMessage" }
        )
        let actualUnnumbered = Set(
            catalog.definitions
                .filter { $0.menuNumbers.isEmpty }
                .map(\.id)
        )

        XCTAssertEqual(actualUnnumbered, intentionallyUnnumbered)
        XCTAssertEqual(catalog.definitions.filter { !$0.menuNumbers.isEmpty }.count, 388)
        XCTAssertTrue(
            catalog.definitions
                .flatMap(\.menuNumbers)
                .allSatisfy { $0.range(of: #"^[0-9]{2}[0-9A-F]$"#, options: .regularExpression) != nil }
        )
        XCTAssertTrue(
            catalog.filtered(query: "980", group: nil).contains { $0.id == "radio.UsbFunction" }
        )
    }

    func testGeneratedTextEncodingsMatchAuthoritativeRadioConstraints() async throws {
        let catalog = try await AzimuthCoreCatalogProvider().catalog()
        let ascii = try XCTUnwrap(catalog.definition(id: "radio.PowerOnMessage"))
        let utf8 = try XCTUnwrap(catalog.definition(id: "radio.BluetoothDeviceName"))

        guard case .text(let asciiLength, let asciiEncoding) = ascii.domain,
              case .text(let utf8Length, let utf8Encoding) = utf8.domain else {
            return XCTFail("Expected generated text domains")
        }
        XCTAssertEqual(asciiLength, 16)
        XCTAssertEqual(asciiEncoding, .ascii)
        XCTAssertFalse(ascii.domain.accepts(.text("café")))
        XCTAssertEqual(utf8Length, 19)
        XCTAssertEqual(utf8Encoding, .utf8)
        XCTAssertTrue(utf8.domain.accepts(.text("café")))
    }

    func testAssistantRejectsMemoryMapTextBeforeAcceptAndPreservesWhitespace() async throws {
        let catalog = try await AzimuthCoreCatalogProvider().catalog()
        let id = "radio.PowerOnMessage"
        let invalid = AssistantPlanValidator.validate(
            request: "Set the power-on message",
            draft: AssistantPlanDraft(
                summary: "Set the message.",
                needsClarification: false,
                changes: [
                    .init(settingID: id, proposedValue: "café", rationale: "Requested text"),
                ]
            ),
            catalog: catalog,
            currentValues: [id: .text("HELLO")]
        )
        let exact = AssistantPlanValidator.validate(
            request: "Pad the power-on message",
            draft: AssistantPlanDraft(
                summary: "Pad the message.",
                needsClarification: false,
                changes: [
                    .init(settingID: id, proposedValue: "  CQ  ", rationale: "Exact text"),
                ]
            ),
            catalog: catalog,
            currentValues: [id: .text("HELLO")]
        )

        guard case .invalidValue = invalid.changes.first?.validation else {
            return XCTFail("Non-ASCII memory-map text must fail before review")
        }
        XCTAssertFalse(invalid.isFullyValidated)
        XCTAssertEqual(exact.changes.first?.proposedValue, .text("  CQ  "))
        XCTAssertEqual(exact.changes.first?.proposedValueText, "  CQ  ")
        XCTAssertEqual(exact.changes.first?.validation, .validated)
    }

    func testAllScaledFieldsUseEditableDisplayUnitDomainsWhileBlobStaysSpecialized() async throws {
        let catalog = try await AzimuthCoreCatalogProvider().catalog()
        let records = settingCatalog()
        let schema = try AzimuthCoreSettingSchema(records: records)
        let scaledRecords = records.filter { $0.presentation == .scaledInteger }
        let blobRecords = records.filter { $0.presentation == .blob }

        XCTAssertEqual(schema.scalarSettingCount, 399)
        XCTAssertEqual(schema.deferredBlobCount, 1)
        XCTAssertEqual(scaledRecords.count, 16)
        for record in scaledRecords {
            let definition = try XCTUnwrap(catalog.definition(id: record.id))
            XCTAssertFalse(definition.isSpecializedEditor, record.id)
            guard case .scaledInteger(let scale) = definition.domain else {
                return XCTFail("\(record.id) did not receive a scaled integer domain")
            }
            XCTAssertEqual(scale.inputUnit, record.storageTransform?.inputUnit)
            XCTAssertEqual(scale.numerator, record.storageTransform?.numerator)
            XCTAssertEqual(scale.denominator, record.storageTransform?.denominator)
            XCTAssertEqual(
                scale.displayDecimalPlaces,
                record.storageTransform.map { Int($0.displayDecimalPlaces) }
            )
        }

        XCTAssertEqual(blobRecords.map(\.id), ["radio.PoweronBitmap"])
        for record in blobRecords {
            XCTAssertEqual(catalog.definition(id: record.id)?.isSpecializedEditor, true, record.id)
        }
    }

    func testScaledCoordinateRoundTripsDisplaySecondsToExactRawStorage() async throws {
        let catalog = try await AzimuthCoreCatalogProvider().catalog()
        let id = "gps.MyPositionList[0].LatitudeSecondEncoded"
        let definition = try XCTUnwrap(catalog.definition(id: id))
        guard case .scaledInteger(let scale) = definition.domain else {
            return XCTFail("Expected an editable scaled coordinate")
        }

        XCTAssertEqual(scale.summary, "0.0–59.9 seconds, precision 0.1 seconds")
        XCTAssertEqual(scale.rawValue(displayText: "30.0"), 5_000)
        XCTAssertEqual(scale.rawValue(displayText: "30 seconds"), 5_000)
        XCTAssertEqual(scale.displayText(rawValue: 5_000), "30.0 seconds")
        XCTAssertEqual(try encodeSettingDisplayValue(settingId: id, displayValue: 30), 5_000)
        XCTAssertEqual(try decodeSettingDisplayValue(settingId: id, rawValue: 5_000), 30)
        XCTAssertNil(scale.rawValue(displayText: "30.01 seconds"))
        XCTAssertNil(scale.rawValue(displayText: "60.0 seconds"))

        for tenth in 0...599 {
            let displayText = "\(tenth / 10).\(tenth % 10)"
            let coreRaw = try encodeSettingDisplayValue(
                settingId: id,
                displayValue: Double(tenth) / 10
            )
            XCTAssertEqual(scale.rawValue(displayText: displayText), Int(coreRaw), displayText)
            XCTAssertEqual(scale.displayText(rawValue: Int(coreRaw)), "\(displayText) seconds")
        }
    }

    func testAssistantConvertsScaledDisplayValueIntoValidatedRawChange() async throws {
        let catalog = try await AzimuthCoreCatalogProvider().catalog()
        let id = "gps.MyPositionList[0].LatitudeSecondEncoded"
        let draft = AssistantPlanDraft(
            summary: "Set the latitude seconds.",
            needsClarification: false,
            changes: [
                .init(
                    settingID: id,
                    proposedValue: "30.0 seconds",
                    rationale: "Use the requested coordinate"
                ),
            ]
        )

        let plan = AssistantPlanValidator.validate(
            request: "Set latitude seconds to 30",
            draft: draft,
            catalog: catalog,
            currentValues: [id: .integer(0)]
        )

        XCTAssertEqual(plan.changes.first?.previousValue, .integer(0))
        XCTAssertEqual(plan.changes.first?.proposedValue, .integer(5_000))
        XCTAssertEqual(plan.changes.first?.validation, .validated)
        XCTAssertTrue(plan.isFullyValidated)
    }

    func testAssistantRejectsScaledValuesOutsideDeclaredDisplayPrecision() async throws {
        let catalog = try await AzimuthCoreCatalogProvider().catalog()
        let id = "gps.MyPositionList[0].LatitudeSecondEncoded"
        let draft = AssistantPlanDraft(
            summary: "Set an over-precise coordinate.",
            needsClarification: false,
            changes: [
                .init(settingID: id, proposedValue: "30.01", rationale: "Coordinate"),
            ]
        )

        let plan = AssistantPlanValidator.validate(
            request: "Set latitude seconds",
            draft: draft,
            catalog: catalog,
            currentValues: [id: .integer(0)]
        )

        guard case .invalidValue = plan.changes.first?.validation else {
            return XCTFail("Over-precise display values must not produce a raw write")
        }
        XCTAssertNil(plan.changes.first?.proposedValue)
        XCTAssertFalse(plan.isFullyValidated)
    }
}

@MainActor
final class AzimuthLiveRadioControllerTests: XCTestCase {
    func testApprovedListExecutesAsOneCoreBatchAndPublishesVerifiedValues() async throws {
        let transport = IntegrationTestTransport()
        let core = IntegrationTestCore()
        let controller = try makeController(transport: transport, core: core)
        try await controller.connect()

        XCTAssertEqual(controller.currentState.telemetry.firmware, "V1.03.AZM")
        XCTAssertEqual(controller.currentState.telemetry.operatingMode, "Automation ABI 3")

        let report = try await controller.applySettings(
            [
                ValidatedRadioSettingChange(
                    settingID: "radio.Beep",
                    previousValue: .boolean(true),
                    targetValue: .boolean(false)
                ),
            ],
            progress: { _ in }
        )

        XCTAssertEqual(core.applyCallCount, 1)
        XCTAssertTrue(report.succeeded)
        XCTAssertEqual(controller.currentState.settingValues["radio.Beep"], .boolean(false))
        await controller.disconnect()
    }

    func testKeyAlwaysUsesFreshestLeaseAfterContinuousCapture() async throws {
        let transport = IntegrationTestTransport()
        let core = IntegrationTestCore()
        let controller = try makeController(transport: transport, core: core)
        try await controller.connect()
        try await Task.sleep(nanoseconds: 380_000_000)

        try await controller.press(.menu)

        XCTAssertEqual(core.guardedTapCallCount, 1)
        XCTAssertEqual(core.lastGuardedLease, core.lastLeasePresentedToTap)
        await controller.disconnect()
    }

    func testMMDVMPreflightStopsBeforeCoreAndExplainsHowToRestoreCAT() async throws {
        let transport = IntegrationTestTransport()
        let core = IntegrationTestCore()
        let connector = IntegrationTestCoreConnector(core: core)
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            connectCore: { transport in
                try await connector.connect(transport: transport)
            },
            prepareRadioForAutomation: { _ in .mmdvm }
        )

        do {
            try await controller.connect()
            XCTFail("MMDVM mode must not enter the strict automation core")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("Menu 650"))
        }

        XCTAssertEqual(connector.callCount, 0)
        guard case .failed(let message) = controller.currentState.connection else {
            return XCTFail("The workspace should publish an actionable connection failure")
        }
        XCTAssertTrue(message.contains("DV Gateway/MMDVM"))
        XCTAssertTrue(message.contains("Menu 650"))
    }

    func testUnresponsiveCDCSessionIsReopenedOnceBeforeCoreConnection() async throws {
        let transport = IntegrationTestTransport()
        let core = IntegrationTestCore()
        let connector = IntegrationTestCoreConnector(core: core)
        let preflight = IntegrationTestModePreflight(
            modes: [.unresponsive, .cat]
        )
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            connectCore: { transport in
                try await connector.connect(transport: transport)
            },
            prepareRadioForAutomation: { _ in try preflight.nextMode() }
        )

        try await controller.connect()

        XCTAssertEqual(preflight.callCount, 2)
        XCTAssertEqual(transport.openCallCount, 2)
        XCTAssertEqual(transport.closeCallCount, 1)
        XCTAssertEqual(connector.callCount, 1)
        XCTAssertTrue(controller.currentState.connection.isConnected)
        await controller.disconnect()
    }

    func testTwoUnresponsiveCDCSessionsNeverEnterCore() async throws {
        let transport = IntegrationTestTransport()
        let core = IntegrationTestCore()
        let connector = IntegrationTestCoreConnector(core: core)
        let preflight = IntegrationTestModePreflight(
            modes: [.unresponsive, .unresponsive]
        )
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            connectCore: { transport in
                try await connector.connect(transport: transport)
            },
            prepareRadioForAutomation: { _ in try preflight.nextMode() }
        )

        do {
            try await controller.connect()
            XCTFail("Two silent CDC sessions must not enter automation control")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("CDC control-line reset"))
        }

        XCTAssertEqual(preflight.callCount, 2)
        XCTAssertEqual(transport.openCallCount, 2)
        XCTAssertEqual(transport.closeCallCount, 2)
        XCTAssertEqual(connector.callCount, 0)
    }

    func testAPRSOwnsSerialLinkUntilCoreConfirmsExactCATRestoration() async throws {
        let transport = IntegrationTestTransport()
        let core = IntegrationTestCore()
        let controller = try makeController(transport: transport, core: core)
        try await controller.connect()

        var configuration = APRSSessionConfiguration.receiveOnly
        configuration.stationCallsign = "N0CALL-7"
        try await controller.startAPRS(configuration)

        XCTAssertEqual(controller.currentAPRSState.status.phase, .active)
        XCTAssertEqual(
            controller.currentAPRSState.status.configuration?.stationCallsign,
            "N0CALL-7"
        )
        XCTAssertFalse(controller.currentState.capabilities.settingRead.isAvailable)
        XCTAssertFalse(controller.currentState.capabilities.screenStreaming.isAvailable)

        let settingsBeforeStop = core.settingReadCallCount
        let capturesBeforeStop = core.screenCaptureCallCount
        let stopTask = Task { try await controller.stopAPRS() }
        try await waitUntil { core.stopAprsCallCount == 1 }

        XCTAssertEqual(controller.currentAPRSState.status.phase, .restoring)
        XCTAssertEqual(core.settingReadCallCount, settingsBeforeStop)
        XCTAssertEqual(core.screenCaptureCallCount, capturesBeforeStop)
        XCTAssertFalse(controller.currentState.capabilities.settingRead.isAvailable)
        XCTAssertFalse(controller.currentState.capabilities.screenStreaming.isAvailable)

        core.completeAprsStop()
        try await stopTask.value

        XCTAssertEqual(controller.currentAPRSState.status.phase, .inactive)
        XCTAssertGreaterThan(core.settingReadCallCount, settingsBeforeStop)
        XCTAssertGreaterThan(core.screenCaptureCallCount, capturesBeforeStop)
        XCTAssertTrue(controller.currentState.capabilities.settingRead.isAvailable)
        XCTAssertTrue(controller.currentState.capabilities.screenStreaming.isAvailable)
        await controller.disconnect()
    }

    func testAPRSStopAutomationRestorationFailureDoesNotReadTerminatedCore() async throws {
        let transport = IntegrationTestTransport()
        let core = IntegrationTestCore()
        let controller = try makeController(transport: transport, core: core)
        try await controller.connect()

        var configuration = APRSSessionConfiguration.receiveOnly
        configuration.stationCallsign = "N0CALL-7"
        try await controller.startAPRS(configuration)

        let settingsBeforeStop = core.settingReadCallCount
        let stopTask = Task { try await controller.stopAPRS() }
        try await waitUntil { core.stopAprsCallCount == 1 }
        core.failAprsStop(
            AutomationError.AutomationRestoration(
                operation: "APRS stop",
                detail: "CAT identity could not be restored"
            )
        )

        do {
            try await stopTask.value
            XCTFail("A failed automation restoration must fail APRS stop")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("CAT identity could not be restored"))
        }

        XCTAssertEqual(
            core.settingReadCallCount,
            settingsBeforeStop,
            "A terminal core restoration failure must not trigger a guaranteed-dead settings read"
        )
        guard case .failed(let message) = controller.currentState.connection else {
            return XCTFail("The failed-closed Rust actor must transition the connection to failed")
        }
        XCTAssertTrue(message.contains("CAT identity could not be restored"))
        XCTAssertFalse(message.contains("CAT recovery also failed"))
        XCTAssertFalse(message.contains("controller ended"))
    }

    func testNonterminalAPRSStopFailureStillAttemptsCATWorkspaceRecovery() async throws {
        let transport = IntegrationTestTransport()
        let core = IntegrationTestCore()
        let controller = try makeController(transport: transport, core: core)
        try await controller.connect()

        try await controller.startAPRS(.receiveOnly)
        let settingsBeforeStop = core.settingReadCallCount
        let stopTask = Task { try await controller.stopAPRS() }
        try await waitUntil { core.stopAprsCallCount == 1 }
        core.failAprsStop(
            AutomationError.AprsOperation(detail: "The stop request was rejected")
        )

        do {
            try await stopTask.value
            XCTFail("The rejected APRS stop must remain visible")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("stop request was rejected"))
        }

        XCTAssertGreaterThan(
            core.settingReadCallCount,
            settingsBeforeStop,
            "A nonterminal APRS error must retain the existing CAT workspace recovery path"
        )
        XCTAssertTrue(controller.currentState.connection.isConnected)
        await controller.disconnect()
    }

    func testAPRSAdapterPreservesExactBytesAndMergesIncrementalRowsOnce() throws {
        let configuration = AprsSessionConfig(
            stationCallsign: "N0CALL-7",
            path: "WIDE1-1",
            dataRate: .bps1200,
            symbolTable: "/",
            symbolCode: ">",
            txDelay10ms: 50,
            persistence: 128,
            slotTime10ms: 10,
            txTail10ms: 3,
            fullDuplex: false
        )
        let firstRecord = IntegrationTestCore.aprsActivity(
            sequence: 41,
            rawAx25: Data([0x82, 0xA0, 0xA4, 0xA6, 0x40, 0x40, 0x60])
        )
        let status = AprsSessionStatus(
            phase: .active,
            sessionId: 9,
            startedAtUnixMs: 1_722_515_040_125,
            configuration: configuration,
            receivedPackets: 1,
            transmittedPackets: 0,
            decodeFailures: 0,
            droppedActivities: 0,
            lastError: nil
        )
        let first = AzimuthCoreAPRSAdapter.operationalState(
            AprsOperationalSnapshot(
                status: status,
                activities: [firstRecord],
                stations: [],
                latestSequence: 41,
                historyTruncated: false
            )
        )
        let merged = AzimuthCoreAPRSAdapter.operationalState(
            AprsOperationalSnapshot(
                status: status,
                activities: [
                    firstRecord,
                    IntegrationTestCore.aprsActivity(
                        sequence: 42,
                        rawAx25: Data([0x03, 0xF0, 0x21])
                    ),
                ],
                stations: [],
                latestSequence: 42,
                historyTruncated: false
            ),
            retaining: first
        )

        XCTAssertEqual(merged.activities.map(\.sequence), [41, 42])
        XCTAssertEqual(merged.activities.first?.rawAX25, firstRecord.rawAx25)
        let firstTimestamp = try XCTUnwrap(
            merged.activities.first?.timestamp.timeIntervalSince1970
        )
        XCTAssertEqual(
            firstTimestamp,
            1_722_515_040.125,
            accuracy: 0.000_1
        )
        XCTAssertEqual(merged.latestSequence, 42)
    }

    private func makeController(
        transport: IntegrationTestTransport,
        core: IntegrationTestCore
    ) throws -> AzimuthLiveRadioController {
        try AzimuthLiveRadioController(
            transport: transport,
            connectCore: { _ in core },
            prepareRadioForAutomation: { _ in .cat }
        )
    }

    private func waitUntil(
        timeoutNanoseconds: UInt64 = 1_000_000_000,
        condition: @escaping @MainActor () -> Bool
    ) async throws {
        let started = ContinuousClock.now
        while !condition() {
            if ContinuousClock.now - started > .nanoseconds(Int64(timeoutNanoseconds)) {
                XCTFail("Timed out waiting for asynchronous controller state")
                return
            }
            try await Task.sleep(nanoseconds: 5_000_000)
        }
    }
}

private final class IntegrationTestCoreConnector: @unchecked Sendable {
    private let lock = NSLock()
    private let core: IntegrationTestCore
    private var calls = 0

    init(core: IntegrationTestCore) {
        self.core = core
    }

    var callCount: Int { lock.withLock { calls } }

    func connect(transport: ByteTransport) async throws -> any AutomationControllerProtocol {
        _ = transport
        lock.withLock { calls += 1 }
        return core
    }
}

private final class IntegrationTestModePreflight: @unchecked Sendable {
    private let lock = NSLock()
    private var remainingModes: [AzimuthRadioWireMode]
    private var calls = 0

    init(modes: [AzimuthRadioWireMode]) {
        remainingModes = modes
    }

    var callCount: Int { lock.withLock { calls } }

    func nextMode() throws -> AzimuthRadioWireMode {
        try lock.withLock {
            guard !remainingModes.isEmpty else {
                throw AzimuthRadioModePreflightError.cdcUnresponsive
            }
            calls += 1
            return remainingModes.removeFirst()
        }
    }
}

private final class IntegrationTestTransport: AzimuthRadioTransport, @unchecked Sendable {
    let device = AzimuthRadioDevice.thD75USBC
    private let lock = NSLock()
    private var currentState: AzimuthRadioTransportState = .disconnected
    private var opens = 0
    private var closes = 0

    var openCallCount: Int { lock.withLock { opens } }
    var closeCallCount: Int { lock.withLock { closes } }

    var state: AzimuthRadioTransportState {
        get async { lock.withLock { currentState } }
    }

    var stateStream: AsyncStream<AzimuthRadioTransportState> {
        AsyncStream { continuation in
            continuation.yield(lock.withLock { currentState })
            continuation.finish()
        }
    }

    func open() async throws {
        lock.withLock {
            opens += 1
            currentState = .connected
        }
    }

    func close() async {
        lock.withLock {
            closes += 1
            currentState = .disconnected
        }
    }

    func setBaudRate(baud: UInt32) throws {}
    func write(_ bytes: [UInt8]) async throws {}
    func read(maxBytes: Int) async throws -> [UInt8] { [] }
}

private final class IntegrationTestCore: AutomationControllerProtocol, @unchecked Sendable {
    private let lock = NSLock()
    private var lease: UInt64 = 0
    private var beep = true
    private var applyCalls = 0
    private var tapCalls = 0
    private var guardedLease: UInt64?
    private var leasePresentedToTap: UInt64?
    private var settingReadCalls = 0
    private var screenCaptureCalls = 0
    private var aprsStopCalls = 0
    private var aprsStatus = IntegrationTestCore.aprsStatus(
        phase: .inactive,
        configuration: nil
    )
    private var aprsRows: [AprsActivityRecord] = []
    private var pendingAprsStop: CheckedContinuation<AprsSessionStatus, Error>?

    var applyCallCount: Int { lock.withLock { applyCalls } }
    var guardedTapCallCount: Int { lock.withLock { tapCalls } }
    var lastGuardedLease: UInt64? { lock.withLock { guardedLease } }
    var lastLeasePresentedToTap: UInt64? { lock.withLock { leasePresentedToTap } }
    var settingReadCallCount: Int { lock.withLock { settingReadCalls } }
    var screenCaptureCallCount: Int { lock.withLock { screenCaptureCalls } }
    var stopAprsCallCount: Int { lock.withLock { aprsStopCalls } }

    func abi() -> AutomationAbiRecord {
        AutomationAbiRecord(version: 3, features: 0x7F, maxKey: 24, maxPhase: 1)
    }

    func applySettingChanges(changes: [SettingChange]) async throws -> SettingApplyReport {
        let result: (previous: UInt64, refreshed: SettingReadResult, changes: [SettingChangeResult]) = lock.withLock {
            applyCalls += 1
            let previous = changes.first?.snapshotId ?? 0
            if let desired = changes.first?.desiredValue,
               case .boolean(let value) = desired {
                beep = value
            }
            let read = SettingReadResult(
                snapshotId: previous + 1,
                values: [SettingValueRecord(settingId: "radio.Beep", value: .boolean(value: beep))]
            )
            let results = changes.map {
                SettingChangeResult(settingId: $0.settingId, outcome: .applied, value: $0.desiredValue)
            }
            return (previous, read, results)
        }
        return SettingApplyReport(
            previousSnapshotId: result.previous,
            pagesWritten: [0],
            changes: result.changes,
            refreshedValues: result.refreshed
        )
    }

    func captureScreen() async throws -> RemoteScreenFrame {
        let nextLease = lock.withLock { () -> UInt64 in
            screenCaptureCalls += 1
            lease += 1
            return lease
        }
        return Self.frame(lease: nextLease)
    }

    func close() async throws {}

    func guardedTap(leaseId: UInt64, key: FrontPanelKey) async throws -> GuardedTapResult {
        let newLease = lock.withLock { () -> UInt64 in
            tapCalls += 1
            guardedLease = leaseId
            leasePresentedToTap = lease
            lease += 1
            return lease
        }
        return GuardedTapResult(disposition: .dispatched, screen: Self.frame(lease: newLease))
    }

    func readSettingValues(settingIds: [String]?) async throws -> SettingReadResult {
        return lock.withLock { () -> SettingReadResult in
            settingReadCalls += 1
            return SettingReadResult(
                snapshotId: 41,
                values: [SettingValueRecord(settingId: "radio.Beep", value: .boolean(value: beep))]
            )
        }
    }

    func aprsSnapshot(afterSequence: UInt64?) -> AprsOperationalSnapshot {
        lock.withLock {
            let rows = aprsRows.filter { row in
                guard let afterSequence else { return true }
                return row.sequence > afterSequence
            }
            return AprsOperationalSnapshot(
                status: aprsStatus,
                activities: rows,
                stations: [],
                latestSequence: aprsRows.last?.sequence ?? 0,
                historyTruncated: false
            )
        }
    }

    func startAprs(config: AprsSessionConfig) async throws -> AprsSessionStatus {
        lock.withLock {
            aprsStatus = Self.aprsStatus(phase: .active, configuration: config)
            aprsRows.append(
                Self.aprsActivity(
                    sequence: UInt64(aprsRows.count + 1),
                    rawAx25: Data(),
                    direction: .system,
                    kind: .session,
                    summary: "APRS KISS active"
                )
            )
            return aprsStatus
        }
    }

    func stopAprs() async throws -> AprsSessionStatus {
        try await withCheckedThrowingContinuation {
            (continuation: CheckedContinuation<AprsSessionStatus, Error>) in
            lock.withLock {
                aprsStopCalls += 1
                pendingAprsStop = continuation
            }
        }
    }

    func completeAprsStop() {
        let completion: (CheckedContinuation<AprsSessionStatus, Error>, AprsSessionStatus)? =
            lock.withLock {
                guard let continuation = pendingAprsStop else { return nil }
                pendingAprsStop = nil
                aprsStatus = Self.aprsStatus(
                    phase: .inactive,
                    configuration: aprsStatus.configuration
                )
                aprsRows.append(
                    Self.aprsActivity(
                        sequence: UInt64(aprsRows.count + 1),
                        rawAx25: Data(),
                        direction: .system,
                        kind: .session,
                        summary: "Qualified automation CAT restored"
                    )
                )
                return (continuation, aprsStatus)
            }
        if let (continuation, status) = completion {
            continuation.resume(returning: status)
        }
    }

    func failAprsStop(_ error: Error) {
        let continuation = lock.withLock {
            let pending = pendingAprsStop
            pendingAprsStop = nil
            return pending
        }
        continuation?.resume(throwing: error)
    }

    func sendAprsMessage(
        addressee: String,
        text: String,
        messageId: String?
    ) async throws -> AprsActivityRecord {
        appendTransmit(summary: "Message to \(addressee): \(text)\(messageId ?? "")")
    }

    func sendAprsPosition(
        latitude: Double,
        longitude: Double,
        comment: String
    ) async throws -> AprsActivityRecord {
        appendTransmit(summary: "Position \(latitude), \(longitude): \(comment)")
    }

    func prepareIfDsp() async throws -> IfDspRadioStatus {
        IfDspRadioStatus(phase: .active, bandBFrequencyHz: 144_390_000, ifCenterHz: 12_000)
    }

    func ifDspStatus() async throws -> IfDspRadioStatus {
        IfDspRadioStatus(phase: .inactive, bandBFrequencyHz: nil, ifCenterHz: 12_000)
    }

    func restoreIfDsp() async throws -> IfDspRadioStatus {
        IfDspRadioStatus(phase: .inactive, bandBFrequencyHz: nil, ifCenterHz: 12_000)
    }

    func retuneIfDsp(frequencyHz: UInt32) async throws -> IfDspRadioStatus {
        IfDspRadioStatus(phase: .active, bandBFrequencyHz: frequencyHz, ifCenterHz: 12_000)
    }

    private func appendTransmit(summary: String) -> AprsActivityRecord {
        lock.withLock {
            let row = Self.aprsActivity(
                sequence: UInt64(aprsRows.count + 1),
                rawAx25: Data([0x03, 0xF0]),
                direction: .tx,
                kind: .message,
                summary: summary
            )
            aprsRows.append(row)
            return row
        }
    }

    fileprivate static func aprsActivity(
        sequence: UInt64,
        rawAx25: Data,
        direction: AprsActivityDirection = .rx,
        kind: AprsActivityKind = .position,
        summary: String = "Position packet"
    ) -> AprsActivityRecord {
        AprsActivityRecord(
            sequence: sequence,
            sessionId: 9,
            timestampUnixMs: 1_722_515_040_125,
            direction: direction,
            kind: kind,
            source: direction == .system ? nil : "N0CALL-7",
            destination: direction == .system ? nil : "APRS",
            path: direction == .system ? [] : ["WIDE1-1"],
            summary: summary,
            rawPacket: direction == .system ? summary : "N0CALL-7>APRS,WIDE1-1:!",
            rawAx25: rawAx25,
            latitude: kind == .position ? 42.3601 : nil,
            longitude: kind == .position ? -71.0589 : nil,
            speedKnots: nil,
            courseDegrees: nil
        )
    }

    private static func aprsStatus(
        phase: AprsSessionPhase,
        configuration: AprsSessionConfig?
    ) -> AprsSessionStatus {
        AprsSessionStatus(
            phase: phase,
            sessionId: phase == .inactive && configuration == nil ? 0 : 9,
            startedAtUnixMs: configuration == nil ? nil : 1_722_515_040_125,
            configuration: configuration,
            receivedPackets: 0,
            transmittedPackets: 0,
            decodeFailures: 0,
            droppedActivities: 0,
            lastError: nil
        )
    }

    private static func frame(lease: UInt64) -> RemoteScreenFrame {
        RemoteScreenFrame(
            leaseId: lease,
            width: 240,
            height: 180,
            rowBytes: 960,
            rgb565Le: Data(repeating: 0, count: 240 * 180 * 2),
            rgba8888: Data(repeating: 0, count: 240 * 180 * 4),
            generation: UInt32(truncatingIfNeeded: lease),
            crc32: 0,
            commandCount: UInt32(truncatingIfNeeded: lease),
            seqlock: 2
        )
    }
}
