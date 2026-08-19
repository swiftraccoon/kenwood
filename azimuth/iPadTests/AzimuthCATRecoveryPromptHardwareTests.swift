// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import XCTest
@testable import Azimuth

/// Opt-in, non-destructive acceptance test for a physical iPad and attached
/// TH-D75 already in DV Gateway/MMDVM mode.
@MainActor
final class AzimuthCATRecoveryPromptHardwareTests: XCTestCase {
    func testLiveMMDVMConnectStopsAtPromptWithoutStartingMenu650Recovery() async throws {
        guard ProcessInfo.processInfo.environment[
            "AZIMUTH_HARDWARE_IPAD_MMDVM_PROMPT"
        ] == "1" else {
            throw XCTSkip(
                "Set AZIMUTH_HARDWARE_IPAD_MMDVM_PROMPT=1 with the USB radio in DV Gateway mode."
            )
        }

        let recoveryProbe = IPadHardwareRecoveryFactoryProbe()
        let records = settingCatalog()
        let controller = try AzimuthLiveRadioController(
            transport: AzimuthUSBSerialTransport.platformDefault(),
            records: records,
            recoverUSBMMDVM: { serialNumber, _ in
                recoveryProbe.makeOperation(serialNumber: serialNumber)
            }
        )
        let provider = try AzimuthCoreCatalogProvider(records: records)
        let model = AzimuthSceneModel(
            radioController: controller,
            catalogProvider: provider,
            assistantPlanner: OnDeviceAssistantPlanner(),
            initialCatalog: provider.initialCatalog
        )

        await model.connectRadio()
        let prompt = model.catRecoveryAlert
        let connection = model.radioState.connection
        let recoveryFactoryCallCount = recoveryProbe.callCount
        await controller.disconnect()

        XCTAssertEqual(
            prompt,
            .usbMmdvmMode(
                automaticRecoveryAvailable: false,
                bluetoothFallbackAvailable: false
            )
        )
        guard case .failed = connection else {
            return XCTFail("The MMDVM preflight must stop at the consent prompt.")
        }
        XCTAssertEqual(
            recoveryFactoryCallCount,
            0,
            "Connecting must never construct or run a Menu 650 recovery operation."
        )
    }
}

private final class IPadHardwareRecoveryFactoryProbe: @unchecked Sendable {
    private let lock = NSLock()
    private var calls = 0

    var callCount: Int { lock.withLock { calls } }

    func makeOperation(
        serialNumber: String
    ) -> any AzimuthCATRecoveryOperation {
        _ = serialNumber
        lock.withLock { calls += 1 }
        return IPadHardwareUnexpectedRecoveryOperation()
    }
}

private final class IPadHardwareUnexpectedRecoveryOperation:
    AzimuthCATRecoveryOperation, @unchecked Sendable
{
    func cancel() {}

    func run() async throws -> DvGatewayRecoveryOutcome {
        throw RadioControllerError.operationFailed(
            "The no-mutation prompt test unexpectedly ran Menu 650 recovery."
        )
    }
}
