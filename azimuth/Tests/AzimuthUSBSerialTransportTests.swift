// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import XCTest
@testable import Azimuth

private final class AzimuthMockUSBLink: AzimuthUSBSerialLink, @unchecked Sendable {
    private let lock = NSLock()
    private var opened = false
    private var buffered: [UInt8] = []
    private var doorbell: (@Sendable (Bool) -> Void)?
    private var recordedWrites: [[UInt8]] = []
    private var recordedBaudRates: [UInt32] = []
    private var closes = 0
    private var opens = 0
    private var serviceAvailable = true
    private var controlServiceAvailable = true
    private var registrationWaitNanoseconds: UInt64 = 0
    private var presenceChecks = 0
    private var controlPresenceChecks = 0
    private var statusChecks = 0
    private var dextLogChecks = 0
    private var drainCalls = 0
    private var doorbellArmCalls = 0
    private var immediateDoorbellCallbacks = 0
    private var afterEmptyDrainInjection: [UInt8]?

    var backpressureResponses = 0
    var failArm = false

    var present: Bool {
        get { lock.withLock { serviceAvailable } }
        set { lock.withLock { serviceAvailable = newValue } }
    }

    var controlPresent: Bool {
        get { lock.withLock { controlServiceAvailable } }
        set { lock.withLock { controlServiceAvailable = newValue } }
    }

    var serviceRegistrationWaitNanoseconds: UInt64 {
        get { lock.withLock { registrationWaitNanoseconds } }
        set { lock.withLock { registrationWaitNanoseconds = newValue } }
    }

    var writes: [[UInt8]] {
        lock.withLock { recordedWrites }
    }

    var baudRates: [UInt32] {
        lock.withLock { recordedBaudRates }
    }

    var closeCount: Int {
        lock.withLock { closes }
    }

    var openCount: Int {
        lock.withLock { opens }
    }

    var servicePresenceCheckCount: Int {
        lock.withLock { presenceChecks }
    }

    var controlServicePresenceCheckCount: Int {
        lock.withLock { controlPresenceChecks }
    }

    var statusCheckCount: Int {
        lock.withLock { statusChecks }
    }

    var dextLogCheckCount: Int {
        lock.withLock { dextLogChecks }
    }

    var drainCallCount: Int {
        lock.withLock { drainCalls }
    }

    var doorbellArmCallCount: Int {
        lock.withLock { doorbellArmCalls }
    }

    var immediateDoorbellCallbackCount: Int {
        lock.withLock { immediateDoorbellCallbacks }
    }

    func injectAfterNextEmptyDrain(_ bytes: [UInt8]) {
        lock.withLock { afterEmptyDrainInjection = bytes }
    }

    func servicePresent() -> Bool {
        lock.withLock {
            presenceChecks += 1
            return serviceAvailable
        }
    }

    func commServicePresent() -> Bool? {
        lock.withLock {
            controlPresenceChecks += 1
            return controlServiceAvailable
        }
    }

    func open() throws {
        try lock.withLock {
            guard serviceAvailable else { throw AzimuthUSBLinkError.serviceNotFound }
            opens += 1
            opened = true
        }
    }

    func close() {
        let callback: (@Sendable (Bool) -> Void)? = lock.withLock {
            opened = false
            closes += 1
            defer { doorbell = nil }
            return doorbell
        }
        callback?(false)
    }

    func setBaudRate(baud: UInt32) throws {
        try lock.withLock {
            guard opened else { throw AzimuthUSBLinkError.notOpen }
            guard AzimuthUSBABIV2.supportedBaudRates.contains(baud) else {
                throw AzimuthUSBLinkError.unsupportedBaudRate(baud)
            }
            recordedBaudRates.append(baud)
        }
    }

    func write(_ bytes: [UInt8]) throws {
        try lock.withLock {
            guard opened else { throw AzimuthUSBLinkError.notOpen }
            if backpressureResponses > 0 {
                backpressureResponses -= 1
                throw AzimuthUSBLinkError.backpressure
            }
            recordedWrites.append(bytes)
        }
    }

