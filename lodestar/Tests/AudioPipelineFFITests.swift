// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Lodestar

final class AudioPipelineFFITests: XCTestCase {
    func testHoldbackAndStats() throws {
        let p = RxAudioPipeline()
        p.startStream()
        // 12 arbitrary bytes decode to SOMETHING (worst case comfort
        // noise); the contract under test is holdback + counters.
        let frame = Data(repeating: 0, count: 12)
        XCTAssertTrue(p.pushVoice(seq: 0, voiceBytes: frame).isEmpty)
        XCTAssertEqual(p.pushVoice(seq: 1, voiceBytes: frame).count, 160)
        let end = p.endStream()
        XCTAssertEqual(end.tailPcm.count, 160)
        XCTAssertEqual(end.stats.received, 2)
    }
}
