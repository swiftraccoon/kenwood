// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Lodestar

@MainActor
final class TransportCoordinatorTests: XCTestCase {
    func testConnectUsesInjectedTransportFactory() async throws {
        let coordinator = TransportCoordinator()
        let mock = MockRadioTransport()
        // connect() runs the MMDVM GetVersion probe. The mock no longer
        // echoes writes, and the prober's timeout can't cancel a blocked
        // read (CheckedContinuation isn't cancellation-aware), so an
        // unanswered probe hangs connect(). Script any non-empty reply so
        // the probe read resumes and connect() finishes.
        await mock.script(response: [0xE0, 0x03, 0x00], for: [0xE0, 0x03, 0x00])
        coordinator.transportFactory = { _ in mock }
        coordinator.select(.mockTHD75)

        await coordinator.connect()

        let state = await mock.state
        XCTAssertEqual(state, .connected, "connect() must open the injected transport")
        XCTAssertNotNil(coordinator.relayTransport, "transport handle must be retained")
    }

    func testUnexpectedDropResetsRadioMode() async throws {
        let coordinator = TransportCoordinator()
        let mock = MockRadioTransport()
        coordinator.transportFactory = { _ in mock }
        coordinator.select(.mockTHD75)
        // Probe gets an MMDVM version response → radioMode == .mmdvm.
        await mock.script(response: [0xE0, 0x04, 0x00, 0x01], for: [0xE0, 0x03, 0x00])
        await coordinator.connect()
        XCTAssertEqual(coordinator.radioMode, .mmdvm)

        await mock.simulateUnexpectedClose()
        // Let the state-observer task drain the yield.
        try await Task.sleep(nanoseconds: 200_000_000)

        XCTAssertEqual(coordinator.radioMode, .unknown,
                       "BT drop must clear radioMode so the relay reconciler stops wanting relay")
        XCTAssertNil(coordinator.relayTransport, "dead transport must be released")
        XCTAssertEqual(coordinator.state, .disconnected)
    }

    func testUnexpectedDropSchedulesReconnect() async throws {
        let coordinator = TransportCoordinator()
        let first = MockRadioTransport()
        let second = MockRadioTransport()
        var handedOut = 0
        coordinator.transportFactory = { _ in
            handedOut += 1
            return handedOut == 1 ? first : second
        }
        coordinator.reconnectDelaysNs = [50_000_000]   // 50 ms for tests
        coordinator.select(.mockTHD75)
        // Script BOTH probes so the initial connect and the reconnect
        // finish promptly instead of waiting out the 2 s probe timeout;
        // classification (.mmdvm here) is irrelevant to the asserts.
        await first.script(response: [0xE0, 0x04, 0x00, 0x01], for: [0xE0, 0x03, 0x00])
        await second.script(response: [0xE0, 0x04, 0x00, 0x01], for: [0xE0, 0x03, 0x00])
        await coordinator.connect()
        await first.simulateUnexpectedClose()
        try await Task.sleep(nanoseconds: 500_000_000)

        XCTAssertEqual(handedOut, 2, "drop must trigger a reconnect attempt")
        XCTAssertEqual(coordinator.state, .connected)
    }

    func testUserDisconnectDoesNotTripUnexpectedPath() async throws {
        let coordinator = TransportCoordinator()
        let mock = MockRadioTransport()
        var handedOut = 0
        coordinator.transportFactory = { _ in
            handedOut += 1
            return mock
        }
        coordinator.reconnectDelaysNs = [50_000_000]
        coordinator.select(.mockTHD75)
        await mock.script(response: [0xE0, 0x04, 0x00, 0x01], for: [0xE0, 0x03, 0x00])
        await coordinator.connect()
        await coordinator.disconnect()
        // Give any (wrongly) scheduled reconnect ample time to fire.
        try await Task.sleep(nanoseconds: 300_000_000)
        XCTAssertEqual(handedOut, 1,
                       "user disconnect must not schedule a reconnect")
        XCTAssertEqual(coordinator.state, .disconnected)
        XCTAssertNil(coordinator.relayTransport)
    }

    func testUserDisconnectCancelsPendingReconnect() async throws {
        let coordinator = TransportCoordinator()
        let first = MockRadioTransport()
        var handedOut = 0
        coordinator.transportFactory = { _ in
            handedOut += 1
            return first
        }
        // Long enough that the reconnect is still sleeping when we
        // disconnect, short enough that a wrongly-surviving reconnect
        // fires well inside the assertion grace period.
        coordinator.reconnectDelaysNs = [150_000_000]
        coordinator.select(.mockTHD75)
        await first.script(response: [0xE0, 0x04, 0x00, 0x01], for: [0xE0, 0x03, 0x00])
        await coordinator.connect()

        // Unexpected drop schedules the reconnect...
        await first.simulateUnexpectedClose()
        try await Task.sleep(nanoseconds: 50_000_000)
        // ...and the user disconnects while it is still pending.
        await coordinator.disconnect()
        try await Task.sleep(nanoseconds: 400_000_000)

        XCTAssertEqual(handedOut, 1,
                       "user disconnect must cancel a pending post-drop reconnect")
        XCTAssertEqual(coordinator.state, .disconnected)
    }

    /// Mock whose open() always throws, for failure-path tests.
    private struct FailingTransport: RadioTransport {
        let device: BluetoothDevice = .mockTHD75
        var state: RadioTransportState { get async { .failed(message: "boom") } }
        var stateStream: AsyncStream<RadioTransportState> { AsyncStream { $0.finish() } }
        func open() async throws { throw RadioTransportError.openFailed(reason: "boom") }
        func close() async {}
        func write(_ bytes: [UInt8]) async throws { throw RadioTransportError.notConnected }
        func read(maxBytes: Int) async throws -> [UInt8] { throw RadioTransportError.notConnected }
    }

    func testConnectFailureReleasesTransport() async throws {
        let coordinator = TransportCoordinator()
        coordinator.transportFactory = { _ in FailingTransport() }
        coordinator.select(.mockTHD75)
        await coordinator.connect()
        guard case .failed = coordinator.state else {
            return XCTFail("expected .failed, got \(coordinator.state)")
        }
        XCTAssertNil(coordinator.relayTransport,
                     "failed connect must not leave a dangling transport")
    }
}
