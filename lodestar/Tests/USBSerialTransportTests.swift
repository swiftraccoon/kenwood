// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Lodestar

/// Scriptable in-memory `USBSerialLink` that mimics the dext contract:
/// non-blocking drains, edge-triggered one-shot doorbell that fires
/// immediately when armed while data is pending.
final class MockUSBSerialLink: USBSerialLink, @unchecked Sendable {
    private let lock = NSLock()
    private var buffered: [UInt8] = []
    private var doorbell: (@Sendable (Bool) -> Void)?
    private var opened = false
    var present = true
    /// Number of writes that answer `.backpressure` before succeeding.
    var backpressureCount = 0
    /// When true, `armDoorbell` throws — simulates a user-client call
    /// failing right after a successful `open()`.
    var failArmDoorbell = false
    private(set) var writes: [[UInt8]] = []
    private(set) var closeCount = 0

    func servicePresent() -> Bool { present }

    func open() throws {
        guard present else { throw USBLinkError.serviceNotFound }
        lock.lock(); opened = true; lock.unlock()
    }

    func close() {
        lock.lock()
        opened = false
        closeCount += 1
        let bell = doorbell
        doorbell = nil
        lock.unlock()
        bell?(false)
    }

    func write(_ bytes: [UInt8]) throws {
        lock.lock()
        guard opened else { lock.unlock(); throw USBLinkError.notOpen }
        if backpressureCount > 0 {
            backpressureCount -= 1
            lock.unlock()
            throw USBLinkError.backpressure
        }
        writes.append(bytes)
        lock.unlock()
    }

    func drain(maxBytes: Int) throws -> [UInt8] {
        lock.lock(); defer { lock.unlock() }
        guard opened else { throw USBLinkError.notOpen }
        let n = min(maxBytes, buffered.count)
        let chunk = Array(buffered.prefix(n))
        buffered.removeFirst(n)
        return chunk
    }

    func armDoorbell(onFire: @escaping @Sendable (Bool) -> Void) throws {
        lock.lock()
        guard opened else { lock.unlock(); throw USBLinkError.notOpen }
        if failArmDoorbell {
            lock.unlock()
            throw USBLinkError.callFailed(kern: -1)
        }
        if buffered.isEmpty {
            doorbell = onFire
            lock.unlock()
        } else {
            // Dext contract: arming while non-empty fires immediately.
            lock.unlock()
            onFire(true)
        }
    }

    /// Simulate the radio sending bytes (bulk-IN completion in the dext).
    func radioSends(_ bytes: [UInt8]) {
        lock.lock()
        let wasEmpty = buffered.isEmpty
        buffered.append(contentsOf: bytes)
        let bell = wasEmpty ? doorbell : nil
        if wasEmpty { doorbell = nil }
        lock.unlock()
        bell?(true)
    }

    /// Simulate USB unplug: doorbell fires false, later calls fail.
    func simulateUnplug() {
        lock.lock()
        opened = false
        let bell = doorbell
        doorbell = nil
        lock.unlock()
        bell?(false)
    }
}

final class USBSerialTransportTests: XCTestCase {
    func testOpenTransitionsToConnected() async throws {
        let link = MockUSBSerialLink()
        let t = USBSerialTransport(link: link)
        try await t.open()
        let state = await t.state
        XCTAssertEqual(state, .connected)
    }

    func testOpenWithoutServiceFails() async {
        let link = MockUSBSerialLink()
        link.present = false
        let t = USBSerialTransport(link: link)
        do {
            try await t.open()
            XCTFail("open() should throw without the dext service")
        } catch { /* expected */ }
        let state = await t.state
        guard case .failed = state else {
            return XCTFail("state should be .failed, got \(state)")
        }
    }

    func testReadDeliversBytesArrivingAfterArm() async throws {
        let link = MockUSBSerialLink()
        let t = USBSerialTransport(link: link)
        try await t.open()
        async let read: [UInt8] = t.read(maxBytes: 64)
        try await Task.sleep(nanoseconds: 50_000_000)
        link.radioSends(Array("ID TH-D75\r".utf8))
        let got = try await read
        XCTAssertEqual(got, Array("ID TH-D75\r".utf8))
    }

    func testBytesArrivingBeforeOpenAreDrainedOnOpen() async throws {
        let link = MockUSBSerialLink()
        // Radio pushed data into the dext ring before the app opened.
        try link.open()          // open the mock so radioSends buffers
        link.radioSends([0x41, 0x42])
        let t = USBSerialTransport(link: link)
        try await t.open()
        let got = try await t.read(maxBytes: 16)
        XCTAssertEqual(got, [0x41, 0x42])
    }

