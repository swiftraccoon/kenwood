// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

#if os(macOS)

import Foundation
import XCTest
@testable import Azimuth
#if os(macOS)
@preconcurrency import CoreBluetooth
#endif

private final class AzimuthGeneratedBluetoothCoreMock:
    BluetoothByteTransportProtocol, @unchecked Sendable
{
    let readStarted: AsyncStream<Void>
    let openStarted: AsyncStream<Void>

    private let lock = NSLock()
    private let readStartedContinuation: AsyncStream<Void>.Continuation
    private let openStartedContinuation: AsyncStream<Void>.Continuation
    private var queuedReadResults: [Result<Data, BluetoothLinkError>]
    private var pendingRead: CheckedContinuation<Data, Error>?
    private var pendingOpen: CheckedContinuation<Void, Error>?
    private var stickyInterrupt = false
    private var stickyOpenInterrupt = false
    private var blockedOpensRemaining: Int
    private var reads = 0
    private var cancellations = 0
    private var opens = 0
    private var openCancellations = 0
    private var closes = 0

    init(
        readResults: [Result<Data, BluetoothLinkError>] = [],
        blockedOpenCount: Int = 0
    ) {
        queuedReadResults = readResults
        blockedOpensRemaining = blockedOpenCount
        var continuation: AsyncStream<Void>.Continuation!
        readStarted = AsyncStream { continuation = $0 }
        readStartedContinuation = continuation
        var openContinuation: AsyncStream<Void>.Continuation!
        openStarted = AsyncStream { openContinuation = $0 }
        openStartedContinuation = openContinuation
    }

    var readCallCount: Int { lock.withLock { reads } }
    var cancellationCallCount: Int { lock.withLock { cancellations } }
    var openCallCount: Int { lock.withLock { opens } }
    var openCancellationCallCount: Int { lock.withLock { openCancellations } }
    var closeCallCount: Int { lock.withLock { closes } }

    func cancelPendingOpen() {
        let continuation = lock.withLock {
            openCancellations += 1
            guard let pendingOpen else {
                stickyOpenInterrupt = true
                return nil as CheckedContinuation<Void, Error>?
            }
            self.pendingOpen = nil
            return pendingOpen
        }
        continuation?.resume(throwing: BluetoothLinkError.OpenInterrupted)
    }

    func cancelPendingRead() {
        let continuation = lock.withLock {
            cancellations += 1
            guard let pendingRead else {
                stickyInterrupt = true
                return nil as CheckedContinuation<Data, Error>?
            }
            self.pendingRead = nil
            return pendingRead
        }
        continuation?.resume(throwing: BluetoothLinkError.ReadInterrupted)
    }

    func close() async throws {
        let continuation = lock.withLock {
            closes += 1
            stickyOpenInterrupt = false
            let continuation = pendingOpen
            pendingOpen = nil
            return continuation
        }
        continuation?.resume(throwing: BluetoothLinkError.OpenInterrupted)
    }

    func matchedCatSerial() throws -> String? { nil }
    func matchedAddress() throws -> String? { nil }

    func open() async throws {
        let shouldBlock = try lock.withLock {
            opens += 1
            if stickyOpenInterrupt {
                stickyOpenInterrupt = false
                throw BluetoothLinkError.OpenInterrupted
            }
            guard blockedOpensRemaining > 0 else { return false }
            blockedOpensRemaining -= 1
            return true
        }
        guard shouldBlock else { return }
        try await withCheckedThrowingContinuation { continuation in
            lock.withLock { pendingOpen = continuation }
            openStartedContinuation.yield(())
        }
    }

    func read(maxLength: UInt32) async throws -> Data {
        _ = maxLength
        let immediate = lock.withLock {
            reads += 1
            guard !queuedReadResults.isEmpty else {
                return nil as Result<Data, BluetoothLinkError>?
            }
            return queuedReadResults.removeFirst()
        }
        if let immediate {
            return try immediate.get()
        }

        return try await withCheckedThrowingContinuation { continuation in
            let wasInterrupted = lock.withLock {
                if stickyInterrupt {
                    stickyInterrupt = false
                    return true
                }
                pendingRead = continuation
                return false
            }
            if wasInterrupted {
                continuation.resume(
                    throwing: BluetoothLinkError.ReadInterrupted
                )
            } else {
                readStartedContinuation.yield(())
            }
        }
    }

    func reopen() async throws {}

    func setBaudRate(baud: UInt32) { _ = baud }

    func write(bytes: Data) async throws { _ = bytes }
}