    func drain(maxBytes: Int) throws -> [UInt8] {
        try lock.withLock {
            guard opened else { throw AzimuthUSBLinkError.notOpen }
            drainCalls += 1
            let count = min(maxBytes, buffered.count)
            let result = Array(buffered.prefix(count))
            buffered.removeFirst(count)
            if result.isEmpty, let injection = afterEmptyDrainInjection {
                afterEmptyDrainInjection = nil
                buffered.append(contentsOf: injection)
            }
            return result
        }
    }

    func armDoorbell(onFire: @escaping @Sendable (Bool) -> Void) throws {
        var fireImmediately = false
        try lock.withLock {
            guard opened else { throw AzimuthUSBLinkError.notOpen }
            if failArm { throw AzimuthUSBLinkError.callFailed(code: -1) }
            doorbellArmCalls += 1
            if buffered.isEmpty {
                doorbell = onFire
            } else {
                immediateDoorbellCallbacks += 1
                fireImmediately = true
            }
        }
        if fireImmediately { onFire(true) }
    }

    func status() throws -> AzimuthUSBDextStatus? {
        try lock.withLock {
            guard opened else { throw AzimuthUSBLinkError.notOpen }
            statusChecks += 1
            return AzimuthUSBDextStatus(
                rxBuffered: UInt64(buffered.count),
                rxOverflowBytes: 0,
                linkUp: true,
                doorbellArmed: doorbell != nil
            )
        }
    }

    func dextLog() throws -> [AzimuthUSBDextLogEntry] {
        try lock.withLock {
            guard opened else { throw AzimuthUSBLinkError.notOpen }
            dextLogChecks += 1
            return []
        }
    }

    func sendFromRadio(_ bytes: [UInt8]) {
        let callback: (@Sendable (Bool) -> Void)? = lock.withLock {
            let wasEmpty = buffered.isEmpty
            buffered.append(contentsOf: bytes)
            guard wasEmpty else { return nil }
            defer { doorbell = nil }
            return doorbell
        }
        callback?(true)
    }

    func unplug() {
        let callback: (@Sendable (Bool) -> Void)? = lock.withLock {
            opened = false
            defer { doorbell = nil }
            return doorbell
        }
        callback?(false)
    }
}

final class AzimuthUSBSerialTransportTests: XCTestCase {
    func testOpenWaitsForDelayedServiceRegistration() async throws {
        let link = AzimuthMockUSBLink()
        link.present = false
        link.serviceRegistrationWaitNanoseconds = 400_000_000
        let transport = AzimuthUSBSerialTransport(link: link)
        let registration = Task {
            try await Task.sleep(nanoseconds: 150_000_000)
            link.present = true
        }

        try await transport.open()
        try await registration.value

        XCTAssertEqual(link.openCount, 1)
        XCTAssertGreaterThanOrEqual(link.servicePresenceCheckCount, 2)
        let state = await transport.state
        XCTAssertEqual(state, .connected)
        await transport.close()
    }

    func testOpenTimesOutWithoutOpeningWhenServiceNeverRegisters() async {
        let link = AzimuthMockUSBLink()
        link.present = false
        link.serviceRegistrationWaitNanoseconds = 200_000_000
        let transport = AzimuthUSBSerialTransport(link: link)

        do {
            try await transport.open()
            XCTFail("open should time out while the service remains absent")
        } catch let error as AzimuthRadioTransportError {
            guard case .openFailed(let reason) = error else {
                return XCTFail("unexpected transport error: \(error)")
            }
            XCTAssertTrue(reason.contains("USB serial"))
        } catch {
            XCTFail("unexpected error: \(error)")
        }

        XCTAssertEqual(link.openCount, 0)
        XCTAssertEqual(link.statusCheckCount, 0)
        XCTAssertEqual(link.dextLogCheckCount, 0)
        XCTAssertGreaterThanOrEqual(link.servicePresenceCheckCount, 3)
        let state = await transport.state
        guard case .failed = state else {
            return XCTFail("transport should publish a failed state, got \(state)")
        }
    }

