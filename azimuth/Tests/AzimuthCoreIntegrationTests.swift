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

    func testDisruptiveCorePoliciesKeepMenus650And980ReadOnly() async throws {
        let catalog = try await AzimuthCoreCatalogProvider().catalog()
        let disruptiveIDs: Set<String> = [
            "dv.DvGatewayModeDvGateway",
            "radio.UsbFunction",
        ]
        let records = settingCatalog().filter { $0.writePolicy == .dedicatedLifecycle }

        XCTAssertEqual(Set(records.map(\.id)), disruptiveIDs)
        for record in records {
            XCTAssertTrue(record.requiresRestart, record.id)
            XCTAssertTrue(record.requiresReconnect, record.id)
            XCTAssertNotNil(record.writeRestriction, record.id)

            let definition = try XCTUnwrap(catalog.definition(id: record.id))
            XCTAssertTrue(definition.isSpecializedEditor, record.id)
            XCTAssertTrue(definition.requiresRestart, record.id)
            XCTAssertTrue(definition.requiresReconnect, record.id)
            XCTAssertEqual(definition.summary, record.writeRestriction, record.id)

            let plan = AssistantPlanValidator.validate(
                request: "Change \(definition.title)",
                draft: AssistantPlanDraft(
                    summary: "Change a disruptive setting.",
                    needsClarification: false,
                    changes: [
                        .init(
                            settingID: record.id,
                            proposedValue: "1",
                            rationale: "Requested change"
                        ),
                    ]
                ),
                catalog: catalog,
                currentValues: [record.id: .choice(rawValue: 0)]
            )
            XCTAssertEqual(plan.changes.first?.validation, .specializedEditorRequired, record.id)
            XCTAssertFalse(plan.isFullyValidated, record.id)
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

#if os(macOS)
@MainActor
final class AzimuthBluetoothHelperPackagingTests: XCTestCase {
    func testBundledRecoveryHelperLaunchesInsideAppSandbox() async throws {
        try await validateBluetoothRecoveryHelper()
    }
}
#endif

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

    func testBluetoothCATPreflightConnectsAndPublishesBluetoothTransport() async throws {
        let transport = IntegrationTestTransport(connectionKind: .bluetooth)
        let core = IntegrationTestCore()
        let connector = IntegrationTestCoreConnector(core: core)
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            connectCore: { transport in
                try await connector.connect(transport: transport)
            },
            prepareRadioForAutomation: { _ in .cat }
        )

        try await controller.connect()

        XCTAssertEqual(transport.openCallCount, 1)
        XCTAssertEqual(connector.callCount, 1)
        XCTAssertEqual(
            controller.currentState.connection,
            .connected(device: "Kenwood TH-D75", transport: "Bluetooth")
        )
        await controller.disconnect()
    }

    func testBluetoothMMDVMRejectsCATWithoutAuthorizingUSBRecovery() async throws {
        let transport = IntegrationTestTransport(connectionKind: .bluetooth)
        let connector = IntegrationTestCoreConnector(core: IntegrationTestCore())
        let recovery = IntegrationTestCATRecovery()
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            connectCore: { transport in
                try await connector.connect(transport: transport)
            },
            prepareRadioForAutomation: { _ in .mmdvm },
            recoverUSBMMDVM: { serialNumber, _ in recovery.operation(serialNumber) },
            automaticCATRecoveryAvailable: true
        )

        do {
            try await controller.connect()
            XCTFail("Bluetooth MMDVM mode must not enter CAT automation")
        } catch let error as RadioControllerError {
            guard case .operationFailed(let reason) = error else {
                return XCTFail("Bluetooth MMDVM must be an operation error, got \(error)")
            }
            XCTAssertTrue(reason.contains("selected Bluetooth interface"))
            XCTAssertTrue(reason.contains("choose USB-C"))
        }

        XCTAssertEqual(transport.openCallCount, 1)
        XCTAssertEqual(transport.closeCallCount, 1)
        XCTAssertEqual(connector.callCount, 0)
        XCTAssertFalse(controller.automaticCATRecoveryAvailable)
        XCTAssertNil(recovery.lastExpectedSerialNumber)

        do {
            try await controller.restoreCATFromUSBMMDVM()
            XCTFail("Bluetooth MMDVM must never authorize the USB Menu 650 recovery path")
        } catch let error as RadioControllerError {
            guard case .capabilityUnavailable(let reason) = error else {
                return XCTFail("Expected unavailable USB recovery, got \(error)")
            }
            XCTAssertTrue(reason.contains("validated USB MMDVM response"))
        }
        XCTAssertEqual(recovery.callCount, 0)
    }

    func testTwoUnresponsiveBluetoothSessionsReopenOnceAndReportBluetoothFailure() async throws {
        let transport = IntegrationTestTransport(connectionKind: .bluetooth)
        let connector = IntegrationTestCoreConnector(core: IntegrationTestCore())
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
            XCTFail("Two silent Bluetooth sessions must not enter CAT automation")
        } catch let error as RadioControllerError {
            guard case .operationFailed(let reason) = error else {
                return XCTFail("Expected a Bluetooth operation error, got \(error)")
            }
            XCTAssertTrue(reason.contains("Bluetooth control link once"))
            XCTAssertTrue(reason.contains("Bluetooth connection is enabled"))
            XCTAssertFalse(reason.contains("CDC control-line reset"))
        }

        XCTAssertEqual(preflight.callCount, 2)
        XCTAssertEqual(transport.openCallCount, 2)
        XCTAssertEqual(transport.closeCallCount, 2)
        XCTAssertEqual(connector.callCount, 0)
        XCTAssertFalse(controller.automaticCATRecoveryAvailable)
    }

    func testMMDVMPreflightStopsBeforeCoreAndPreservesTypedRecoveryCondition() async throws {
        let transport = IntegrationTestTransport()
        let core = IntegrationTestCore()
        let connector = IntegrationTestCoreConnector(core: core)
        let recovery = IntegrationTestCATRecovery()
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            connectCore: { transport in
                try await connector.connect(transport: transport)
            },
            prepareRadioForAutomation: { _ in .mmdvm },
            recoverUSBMMDVM: { serialNumber, _ in recovery.operation(serialNumber) },
            automaticCATRecoveryAvailable: true
        )

        do {
            try await controller.connect()
            XCTFail("MMDVM mode must not enter the strict automation core")
        } catch let error as RadioControllerError {
            XCTAssertEqual(error, .usbMmdvmMode)
        }

        XCTAssertEqual(connector.callCount, 0)
        XCTAssertEqual(recovery.callCount, 0)
        XCTAssertEqual(transport.closeCallCount, 1)
        guard case .failed(let message) = controller.currentState.connection else {
            return XCTFail("The workspace should publish an actionable connection failure")
        }
        XCTAssertTrue(message.contains("valid MMDVM response"))
        XCTAssertTrue(message.contains("CAT control is unavailable"))
    }

    func testDeniedBluetoothAuthorizationDoesNotConsumeMMDVMRecoveryOrLaunchHelper() async throws {
        let transport = IntegrationTestTransport()
        let authorization = IntegrationTestBluetoothRecoveryAuthorization(.denied)
        let recoveryFactoryCalls = IntegrationTestCallCounter()
        let recovery = IntegrationTestCATRecovery()
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            prepareRadioForAutomation: { _ in .mmdvm },
            authorizeBluetoothRecovery: {
                try await authorization.authorize()
            },
            recoverUSBMMDVM: { serialNumber, _ in
                recoveryFactoryCalls.record()
                return recovery.operation(serialNumber)
            },
            automaticCATRecoveryAvailable: true
        )

        do {
            try await controller.connect()
            XCTFail("MMDVM mode must stop ordinary CAT connection")
        } catch let error as RadioControllerError {
            XCTAssertEqual(error, .usbMmdvmMode)
        }
        let failedState = controller.currentState

        do {
            try await controller.restoreCATFromUSBMMDVM()
            XCTFail("Denied foreground Bluetooth authorization must fail closed")
        } catch let error as AzimuthBluetoothAuthorizationError {
            XCTAssertEqual(error, .denied)
        }

        XCTAssertEqual(authorization.callCount, 1)
        XCTAssertEqual(recoveryFactoryCalls.callCount, 0)
        XCTAssertEqual(recovery.callCount, 0)
        XCTAssertTrue(controller.automaticCATRecoveryAvailable)
        XCTAssertEqual(controller.currentState, failedState)
    }

    func testCancelledBluetoothAuthorizationDoesNotConsumeMMDVMRecoveryOrLaunchHelper() async throws {
        let transport = IntegrationTestTransport()
        let authorization = IntegrationTestBluetoothRecoveryAuthorization(.blocked)
        let recoveryFactoryCalls = IntegrationTestCallCounter()
        let recovery = IntegrationTestCATRecovery()
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            prepareRadioForAutomation: { _ in .mmdvm },
            authorizeBluetoothRecovery: {
                try await authorization.authorize()
            },
            recoverUSBMMDVM: { serialNumber, _ in
                recoveryFactoryCalls.record()
                return recovery.operation(serialNumber)
            },
            automaticCATRecoveryAvailable: true
        )

        do {
            try await controller.connect()
            XCTFail("MMDVM mode must stop ordinary CAT connection")
        } catch let error as RadioControllerError {
            XCTAssertEqual(error, .usbMmdvmMode)
        }
        var authorizationStarted = authorization.started.makeAsyncIterator()
        let restore = Task { try await controller.restoreCATFromUSBMMDVM() }
        let didStart: Void? = await authorizationStarted.next()
        XCTAssertNotNil(didStart)
        restore.cancel()

        do {
            try await restore.value
            XCTFail("Cancelled foreground authorization must stop recovery")
        } catch is CancellationError {
            // Expected.
        }
        XCTAssertEqual(recoveryFactoryCalls.callCount, 0)
        XCTAssertEqual(recovery.callCount, 0)
        XCTAssertTrue(controller.automaticCATRecoveryAvailable)
        guard case .failed = controller.currentState.connection else {
            return XCTFail("Cancellation before recovery must preserve the MMDVM failure state")
        }
    }

    func testApprovedMMDVMRecoveryUsesBluetoothThenProvesUSBAndConnectsCore() async throws {
        let transport = IntegrationTestTransport()
        let core = IntegrationTestCore()
        let connector = IntegrationTestCoreConnector(core: core)
        let preflight = IntegrationTestModePreflight(modes: [.mmdvm, .cat, .cat])
        let recovery = IntegrationTestCATRecovery()
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            connectCore: { transport in
                try await connector.connect(transport: transport)
            },
            prepareRadioForAutomation: { _ in try preflight.nextMode() },
            recoverUSBMMDVM: { serialNumber, _ in recovery.operation(serialNumber) },
            automaticCATRecoveryAvailable: true,
            catRecoveryWindow: .seconds(1),
            catRecoveryPollInterval: .milliseconds(1)
        )

        do {
            try await controller.connect()
            XCTFail("The first MMDVM observation must require user consent")
        } catch let error as RadioControllerError {
            XCTAssertEqual(error, .usbMmdvmMode)
        }
        XCTAssertEqual(recovery.callCount, 0)

        try await controller.restoreCATFromUSBMMDVM()

        XCTAssertEqual(recovery.callCount, 1)
        XCTAssertEqual(recovery.lastExpectedSerialNumber, "C3C10368")
        XCTAssertEqual(preflight.callCount, 3)
        XCTAssertEqual(connector.callCount, 1)
        XCTAssertTrue(controller.currentState.connection.isConnected)
        await controller.disconnect()
    }

    func testMMDVMRecoveryPassesKnownQualifiedBluetoothAddress() async throws {
        let baseTransport = IntegrationTestTransport()
        let transport = IntegrationSameRadioTransport(
            base: baseTransport,
            knownQualifiedAddress: "AA:BB:CC:DD:EE:FF"
        )
        let core = IntegrationTestCore()
        let connector = IntegrationTestCoreConnector(core: core)
        let preflight = IntegrationTestModePreflight(modes: [.mmdvm, .cat, .cat])
        let recovery = IntegrationTestCATRecovery()
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            connectCore: { transport in
                try await connector.connect(transport: transport)
            },
            prepareRadioForAutomation: { _ in try preflight.nextMode() },
            recoverUSBMMDVM: { serialNumber, qualifiedAddress in
                recovery.operation(
                    serialNumber,
                    qualifiedBluetoothAddress: qualifiedAddress
                )
            },
            automaticCATRecoveryAvailable: true,
            catRecoveryWindow: .seconds(1),
            catRecoveryPollInterval: .milliseconds(1)
        )
        do {
            try await controller.connect()
            XCTFail("Initial USB MMDVM detection must require consent")
        } catch let error as RadioControllerError {
            XCTAssertEqual(error, .usbMmdvmMode)
        }

        try await controller.restoreCATFromUSBMMDVM()

        XCTAssertEqual(recovery.lastExpectedSerialNumber, "C3C10368")
        XCTAssertEqual(
            recovery.lastQualifiedBluetoothAddress,
            "AA:BB:CC:DD:EE:FF"
        )
        XCTAssertEqual(transport.knownAddressRequestCount, 1)
        XCTAssertGreaterThanOrEqual(transport.usbRecoverySelectionCount, 1)
        XCTAssertEqual(connector.callCount, 1)
        await controller.disconnect()
    }

    func testDisconnectBeforeBluetoothRecoveryStartsPreventsMutation() async throws {
        let transport = IntegrationTestTransport()
        let recovery = IntegrationTestCATRecovery()
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            prepareRadioForAutomation: { _ in .mmdvm },
            recoverUSBMMDVM: { serialNumber, _ in recovery.operation(serialNumber) },
            automaticCATRecoveryAvailable: true
        )

        do {
            try await controller.connect()
            XCTFail("MMDVM mode must stop ordinary CAT connection")
        } catch let error as RadioControllerError {
            XCTAssertEqual(error, .usbMmdvmMode)
        }

        transport.blockNextClose()
        let restoreTask = Task {
            try await controller.restoreCATFromUSBMMDVM()
        }
        try await waitUntil { transport.hasBlockedClose }

        await controller.disconnect()

        XCTAssertEqual(recovery.callCount, 0)
        XCTAssertEqual(controller.currentState.connection, .disconnected)
        transport.releaseBlockedClose()
        do {
            try await restoreTask.value
            XCTFail("Recovery invalidated before Bluetooth starts must be cancelled")
        } catch {
            XCTAssertTrue(error is CancellationError)
        }
        XCTAssertEqual(recovery.callCount, 0)
    }

    func testDisconnectDuringBluetoothHandoffDoesNotReauthorizeRecovery() async throws {
        let baseTransport = IntegrationTestTransport()
        let transport = IntegrationSameRadioTransport(
            base: baseTransport,
            knownQualifiedAddress: nil
        )
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            prepareRadioForAutomation: { _ in .mmdvm },
            automaticCATRecoveryAvailable: true
        )

        do {
            try await controller.connect()
            XCTFail("MMDVM mode must stop ordinary CAT connection")
        } catch let error as RadioControllerError {
            XCTAssertEqual(error, .usbMmdvmMode)
        }
        XCTAssertTrue(controller.automaticCATRecoveryAvailable)
        XCTAssertTrue(controller.bluetoothCATFallbackAvailable)

        baseTransport.blockOpen(call: 2)
        let handoff = Task {
            try await controller.connectViaBluetoothFromUSBMMDVM()
        }
        try await waitUntil { baseTransport.hasBlockedOpen }

        await controller.disconnect()

        do {
            try await handoff.value
            XCTFail("Disconnect must cancel the Bluetooth handoff")
        } catch {
            XCTAssertTrue(error is CancellationError)
        }
        XCTAssertEqual(controller.currentState.connection, .disconnected)
        XCTAssertFalse(controller.automaticCATRecoveryAvailable)
        XCTAssertFalse(controller.bluetoothCATFallbackAvailable)
    }

    func testDisconnectCancelsAndWaitsForInFlightBluetoothRecovery() async throws {
        let transport = IntegrationTestTransport()
        let recovery = IntegrationTestBlockingCATRecovery()
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            prepareRadioForAutomation: { _ in .mmdvm },
            recoverUSBMMDVM: { serialNumber, _ in recovery.operation(serialNumber) },
            automaticCATRecoveryAvailable: true
        )

        do {
            try await controller.connect()
            XCTFail("MMDVM mode must stop ordinary CAT connection")
        } catch let error as RadioControllerError {
            XCTAssertEqual(error, .usbMmdvmMode)
        }

        let restoreTask = Task {
            try await controller.restoreCATFromUSBMMDVM()
        }
        try await waitUntil { recovery.hasStarted }
        let disconnectTask = Task {
            await controller.disconnect()
        }
        try await waitUntil { recovery.cancellationObserved }

        XCTAssertNotEqual(controller.currentState.connection, .disconnected)
        recovery.complete()
        await disconnectTask.value
        do {
            try await restoreTask.value
            XCTFail("A completed Menu 650 result must remain visible after disconnect")
        } catch let error as RadioControllerError {
            guard case .operationFailed(let detail) = error else {
                return XCTFail("Expected a truthful completed-operation error, got \(error)")
            }
            XCTAssertTrue(detail.contains("Menu 650 was changed to Off"))
            XCTAssertTrue(detail.contains("USB-C CAT reconnect was stopped"))
        }
        XCTAssertEqual(controller.currentState.connection, .disconnected)
        XCTAssertTrue(recovery.cancellationObserved)
    }

    func testSwiftTaskCancellationSignalsNativeOperationAndAwaitsItsFinish() async throws {
        let transport = IntegrationTestTransport()
        let recovery = IntegrationTestBlockingCATRecovery()
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            prepareRadioForAutomation: { _ in .mmdvm },
            recoverUSBMMDVM: { serialNumber, _ in recovery.operation(serialNumber) },
            automaticCATRecoveryAvailable: true
        )

        do {
            try await controller.connect()
            XCTFail("MMDVM mode must stop ordinary CAT connection")
        } catch let error as RadioControllerError {
            XCTAssertEqual(error, .usbMmdvmMode)
        }

        let restoreTask = Task {
            try await controller.restoreCATFromUSBMMDVM()
        }
        try await waitUntil { recovery.hasStarted }
        restoreTask.cancel()
        try await waitUntil { recovery.cancellationObserved }

        XCTAssertFalse(recovery.hasFinished)
        recovery.complete()
        do {
            try await restoreTask.value
            XCTFail("A completed Menu 650 result must remain visible after late cancellation")
        } catch let error as RadioControllerError {
            guard case .operationFailed(let detail) = error else {
                return XCTFail("Expected a truthful completed-operation error, got \(error)")
            }
            XCTAssertTrue(detail.contains("Menu 650 was changed to Off"))
            XCTAssertTrue(detail.contains("USB-C CAT reconnect was stopped"))
        }
        XCTAssertTrue(recovery.hasFinished)
        await controller.disconnect()
    }

    func testStoppingPostMutationUSBCATPollPreservesCompletedOutcome() async throws {
        let transport = IntegrationTestTransport()
        transport.blockOpen(call: 2)
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            prepareRadioForAutomation: { _ in .mmdvm },
            recoverUSBMMDVM: { _, _ in
                IntegrationTestImmediateCATRecoveryOperation {
                    .changedRadioRebooting
                }
            },
            automaticCATRecoveryAvailable: true,
            catRecoveryWindow: .seconds(60),
            catRecoveryPollInterval: .milliseconds(1)
        )

        do {
            try await controller.connect()
            XCTFail("MMDVM mode must stop ordinary CAT connection")
        } catch let error as RadioControllerError {
            XCTAssertEqual(error, .usbMmdvmMode)
        }

        let restoreTask = Task {
            try await controller.restoreCATFromUSBMMDVM()
        }
        try await waitUntil { transport.hasBlockedOpen }
        restoreTask.cancel()
        await controller.disconnect()

        do {
            try await restoreTask.value
            XCTFail("Stopping the USB poll must report the completed Menu 650 result")
        } catch let error as RadioControllerError {
            guard case .operationFailed(let detail) = error else {
                return XCTFail("Expected a truthful completed-operation error, got \(error)")
            }
            XCTAssertTrue(detail.contains("Menu 650 was changed to Off"))
            XCTAssertTrue(detail.contains("USB-C CAT reconnect was stopped"))
        }
        XCTAssertEqual(transport.openCallCount, 2)
        XCTAssertEqual(controller.currentState.connection, .disconnected)
    }

    func testAutomaticRecoveryIsRejectedAfterAnUnrelatedConnectionFailure() async throws {
        let transport = IntegrationTestTransport()
        let recovery = IntegrationTestCATRecovery()
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            prepareRadioForAutomation: { _ in .unresponsive },
            recoverUSBMMDVM: { serialNumber, _ in recovery.operation(serialNumber) },
            automaticCATRecoveryAvailable: true
        )

        do {
            try await controller.connect()
            XCTFail("An unresponsive USB connection must fail")
        } catch {
            XCTAssertFalse(error is CancellationError)
        }

        do {
            try await controller.restoreCATFromUSBMMDVM()
            XCTFail("An unrelated failure must not authorize a Menu 650 write")
        } catch let error as RadioControllerError {
            guard case .capabilityUnavailable(let reason) = error else {
                return XCTFail("Expected capabilityUnavailable, got \(error)")
            }
            XCTAssertTrue(reason.contains("validated USB MMDVM response"))
        }
        XCTAssertEqual(recovery.callCount, 0)
    }

    func testMMDVMRecoveryIsUnavailableWithoutAStableUSBSerialIdentity() async throws {
        let transport = IntegrationTestTransport(hardwareSerialNumber: nil)
        let recovery = IntegrationTestCATRecovery()
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            prepareRadioForAutomation: { _ in .mmdvm },
            recoverUSBMMDVM: { serialNumber, _ in recovery.operation(serialNumber) },
            automaticCATRecoveryAvailable: true
        )

        do {
            try await controller.connect()
            XCTFail("MMDVM mode must stop ordinary CAT connection")
        } catch let error as RadioControllerError {
            XCTAssertEqual(error, .usbMmdvmMode)
        }
        XCTAssertFalse(controller.automaticCATRecoveryAvailable)

        do {
            try await controller.restoreCATFromUSBMMDVM()
            XCTFail("Recovery without a USB serial identity must fail closed")
        } catch let error as RadioControllerError {
            guard case .capabilityUnavailable(let reason) = error else {
                return XCTFail("Expected capabilityUnavailable, got \(error)")
            }
            XCTAssertTrue(reason.contains("stable radio serial number"))
        }
        XCTAssertEqual(recovery.callCount, 0)
    }

    func testRecoveryRejectsADifferentUSBRadioAfterBluetoothWork() async throws {
        let transport = IntegrationTestTransport()
        let connector = IntegrationTestCoreConnector(core: IntegrationTestCore())
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            connectCore: { transport in
                try await connector.connect(transport: transport)
            },
            prepareRadioForAutomation: { _ in .mmdvm },
            recoverUSBMMDVM: { _, _ in
                IntegrationTestImmediateCATRecoveryOperation {
                    transport.setHardwareSerialNumber("C5310165")
                    return .changedRadioRebooting
                }
            },
            automaticCATRecoveryAvailable: true,
            catRecoveryWindow: .milliseconds(20),
            catRecoveryPollInterval: .milliseconds(1)
        )

        do {
            try await controller.connect()
            XCTFail("MMDVM mode must stop ordinary CAT connection")
        } catch let error as RadioControllerError {
            XCTAssertEqual(error, .usbMmdvmMode)
        }

        do {
            try await controller.restoreCATFromUSBMMDVM()
            XCTFail("A different USB radio must not be accepted after recovery")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("different USB radio"))
            XCTAssertTrue(error.localizedDescription.contains("C3C10368"))
            XCTAssertTrue(error.localizedDescription.contains("C5310165"))
        }
        XCTAssertEqual(connector.callCount, 0)
    }

    func testRecoveryFinalConnectRechecksIdentityAfterCDCReopen() async throws {
        let transport = IntegrationTestTransport()
        transport.setHardwareSerialNumber("C5310165", onOpen: 4)
        let connector = IntegrationTestCoreConnector(core: IntegrationTestCore())
        let preflight = IntegrationTestModePreflight(
            modes: [.mmdvm, .cat, .unresponsive, .cat]
        )
        let recovery = IntegrationTestCATRecovery()
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            connectCore: { transport in
                try await connector.connect(transport: transport)
            },
            prepareRadioForAutomation: { _ in try preflight.nextMode() },
            recoverUSBMMDVM: { serialNumber, _ in recovery.operation(serialNumber) },
            automaticCATRecoveryAvailable: true,
            catRecoveryWindow: .seconds(1),
            catRecoveryPollInterval: .milliseconds(1)
        )

        do {
            try await controller.connect()
            XCTFail("The first MMDVM observation must require user consent")
        } catch let error as RadioControllerError {
            XCTAssertEqual(error, .usbMmdvmMode)
        }

        do {
            try await controller.restoreCATFromUSBMMDVM()
            XCTFail("A replacement radio on the CDC retry must not enter the core")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("USB-C recovery reopen"))
            XCTAssertTrue(error.localizedDescription.contains("C3C10368"))
            XCTAssertTrue(error.localizedDescription.contains("C5310165"))
        }
        XCTAssertEqual(transport.openCallCount, 4)
        XCTAssertEqual(preflight.callCount, 3)
        XCTAssertEqual(connector.callCount, 0)
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

    func testDisconnectCancelsAndAwaitsConnectionBeforeAReopen() async throws {
        let transport = IntegrationTestTransport()
        let core = IntegrationTestCore()
        let connector = IntegrationTestCoreConnector(core: core)
        let preflight = IntegrationTestBlockingModePreflight()
        let controller = try AzimuthLiveRadioController(
            transport: transport,
            connectCore: { transport in
                try await connector.connect(transport: transport)
            },
            prepareRadioForAutomation: { _ in
                try await preflight.nextMode()
            }
        )
        let staleConnect = Task { try await controller.connect() }
        try await waitUntil { preflight.hasStarted }

        let disconnect = Task { await controller.disconnect() }
        try await waitUntil { preflight.cancellationObserved }

        XCTAssertFalse(preflight.hasFinished)
        XCTAssertEqual(transport.closeCallCount, 0)
        preflight.releaseFirstCall()
        await disconnect.value

        do {
            try await staleConnect.value
            XCTFail("the disconnected connection attempt must report cancellation")
        } catch is CancellationError {
            // Disconnect invalidated and joined the complete connection task.
        } catch {
            XCTFail("unexpected stale-connection error: \(error)")
        }

        XCTAssertTrue(preflight.hasFinished)
        XCTAssertEqual(transport.openCallCount, 1)
        XCTAssertEqual(transport.closeCallCount, 1)
        XCTAssertEqual(connector.callCount, 0)
        XCTAssertEqual(controller.currentState.connection, .disconnected)

        try await controller.connect()

        XCTAssertEqual(preflight.callCount, 2)
        XCTAssertEqual(transport.openCallCount, 2)
        XCTAssertEqual(transport.closeCallCount, 1)
        XCTAssertEqual(connector.callCount, 1)
        XCTAssertTrue(controller.currentState.connection.isConnected)
        await controller.disconnect()
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

    func testUSBAPRSStartsWhenMenu983RoutesKISSToUSB() async throws {
        let transport = IntegrationTestTransport(connectionKind: .usb)
        let core = IntegrationTestCore(kissInterfaceRawValue: 0)
        let controller = try makeController(transport: transport, core: core)
        try await controller.connect()

        try await controller.startAPRS(.receiveOnly)

        XCTAssertEqual(core.startAprsCallCount, 1)
        XCTAssertEqual(controller.currentAPRSState.status.phase, .active)
        await controller.disconnect()
    }

    func testUSBAPRSRejectsMenu983BluetoothBeforeKISSEntry() async throws {
        let transport = IntegrationTestTransport(connectionKind: .usb)
        let core = IntegrationTestCore(kissInterfaceRawValue: 1)
        let controller = try makeController(transport: transport, core: core)
        try await controller.connect()

        do {
            try await controller.startAPRS(.receiveOnly)
            XCTFail("USB control must reject a Bluetooth-routed KISS session")
        } catch let error as RadioControllerError {
            guard case .capabilityUnavailable(let reason) = error else {
                return XCTFail("Expected a Menu 983 capability error, got \(error)")
            }
            XCTAssertTrue(reason.contains("Menu 983"))
            XCTAssertTrue(reason.contains("routes KISS to Bluetooth"))
            XCTAssertTrue(reason.contains("Set Menu 983 (KISS) to USB-C"))
        }

        XCTAssertEqual(core.startAprsCallCount, 0)
        XCTAssertEqual(controller.currentAPRSState.status.phase, .inactive)
        XCTAssertTrue(controller.currentState.capabilities.settingRead.isAvailable)
        await controller.disconnect()
    }

    func testBluetoothAPRSStartsWhenMenu983RoutesKISSToBluetooth() async throws {
        let transport = IntegrationTestTransport(connectionKind: .bluetooth)
        let core = IntegrationTestCore(kissInterfaceRawValue: 1)
        let controller = try makeController(transport: transport, core: core)
        try await controller.connect()

        try await controller.startAPRS(.receiveOnly)

        XCTAssertEqual(core.startAprsCallCount, 1)
        XCTAssertEqual(controller.currentAPRSState.status.phase, .active)
        await controller.disconnect()
    }

    func testBluetoothAPRSRejectsMenu983USBBeforeKISSEntry() async throws {
        let transport = IntegrationTestTransport(connectionKind: .bluetooth)
        let core = IntegrationTestCore(kissInterfaceRawValue: 0)
        let controller = try makeController(transport: transport, core: core)
        try await controller.connect()

        do {
            try await controller.startAPRS(.receiveOnly)
            XCTFail("Bluetooth control must reject a USB-routed KISS session")
        } catch let error as RadioControllerError {
            guard case .capabilityUnavailable(let reason) = error else {
                return XCTFail("Expected a Menu 983 capability error, got \(error)")
            }
            XCTAssertTrue(reason.contains("Menu 983"))
            XCTAssertTrue(reason.contains("routes KISS to USB-C"))
            XCTAssertTrue(reason.contains("Set Menu 983 (KISS) to Bluetooth"))
        }

        XCTAssertEqual(core.startAprsCallCount, 0)
        XCTAssertEqual(controller.currentAPRSState.status.phase, .inactive)
        XCTAssertTrue(controller.currentState.capabilities.settingRead.isAvailable)
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

private final class IntegrationTestBlockingModePreflight: @unchecked Sendable {
    private let lock = NSLock()
    private var calls = 0
    private var started = false
    private var cancelled = false
    private var finished = false
    private var continuation: CheckedContinuation<Void, Never>?

    var callCount: Int { lock.withLock { calls } }
    var hasStarted: Bool { lock.withLock { started } }
    var cancellationObserved: Bool { lock.withLock { cancelled } }
    var hasFinished: Bool { lock.withLock { finished } }

    func nextMode() async throws -> AzimuthRadioWireMode {
        let shouldBlock = lock.withLock { () -> Bool in
            calls += 1
            return calls == 1
        }
        guard shouldBlock else { return .cat }

        await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                lock.withLock {
                    self.continuation = continuation
                    started = true
                }
            }
        } onCancel: {
            self.lock.withLock { self.cancelled = true }
        }
        lock.withLock { finished = true }
        try Task.checkCancellation()
        return .cat
    }

    func releaseFirstCall() {
        let pending = lock.withLock {
            let pending = continuation
            continuation = nil
            return pending
        }
        pending?.resume()
    }
}

