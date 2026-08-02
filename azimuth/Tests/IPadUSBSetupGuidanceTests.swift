// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Azimuth

final class IPadUSBSetupGuidanceTests: XCTestCase {
    func testInitialDisconnectedStateWithNoServicesStaysHidden() {
        let guidance = IPadUSBSetupGuidance.resolve(
            connection: .disconnected,
            dataServicePresent: false,
            controlServicePresent: false,
            isSimulator: false
        )

        XCTAssertEqual(guidance, .hidden)
    }

    func testFailedConnectionWithMissingOrPartialServicesShowsTroubleshooting() {
        let unavailableServicePairs = [
            (data: false, control: false),
            (data: true, control: false),
            (data: false, control: true),
        ]

        for services in unavailableServicePairs {
            let guidance = IPadUSBSetupGuidance.resolve(
                connection: .failed(message: "USB connection failed"),
                dataServicePresent: services.data,
                controlServicePresent: services.control,
                isSimulator: false
            )

            XCTAssertEqual(
                guidance,
                .connectionTroubleshooting,
                "Expected troubleshooting for data=\(services.data), control=\(services.control)"
            )
        }
    }

    func testBothServicesPresentKeepsSetupHidden() {
        let connections: [RadioConnectionState] = [
            .disconnected,
            .failed(message: "Radio did not answer"),
        ]

        for connection in connections {
            let guidance = IPadUSBSetupGuidance.resolve(
                connection: connection,
                dataServicePresent: true,
                controlServicePresent: true,
                isSimulator: false
            )

            XCTAssertEqual(guidance, .hidden)
        }
    }

    func testSimulatorUsesSimulatorGuidance() {
        let guidance = IPadUSBSetupGuidance.resolve(
            connection: .disconnected,
            dataServicePresent: false,
            controlServicePresent: false,
            isSimulator: true
        )

        XCTAssertEqual(guidance, .simulator)
    }
}
