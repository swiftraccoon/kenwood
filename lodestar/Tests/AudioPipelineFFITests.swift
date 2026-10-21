// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Lodestar

final class AudioPipelineFFITests: XCTestCase {
    func testAlwaysEnhancedStreamingAndStats() throws {
        let p = RxAudioPipeline()
        p.startStream()
        let frame = Data([
            0xD2, 0x4B, 0x28, 0xB2, 0x57, 0x44, 0xE4, 0x08, 0x1C,
            0, 0, 0
        ])
        var pcm: [Int16] = []
        for seq in UInt8(0)..<5 {
            let ready = try p.pushVoice(seq: seq, voiceBytes: frame)
            if seq < 3 {
                XCTAssertTrue(
                    ready.isEmpty,
                    "causal enhancement should retain its initial lookahead"
                )
            }
            XCTAssertEqual(ready.count % 160, 0)
            pcm.append(contentsOf: ready)
        }
        let end = try p.endStream()
        pcm.append(contentsOf: end.tailPcm)
        XCTAssertEqual(pcm.count, 5 * 160)
        XCTAssertEqual(end.stats.received, 5)
        XCTAssertEqual(end.stats.lost, 0)
    }

    func testInvalidVoiceInputsAreErrorsAndLeaveTheStreamEmpty() throws {
        let pipeline = RxAudioPipeline()
        pipeline.startStream()
        XCTAssertThrowsError(try pipeline.pushVoice(seq: 0, voiceBytes: Data(repeating: 0, count: 5))) { error in
            XCTAssertEqual(error as? RxAudioError, .WrongVoiceLength(actual: 5))
        }
        XCTAssertThrowsError(try pipeline.pushVoice(seq: 21, voiceBytes: Data(repeating: 0, count: 12)))
        let end = try pipeline.endStream()
        XCTAssertEqual(end.stats.received, 0)
        XCTAssertEqual(end.stats.lost, 0)
        XCTAssertTrue(end.tailPcm.isEmpty)
    }
}