private final class IntegrationTestCATRecovery: AzimuthCATRecoveryOperation, @unchecked Sendable {
    private let lock = NSLock()
    private var calls = 0
    private var expectedSerialNumber: String?
    private var qualifiedBluetoothAddress: String?

    var callCount: Int { lock.withLock { calls } }
    var lastExpectedSerialNumber: String? { lock.withLock { expectedSerialNumber } }
    var lastQualifiedBluetoothAddress: String? {
        lock.withLock { qualifiedBluetoothAddress }
    }

    func operation(
        _ serialNumber: String,
        qualifiedBluetoothAddress: String? = nil
    ) -> any AzimuthCATRecoveryOperation {
        lock.withLock {
            expectedSerialNumber = serialNumber
            self.qualifiedBluetoothAddress = qualifiedBluetoothAddress
        }
        return self
    }

    func cancel() {}

    func run() async throws -> DvGatewayRecoveryOutcome {
        lock.withLock {
            calls += 1
        }
        return .changedRadioRebooting
    }
}

private final class IntegrationTestCallCounter: @unchecked Sendable {
    private let lock = NSLock()
    private var calls = 0

    var callCount: Int { lock.withLock { calls } }

    func record() {
        lock.withLock { calls += 1 }
    }
}

private final class IntegrationTestBluetoothRecoveryAuthorization: @unchecked Sendable {
    enum Behavior: Sendable {
        case denied
        case blocked
    }

