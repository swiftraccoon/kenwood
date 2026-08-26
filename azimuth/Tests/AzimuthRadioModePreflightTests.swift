// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import XCTest
@testable import Azimuth

final class AzimuthRadioModePreflightTests: XCTestCase {
    private static let identifyWrite = Array("ID\r".utf8)
    private static let mmdvmProbeWrite: [UInt8] = [0xE0, 0x03, 0x00]
    private static let packetModeRecoveryWrites: [[UInt8]] = [
        [0x0D],
        [0x0D],
        [0x03],
        [0xC0, 0xFF, 0xC0],
        Array("\rTC 1\r".utf8),
        Array("TN 0,0\r".utf8),
    ]
    private static let recoveredCATWrites = [identifyWrite]
        + packetModeRecoveryWrites
        + [identifyWrite]
    private static let persistentMMDVMWrites = recoveredCATWrites + [
        mmdvmProbeWrite,
    ]
    private static let residueDetectedRecoveredCATWrites = packetModeRecoveryWrites + [
        identifyWrite,
    ]
    private static let residueDetectedPersistentMMDVMWrites =
        residueDetectedRecoveredCATWrites + [
            mmdvmProbeWrite,
        ]
    private static let focusedRecoveryTiming = AzimuthPacketModeRecoveryTiming(
        initialFlushDelay: .zero,
        kissReturnDelay: .zero,
        tncExitDelay: .zero,
        finalSettleDelay: .zero,
        quietWindow: .milliseconds(1),
        residueDrainLimit: .milliseconds(50)
    )
    private static let residueDetectingCATTiming = AzimuthCATPreservationTiming(
        quietWindow: .milliseconds(5),
        responseTimeout: .milliseconds(5)
    )
    private static let immediateCATTiming = AzimuthCATPreservationTiming(
        quietWindow: .zero,
        responseTimeout: .milliseconds(5)
    )
    private static let drainOnlyRecoveryTiming = AzimuthPacketModeRecoveryTiming(
        initialFlushDelay: .zero,
        kissReturnDelay: .zero,
        tncExitDelay: .zero,
        finalSettleDelay: .zero,
        quietWindow: .milliseconds(100),
        residueDrainLimit: .seconds(1)
    )

    private func makePreflight(
        transport: any AzimuthRadioTransport,
        probeTimeout: Duration,
        catIdentityTimeout: Duration,
        catPreservationTiming: AzimuthCATPreservationTiming? = nil,
        packetModeRecoveryTiming: AzimuthPacketModeRecoveryTiming =
            AzimuthRadioModePreflightTests.focusedRecoveryTiming
    ) -> AzimuthRadioModePreflight {
        AzimuthRadioModePreflight(
            transport: transport,
            probeTimeout: probeTimeout,
            catIdentityTimeout: catIdentityTimeout,
            catPreservationTiming: catPreservationTiming ?? AzimuthCATPreservationTiming(
                quietWindow: .milliseconds(1),
                responseTimeout: catIdentityTimeout
            ),
            packetModeRecoveryTiming: packetModeRecoveryTiming
        )
    }

