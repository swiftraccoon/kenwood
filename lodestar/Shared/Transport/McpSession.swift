// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import OSLog

private let log = Logger(subsystem: "org.swiftraccoon.lodestar", category: "mcp")

/// Runs the binary MCP programming protocol over any `RadioTransport`.
///
/// The orchestration mirrors thd75's `Radio::enter_programming_mode` /
/// `read_single_page` / `write_single_page` / `exit_programming_mode`
/// sequence exactly, with the same 10 ms post-write delays and ACK
/// exchange. All protocol bytes are produced/parsed by the Rust side
/// (`lodestar-core::mcp`); this type only sequences them.
///
/// **Important:** Exiting programming mode causes the TH-D75 to drop
/// the BT/USB connection and reboot. Callers must close the transport
/// after `exitProgramming()` and reconnect from scratch.
public actor McpSession {
    public let transport: RadioTransport

    public init(transport: RadioTransport) {
        self.transport = transport
    }

    // MARK: - Primitive steps

    /// Send `0M PROGRAM\r` and wait for `0M\r` confirmation.
    public func enterProgramming() async throws {
        log.info("MCP enter: sending 0M PROGRAM")
        try await transport.write(Array(buildEnterCmd()))
        try await Task.sleep(nanoseconds: 10_000_000)

        let expected = Array("0M\r".utf8)
        var buffer: [UInt8] = []
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))

        while !contains(buffer, expected) {
            if ContinuousClock.now >= deadline {
                log.error("MCP enter: timeout; received so far: \(Self.hex(buffer))")
                throw McpOrchestratorError.enterTimeout(receivedSoFar: buffer)
            }
            let chunk = try await transport.read(maxBytes: 64)
            if chunk.isEmpty {
                try await Task.sleep(nanoseconds: 50_000_000)
                continue
            }
            buffer.append(contentsOf: chunk)
            // thd75 caps the scan at 20 bytes; we match.
            if buffer.count > 20 {
                log.error("MCP enter: unexpected reply: \(Self.hex(buffer))")
                throw McpOrchestratorError.enterUnexpectedReply(received: buffer)
            }
        }
        log.info("MCP enter: confirmed")
    }

    /// Read one 256-byte page. Returns the page's raw contents.
    public func readPage(_ page: UInt16) async throws -> Data {
        log.info("MCP read page 0x\(String(page, radix: 16, uppercase: true))")

        try await transport.write(Array(buildReadPageCmd(page: page)))
        try await Task.sleep(nanoseconds: 10_000_000)

        let frame = try await readExact(count: 261, timeoutSeconds: 5)
        let parsed = try parseWFrame(bytes: frame)

        // Send our ACK. The radio echoes one back but thd75 treats the
        // echo as best-effort — a missing echo doesn't fail the read.
        try await transport.write([0x06])
        try await Task.sleep(nanoseconds: 10_000_000)
        _ = try? await readExact(count: 1, timeoutSeconds: 1)

        log.info("MCP read page 0x\(String(page, radix: 16, uppercase: true)) complete")
        return parsed.data
    }

    /// Write one 256-byte page. Throws if the radio doesn't ACK with 0x06.
    public func writePage(_ page: UInt16, data: Data) async throws {
        log.info("MCP write page 0x\(String(page, radix: 16, uppercase: true))")

        let cmd = try buildWritePageCmd(page: page, data: data)
        try await transport.write(Array(cmd))
        try await Task.sleep(nanoseconds: 10_000_000)

        let ack = try await readExact(count: 1, timeoutSeconds: 5)
        guard let b = ack.first, b == 0x06 else {
            let got = ack.first ?? 0
            log.error("MCP write: bad ACK 0x\(String(got, radix: 16))")
            throw McpOrchestratorError.badWriteAck(actual: got)
        }
        log.info("MCP write page 0x\(String(page, radix: 16, uppercase: true)) ACKed")
    }

    /// Send the `E` byte. The radio drops the connection immediately after.
    public func exitProgramming() async throws {
        log.info("MCP exit")
        try await transport.write(Array(buildExitCmd()))
        // No read — transport will close.
    }

    // MARK: - High-level orchestration

    /// Flip Menu 650 (DV Gateway) to Reflector Terminal Mode.
    ///
    /// Full sequence: enter programming → read page 0x1C → patch byte
    /// 0xA0 = 1 → write page 0x1C → exit programming. After this
    /// returns (successfully or not), the caller **must** close the
    /// transport; the radio will reboot into the new mode.
    public func enableReflectorTerminalMode() async throws {
        try await enterProgramming()

        let offset = UniFFI_GatewayModeOffset
        let page = pageOf(offset: offset)
        let byte = byteOf(offset: offset)

        let current = try await readPage(page)
        let patched = try patchPageByte(
            pageData: current,
            offset: byte,
            value: 1 // GATEWAY_MODE_REFLECTOR_TERMINAL
        )

        // Idempotence: if the radio is already in Reflector Terminal
        // Mode, skip the write. Saves a flash cycle.
        if current == patched {
            log.info("MCP: radio already in Reflector Terminal Mode, skipping write")
        } else {
            try await writePage(page, data: patched)
        }

        try await exitProgramming()
    }

    // MARK: - Private helpers

    private func readExact(count: Int, timeoutSeconds: Double) async throws -> Data {
        var buffer = Data()
        buffer.reserveCapacity(count)
        let deadline = ContinuousClock.now.advanced(by: .seconds(timeoutSeconds))

        while buffer.count < count {
            if ContinuousClock.now >= deadline {
                log.error(
                    "MCP readExact: timeout after \(timeoutSeconds)s, got \(buffer.count)/\(count) bytes: \(Self.hex(Array(buffer)))"
                )
                throw McpOrchestratorError.readTimeout(expected: count, got: buffer.count)
            }
            let remaining = count - buffer.count
            let chunk = try await transport.read(maxBytes: remaining)
            if chunk.isEmpty {
                try await Task.sleep(nanoseconds: 50_000_000)
                continue
            }
            buffer.append(contentsOf: chunk)
        }
        return buffer
    }

    private func contains(_ haystack: [UInt8], _ needle: [UInt8]) -> Bool {
        guard haystack.count >= needle.count else { return false }
        let end = haystack.count - needle.count
        for i in 0...end where Array(haystack[i..<(i + needle.count)]) == needle {
            return true
        }
        return false
    }

    private static func hex(_ bytes: [UInt8]) -> String {
        bytes.map { String(format: "%02x", $0) }.joined(separator: " ")
    }
}

/// MCP gateway-mode offset (Menu 650). Mirrors the Rust-side constant
/// `GATEWAY_MODE_OFFSET = 0x1CA0`. We define it here instead of calling
/// a UniFFI-generated accessor because UniFFI doesn't emit plain Rust
/// constants to Swift; round-tripping through a function would be
/// overkill for a single `u16`.
private let UniFFI_GatewayModeOffset: UInt16 = 0x1CA0

/// Errors from `McpSession`.
public enum McpOrchestratorError: Error, Equatable, Sendable {
    /// Did not receive `0M\r` from the radio within the timeout.
    case enterTimeout(receivedSoFar: [UInt8])
    /// Received something other than `0M\r` during entry.
    case enterUnexpectedReply(received: [UInt8])
    /// Expected `count` bytes, only got `got` before the timeout.
    case readTimeout(expected: Int, got: Int)
    /// Radio replied with a non-0x06 byte after a page write.
    case badWriteAck(actual: UInt8)
}