    let started: AsyncStream<Void>

    private let lock = NSLock()
    private let behavior: Behavior
    private let startedContinuation: AsyncStream<Void>.Continuation
    private var calls = 0

    init(_ behavior: Behavior) {
        self.behavior = behavior
        var continuation: AsyncStream<Void>.Continuation!
        started = AsyncStream { continuation = $0 }
        startedContinuation = continuation
    }

    var callCount: Int { lock.withLock { calls } }

    func authorize() async throws {
        lock.withLock { calls += 1 }
        startedContinuation.yield(())
        switch behavior {
        case .denied:
            throw AzimuthBluetoothAuthorizationError.denied
        case .blocked:
            try await Task.sleep(for: .seconds(60))
        }
    }
}

private final class IntegrationTestBlockingCATRecovery: AzimuthCATRecoveryOperation, @unchecked Sendable {
    private let lock = NSLock()
    private var started = false
    private var cancelled = false
    private var finished = false
    private var continuation: CheckedContinuation<DvGatewayRecoveryOutcome, Never>?

    var hasStarted: Bool { lock.withLock { started } }
    var cancellationObserved: Bool { lock.withLock { cancelled } }
    var hasFinished: Bool { lock.withLock { finished } }

    func operation(_ serialNumber: String) -> any AzimuthCATRecoveryOperation {
        _ = serialNumber
        return self
    }

