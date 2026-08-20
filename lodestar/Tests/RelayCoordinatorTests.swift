// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Lodestar

@MainActor
final class RelayCoordinatorTests: XCTestCase {
    func testReaderStreamEndClearsRelayHook() async throws {
        let transportCoordinator = TransportCoordinator()
        let mock = MockRadioTransport()
        transportCoordinator.transportFactory = { _ in mock }
        transportCoordinator.select(.mockTHD75)
        let version = [0xE0, 0x0E, 0x00, 0x01] + Array("MMDVM 2018".utf8)
        await mock.script(response: version, for: [0xE0, 0x03, 0x00])
        await transportCoordinator.connect()
        XCTAssertEqual(transportCoordinator.radioMode, .mmdvm)

        let reflectorCoordinator = ReflectorCoordinator()
        let relay = RelayCoordinator(transport: transportCoordinator, reflector: reflectorCoordinator)
        // No live session: start() must fail-fast, not install the hook.
        await relay.start()
        guard case .failed = relay.state else {
            return XCTFail("expected failed start without session, got \(relay.state)")
        }
        XCTAssertNil(reflectorCoordinator.relayHook)

        // Install a hook manually to prove markStopped clears it: drive
        // the private path via the reader-ended route. Simulate: hook
        // set + running reader, then transport dies.
        reflectorCoordinator.relayHook = { _ in }
        relay.simulateRunningForTests()
        // Let the reader spin up and park its `transport.read` BEFORE the
        // close, so the close resumes that parked read with an empty
        // chunk (EOF), the signal that ends the reader stream. A read
        // that parks *after* close would block forever, so this settle
        // is load-bearing (mirrors MockTransportTests' park-then-close).
        try await Task.sleep(nanoseconds: 150_000_000)
        await mock.simulateUnexpectedClose()

        // After close, two MainActor paths race benignly: the reader
        // stream ends → markStopped() → .stopped, and the transport
        // coordinator's unexpected-drop handling fires. Poll for the
        // teardown result with a deadline rather than a fixed sleep so
        // the test isn't brittle.
        let deadline = ContinuousClock.now.advanced(by: .seconds(2))
        while relay.state != .stopped, ContinuousClock.now < deadline {
            try await Task.sleep(nanoseconds: 20_000_000)
        }

        XCTAssertNil(reflectorCoordinator.relayHook,
                     "reader death must clear the reflector→radio hook")
        XCTAssertEqual(relay.state, .stopped)
    }
}
