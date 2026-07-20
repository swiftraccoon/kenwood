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
            // Race the read against the deadline: transport reads BLOCK
            // until data arrives, so a silent radio would otherwise park
            // this loop forever and the deadline above never re-fires.
            guard let chunk = try await readRacingDeadline(maxBytes: 64, deadline: deadline) else {
                continue // timed out — top of loop throws enterTimeout
            }
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

    /// Read every radio setting the USB reflector relay depends on, fix
    /// any that are wrong, and report what was found and changed — one
    /// programming pass, fully automated, no menu keypresses.
    ///
    /// Two settings gate USB relay (both hardware-verified):
    /// - **Menu 650 / `0x1CA0`** must be `1` (Reflector Terminal Mode).
    /// - **Menu 985 / `0x1093`** (DV Gateway Interface) must be `USB`.
    ///   If it points at Bluetooth, the gateway's MMDVM framing goes out
    ///   the BT port and the USB port stays in plain CAT — the exact
    ///   "CAT works but MMDVM silent" symptom.
    ///
    /// The radio reboots on programming-mode exit iff anything changed
    /// (`report.rebooted`); the caller must then reconnect and poll for
    /// MMDVM (terminal mode engages ~50 s after the reboot).
    public func prepareForUsbRelay() async throws -> UsbRelaySetupReport {
        try await enterProgramming()

        // Gateway mode (page 0x1C, byte 0xA0) and interface (page 0x10,
        // byte 0x93) live on different pages — read each.
        let gwPage = pageOf(offset: UniFFI_GatewayModeOffset)
        let gwByte = byteOf(offset: UniFFI_GatewayModeOffset)
        let gwData = try await readPage(gwPage)
        let gwBefore = [UInt8](gwData)[Int(gwByte)]

        let ifPage = pageOf(offset: UniFFI_DvGatewayInterfaceOffset)
        let ifByte = byteOf(offset: UniFFI_DvGatewayInterfaceOffset)
        let ifData = try await readPage(ifPage)
        let ifBefore = [UInt8](ifData)[Int(ifByte)]

        log.info("MCP relay setup: gatewayMode=\(gwBefore) interface=\(ifBefore) (target: mode=1 interface=\(UniFFI_DvGatewayInterfaceUsb))")

        var wroteGateway = false
        if gwBefore != UniFFI_GatewayModeReflectorTerminal {
            let patched = try patchPageByte(
                pageData: gwData, offset: gwByte,
                value: UniFFI_GatewayModeReflectorTerminal
            )
            try await writePage(gwPage, data: patched)
            wroteGateway = true
        }

        var wroteInterface = false
        if ifBefore != UniFFI_DvGatewayInterfaceUsb {
            let patched = try patchPageByte(
                pageData: ifData, offset: ifByte,
                value: UniFFI_DvGatewayInterfaceUsb
            )
            try await writePage(ifPage, data: patched)
            wroteInterface = true
        }

        try await exitProgramming()

        return UsbRelaySetupReport(
            gatewayModeBefore: gwBefore,
            interfaceBefore: ifBefore,
            usbInterfaceValue: UniFFI_DvGatewayInterfaceUsb,
            wroteGatewayMode: wroteGateway,
            wroteInterface: wroteInterface
        )
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
            guard let chunk = try await readRacingDeadline(maxBytes: remaining, deadline: deadline) else {
                continue // timed out — top of loop throws readTimeout
            }
            if chunk.isEmpty {
                try await Task.sleep(nanoseconds: 50_000_000)
                continue
            }
            buffer.append(contentsOf: chunk)
        }
        return buffer
    }

    /// Race a blocking transport read against an absolute deadline;
    /// nil means the deadline fired first. (Cancellation of the losing
    /// read is safe: transports resume a cancelled read with `[]`
    /// without disturbing other readers.)
    private func readRacingDeadline(
        maxBytes: Int, deadline: ContinuousClock.Instant
    ) async throws -> [UInt8]? {
        try await withThrowingTaskGroup(of: [UInt8]?.self) { group in
            group.addTask { [transport] in
                try await transport.read(maxBytes: maxBytes)
            }
            group.addTask {
                try? await Task.sleep(until: deadline, clock: .continuous)
                return nil
            }
            defer { group.cancelAll() }
            return try await group.next() ?? nil
        }
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

/// Gateway-mode value for Reflector Terminal Mode (Menu 650 = 1).
private let UniFFI_GatewayModeReflectorTerminal: UInt8 = 1

/// MCP offset of the DV Gateway Interface setting (Menu 985), from the
/// MCP-D75 registry field `radio.DvGatewayInterface`. Selects which
/// physical port carries the gateway/MMDVM stream: USB or Bluetooth.
/// When this points at Bluetooth, the USB CDC port stays in CAT and the
/// `E0 03 00` MMDVM probe gets no reply — the whole "terminal mode is
/// on but the app can't see it over USB" failure.
private let UniFFI_DvGatewayInterfaceOffset: UInt16 = 0x1093

/// Interface value for USB. The Kenwood menu lists "[USB] or
/// [Bluetooth]" (USB first) and the MCP enum maps the first option to
/// 0; `prepareForUsbRelay` reports the read-back so this is verifiable
/// against hardware, and the write is reversible if it ever proves
/// inverted on a firmware revision.
private let UniFFI_DvGatewayInterfaceUsb: UInt8 = 0

/// Outcome of `McpSession.prepareForUsbRelay`: what the radio's relay
/// settings were, and what got changed. `rebooted` is true iff a flash
/// write happened (the radio reboots on programming-mode exit only when
/// something changed).
public struct UsbRelaySetupReport: Sendable, Equatable {
    /// Menu 650 value read before any change (1 = already terminal mode).
    public let gatewayModeBefore: UInt8
    /// Menu 985 value read before any change.
    public let interfaceBefore: UInt8
    /// The value written to mean USB (for read-back verification).
    public let usbInterfaceValue: UInt8
    /// Whether Menu 650 was flipped to Reflector Terminal Mode.
    public let wroteGatewayMode: Bool
    /// Whether Menu 985 was switched to USB.
    public let wroteInterface: Bool

    /// The radio reboots on exit only if a flash write occurred.
    public var rebooted: Bool { wroteGatewayMode || wroteInterface }

    /// One-line human summary for the diagnostics card.
    public var summary: String {
        var parts: [String] = []
        parts.append("Menu 650 (terminal mode): was \(gatewayModeBefore)"
            + (wroteGatewayMode ? " → 1" : " (already on)"))
        let ifName = { (v: UInt8) in v == usbInterfaceValue ? "USB" : "Bluetooth" }
        parts.append("Menu 985 (interface): was \(ifName(interfaceBefore))"
            + (wroteInterface ? " → USB" : " (already USB)"))
        return parts.joined(separator: "\n")
    }
}

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