    func cancel() {
        lock.withLock { cancelled = true }
    }

    func run() async throws -> DvGatewayRecoveryOutcome {
        let outcome = await withCheckedContinuation { continuation in
            lock.withLock {
                self.continuation = continuation
                started = true
            }
        }
        lock.withLock { finished = true }
        return outcome
    }

    func complete() {
        let pending = lock.withLock {
            let pending = continuation
            continuation = nil
            return pending
        }
        pending?.resume(returning: .changedRadioRebooting)
    }
}

private final class IntegrationTestImmediateCATRecoveryOperation:
    AzimuthCATRecoveryOperation,
    @unchecked Sendable
{
    private let body: @Sendable () -> DvGatewayRecoveryOutcome

    init(body: @escaping @Sendable () -> DvGatewayRecoveryOutcome) {
        self.body = body
    }

    func cancel() {}

    func run() async throws -> DvGatewayRecoveryOutcome {
        body()
    }
}

private final class IntegrationTestTransport: AzimuthRadioTransport, @unchecked Sendable {
    let device: AzimuthRadioDevice
    private let lock = NSLock()
    private var serialNumber: String?
    private var currentState: AzimuthRadioTransportState = .disconnected
    private var opens = 0
    private var closes = 0
    private var serialNumberByOpen: [Int: String] = [:]
    private var blockedOpenCall: Int?
    private var openIsBlocked = false
    private var shouldBlockNextClose = false
    private var blockedCloseContinuation: CheckedContinuation<Void, Never>?

