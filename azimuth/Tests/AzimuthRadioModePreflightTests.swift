// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import XCTest
@testable import Azimuth

final class AzimuthRadioModePreflightTests: XCTestCase {
    func testMMDVMResponseIsClassifiedAndEntireFrameIsDrained() async throws {
        let transport = RadioModeTestTransport(
            reads: [[0xE0], [0x06], [0x00, 0x41, 0x42, 0x43]]
        )
        let preflight = AzimuthRadioModePreflight(
            transport: transport,
            probeTimeout: .milliseconds(100),
            parserResetTimeout: .milliseconds(5)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .mmdvm)
        XCTAssertEqual(transport.writes, [[0xE0, 0x03, 0x00]])
        XCTAssertEqual(transport.remainingScriptedReadCount, 0)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testSilenceFromBothProbesIsNotMisreportedAsCAT() async throws {
        let transport = RadioModeTestTransport()
        let preflight = AzimuthRadioModePreflight(
            transport: transport,
            probeTimeout: .milliseconds(5),
            parserResetTimeout: .milliseconds(5)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertEqual(transport.writes, [[0xE0, 0x03, 0x00], [0x0D]])
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testSilentModeProbeUsesCATResetResponseAsPositiveClassification() async throws {
        let transport = RadioModeTestTransport(
            reads: [[0x3F, 0x0D]],
            silentReadCount: 1
        )
        let preflight = AzimuthRadioModePreflight(
            transport: transport,
            probeTimeout: .milliseconds(5),
            parserResetTimeout: .milliseconds(20)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .cat)
        XCTAssertEqual(transport.writes, [[0xE0, 0x03, 0x00], [0x0D]])
        XCTAssertEqual(transport.remainingScriptedReadCount, 0)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testCATRejectionIsDrainedAndParserIsResetBeforeAutomation() async throws {
        let transport = RadioModeTestTransport(
            reads: [[0x3F, 0x0D], [0x3F, 0x0D]]
        )
        let preflight = AzimuthRadioModePreflight(
            transport: transport,
            probeTimeout: .milliseconds(100),
            parserResetTimeout: .milliseconds(5)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .cat)
        XCTAssertEqual(transport.writes, [[0xE0, 0x03, 0x00], [0x0D]])
        XCTAssertEqual(transport.remainingScriptedReadCount, 0)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testCancellationRemovesParkedProbeReadWithoutResettingParser() async throws {
        let transport = RadioModeTestTransport()
        let preflight = AzimuthRadioModePreflight(
            transport: transport,
            probeTimeout: .seconds(30),
            parserResetTimeout: .milliseconds(5)
        )
        let task = Task { try await preflight.prepareForAutomation() }

        let readStartDeadline = ContinuousClock.now.advanced(by: .seconds(1))
        while transport.activeReadCount == 0,
              ContinuousClock.now < readStartDeadline {
            try await Task.sleep(for: .milliseconds(1))
        }
        XCTAssertEqual(transport.activeReadCount, 1)
        task.cancel()

        do {
            _ = try await task.value
            XCTFail("A cancelled mode probe must not continue into CAT reset")
        } catch is CancellationError {
            // Expected.
        }
        XCTAssertEqual(transport.writes, [[0xE0, 0x03, 0x00]])
        XCTAssertEqual(transport.activeReadCount, 0)
    }
}

private final class RadioModeTestTransport: AzimuthRadioTransport, @unchecked Sendable {
    let device = AzimuthRadioDevice.thD75USBC

    private let lock = NSLock()
    private var scriptedReads: [[UInt8]]
    private var silentReads: Int
    private var capturedWrites: [[UInt8]] = []
    private var activeReads = 0

    init(reads: [[UInt8]] = [], silentReadCount: Int = 0) {
        scriptedReads = reads
        silentReads = silentReadCount
    }

    var writes: [[UInt8]] { lock.withLock { capturedWrites } }
    var remainingScriptedReadCount: Int { lock.withLock { scriptedReads.count } }
    var activeReadCount: Int { lock.withLock { activeReads } }

    var state: AzimuthRadioTransportState { get async { .connected } }

    var stateStream: AsyncStream<AzimuthRadioTransportState> {
        AsyncStream { continuation in
            continuation.yield(.connected)
            continuation.finish()
        }
    }

    func open() async throws {}
    func close() async {}
    func setBaudRate(baud: UInt32) throws { _ = baud }

    func write(_ bytes: [UInt8]) async throws {
        lock.withLock { capturedWrites.append(bytes) }
    }

    func read(maxBytes: Int) async throws -> [UInt8] {
        let shouldRemainSilent = lock.withLock { () -> Bool in
            guard silentReads > 0 else { return false }
            silentReads -= 1
            return true
        }
        if !shouldRemainSilent, let bytes = lock.withLock({ () -> [UInt8]? in
            guard !scriptedReads.isEmpty else { return nil }
            let next = scriptedReads.removeFirst()
            guard next.count > maxBytes else { return next }
            scriptedReads.insert(Array(next.dropFirst(maxBytes)), at: 0)
            return Array(next.prefix(maxBytes))
        }) {
            return bytes
        }

        lock.withLock { activeReads += 1 }
        defer { lock.withLock { activeReads -= 1 } }
        try await Task.sleep(nanoseconds: 60_000_000_000)
        return []
    }
}
