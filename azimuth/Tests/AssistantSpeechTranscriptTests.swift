// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Azimuth

final class AssistantSpeechTranscriptTests: XCTestCase {
    func testVolatileResultReplacesPreviousVolatileResult() {
        var transcript = AssistantSpeechTranscript(originalText: "Keep GPS enabled")

        transcript.accept("and turn", isFinal: false)
        XCTAssertEqual(transcript.composedText, "Keep GPS enabled and turn")

        transcript.accept("and turn off key beeps", isFinal: false)
        XCTAssertEqual(transcript.composedText, "Keep GPS enabled and turn off key beeps")
    }

    func testFinalizedSegmentsAccumulateWithoutRepeatingInterimText() {
        var transcript = AssistantSpeechTranscript(originalText: "")

        transcript.accept("Turn off key beeps", isFinal: false)
        transcript.accept("Turn off key beeps", isFinal: true)
        transcript.accept("and dim the display", isFinal: false)

        XCTAssertEqual(transcript.composedText, "Turn off key beeps and dim the display")
    }

    func testCancelRestoresExactlyWhatWasTypedBeforeDictation() {
        var transcript = AssistantSpeechTranscript(originalText: "  Keep APRS enabled  ")
        transcript.accept("but reduce beaconing", isFinal: true)

        XCTAssertEqual(transcript.cancelledText, "  Keep APRS enabled  ")
    }
}