    init(
        hardwareSerialNumber: String? = "C3C10368",
        connectionKind: AzimuthRadioConnectionKind = .usb
    ) {
        switch connectionKind {
        case .usb:
            device = .thD75USBC
        case .bluetooth:
            device = AzimuthRadioDevice(
                id: "bluetooth:00-11-22-33-44-55",
                name: "Kenwood TH-D75",
                connectionKind: .bluetooth,
                connection: "Bluetooth"
            )
        }
        serialNumber = hardwareSerialNumber
    }

    var hardwareSerialNumber: String? { lock.withLock { serialNumber } }

    func setHardwareSerialNumber(_ value: String?) {
        lock.withLock { serialNumber = value }
    }

    func setHardwareSerialNumber(_ value: String, onOpen openCall: Int) {
        lock.withLock { serialNumberByOpen[openCall] = value }
    }

    var openCallCount: Int { lock.withLock { opens } }
    var closeCallCount: Int { lock.withLock { closes } }
    var hasBlockedClose: Bool { lock.withLock { blockedCloseContinuation != nil } }
    var hasBlockedOpen: Bool { lock.withLock { openIsBlocked } }

    func blockOpen(call: Int) {
        lock.withLock { blockedOpenCall = call }
    }

    func blockNextClose() {
        lock.withLock { shouldBlockNextClose = true }
    }

