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
    private var writeAttempts = 0
    private var recordedBaudRates: [UInt32] = []
    private var closes = 0
    private var opens = 0
    private var openAttempts = 0
    private var scriptedOpenFailures: [AzimuthUSBLinkError] = []
    private var serviceAvailable = true
    private var controlServiceAvailable = true
    private var registrationWaitNanoseconds: UInt64 = 0
    private var presenceChecks = 0
    private var controlPresenceChecks = 0
    private var statusChecks = 0
    private var dextLogChecks = 0
    private var drainCalls = 0
    private var doorbellArmCalls = 0
    private var doorbellArmAttempts = 0
    private var scriptedArmFailures: [AzimuthUSBLinkError] = []
    private var immediateDoorbellCallbacks = 0
    private var afterEmptyDrainInjection: [UInt8]?
    private var savedCloseDoorbells: [@Sendable (Bool) -> Void] = []
    private var remainingBackpressureResponses = 0

    var failArm = false
    var saveDoorbellOnClose = false

    var backpressureResponses: Int {
        get { lock.withLock { remainingBackpressureResponses } }
        set { lock.withLock { remainingBackpressureResponses = newValue } }
    }

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

    var writeAttemptCount: Int {
        lock.withLock { writeAttempts }
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

    var openAttemptCount: Int {
        lock.withLock { openAttempts }
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

    var doorbellArmAttemptCount: Int {
        lock.withLock { doorbellArmAttempts }
    }

    var immediateDoorbellCallbackCount: Int {
        lock.withLock { immediateDoorbellCallbacks }
    }

    func injectAfterNextEmptyDrain(_ bytes: [UInt8]) {
        lock.withLock { afterEmptyDrainInjection = bytes }
    }

    func failNextOpen(with error: AzimuthUSBLinkError) {
        lock.withLock { scriptedOpenFailures.append(error) }
    }

    func failNextArm(with error: AzimuthUSBLinkError) {
        lock.withLock { scriptedArmFailures.append(error) }
    }

    func takeSavedCloseDoorbell() -> (@Sendable (Bool) -> Void)? {
        lock.withLock {
            guard !savedCloseDoorbells.isEmpty else { return nil }
            return savedCloseDoorbells.removeFirst()
        }
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
            openAttempts += 1
            guard serviceAvailable else { throw AzimuthUSBLinkError.serviceNotFound }
            if !scriptedOpenFailures.isEmpty {
                throw scriptedOpenFailures.removeFirst()
            }
            opens += 1
            opened = true
        }
    }

    func close() {
        let callback: (@Sendable (Bool) -> Void)? = lock.withLock {
            opened = false
            closes += 1
            let callback = doorbell
            doorbell = nil
            if saveDoorbellOnClose, let callback {
                savedCloseDoorbells.append(callback)
                return nil
            }
            return callback
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
            writeAttempts += 1
            if remainingBackpressureResponses > 0 {
                remainingBackpressureResponses -= 1
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
            doorbellArmAttempts += 1
            if !scriptedArmFailures.isEmpty {
                throw scriptedArmFailures.removeFirst()
            }
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

    func testOpenRetriesStaleNoDeviceServiceWithinRegistrationWindow() async throws {
        try await assertTransientOpenRetry(
            code: Int32(bitPattern: 0xE00002C0)
        )
    }

    func testOpenRetriesStaleNotAttachedServiceWithinRegistrationWindow() async throws {
        try await assertTransientOpenRetry(
            code: Int32(bitPattern: 0xE00002D9)
        )
    }

    func testOpenRetriesServiceDisappearingBetweenPresenceCheckAndOpen() async throws {
        let link = AzimuthMockUSBLink()
        link.serviceRegistrationWaitNanoseconds = 400_000_000
        link.failNextOpen(with: .serviceNotFound)
        let transport = AzimuthUSBSerialTransport(link: link)

        try await transport.open()

        XCTAssertEqual(link.openAttemptCount, 2)
        XCTAssertEqual(link.openCount, 1)
        let connectedState = await transport.state
        XCTAssertEqual(connectedState, .connected)
        await transport.close()
    }

    func testOpenRetriesTransientDoorbellFailureFromTerminatingDext() async throws {
        let link = AzimuthMockUSBLink()
        link.serviceRegistrationWaitNanoseconds = 400_000_000
        link.failNextArm(
            with: .callFailed(code: Int32(bitPattern: 0xE00002D6))
        )
        let transport = AzimuthUSBSerialTransport(link: link)

        try await transport.open()

        XCTAssertEqual(link.openAttemptCount, 2)
        XCTAssertEqual(link.openCount, 2)
        XCTAssertEqual(link.closeCount, 1)
        XCTAssertEqual(link.doorbellArmAttemptCount, 2)
        XCTAssertEqual(link.doorbellArmCallCount, 1)
        let connectedState = await transport.state
        XCTAssertEqual(connectedState, .connected)
        await transport.close()
    }

    func testCloseDuringTransientSessionRetryCannotClobberReopenedLink() async throws {
        let link = AzimuthMockUSBLink()
        link.serviceRegistrationWaitNanoseconds = 400_000_000
        link.failNextOpen(
            with: .openFailed(code: Int32(bitPattern: 0xE00002C0))
        )
        let transport = AzimuthUSBSerialTransport(link: link)
        let staleOpen = Task { try await transport.open() }

        for _ in 0..<1_000 {
            if link.openAttemptCount >= 1 { break }
            await Task.yield()
        }
        XCTAssertEqual(link.openAttemptCount, 1)

        await transport.close()
        try await transport.open()

        do {
            try await staleOpen.value
            XCTFail("the superseded retry must report cancellation")
        } catch is CancellationError {
            // The explicit close and reopen own the newer generation.
        } catch {
            XCTFail("unexpected stale-open error: \(error)")
        }

        XCTAssertEqual(link.openCount, 1)
        XCTAssertEqual(link.closeCount, 2)
        let connectedState = await transport.state
        XCTAssertEqual(connectedState, .connected)
        try await transport.write([0x51])
        XCTAssertEqual(link.writes, [[0x51]])
        await transport.close()
    }

    func testOpenDoesNotRetryTransientServiceFailureWithoutRegistrationWindow() async {
        let link = AzimuthMockUSBLink()
        link.failNextOpen(
            with: .openFailed(code: Int32(bitPattern: 0xE00002C0))
        )
        let transport = AzimuthUSBSerialTransport(link: link)

        do {
            try await transport.open()
            XCTFail("a link without a registration window must fail immediately")
        } catch let error as AzimuthRadioTransportError {
            guard case .openFailed(let reason) = error else {
                return XCTFail("unexpected transport error: \(error)")
            }
            XCTAssertTrue(reason.contains("kIOReturnNoDevice"))
        } catch {
            XCTFail("unexpected error: \(error)")
        }

        XCTAssertEqual(link.openAttemptCount, 1)
        XCTAssertEqual(link.openCount, 0)
    }

    func testClosedServiceWaitCannotReopenOrClobberNewerConnection() async throws {
        let link = AzimuthMockUSBLink()
        link.present = false
        link.serviceRegistrationWaitNanoseconds = 400_000_000
        let transport = AzimuthUSBSerialTransport(link: link)
        let staleOpen = Task { try await transport.open() }

        for _ in 0..<1_000 {
            if link.servicePresenceCheckCount >= 2 { break }
            await Task.yield()
        }
        XCTAssertGreaterThanOrEqual(link.servicePresenceCheckCount, 2)

        await transport.close()
        link.present = true
        try await transport.open()

        do {
            try await staleOpen.value
            XCTFail("superseded open should report cancellation")
        } catch is CancellationError {
            // The close/new open generation intentionally superseded it.
        } catch {
            XCTFail("unexpected stale-open error: \(error)")
        }

        XCTAssertEqual(link.openCount, 1)
        let connectedState = await transport.state
        XCTAssertEqual(connectedState, .connected)

        async let read = transport.read(maxBytes: 8)
        link.sendFromRadio([0x44])
        let bytes = try await read
        XCTAssertEqual(bytes, [0x44])
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

    func testBackpressuredWriteCannotCrossCloseAndReopenGeneration() async throws {
        let link = AzimuthMockUSBLink()
        link.backpressureResponses = 20
        let transport = AzimuthUSBSerialTransport(link: link)
        try await transport.open()
        let staleWrite = Task { try await transport.write([0x41]) }

        for _ in 0..<1_000 {
            if link.writeAttemptCount >= 1 { break }
            await Task.yield()
        }
        XCTAssertGreaterThanOrEqual(link.writeAttemptCount, 1)

        await transport.close()
        try await transport.open()

        do {
            try await staleWrite.value
            XCTFail("a write owned by the closed generation must fail")
        } catch let error as AzimuthRadioTransportError {
            XCTAssertEqual(error, .notConnected)
        } catch {
            XCTFail("unexpected stale-write error: \(error)")
        }

        XCTAssertEqual(link.writes, [])
        link.backpressureResponses = 0
        try await transport.write([0x42])
        XCTAssertEqual(link.writes, [[0x42]])
        await transport.close()
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

    func testLinkDropDiscardsSwiftReceiveBufferBeforeReopen() async throws {
        let link = AzimuthMockUSBLink()
        let transport = AzimuthUSBSerialTransport(link: link)
        try await transport.open()
        let drainsBeforeArrival = link.drainCallCount

        link.sendFromRadio([0x11, 0x22])
        for _ in 0..<1_000 {
            if link.drainCallCount >= drainsBeforeArrival + 2 { break }
            await Task.yield()
        }
        XCTAssertGreaterThanOrEqual(link.drainCallCount, drainsBeforeArrival + 2)

        link.unplug()
        for _ in 0..<1_000 {
            if await transport.state == .disconnected { break }
            await Task.yield()
        }
        let disconnectedState = await transport.state
        XCTAssertEqual(disconnectedState, .disconnected)

        try await transport.open()
        async let reopenedRead = transport.read(maxBytes: 8)
        try await Task.sleep(nanoseconds: 20_000_000)
        link.sendFromRadio([0x33])
        let reopenedBytes = try await reopenedRead
        XCTAssertEqual(reopenedBytes, [0x33])
        await transport.close()
    }

    func testDelayedDoorbellsFromClosedGenerationsCannotAffectReopenedLink() async throws {
        let link = AzimuthMockUSBLink()
        link.saveDoorbellOnClose = true
        let transport = AzimuthUSBSerialTransport(link: link)

        try await transport.open()
        await transport.close()
        let staleLinkDown = try XCTUnwrap(link.takeSavedCloseDoorbell())

        try await transport.open()
        let closesAfterFirstReopen = link.closeCount
        staleLinkDown(false)
        for _ in 0..<20 { await Task.yield() }
        let stateAfterStaleLinkDown = await transport.state
        XCTAssertEqual(stateAfterStaleLinkDown, .connected)
        XCTAssertEqual(link.closeCount, closesAfterFirstReopen)

        async let firstRead = transport.read(maxBytes: 8)
        link.sendFromRadio([0x31])
        let firstBytes = try await firstRead
        XCTAssertEqual(firstBytes, [0x31])

        await transport.close()
        let staleReadability = try XCTUnwrap(link.takeSavedCloseDoorbell())
        try await transport.open()
        let drainCallsAfterSecondReopen = link.drainCallCount
        let armCallsAfterSecondReopen = link.doorbellArmCallCount
        let closesAfterSecondReopen = link.closeCount

        staleReadability(true)
        for _ in 0..<20 { await Task.yield() }
        let stateAfterStaleReadability = await transport.state
        XCTAssertEqual(stateAfterStaleReadability, .connected)
        XCTAssertEqual(link.drainCallCount, drainCallsAfterSecondReopen)
        XCTAssertEqual(link.doorbellArmCallCount, armCallsAfterSecondReopen)
        XCTAssertEqual(link.closeCount, closesAfterSecondReopen)

        async let secondRead = transport.read(maxBytes: 8)
        link.sendFromRadio([0x32])
        let secondBytes = try await secondRead
        XCTAssertEqual(secondBytes, [0x32])
        await transport.close()
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

    private func assertTransientOpenRetry(code: Int32) async throws {
        let link = AzimuthMockUSBLink()
        link.serviceRegistrationWaitNanoseconds = 400_000_000
        link.failNextOpen(with: .openFailed(code: code))
        let transport = AzimuthUSBSerialTransport(link: link)

        try await transport.open()

        XCTAssertEqual(link.openAttemptCount, 2)
        XCTAssertEqual(link.openCount, 1)
        XCTAssertEqual(link.closeCount, 1)
        let connectedState = await transport.state
        XCTAssertEqual(connectedState, .connected)
        await transport.close()
    }
}
