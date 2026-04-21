// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Lodestar

final class CatFFITests: XCTestCase {
    func testEncodeIdentifyIsIdCr() {
        let bytes = encodeCat(command: .identify)
        XCTAssertEqual(bytes, Array("ID\r".utf8))
    }

    func testParseIdentifyResponse() {
        let resp = parseCatLine(line: Array("ID TH-D75A".utf8))
        if case .identify(let model) = resp {
            XCTAssertEqual(model, "TH-D75A")
        } else {
            XCTFail("expected .identify, got \(resp)")
        }
    }

    func testParseUnknown() {
        let resp = parseCatLine(line: Array("?".utf8))
        if case .unknown = resp { return }
        XCTFail("expected .unknown, got \(resp)")
    }

    func testParseNotAvailable() {
        let resp = parseCatLine(line: Array("N".utf8))
        if case .notAvailableInMode = resp { return }
        XCTFail("expected .notAvailableInMode, got \(resp)")
    }

    func testParseRawFallback() {
        let resp = parseCatLine(line: Array("MYSTERY".utf8))
        if case .raw(let line) = resp {
            XCTAssertEqual(line, "MYSTERY")
        } else {
            XCTFail("expected .raw, got \(resp)")
        }
    }

    func testMockTransportIdentifyRoundTrip() async throws {
        let transport = MockRadioTransport()
        try await transport.open()

        let cmd = encodeCat(command: .identify)
        try await transport.write(cmd)

        var buffer: [UInt8] = []
        while !buffer.contains(0x0D) {
            let chunk = try await transport.read(maxBytes: 64)
            buffer.append(contentsOf: chunk)
            if chunk.isEmpty { break }
        }

        // Split at CR and parse the first line.
        let crIndex = buffer.firstIndex(of: 0x0D) ?? buffer.endIndex
        let line = Array(buffer[..<crIndex])
        let response = parseCatLine(line: line)

        if case .identify(let model) = response {
            XCTAssertEqual(model, "TH-D75A")
        } else {
            XCTFail("expected .identify, got \(response)")
        }

        await transport.close()
    }
}