    func releaseBlockedClose() {
        let continuation = lock.withLock {
            let continuation = blockedCloseContinuation
            blockedCloseContinuation = nil
            return continuation
        }
        continuation?.resume()
    }

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
        let shouldBlock = lock.withLock { () -> Bool in
            opens += 1
            if let scheduledSerialNumber = serialNumberByOpen.removeValue(forKey: opens) {
                serialNumber = scheduledSerialNumber
            }
            let shouldBlock = blockedOpenCall == opens
            if shouldBlock {
                blockedOpenCall = nil
                openIsBlocked = true
                currentState = .connecting
            }
            return shouldBlock
        }
        if shouldBlock {
            defer { lock.withLock { openIsBlocked = false } }
            try await Task.sleep(for: .seconds(60))
        }
        lock.withLock { currentState = .connected }
    }

    func close() async {
        let shouldBlock = lock.withLock {
            closes += 1
            currentState = .disconnected
            let shouldBlock = shouldBlockNextClose
            shouldBlockNextClose = false
            return shouldBlock
        }
        if shouldBlock {
            await withCheckedContinuation { continuation in
                lock.withLock { blockedCloseContinuation = continuation }
            }
        }
    }

    func setBaudRate(baud: UInt32) throws {}
    func write(_ bytes: [UInt8]) async throws {}
    func read(maxBytes: Int) async throws -> [UInt8] { [] }
}

