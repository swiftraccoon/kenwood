// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import XCTest
@testable import Azimuth

/// Opt-in acceptance test for the signed Azimuth host, native Bluetooth
/// helper, conditional Menu 650 verify-and-disable operation, USB reboot wait,
/// and full automation reconnect.
///
/// The direct Bluetooth test uses `AZIMUTH_HARDWARE_BLUETOOTH_RECOVERY=1`
/// plus the radio's exact CAT serial in `AZIMUTH_HARDWARE_RADIO_SERIAL`.
/// It accepts either a verified write or proof that Menu 650 was already off.
/// The non-destructive primary-link test uses
/// `AZIMUTH_HARDWARE_BLUETOOTH_PRIMARY=1` and the exact paired address in
/// `AZIMUTH_HARDWARE_BLUETOOTH_ADDRESS`; it runs the ordinary Azimuth mode
/// preflight, automation qualification, settings read, and screen capture.
/// When the selected Bluetooth interface intentionally carries persistent
/// MMDVM traffic, `AZIMUTH_HARDWARE_BLUETOOTH_MMDVM=1` plus the same exact
/// address proves discovery, exact-address opening, and typed MMDVM detection
/// without changing Menu 650.
/// The full prompt and USB reconnect test additionally requires the attached
/// radio to begin in DV Gateway mode and uses
/// `AZIMUTH_HARDWARE_DV_GATEWAY_RECOVERY=1`.
@MainActor
final class AzimuthCATRecoveryHardwareTests: XCTestCase {
    func testLiveSelectableBluetoothEndpointProvesMMDVMWithoutMutation() async throws {
        guard ProcessInfo.processInfo.environment[
            "AZIMUTH_HARDWARE_BLUETOOTH_MMDVM"
        ] == "1" else {
            throw XCTSkip(
                "Set AZIMUTH_HARDWARE_BLUETOOTH_MMDVM=1 for the live radio test."
            )
        }
        guard let expectedAddress = ProcessInfo.processInfo.environment[
            "AZIMUTH_HARDWARE_BLUETOOTH_ADDRESS"
        ], !expectedAddress.isEmpty else {
            throw XCTSkip(
                "Set AZIMUTH_HARDWARE_BLUETOOTH_ADDRESS to the exact paired radio address."
            )
        }

        let records = settingCatalog()
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: AzimuthPlatformUSBTransportFactory(),
            bluetoothFactory: AzimuthGeneratedBluetoothLinkFactory()
        )
        let selector = AzimuthSelectableRadioEndpointSelector(router: router)
        let controller = try AzimuthLiveRadioController(
            transport: router,
            records: records
        )
        let provider = try AzimuthCoreCatalogProvider(records: records)
        let model = AzimuthSceneModel(
            radioController: controller,
            catalogProvider: provider,
            assistantPlanner: OnDeviceAssistantPlanner(),
            radioEndpointSelector: selector,
            initialCatalog: provider.initialCatalog
        )

        await model.refreshRadioEndpoints().value
        let normalizedExpectedAddress = expectedAddress.replacingOccurrences(
            of: "-",
            with: ":"
        )
        let matching = model.radioEndpoints.filter {
            $0.transport == .bluetooth
                && $0.detail?
                    .replacingOccurrences(of: "-", with: ":")
                    .caseInsensitiveCompare(normalizedExpectedAddress) == .orderedSame
        }
        XCTAssertEqual(
            matching.count,
            1,
            "Paired discovery must expose the exact selected address once. Refresh state: \(model.radioEndpointRefreshState). Bluetooth warning: \(model.radioEndpointDiscoveryWarning ?? "none"). Refresh error: \(model.radioEndpointRefreshError ?? "none")."
        )
        guard let endpoint = matching.first else { return }
        model.selectRadioEndpoint(id: endpoint.id)

        await model.connectRadio()
        let operationError = model.operationError
        let recoveryAlert = model.catRecoveryAlert
        let connection = model.radioState.connection
        await controller.disconnect()