private final class AzimuthBluetoothAuthorizationCallCounter:
    @unchecked Sendable
{
    private let lock = NSLock()
    private var calls = 0

    var callCount: Int { lock.withLock { calls } }

    func record() {
        lock.withLock { calls += 1 }
    }
}

private final class AzimuthBlockingBluetoothAuthorization:
    AzimuthBluetoothAuthorizationProviding,
    @unchecked Sendable
{
    let started: AsyncStream<Void>
    private let continuation: AsyncStream<Void>.Continuation

    init() {
        var continuation: AsyncStream<Void>.Continuation!
        started = AsyncStream { continuation = $0 }
        self.continuation = continuation
    }

    func ensureBluetoothAuthorization() async throws {
        continuation.yield(())
        try await Task.sleep(for: .seconds(60))
    }
}

#if os(macOS)
private final class AzimuthForegroundActivationProbe: @unchecked Sendable {
    let pauseStarted: AsyncStream<Void>

    private let lock = NSLock()
    private let pauseStartedContinuation: AsyncStream<Void>.Continuation
    private var active = false
    private var activationRequests = 0
    private var pauseContinuation: CheckedContinuation<Void, Never>?

    init() {
        var continuation: AsyncStream<Void>.Continuation!
        pauseStarted = AsyncStream { continuation = $0 }
        pauseStartedContinuation = continuation
    }

    var isActive: Bool { lock.withLock { active } }
    var activationRequestCount: Int { lock.withLock { activationRequests } }

    func requestActivation() {
        lock.withLock { activationRequests += 1 }
    }

    func pause() async {
        await withCheckedContinuation { continuation in
            lock.withLock { pauseContinuation = continuation }
            pauseStartedContinuation.yield(())
        }
    }

    func becomeActive() {
        let continuation = lock.withLock {
            active = true
            let continuation = pauseContinuation
            pauseContinuation = nil
            return continuation
        }
        continuation?.resume()
    }
}
#endif

final class AzimuthGeneratedBluetoothRadioBridgeTests: XCTestCase {
    #if os(macOS)
    func testForegroundActivationWaitsForActualActiveState() async throws {
        let probe = AzimuthForegroundActivationProbe()
        let activation = AzimuthMacBluetoothForegroundActivation(
            maximumChecks: 2,
            isActive: { probe.isActive },
            requestActivation: { probe.requestActivation() },
            pause: { await probe.pause() }
        )
        var pauseStarted = probe.pauseStarted.makeAsyncIterator()
        let task = Task { try await activation.ensureForeground() }

        let didPause: Void? = await pauseStarted.next()
        XCTAssertNotNil(didPause)
        XCTAssertEqual(probe.activationRequestCount, 1)

        probe.becomeActive()
        try await task.value
        XCTAssertTrue(probe.isActive)
    }

    func testProviderRechecksForegroundImmediatelyBeforeManagerCreation() async throws {
        let managerCreations = AzimuthBluetoothAuthorizationCallCounter()
        let provider = AzimuthMacBluetoothAuthorizationProvider(
            authorizeFromForeground: {},
            currentAuthorization: { nil },
            isForegroundActive: { false },
            makeManager: { _ in
                managerCreations.record()
                fatalError("A background provider must not construct CBCentralManager")
            }
        )

        do {
            try await provider.ensureBluetoothAuthorization()
            XCTFail("A provider which lost foreground status must fail closed")
        } catch let error as AzimuthBluetoothAuthorizationError {
            XCTAssertEqual(error, .foregroundActivationRequired)
        }
        XCTAssertEqual(managerCreations.callCount, 0)
    }

    func testUnsupportedBluetoothStateIsTerminalAndActionable() throws {
        let result = AzimuthMacBluetoothAuthorizationProvider.authorizationResult(
            authorization: .notDetermined,
            centralState: .unsupported
        )
        do {
            try XCTUnwrap(result).get()
            XCTFail("Unsupported Bluetooth must not leave authorization pending")
        } catch let error as AzimuthBluetoothAuthorizationError {
            XCTAssertEqual(error, .bluetoothUnavailable)
            XCTAssertTrue(error.localizedDescription.contains("USB-C"))
        }
    }
    #endif

    func testAllowedBluetoothAuthorizationPrecedesNativeOpen() async throws {
        let core = AzimuthGeneratedBluetoothCoreMock()
        let counter = AzimuthBluetoothAuthorizationCallCounter()
        let link = AzimuthGeneratedBluetoothByteLink(
            core: core,
            authorization: AzimuthBluetoothAuthorizationBridge {
                counter.record()
            }
        )

        try await link.open()

        XCTAssertEqual(counter.callCount, 1)
        XCTAssertEqual(core.openCallCount, 1)
    }