private final class IntegrationSameRadioTransport:
    AzimuthRadioTransport, AzimuthSameRadioBluetoothSelecting, @unchecked Sendable
{
    private let lock = NSLock()
    private let base: IntegrationTestTransport
    private let knownQualifiedAddress: String?
    private var knownAddressRequests = 0
    private var usbRecoverySelections = 0

    init(
        base: IntegrationTestTransport,
        knownQualifiedAddress: String?
    ) {
        self.base = base
        self.knownQualifiedAddress = knownQualifiedAddress
    }

    var knownAddressRequestCount: Int {
        lock.withLock { knownAddressRequests }
    }

    var usbRecoverySelectionCount: Int {
        lock.withLock { usbRecoverySelections }
    }

    var device: AzimuthRadioDevice { base.device }
    var state: AzimuthRadioTransportState { get async { await base.state } }
    var stateStream: AsyncStream<AzimuthRadioTransportState> { base.stateStream }
    var hardwareSerialNumber: String? { get async { base.hardwareSerialNumber } }

    func open() async throws { try await base.open() }
    func close() async { await base.close() }
    func setBaudRate(baud: UInt32) throws { try base.setBaudRate(baud: baud) }
    func write(_ bytes: [UInt8]) async throws { try await base.write(bytes) }
    func read(maxBytes: Int) async throws -> [UInt8] {
        try await base.read(maxBytes: maxBytes)
    }

    func selectBluetoothForSameRadio(expectedSerialNumber: String) async throws {
        _ = expectedSerialNumber
    }

    func knownQualifiedBluetoothAddress(
        expectedSerialNumber: String
    ) async throws -> String? {
        _ = expectedSerialNumber
        lock.withLock { knownAddressRequests += 1 }
        return knownQualifiedAddress
    }

    func selectUSBForRecovery(expectedSerialNumber: String) async throws {
        _ = expectedSerialNumber
        lock.withLock { usbRecoverySelections += 1 }
    }
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
    private var aprsStartCalls = 0
    private let kissInterfaceRawValue: UInt64
    private var aprsStatus = IntegrationTestCore.aprsStatus(
        phase: .inactive,
        configuration: nil
    )
    private var aprsRows: [AprsActivityRecord] = []
    private var pendingAprsStop: CheckedContinuation<AprsSessionStatus, Error>?

    init(kissInterfaceRawValue: UInt64 = 0) {
        self.kissInterfaceRawValue = kissInterfaceRawValue
    }

    var applyCallCount: Int { lock.withLock { applyCalls } }
    var guardedTapCallCount: Int { lock.withLock { tapCalls } }
    var lastGuardedLease: UInt64? { lock.withLock { guardedLease } }
    var lastLeasePresentedToTap: UInt64? { lock.withLock { leasePresentedToTap } }
    var settingReadCallCount: Int { lock.withLock { settingReadCalls } }
    var screenCaptureCallCount: Int { lock.withLock { screenCaptureCalls } }
    var stopAprsCallCount: Int { lock.withLock { aprsStopCalls } }
    var startAprsCallCount: Int { lock.withLock { aprsStartCalls } }

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
                values: [
                    SettingValueRecord(
                        settingId: "radio.Beep",
                        value: .boolean(value: beep)
                    ),
                    SettingValueRecord(
                        settingId: "radio.KissModeInterface",
                        value: .unsigned(value: kissInterfaceRawValue)
                    ),
                ]
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
                values: [
                    SettingValueRecord(
                        settingId: "radio.Beep",
                        value: .boolean(value: beep)
                    ),
                    SettingValueRecord(
                        settingId: "radio.KissModeInterface",
                        value: .unsigned(value: kissInterfaceRawValue)
                    ),
                ]
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
            aprsStartCalls += 1
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