        XCTAssertNil(operationError)
        guard case .bluetoothMmdvmMode = recoveryAlert else {
            return XCTFail(
                "Azimuth must prove persistent Bluetooth MMDVM mode and stop before CAT automation."
            )
        }
        XCTAssertFalse(connection.isConnected)
    }

    func testLiveSelectableBluetoothEndpointCompletesAutomationConnection() async throws {
        guard ProcessInfo.processInfo.environment[
            "AZIMUTH_HARDWARE_BLUETOOTH_PRIMARY"
        ] == "1" else {
            throw XCTSkip("Set AZIMUTH_HARDWARE_BLUETOOTH_PRIMARY=1 for the live radio test.")
        }
        guard let expectedAddress = ProcessInfo.processInfo.environment[
            "AZIMUTH_HARDWARE_BLUETOOTH_ADDRESS"
        ], !expectedAddress.isEmpty else {
            throw XCTSkip(
                "Set AZIMUTH_HARDWARE_BLUETOOTH_ADDRESS to the exact paired radio address."
            )
        }

        let records = settingCatalog()
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: AzimuthPlatformUSBTransportFactory(),
            bluetoothFactory: AzimuthGeneratedBluetoothLinkFactory()
        )
        let selector = AzimuthSelectableRadioEndpointSelector(router: router)
        let controller = try AzimuthLiveRadioController(
            transport: router,
            records: records
        )
        let provider = try AzimuthCoreCatalogProvider(records: records)
        let model = AzimuthSceneModel(
            radioController: controller,
            catalogProvider: provider,
            assistantPlanner: OnDeviceAssistantPlanner(),
            radioEndpointSelector: selector,
            initialCatalog: provider.initialCatalog
        )

        await model.refreshRadioEndpoints().value
        let normalizedExpectedAddress = expectedAddress.replacingOccurrences(of: "-", with: ":")
        let matching = model.radioEndpoints.filter {
            $0.transport == .bluetooth
                && $0.detail?
                    .replacingOccurrences(of: "-", with: ":")
                    .caseInsensitiveCompare(normalizedExpectedAddress) == .orderedSame
        }
        XCTAssertEqual(
            matching.count,
            1,
            "Paired discovery must resolve the requested address exactly once. Refresh state: \(model.radioEndpointRefreshState). Bluetooth warning: \(model.radioEndpointDiscoveryWarning ?? "none"). Refresh error: \(model.radioEndpointRefreshError ?? "none"). If authorization is not determined, launch Azimuth in the foreground and allow Bluetooth before rerunning this opt-in test."
        )
        guard let endpoint = matching.first else { return }
        model.selectRadioEndpoint(id: endpoint.id)

        await model.connectRadio()
        let connectedState = model.radioState.connection
        let operationError = model.operationError
        let recoveryAlert = model.catRecoveryAlert
        await controller.disconnect()

        XCTAssertNil(operationError)
        XCTAssertNil(recoveryAlert)
        guard case .connected(let device, let transport) = connectedState else {
            return XCTFail("Azimuth did not complete the Bluetooth automation connection.")
        }
        XCTAssertEqual(device, endpoint.name)
        XCTAssertEqual(transport, "Bluetooth")
    }

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

        try await AzimuthMacBluetoothAuthorizationProvider.shared
            .ensureBluetoothAuthorization()
        let operation = try DvGatewayRecoveryOperation(
            expectedRadioSerialNumber: expectedSerialNumber,
            bluetoothSelector: nil
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
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: AzimuthPlatformUSBTransportFactory(),
            bluetoothFactory: AzimuthGeneratedBluetoothLinkFactory()
        )
        let selector = AzimuthSelectableRadioEndpointSelector(router: router)
        let controller = try AzimuthLiveRadioController(
            transport: router,
            records: records,
            authorizeBluetoothRecovery: {
                try await AzimuthMacBluetoothAuthorizationProvider.shared
                    .ensureBluetoothAuthorization()
            }
        )
        let provider = try AzimuthCoreCatalogProvider(records: records)
        let model = AzimuthSceneModel(
            radioController: controller,
            catalogProvider: provider,
            assistantPlanner: OnDeviceAssistantPlanner(),
            radioEndpointSelector: selector,
            initialCatalog: provider.initialCatalog
        )

        await model.refreshRadioEndpoints().value
        guard (model.pairedBluetoothDeviceCount ?? 0) > 0 else {
            return XCTFail(
                "The live recovery test requires at least one paired Bluetooth device."
            )
        }
        await model.connectRadio()
        XCTAssertEqual(
            model.catRecoveryAlert,
            .usbMmdvmMode(
                automaticRecoveryAvailable: true,
                bluetoothFallbackAvailable: true
            ),
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