    func testWriteForwardsToLink() async throws {
        let link = MockUSBSerialLink()
        let t = USBSerialTransport(link: link)
        try await t.open()
        try await t.write(Array("ID\r".utf8))
        XCTAssertEqual(link.writes, [Array("ID\r".utf8)])
    }

    func testWriteRetriesThroughBackpressure() async throws {
        let link = MockUSBSerialLink()
        link.backpressureCount = 3
        let t = USBSerialTransport(link: link)
        try await t.open()
        try await t.write([0x01])
        XCTAssertEqual(link.writes, [[0x01]])
    }

    func testWritePersistentBackpressureThrows() async throws {
        let link = MockUSBSerialLink()
        link.backpressureCount = .max
        let t = USBSerialTransport(link: link)
        try await t.open()
        do {
            try await t.write([0x01])
            XCTFail("persistent backpressure should throw")
        } catch let e as RadioTransportError {
            guard case .writeFailed = e else {
                return XCTFail("expected .writeFailed, got \(e)")
            }
        }
    }

    func testUnplugResumesParkedReadEmptyAndDisconnects() async throws {
        let link = MockUSBSerialLink()
        let t = USBSerialTransport(link: link)
        try await t.open()
        async let read: [UInt8] = t.read(maxBytes: 64)
        try await Task.sleep(nanoseconds: 50_000_000)
        link.simulateUnplug()
        let got = try await read
        XCTAssertEqual(got, [])
        // State stream should have reported the drop.
        try await Task.sleep(nanoseconds: 50_000_000)
        let state = await t.state
        XCTAssertEqual(state, .disconnected)
    }

    func testCloseResumesParkedReadsEmpty() async throws {
        let link = MockUSBSerialLink()
        let t = USBSerialTransport(link: link)
        try await t.open()
        async let read: [UInt8] = t.read(maxBytes: 64)
        try await Task.sleep(nanoseconds: 50_000_000)
        await t.close()
        let got = try await read
        XCTAssertEqual(got, [])
    }

    func testCancellingOneReadLeavesOthersParked() async throws {
        let link = MockUSBSerialLink()
        let t = USBSerialTransport(link: link)
        try await t.open()
        let cancelled = Task { try await t.read(maxBytes: 64) }
        try await Task.sleep(nanoseconds: 50_000_000)
        async let survivor: [UInt8] = t.read(maxBytes: 64)
        try await Task.sleep(nanoseconds: 50_000_000)

        cancelled.cancel()
        let cancelledResult = try await cancelled.value
        XCTAssertEqual(cancelledResult, [], "cancelled read resumes empty")

        // The OTHER parked read must still be alive and receive data —
        // cancellation must not broadcast the link-closed sentinel.
        link.radioSends([0x42])
        let got = try await survivor
        XCTAssertEqual(got, [0x42], "sibling read must survive a cancellation")
    }

    func testParkedReadRespectsMaxBytes() async throws {
        let link = MockUSBSerialLink()
        let t = USBSerialTransport(link: link)
        try await t.open()
        async let first: [UInt8] = t.read(maxBytes: 4)
        try await Task.sleep(nanoseconds: 50_000_000)
        link.radioSends([1, 2, 3, 4, 5, 6, 7, 8, 9, 10])
        let head = try await first
        XCTAssertEqual(head, [1, 2, 3, 4], "parked read must honor its maxBytes")
        let rest = try await t.read(maxBytes: 64)
        XCTAssertEqual(rest, [5, 6, 7, 8, 9, 10], "remainder stays buffered")
    }

    func testOpenFailureAfterLinkOpenClosesLink() async {
        let link = MockUSBSerialLink()
        link.failArmDoorbell = true
        let t = USBSerialTransport(link: link)
        do {
            try await t.open()
            XCTFail("open() should throw when arming fails")
        } catch { /* expected */ }
        XCTAssertEqual(link.closeCount, 1,
                       "a half-open link must be closed, not leaked")
    }

    func testMultiChunkDrainDeliversEverything() async throws {
        let link = MockUSBSerialLink()
        let t = USBSerialTransport(link: link)
        try await t.open()
        let big = [UInt8](repeating: 0x55, count: 10_000)
        link.radioSends(big)
        var got: [UInt8] = []
        while got.count < big.count {
            got += try await t.read(maxBytes: 4096)
        }
        XCTAssertEqual(got, big)
    }
}