    func testNormalCATIsProvedBeforeRecoveryAndNeverWritesPacketModeExit() async throws {
        let transport = RadioModeTestTransport(
            responsesForWrite: { bytes in
                bytes == Self.identifyWrite ? [Array("ID TH-D75\r".utf8)] : []
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(5),
            catIdentityTimeout: .milliseconds(5),
            catPreservationTiming: Self.residueDetectingCATTiming
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .cat)
        XCTAssertEqual(transport.writes, [Self.identifyWrite])
        XCTAssertFalse(transport.writes.contains(Array("TN 0,0\r".utf8)))
        XCTAssertEqual(transport.remainingScriptedReadCount, 0)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testCATOnlyProofFailureNeverWritesRecoveryOrMMDVMProbe() async throws {
        let transport = RadioModeTestTransport()
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(5),
            catIdentityTimeout: .milliseconds(5),
            catPreservationTiming: Self.immediateCATTiming
        )

        let mode = try await preflight.proveCATWithoutPacketModeRecovery()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertEqual(transport.writes, [Self.identifyWrite])
        XCTAssertFalse(transport.writes.contains(Array("TN 0,0\r".utf8)))
        XCTAssertFalse(transport.writes.contains(Self.mmdvmProbeWrite))
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testCATOnlyProofRejectsQueuedResidueWithoutWritingAnyProbe() async throws {
        let transport = RadioModeTestTransport(reads: [[0x3F, 0x0D]])
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(5),
            catIdentityTimeout: .milliseconds(5),
            catPreservationTiming: Self.residueDetectingCATTiming
        )

        let mode = try await preflight.proveCATWithoutPacketModeRecovery()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertTrue(transport.writes.isEmpty)
        XCTAssertFalse(transport.writes.contains(Array("TN 0,0\r".utf8)))
        XCTAssertFalse(transport.writes.contains(Self.mmdvmProbeWrite))
        XCTAssertEqual(transport.remainingScriptedReadCount, 1)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testPersistentMMDVMResponseAfterRecoveryIsClassifiedAndEntireFrameIsDrained() async throws {
        let transport = RadioModeTestTransport(
            responsesForWrite: { bytes in
                guard bytes == [0xE0, 0x03, 0x00] else { return [] }
                return [[0xE0], [0x12], [0x00, 0x01] + Array("TH-D75 RTM1.00".utf8)]
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(100),
            catIdentityTimeout: .milliseconds(5)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .mmdvm)
        XCTAssertEqual(transport.writes, Self.persistentMMDVMWrites)
        XCTAssertEqual(transport.remainingScriptedReadCount, 0)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testStaleByteBeforeMMDVMResponseDoesNotHideValidatedFrame() async throws {
        let transport = RadioModeTestTransport(
            responsesForWrite: { bytes in
                guard bytes == [0xE0, 0x03, 0x00] else { return [] }
                return [
                    [0x7F],
                    [0xE0, 0x12, 0x00, 0x01] + Array("TH-D75 RTM1.00".utf8),
                ]
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(100),
            catIdentityTimeout: .milliseconds(5)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .mmdvm)
        XCTAssertEqual(transport.writes, Self.persistentMMDVMWrites)
        XCTAssertEqual(transport.remainingScriptedReadCount, 0)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testPreRecoveryMMDVMResidueCannotAuthorizeMenu650Prompt() async throws {
        let transport = RadioModeTestTransport(
            reads: [[0xE0, 0x12, 0x00, 0x01] + Array("TH-D75 RTM1.00".utf8)]
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(5),
            catIdentityTimeout: .milliseconds(100),
            catPreservationTiming: Self.residueDetectingCATTiming,
            packetModeRecoveryTiming: Self.drainOnlyRecoveryTiming
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertEqual(transport.writes, Self.residueDetectedPersistentMMDVMWrites)
        XCTAssertEqual(transport.remainingScriptedReadCount, 0)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testTruncatedMMDVMFrameCannotAuthorizeRecovery() async throws {
        let transport = RadioModeTestTransport(
            responsesForWrite: { bytes in
                guard bytes == [0xE0, 0x03, 0x00] else { return [] }
                return [[0xE0], [0x12], [0x00, 0x01] + Array("TH-D75".utf8)]
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(10),
            catIdentityTimeout: .milliseconds(5)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertEqual(transport.writes, Self.persistentMMDVMWrites)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testExactIdentityAfterDrainedMMDVMSyncClassifiesCAT() async throws {
        let transport = RadioModeTestTransport(
            reads: [[0x3F, 0x0D, 0xE0]],
            responsesForWrite: { bytes in
                guard bytes == Array("ID\r".utf8) else { return [] }
                return [Array("ID TH-D75\r".utf8)]
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(100),
            catIdentityTimeout: .milliseconds(20),
            catPreservationTiming: Self.residueDetectingCATTiming,
            packetModeRecoveryTiming: Self.drainOnlyRecoveryTiming
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .cat)
        XCTAssertEqual(
            transport.writes,
            Self.residueDetectedRecoveredCATWrites
        )
        XCTAssertEqual(transport.remainingScriptedReadCount, 0)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testEchoedMMDVMRequestCannotAuthorizeRecovery() async throws {
        let transport = RadioModeTestTransport(
            responsesForWrite: { bytes in
                bytes == [0xE0, 0x03, 0x00] ? [[0xE0], [0x03], [0x00]] : []
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(10),
            catIdentityTimeout: .milliseconds(5)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertEqual(transport.writes, Self.persistentMMDVMWrites)
    }

    func testWrongMMDVMCommandCannotAuthorizeRecovery() async throws {
        let transport = RadioModeTestTransport(
            responsesForWrite: { bytes in
                bytes == [0xE0, 0x03, 0x00]
                    ? [[0xE0], [0x05], [0x01, 0x01, 0x41]]
                    : []
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(10),
            catIdentityTimeout: .milliseconds(5)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertEqual(transport.writes, Self.persistentMMDVMWrites)
    }

    func testNonTextMMDVMDescriptionCannotAuthorizeRecovery() async throws {
        let transport = RadioModeTestTransport(
            responsesForWrite: { bytes in
                bytes == [0xE0, 0x03, 0x00] ? [[0xE0, 0x05, 0x00, 0x01, 0xFF]] : []
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(10),
            catIdentityTimeout: .milliseconds(5)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertEqual(transport.writes, Self.persistentMMDVMWrites)
    }

    func testControlByteInMMDVMDescriptionCannotAuthorizeRecovery() async throws {
        let transport = RadioModeTestTransport(
            responsesForWrite: { bytes in
                bytes == [0xE0, 0x03, 0x00]
                    ? [[0xE0, 0x0D, 0x00, 0x01] + Array("TH-D75 ".utf8) + [0x09, 0x58]]
                    : []
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(10),
            catIdentityTimeout: .milliseconds(5)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertEqual(transport.writes, Self.persistentMMDVMWrites)
    }

    func testAnotherMMDVMImplementationCannotAuthorizeTHD75Recovery() async throws {
        let transport = RadioModeTestTransport(
            responsesForWrite: { bytes in
                bytes == [0xE0, 0x03, 0x00]
                    ? [[0xE0, 0x0E, 0x00, 0x01] + Array("MMDVM 2018".utf8)]
                    : []
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(10),
            catIdentityTimeout: .milliseconds(5)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertEqual(transport.writes, Self.persistentMMDVMWrites)
    }

    func testSilenceFromBothProbesIsNotMisreportedAsCAT() async throws {
        let transport = RadioModeTestTransport()
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(5),
            catIdentityTimeout: .milliseconds(5)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertEqual(transport.writes, Self.persistentMMDVMWrites)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testFailedFastIdentityFallsBackExactlyOnceAndRecoversCAT() async throws {
        let identityWrites = ThreadSafeOccurrenceCounter()
        let transport = RadioModeTestTransport(
            responsesForWrite: { bytes in
                guard bytes == Self.identifyWrite else { return [] }
                return identityWrites.next() == 2
                    ? [Array("ID TH-D75\r".utf8)]
                    : []
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(5),
            catIdentityTimeout: .milliseconds(20),
            catPreservationTiming: Self.immediateCATTiming
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .cat)
        XCTAssertEqual(
            transport.writes,
            Self.recoveredCATWrites
        )
        XCTAssertEqual(
            transport.writes.filter { $0 == Array("TN 0,0\r".utf8) }.count,
            1
        )
        XCTAssertEqual(transport.remainingScriptedReadCount, 0)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testAmbiguousFastIdentityFallsBackExactlyOnceAndRecoversCAT() async throws {
        let identityWrites = ThreadSafeOccurrenceCounter()
        let transport = RadioModeTestTransport(
            responsesForWrite: { bytes in
                guard bytes == Self.identifyWrite else { return [] }
                return identityWrites.next() == 1
                    ? [Array("ID TH-D75\r".utf8) + [0x3F]]
                    : [Array("ID TH-D75\r".utf8)]
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(5),
            catIdentityTimeout: .milliseconds(20),
            catPreservationTiming: Self.immediateCATTiming
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .cat)
        XCTAssertEqual(transport.writes, Self.recoveredCATWrites)
        XCTAssertEqual(
            transport.writes.filter { $0 == Array("TN 0,0\r".utf8) }.count,
            1
        )
        XCTAssertEqual(transport.remainingScriptedReadCount, 0)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testProductionRecoveryTimingMatchesProvenRadioSequence() {
        XCTAssertEqual(
            AzimuthCATPreservationTiming.radio.quietWindow,
            .milliseconds(100)
        )
        XCTAssertEqual(
            AzimuthCATPreservationTiming.radio.responseTimeout,
            .milliseconds(500)
        )
        XCTAssertEqual(
            AzimuthPacketModeRecoveryTiming.radio.initialFlushDelay,
            .milliseconds(300)
        )
        XCTAssertEqual(
            AzimuthPacketModeRecoveryTiming.radio.kissReturnDelay,
            .milliseconds(100)
        )
        XCTAssertEqual(
            AzimuthPacketModeRecoveryTiming.radio.tncExitDelay,
            .milliseconds(100)
        )
        XCTAssertEqual(
            AzimuthPacketModeRecoveryTiming.radio.finalSettleDelay,
            .milliseconds(300)
        )
        XCTAssertEqual(
            AzimuthPacketModeRecoveryTiming.radio.quietWindow,
            .milliseconds(500)
        )
        XCTAssertEqual(
            AzimuthPacketModeRecoveryTiming.radio.residueDrainLimit,
            .seconds(5)
        )
    }

    func testBareCRRejectionIsNotCATProofWithoutIdentityResponse() async throws {
        let transport = RadioModeTestTransport(
            responsesForWrite: { bytes in
                bytes == Array("ID\r".utf8) ? [[0x3F, 0x0D]] : []
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(5),
            catIdentityTimeout: .milliseconds(20)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertEqual(transport.writes, Self.persistentMMDVMWrites)
        XCTAssertEqual(transport.remainingScriptedReadCount, 0)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testCRRejectionBeforeExactIdentityDoesNotSatisfyIsolatedCATProof() async throws {
        let transport = RadioModeTestTransport(
            responsesForWrite: { bytes in
                guard bytes == Array("ID\r".utf8) else { return [] }
                return [[0x3F, 0x0D], Array("ID TH-D75\r".utf8)]
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(5),
            catIdentityTimeout: .milliseconds(20)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertEqual(transport.writes, Self.persistentMMDVMWrites)
        XCTAssertEqual(transport.remainingScriptedReadCount, 0)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testTrailingByteInIdentityChunkDoesNotSatisfyIsolatedCATProof() async throws {
        let transport = RadioModeTestTransport(
            responsesForWrite: { bytes in
                bytes == Array("ID\r".utf8)
                    ? [Array("ID TH-D75\r".utf8) + [0x3F]]
                    : []
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(5),
            catIdentityTimeout: .milliseconds(20)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertEqual(transport.writes, Self.persistentMMDVMWrites)
        XCTAssertEqual(transport.remainingScriptedReadCount, 0)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testBytesAfterIdentityResponseFailPostIdentityQuietCheck() async throws {
        let transport = RadioModeTestTransport(
            responsesForWrite: { bytes in
                guard bytes == Array("ID\r".utf8) else { return [] }
                return [Array("ID TH-D75\r".utf8), [0x3F]]
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(5),
            catIdentityTimeout: .milliseconds(20),
            packetModeRecoveryTiming: Self.drainOnlyRecoveryTiming
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertEqual(transport.writes, Self.persistentMMDVMWrites)
        XCTAssertEqual(transport.remainingScriptedReadCount, 0)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testNonTerminatedGarbageDuringCATResetIsNotCATProof() async throws {
        let transport = RadioModeTestTransport(
            responsesForWrite: { bytes in
                bytes == Array("ID\r".utf8) ? [[0x41, 0x42, 0x43]] : []
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(5),
            catIdentityTimeout: .milliseconds(20)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertEqual(transport.writes, Self.persistentMMDVMWrites)
        XCTAssertEqual(transport.remainingScriptedReadCount, 0)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testCATErrorResponsesDoNotClassifyCATWithoutExactIdentity() async throws {
        let transport = RadioModeTestTransport(
            responsesForWrite: { bytes in
                bytes == Array("ID\r".utf8) ? [[0x3F, 0x0D], [0x3F, 0x0D]] : []
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(100),
            catIdentityTimeout: .milliseconds(5)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertEqual(transport.writes, Self.persistentMMDVMWrites)
        XCTAssertEqual(transport.remainingScriptedReadCount, 0)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testWrongRadioIdentityDoesNotClassifyCAT() async throws {
        let transport = RadioModeTestTransport(
            responsesForWrite: { bytes in
                bytes == Array("ID\r".utf8) ? [Array("ID TH-D74\r".utf8)] : []
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(5),
            catIdentityTimeout: .milliseconds(20)
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertEqual(transport.writes, Self.persistentMMDVMWrites)
        XCTAssertEqual(transport.remainingScriptedReadCount, 0)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testStaleIdentityBeforeExplicitQueryDoesNotClassifyCAT() async throws {
        let transport = RadioModeTestTransport(
            reads: [
                Array("TN 0,0\r".utf8),
                Array("ID TH-D75\r".utf8),
            ],
            responsesForWrite: { bytes in
                bytes == Array("ID\r".utf8) ? [[0x3F, 0x0D]] : []
            }
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(100),
            catIdentityTimeout: .milliseconds(20),
            catPreservationTiming: Self.residueDetectingCATTiming,
            packetModeRecoveryTiming: Self.drainOnlyRecoveryTiming
        )

        let mode = try await preflight.prepareForAutomation()

        XCTAssertEqual(mode, .unresponsive)
        XCTAssertEqual(transport.writes, Self.residueDetectedPersistentMMDVMWrites)
        XCTAssertEqual(transport.remainingScriptedReadCount, 0)
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testCancellationDuringResidualDrainStopsBeforeIdentityProbe() async throws {
        let transport = RadioModeTestTransport()
        let timing = AzimuthPacketModeRecoveryTiming(
            initialFlushDelay: .zero,
            kissReturnDelay: .zero,
            tncExitDelay: .zero,
            finalSettleDelay: .zero,
            quietWindow: .seconds(30),
            residueDrainLimit: .seconds(30)
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .seconds(30),
            catIdentityTimeout: .seconds(30),
            catPreservationTiming: Self.immediateCATTiming,
            packetModeRecoveryTiming: timing
        )
        let task = Task { try await preflight.prepareForAutomation() }

        let drainStartDeadline = ContinuousClock.now.advanced(by: .seconds(1))
        while (transport.writes.count < Self.recoveredCATWrites.count - 1
                || transport.activeReadCount == 0),
              ContinuousClock.now < drainStartDeadline {
            try await Task.sleep(for: .milliseconds(1))
        }
        XCTAssertEqual(
            transport.writes,
            [Self.identifyWrite] + Self.packetModeRecoveryWrites
        )
        XCTAssertEqual(transport.activeReadCount, 1)
        task.cancel()

        do {
            _ = try await task.value
            XCTFail("cancellation during residue draining must stop before CAT probing")
        } catch is CancellationError {
            // Expected.
        }
        XCTAssertEqual(
            transport.writes,
            [Self.identifyWrite] + Self.packetModeRecoveryWrites
        )
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testCancellationDuringPreRecoveryQuietSendsNoRecoveryBytes() async throws {
        let transport = RadioModeTestTransport()
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .seconds(30),
            catIdentityTimeout: .seconds(30),
            catPreservationTiming: AzimuthCATPreservationTiming(
                quietWindow: .seconds(30),
                responseTimeout: .seconds(30)
            )
        )
        let task = Task { try await preflight.prepareForAutomation() }

        let readStartDeadline = ContinuousClock.now.advanced(by: .seconds(1))
        while transport.activeReadCount == 0,
              ContinuousClock.now < readStartDeadline {
            try await Task.sleep(for: .milliseconds(1))
        }
        XCTAssertEqual(transport.activeReadCount, 1)
        XCTAssertTrue(transport.writes.isEmpty)
        task.cancel()

        do {
            _ = try await task.value
            XCTFail("A cancelled pre-recovery quiet check must not enter recovery")
        } catch is CancellationError {
            // Expected.
        }
        XCTAssertTrue(transport.writes.isEmpty)
        XCTAssertFalse(transport.writes.contains(Array("TN 0,0\r".utf8)))
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testCancellationDuringFastCATIdentityReadSendsNoRecoveryBytes() async throws {
        let transport = RadioModeTestTransport()
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .seconds(30),
            catIdentityTimeout: .seconds(30),
            catPreservationTiming: AzimuthCATPreservationTiming(
                quietWindow: .zero,
                responseTimeout: .seconds(30)
            )
        )
        let task = Task { try await preflight.prepareForAutomation() }

        let readStartDeadline = ContinuousClock.now.advanced(by: .seconds(1))
        while transport.activeReadCount == 0,
              ContinuousClock.now < readStartDeadline {
            try await Task.sleep(for: .milliseconds(1))
        }
        XCTAssertEqual(transport.activeReadCount, 1)
        XCTAssertEqual(transport.writes, [Self.identifyWrite])
        task.cancel()

        do {
            _ = try await task.value
            XCTFail("A cancelled fast CAT identity probe must not enter recovery")
        } catch is CancellationError {
            // Expected.
        }
        XCTAssertEqual(transport.writes, [Self.identifyWrite])
        XCTAssertFalse(transport.writes.contains(Array("TN 0,0\r".utf8)))
        XCTAssertEqual(transport.activeReadCount, 0)
    }

    func testCancellationDuringPacketRecoveryStopsBeforeKISSExitWrites() async throws {
        let transport = RadioModeTestTransport()
        let timing = AzimuthPacketModeRecoveryTiming(
            initialFlushDelay: .seconds(30),
            kissReturnDelay: .zero,
            tncExitDelay: .zero,
            finalSettleDelay: .zero,
            quietWindow: .zero,
            residueDrainLimit: .zero
        )
        let preflight = makePreflight(
            transport: transport,
            probeTimeout: .milliseconds(5),
            catIdentityTimeout: .milliseconds(5),
            catPreservationTiming: Self.immediateCATTiming,
            packetModeRecoveryTiming: timing
        )
        let task = Task { try await preflight.prepareForAutomation() }

        let recoveryStartDeadline = ContinuousClock.now.advanced(by: .seconds(1))
        while transport.writes.count < 3,
              ContinuousClock.now < recoveryStartDeadline {
            try await Task.sleep(for: .milliseconds(1))
        }
        XCTAssertEqual(
            transport.writes,
            [Self.identifyWrite, [0x0D], [0x0D]]
        )
        task.cancel()

        do {
            _ = try await task.value
            XCTFail("cancellation during recovery must stop later mode-changing writes")
        } catch is CancellationError {
            // Expected.
        }
        XCTAssertEqual(
            transport.writes,
            [Self.identifyWrite, [0x0D], [0x0D]]
        )
        XCTAssertEqual(transport.activeReadCount, 0)
    }
}

private final class ThreadSafeOccurrenceCounter: @unchecked Sendable {
    private let lock = NSLock()
    private var value = 0

    func next() -> Int {
        lock.withLock {
            value += 1
            return value
        }
    }
}

private final class RadioModeTestTransport: AzimuthRadioTransport, @unchecked Sendable {
    let device = AzimuthRadioDevice.thD75USBC

    private let lock = NSLock()
    private var scriptedReads: [[UInt8]]
    private var silentReads: Int
    private var capturedWrites: [[UInt8]] = []
    private var activeReads = 0
    private let responsesForWrite: @Sendable ([UInt8]) -> [[UInt8]]

    init(
        reads: [[UInt8]] = [],
        silentReadCount: Int = 0,
        responsesForWrite: @escaping @Sendable ([UInt8]) -> [[UInt8]] = { _ in [] }
    ) {
        scriptedReads = reads
        silentReads = silentReadCount
        self.responsesForWrite = responsesForWrite
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
        let responses = responsesForWrite(bytes)
        lock.withLock {
            capturedWrites.append(bytes)
            scriptedReads.append(contentsOf: responses)
        }
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