    func testDeniedBluetoothAuthorizationPreventsNativeOpen() async throws {
        let core = AzimuthGeneratedBluetoothCoreMock()
        let link = AzimuthGeneratedBluetoothByteLink(
            core: core,
            authorization: AzimuthBluetoothAuthorizationBridge {
                throw AzimuthBluetoothAuthorizationError.denied
            }
        )

        do {
            try await link.open()
            XCTFail("Denied Bluetooth authorization must stop before native open")
        } catch let error as AzimuthBluetoothAuthorizationError {
            XCTAssertEqual(error, .denied)
            XCTAssertTrue(error.localizedDescription.contains("System Settings"))
        }
        XCTAssertEqual(core.openCallCount, 0)
    }

    func testCancelledBluetoothAuthorizationPreventsNativeOpen() async throws {
        let core = AzimuthGeneratedBluetoothCoreMock()
        let authorization = AzimuthBlockingBluetoothAuthorization()
        let link = AzimuthGeneratedBluetoothByteLink(
            core: core,
            authorization: authorization
        )
        var started = authorization.started.makeAsyncIterator()
        let open = Task { try await link.open() }
        let didStart: Void? = await started.next()
        XCTAssertNotNil(didStart)

        open.cancel()

        do {
            try await open.value
            XCTFail("Cancelled Bluetooth authorization must stop the open")
        } catch is CancellationError {
            // Expected.
        }
        XCTAssertEqual(core.openCallCount, 0)
    }

    func testCancellingOpenInterruptsNativeHelperAndLeavesReopenClean() async throws {
        let core = AzimuthGeneratedBluetoothCoreMock(blockedOpenCount: 1)
        let link = AzimuthGeneratedBluetoothByteLink(core: core)
        var started = core.openStarted.makeAsyncIterator()
        let openTask = Task {
            try await link.open()
        }

        let openDidStart: Void? = await started.next()
        XCTAssertNotNil(openDidStart)
        openTask.cancel()

        do {
            try await openTask.value
            XCTFail("Expected the cancelled Bluetooth open to throw")
        } catch is CancellationError {
            // Expected.
        } catch {
            XCTFail("Expected CancellationError, received \(error)")
        }
        XCTAssertEqual(core.openCancellationCallCount, 1)
        XCTAssertEqual(core.closeCallCount, 1)

        try await link.open()
        XCTAssertEqual(core.openCallCount, 2)
    }

    func testAlreadyCancelledAuthorizationSkipsNativeOpenAndLeavesReopenClean() async throws {
        let core = AzimuthGeneratedBluetoothCoreMock()
        let link = AzimuthGeneratedBluetoothByteLink(core: core)
        let openTask = Task {
            try await link.open()
        }
        openTask.cancel()

        do {
            try await openTask.value
            XCTFail("Expected the cancelled Bluetooth open to throw")
        } catch is CancellationError {
            // Expected.
        } catch {
            XCTFail("Expected CancellationError, received \(error)")
        }
        XCTAssertEqual(core.openCancellationCallCount, 0)
        XCTAssertEqual(core.closeCallCount, 1)

        try await link.open()
        XCTAssertEqual(core.openCallCount, 1)
    }

    func testLiveReadRetriesNativeInterrupt() async throws {
        let core = AzimuthGeneratedBluetoothCoreMock(
            readResults: [
                .failure(BluetoothLinkError.ReadInterrupted),
                .success(Data([4, 5, 6])),
            ]
        )
        let link = AzimuthGeneratedBluetoothByteLink(core: core)

        let bytes = try await link.read(maxBytes: 32)

        XCTAssertEqual(bytes, [4, 5, 6])
        XCTAssertEqual(core.readCallCount, 2)
        XCTAssertEqual(core.cancellationCallCount, 0)
    }

    func testCancellingReadInterruptsNativeFutureAndReturnsCancellation() async {
        let core = AzimuthGeneratedBluetoothCoreMock()
        let link = AzimuthGeneratedBluetoothByteLink(core: core)
        var started = core.readStarted.makeAsyncIterator()
        let readTask = Task {
            try await link.read(maxBytes: 32)
        }

        let readDidStart: Void? = await started.next()
        XCTAssertNotNil(readDidStart)
        readTask.cancel()

        do {
            _ = try await readTask.value
            XCTFail("Expected the cancelled Bluetooth read to throw")
        } catch is CancellationError {
            // Expected: the native interrupt is translated to Swift task
            // cancellation only for the task which owns that read.
        } catch {
            XCTFail("Expected CancellationError, received \(error)")
        }
        XCTAssertEqual(core.cancellationCallCount, 1)
        XCTAssertEqual(core.readCallCount, 1)
    }
}

#endif
