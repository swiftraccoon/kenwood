// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Azimuth

/// Opt-in acceptance test for the signed Azimuth host, native Bluetooth
/// helper, conditional Menu 650 verify-and-disable operation, USB reboot wait,
/// and full automation reconnect.
///
/// The direct Bluetooth test uses `AZIMUTH_HARDWARE_BLUETOOTH_RECOVERY=1`
/// plus the radio's exact CAT serial in `AZIMUTH_HARDWARE_RADIO_SERIAL`.
/// It accepts either a verified write or proof that Menu 650 was already off.
/// The full prompt and USB reconnect test additionally requires the attached
/// radio to begin in DV Gateway mode and uses
/// `AZIMUTH_HARDWARE_DV_GATEWAY_RECOVERY=1`.
@MainActor
final class AzimuthCATRecoveryHardwareTests: XCTestCase {
    func testLiveSandboxedHostCanVerifyAndDisableMenu650OverBluetooth() async throws {
        guard ProcessInfo.processInfo.environment[
            "AZIMUTH_HARDWARE_BLUETOOTH_RECOVERY"
        ] == "1" else {
            throw XCTSkip("Set AZIMUTH_HARDWARE_BLUETOOTH_RECOVERY=1 for the live radio test.")
        }
        guard let expectedSerialNumber = ProcessInfo.processInfo.environment[
            "AZIMUTH_HARDWARE_RADIO_SERIAL"
        ], !expectedSerialNumber.isEmpty else {
            throw XCTSkip("Set AZIMUTH_HARDWARE_RADIO_SERIAL to the attached radio's CAT serial.")
        }

        let operation = DvGatewayRecoveryOperation(
            expectedRadioSerialNumber: expectedSerialNumber,
            bluetoothDeviceName: nil
        )
        let outcome = try await withTaskCancellationHandler {
            try await operation.run()
        } onCancel: {
            operation.cancel()
        }
        print("[Azimuth Hardware] Bluetooth Menu 650 verify-and-disable outcome: \(outcome)")

        XCTAssertTrue(
            outcome == .changedRadioRebooting || outcome == .alreadyOffCatReady
        )
    }

    func testLivePromptActionRestoresUSBCATThroughBluetooth() async throws {
        guard ProcessInfo.processInfo.environment[
            "AZIMUTH_HARDWARE_DV_GATEWAY_RECOVERY"
        ] == "1" else {
            throw XCTSkip("Set AZIMUTH_HARDWARE_DV_GATEWAY_RECOVERY=1 for the live radio test.")
        }

        let records = settingCatalog()
        let controller = try AzimuthLiveRadioController(
            transport: AzimuthUSBSerialTransport.platformDefault(),
            records: records
        )
        let provider = try AzimuthCoreCatalogProvider(records: records)
        let model = AzimuthSceneModel(
            radioController: controller,
            catalogProvider: provider,
            assistantPlanner: OnDeviceAssistantPlanner(),
            initialCatalog: provider.initialCatalog
        )

        await model.connectRadio()
        XCTAssertEqual(
            model.catRecoveryAlert,
            .usbMmdvmMode(automaticRecoveryAvailable: true),
            "The live USB connection must prove MMDVM before the test permits a Menu 650 write."
        )
        XCTAssertNil(model.operationError)

        await model.restoreCATFromUSBMMDVM()

        let failure = model.catRecoveryAlert
        let connected = model.radioState.connection.isConnected
        await controller.disconnect()
        XCTAssertNil(failure)
        XCTAssertTrue(connected, "Azimuth must prove full USB CAT automation after the reboot.")
    }
}
