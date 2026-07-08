// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Lodestar

@MainActor
final class ReflectorCoordinatorTests: XCTestCase {
    /// The coordinator persists through `UserDefaults.standard`, so
    /// tests that touch persisted settings snapshot and restore the
    /// affected keys — otherwise running the suite would silently
    /// rewrite the developer's own app settings (e.g. mute monitor
    /// audio on the next launch).
    private let touchedKeys = [
        "lodestar.recentlyHeardArchive",
        "lodestar.persistRecentlyHeard",
        "lodestar.monitorAudio",
    ]
    private var savedDefaults: [String: Any] = [:]

    override func setUp() {
        super.setUp()
        let defaults = UserDefaults.standard
        savedDefaults = [:]
        for key in touchedKeys {
            if let value = defaults.object(forKey: key) {
                savedDefaults[key] = value
            }
            defaults.removeObject(forKey: key)
        }
    }

    override func tearDown() {
        let defaults = UserDefaults.standard
        for key in touchedKeys {
            if let value = savedDefaults[key] {
                defaults.set(value, forKey: key)
            } else {
                defaults.removeObject(forKey: key)
            }
        }
        super.tearDown()
    }

    func testEventsApplyInArrivalOrder() async throws {
        // setUp cleared the persisted keys, so `recentlyHeard` starts
        // empty regardless of prior app runs — hermetic like CI.
        let coordinator = ReflectorCoordinator()
        // Silence on-device playback: the voiceFrame events below carry
        // 12 zero bytes that would otherwise reach the audio pipeline.
        coordinator.monitorAudioEnabled = false
        var seen: [String] = []
        coordinator.relayHook = { event in
            switch event {
            case .voiceStart: seen.append("start")
            case .voiceFrame: seen.append("frame")
            case .voiceEnd: seen.append("end")
            default: break
            }
        }
        // Fire a start + 20 frames + end from off the main actor, the
        // way the tokio callback does.
        let header = Data(repeating: 0, count: 41)
        let voice = Data(repeating: 0, count: 12)
        await Task.detached {
            coordinator.onEvent(event: .voiceStart(
                streamId: 1, mycall: "W1AW", suffix: "", urcall: "CQCQCQ",
                rpt1: "", rpt2: "", headerBytes: header
            ))
            for i in 0..<20 {
                coordinator.onEvent(event: .voiceFrame(
                    streamId: 1, seq: UInt8(i), voiceBytes: voice
                ))
            }
            coordinator.onEvent(event: .voiceEnd(
                streamId: 1, reason: .eot, text: nil, position: nil
            ))
        }.value
        // Poll until all 22 events pump through the serial mailbox, with a
        // 2 s deadline. A fixed sleep is flaky on a loaded machine; polling
        // is deterministic once ordering is guaranteed.
        let deadline = ContinuousClock.now.advanced(by: .seconds(2))
        while seen.count < 22, ContinuousClock.now < deadline {
            try await Task.sleep(nanoseconds: 10_000_000)
        }

        XCTAssertEqual(seen.first, "start", "header must be applied before any frame")
        XCTAssertEqual(seen.last, "end")
        XCTAssertEqual(seen.count, 22)
        XCTAssertEqual(coordinator.recentlyHeard.count, 1,
                       "in-order delivery must produce exactly one heard entry")
    }

    func testHeardEntryIdentitySurvivesPersistenceRoundTrip() throws {
        // setUp cleared the archive, so both coordinators below see
        // only what this test writes.
        let coordinator = ReflectorCoordinator()
        coordinator.persistRecentlyHeard = true
        coordinator.logLocalTransmission(
            mycall: "W1AW", suffix: "", urcall: "CQCQCQ",
            startedAt: .now, frames: 10, text: nil
        )
        let originalId = try XCTUnwrap(coordinator.recentlyHeard.first?.id)

        let reloaded = ReflectorCoordinator()
        XCTAssertEqual(reloaded.recentlyHeard.first?.id, originalId,
                       "list identity must survive a relaunch")
    }
}