    func testOpenWaitsForControlServiceAfterDataServiceIsReady() async throws {
        let link = AzimuthMockUSBLink()
        link.controlPresent = false
        link.serviceRegistrationWaitNanoseconds = 400_000_000
        let transport = AzimuthUSBSerialTransport(link: link)
        let registration = Task {
            try await Task.sleep(nanoseconds: 150_000_000)
            link.controlPresent = true
        }

        try await transport.open()
        try await registration.value

        XCTAssertEqual(link.openCount, 1)
        XCTAssertGreaterThanOrEqual(link.controlServicePresenceCheckCount, 2)
        let state = await transport.state
        XCTAssertEqual(state, .connected)
        await transport.close()
    }

    func testOpenRejectsDataOnlyDriverRegistration() async {
        let link = AzimuthMockUSBLink()
        link.controlPresent = false
        link.serviceRegistrationWaitNanoseconds = 200_000_000
        let transport = AzimuthUSBSerialTransport(link: link)

        do {
            try await transport.open()
            XCTFail("open must not proceed without the CDC control service")
        } catch let error as AzimuthRadioTransportError {
            guard case .openFailed(let reason) = error else {
                return XCTFail("unexpected transport error: \(error)")
            }
            XCTAssertTrue(reason.contains("control interface"))
        } catch {
            XCTFail("unexpected error: \(error)")
        }

        XCTAssertEqual(link.openCount, 0)
        XCTAssertGreaterThanOrEqual(link.controlServicePresenceCheckCount, 3)
    }

    func testOpenAndClosePublishState() async throws {
        let link = AzimuthMockUSBLink()
        let transport = AzimuthUSBSerialTransport(link: link)
        try await transport.open()
        let connectedState = await transport.state
        XCTAssertEqual(connectedState, .connected)
        XCTAssertEqual(link.statusCheckCount, 1)
        XCTAssertEqual(link.dextLogCheckCount, 1)
        await transport.close()
        let disconnectedState = await transport.state
        XCTAssertEqual(disconnectedState, .disconnected)
        XCTAssertEqual(link.statusCheckCount, 2)
        XCTAssertEqual(link.dextLogCheckCount, 2)
    }

    func testProgrammingBaudRatesReachLinkSynchronously() async throws {
        let link = AzimuthMockUSBLink()
        let transport = AzimuthUSBSerialTransport(link: link)
        try await transport.open()
        try transport.setBaudRate(baud: 9_600)
        try transport.setBaudRate(baud: 115_200)
        XCTAssertEqual(link.baudRates, [9_600, 115_200])
    }

    func testUnsupportedBaudIsRejectedBeforeLink() async throws {
        let link = AzimuthMockUSBLink()
        let transport = AzimuthUSBSerialTransport(link: link)
        try await transport.open()
        XCTAssertThrowsError(try transport.setBaudRate(baud: 57_600)) { error in
            XCTAssertEqual(error as? AzimuthUSBLinkError, .unsupportedBaudRate(57_600))
        }
        XCTAssertEqual(link.baudRates, [])
    }

    func testLargeWriteIsSplitAtSelectorBound() async throws {
        let link = AzimuthMockUSBLink()
        let transport = AzimuthUSBSerialTransport(link: link)
        try await transport.open()
        try await transport.write([UInt8](repeating: 0x5a, count: 9_000))
        XCTAssertEqual(link.writes.map(\.count), [4_096, 4_096, 808])
    }

    func testTransientBackpressureRetriesWithoutDuplicateAcceptedWrite() async throws {
        let link = AzimuthMockUSBLink()
        link.backpressureResponses = 3
        let transport = AzimuthUSBSerialTransport(link: link)
        try await transport.open()
        try await transport.write([1, 2, 3])
        XCTAssertEqual(link.writes, [[1, 2, 3]])
    }

