// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Azimuth

@MainActor
final class IFDSPStartupLifecycleTests: XCTestCase {
    func testInvalidatedSessionCannotPrepareResourceAfterSuspension() async {
        let (resumeStream, resumeContinuation) = AsyncStream<Void>.makeStream(
            bufferingPolicy: .bufferingNewest(1)
        )
        var sessionIsCurrent = true
        var preparationCount = 0

        let startup = Task { @MainActor in
            await resumeIFDSPStartupIfCurrent(
                after: {
                    for await _ in resumeStream { break }
                },
                isCurrent: { sessionIsCurrent },
                prepare: {
                    preparationCount += 1
                    return "activated"
                }
            )
        }

        // Model Stop/background invalidating the UUID while processor.reset()
        // is still suspended, then let the old startup resume.
        await Task.yield()
        sessionIsCurrent = false
        resumeContinuation.yield(())
        resumeContinuation.finish()

        let prepared = await startup.value
        XCTAssertNil(prepared)
        XCTAssertEqual(
            preparationCount,
            0,
            "a stale startup must not activate AVAudioSession or select an input"
        )
    }
}