    func testParkedReadHonorsLimitAndRetainsRemainder() async throws {
        let link = AzimuthMockUSBLink()
        let transport = AzimuthUSBSerialTransport(link: link)
        try await transport.open()
        async let first = transport.read(maxBytes: 4)
        try await Task.sleep(nanoseconds: 20_000_000)
        link.sendFromRadio([1, 2, 3, 4, 5, 6])
        let firstBytes = try await first
        XCTAssertEqual(firstBytes, [1, 2, 3, 4])
        let remainingBytes = try await transport.read(maxBytes: 8)
        XCTAssertEqual(remainingBytes, [5, 6])
    }

    func testDoorbellDrainsBeforeRearmingWithoutImmediateEcho() async throws {
        let link = AzimuthMockUSBLink()
        let transport = AzimuthUSBSerialTransport(link: link)
        try await transport.open()
        let initialDrains = link.drainCallCount
        let initialArms = link.doorbellArmCallCount

        async let read = transport.read(maxBytes: 64)
        try await Task.sleep(nanoseconds: 20_000_000)
        link.sendFromRadio([0x11])

        let bytes = try await read
        XCTAssertEqual(bytes, [0x11])
        try await Task.sleep(nanoseconds: 20_000_000)
        XCTAssertEqual(link.drainCallCount - initialDrains, 2)
        XCTAssertEqual(link.doorbellArmCallCount - initialArms, 1)
        XCTAssertEqual(link.immediateDoorbellCallbackCount, 0)
    }

    func testArrivalBetweenFinalDrainAndRearmUsesOneImmediateDoorbell() async throws {
        let link = AzimuthMockUSBLink()
        let transport = AzimuthUSBSerialTransport(link: link)
        try await transport.open()
        let initialDrains = link.drainCallCount
        let initialArms = link.doorbellArmCallCount
        link.injectAfterNextEmptyDrain([0x22])

        async let firstRead = transport.read(maxBytes: 64)
        try await Task.sleep(nanoseconds: 20_000_000)
        link.sendFromRadio([0x11])

        let firstBytes = try await firstRead
        let secondBytes = try await transport.read(maxBytes: 64)
        XCTAssertEqual(firstBytes, [0x11])
        XCTAssertEqual(secondBytes, [0x22])
        try await Task.sleep(nanoseconds: 20_000_000)
        XCTAssertEqual(link.drainCallCount - initialDrains, 4)
        XCTAssertEqual(link.doorbellArmCallCount - initialArms, 2)
        XCTAssertEqual(link.immediateDoorbellCallbackCount, 1)
    }

    func testHotUnplugWakesParkedReadAndDisconnects() async throws {
        let link = AzimuthMockUSBLink()
        let transport = AzimuthUSBSerialTransport(link: link)
        try await transport.open()
        async let read = transport.read(maxBytes: 64)
        try await Task.sleep(nanoseconds: 20_000_000)
        link.unplug()
        let unpluggedBytes = try await read
        XCTAssertEqual(unpluggedBytes, [])
        try await Task.sleep(nanoseconds: 20_000_000)
        let state = await transport.state
        XCTAssertEqual(state, .disconnected)
    }

    func testOneCancelledReadDoesNotCloseSibling() async throws {
        let link = AzimuthMockUSBLink()
        let transport = AzimuthUSBSerialTransport(link: link)
        try await transport.open()
        let cancelled = Task { try await transport.read(maxBytes: 8) }
        try await Task.sleep(nanoseconds: 20_000_000)
        async let survivor = transport.read(maxBytes: 8)
        try await Task.sleep(nanoseconds: 20_000_000)
        cancelled.cancel()
        let cancelledBytes = try await cancelled.value
        XCTAssertEqual(cancelledBytes, [])
        link.sendFromRadio([0x42])
        let survivingBytes = try await survivor
        XCTAssertEqual(survivingBytes, [0x42])
    }

    func testArmFailureClosesHalfOpenLink() async {
        let link = AzimuthMockUSBLink()
        link.failArm = true
        let transport = AzimuthUSBSerialTransport(link: link)
        do {
            try await transport.open()
            XCTFail("open should fail")
        } catch {
            XCTAssertEqual(link.closeCount, 1)
            XCTAssertEqual(link.statusCheckCount, 1)
            XCTAssertEqual(link.dextLogCheckCount, 1)
        }
    }
}
