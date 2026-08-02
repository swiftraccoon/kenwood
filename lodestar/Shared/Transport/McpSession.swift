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
/// **Important:** Every firmware-offset flow first proves the exact
/// supported target (`ID TH-D75`, `FV 1.03`). Once entry wire traffic
/// starts, this actor owns cleanup: every exit attempt is one-shot, the
/// transport is closed, and the session becomes terminal.
public actor McpSession {
    public let transport: RadioTransport

    private enum Phase: String, Sendable {
        case inactive
        case qualifying
        case entering
        case active
        case exitSent
        case terminal
    }

    private static let supportedModel = "TH-D75"
    private static let supportedFirmwareWireValues: Set<String> = [
        "1.03",
        "1.03.000",
    ]

    private var phase: Phase = .inactive
    private var operationInFlight = false
    private var exclusiveFlowInProgress = false
    private var exitAcknowledged = false
    private var mcpDesynchronized = false
    private let pageReadTimeoutSeconds: Double
    private let catTimeoutSeconds: Double

    public init(transport: RadioTransport) {
        self.transport = transport
        self.pageReadTimeoutSeconds = 5
        self.catTimeoutSeconds = 2
    }

    init(
        transport: RadioTransport,
        pageReadTimeoutSeconds: Double,
        catTimeoutSeconds: Double = 2
    ) {
        self.transport = transport
        self.pageReadTimeoutSeconds = pageReadTimeoutSeconds
        self.catTimeoutSeconds = catTimeoutSeconds
    }

    // MARK: - Primitive steps

    /// Prove `TH-D75` firmware `1.03`, then send `0M PROGRAM\r`.
    ///
    /// The `FV` response is the last completed transaction before the
    /// MCP entry write. The phase becomes `entering` before that write so
    /// an ambiguous write/cancellation can never be mistaken for safe CAT.
    public func enterProgramming() async throws {
        try requireNoExclusiveFlow(operation: "enter programming mode")
        try await enterProgrammingOwned()
    }

    private func enterProgrammingOwned() async throws {
        try requireCleanupCapablePlatform()
        try requirePhase(.inactive, operation: "enter programming mode")
        phase = .qualifying
        do {
            try await qualifyFirmwareOffsetTarget()
        } catch {
            if isCompletedQualificationRejection(error) {
                // The radio returned complete typed CAT lines and no MCP byte
                // was sent, so this specific refusal is safe to retry.
                phase = .inactive
            } else {
                // A timeout, cancellation, transport failure, or malformed
                // framing leaves the outcome of CAT I/O unproved. Close the
                // byte stream and make this McpSession permanently terminal.
                phase = .terminal
                await transport.close()
            }
            throw error
        }

        phase = .entering
        operationInFlight = true
        do {
            log.info("MCP enter: sending 0M PROGRAM to qualified TH-D75 firmware 1.03")
            try await writeWithDeadline(
                Array(buildEnterCmd()),
                operation: "MCP entry",
                timeoutNanoseconds: 5_000_000_000
            )
            try await Task.sleep(nanoseconds: 10_000_000)

            let expected = Array("0M\r".utf8)
            var buffer: [UInt8] = []
            let deadline = ContinuousClock.now.advanced(by: .seconds(5))

            while !contains(buffer, expected) {
                if ContinuousClock.now >= deadline {
                    log.error("MCP enter: timeout; received so far: \(Self.hex(buffer))")
                    throw McpOrchestratorError.enterTimeout(receivedSoFar: buffer)
                }
                guard let chunk = try await readRacingDeadline(
                    maxBytes: 64, deadline: deadline
                ) else {
                    continue
                }
                if chunk.isEmpty {
                    try await Task.sleep(nanoseconds: 50_000_000)
                    continue
                }
                buffer.append(contentsOf: chunk)
                if buffer.count > 20 {
                    log.error("MCP enter: unexpected reply: \(Self.hex(buffer))")
                    throw McpOrchestratorError.enterUnexpectedReply(received: buffer)
                }
            }
            operationInFlight = false
            phase = .active
            log.info("MCP enter: confirmed")
        } catch {
            mcpDesynchronized = true
            operationInFlight = false
            throw await terminateAfterOperationFailure(error)
        }
    }

    /// Read one 256-byte page and require its echoed address to match.
    public func readPage(_ page: UInt16) async throws -> Data {
        try requireNoExclusiveFlow(operation: "read MCP page")
        return try await readPageOwned(page)
    }

    private func readPageOwned(_ page: UInt16) async throws -> Data {
        try beginActiveOperation("read MCP page")
        do {
            let data = try await readPageWithRetry(page)
            operationInFlight = false
            return data
        } catch {
            let terminalError = await terminateAfterOperationFailure(error)
            operationInFlight = false
            throw terminalError
        }
    }

    /// Write one page, require ACK, then verify all 256 bytes by read-back.
    public func writePage(_ page: UInt16, data: Data) async throws {
        try requireNoExclusiveFlow(operation: "write MCP page")
        try await writePageOwned(page, data: data)
    }

    private func writePageOwned(_ page: UInt16, data: Data) async throws {
        try beginActiveOperation("write MCP page")
        var writeWireStarted = false
        do {
            log.info("MCP write page 0x\(String(page, radix: 16, uppercase: true))")

            let cmd = try buildWritePageCmd(page: page, data: data)
            writeWireStarted = true
            try await writeWithDeadline(
                Array(cmd),
                operation: "MCP page write",
                timeoutNanoseconds: 5_000_000_000
            )
            try await Task.sleep(nanoseconds: 10_000_000)

            let ack = try await readExact(count: 1, timeoutSeconds: 5)
            guard let actual = ack.first, actual == 0x06 else {
                let got = ack.first ?? 0
                log.error("MCP write: bad ACK 0x\(String(got, radix: 16))")
                throw McpOrchestratorError.badWriteAck(actual: got)
            }

            let verified = try await readPageWithRetry(page)
            let expectedBytes = [UInt8](data)
            let actualBytes = [UInt8](verified)
            if let mismatch = zip(expectedBytes, actualBytes).enumerated().first(
                where: { $0.element.0 != $0.element.1 }
            ) {
                throw McpOrchestratorError.writeVerificationMismatch(
                    page: page,
                    offset: mismatch.offset,
                    expected: mismatch.element.0,
                    actual: mismatch.element.1
                )
            }
            guard expectedBytes.count == actualBytes.count else {
                throw McpOrchestratorError.writeVerificationLengthMismatch(
                    page: page,
                    expected: expectedBytes.count,
                    actual: actualBytes.count
                )
            }
            log.info("MCP write page 0x\(String(page, radix: 16, uppercase: true)) verified")
            operationInFlight = false
        } catch {
            if writeWireStarted && !Self.isKnownAlignedFailure(error) {
                mcpDesynchronized = true
            }
            let terminalError = await terminateAfterOperationFailure(error)
            operationInFlight = false
            throw terminalError
        }
    }

    /// Send exactly one `E`, require `0x06`, close, and terminalize.
    public func exitProgramming() async throws {
        try requireNoExclusiveFlow(operation: "exit programming mode")
        try await exitProgrammingOwned()
    }

    private func exitProgrammingOwned() async throws {
        try beginActiveOperation("exit programming mode")
        defer { operationInFlight = false }
        do {
            try await sendExitOnce()
        } catch let error as McpOrchestratorError {
            await transport.close()
            throw error
        } catch {
            await transport.close()
            throw McpOrchestratorError.exitNotProved(detail: error.displayMessage)
        }
        await transport.close()
    }

    /// Whether a coordinator must detach its reference to this transport.
    ///
    /// False only before qualification starts, or after the radio returned a
    /// complete typed rejection. Ambiguous CAT I/O is terminal and requires
    /// the coordinator to detach this transport just like attempted MCP entry.
    public func requiresTransportDetach() -> Bool {
        switch phase {
        case .inactive:
            return false
        case .qualifying, .entering, .active, .exitSent, .terminal:
            return true
        }
    }

    /// True only when the radio returned the exact ACK for this session's E.
    public func exitWasProved() -> Bool {
        exitAcknowledged
    }

    // MARK: - High-level orchestration

    /// Flip Menu 650 (DV Gateway) to Reflector Terminal Mode.
    ///
    /// Full sequence: enter programming → read page 0x1C → patch byte
    /// 0xA0 = 1 → write page 0x1C → exit programming. After this
    /// returns (successfully or not), the caller **must** close the
    /// transport; the radio will reboot into the new mode.
    public func enableReflectorTerminalMode() async throws {
        try beginExclusiveFlow(operation: "enable Reflector Terminal Mode")
        defer { exclusiveFlowInProgress = false }
        do {
            try await enterProgrammingOwned()

            let offset = UniFFI_GatewayModeOffset
            let page = pageOf(offset: offset)
            let byte = byteOf(offset: offset)

            let current = try await readPageOwned(page)
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
                try await writePageOwned(page, data: patched)
            }

            try await exitProgrammingOwned()
        } catch {
            throw await finishHighLevelFailure(error)
        }
    }

    // MARK: - Private helpers

    private func requireCleanupCapablePlatform() throws {
        #if os(iOS)
        // iOS can suspend the process without enough bounded execution time
        // to prove the one-shot MCP exit handshake. Refuse before any CAT or
        // programming-mode byte, even if this actor is used without the UI
        // coordinator's platform gate.
        throw McpOrchestratorError.platformCleanupNotGuaranteed
        #endif
    }

    private func qualifyFirmwareOffsetTarget() async throws {
        let identity = try await transactCat(.identify)
        guard case .identify(let model) = identity else {
            throw McpOrchestratorError.unexpectedCatResponse(
                command: "ID", actual: String(describing: identity)
            )
        }
        guard model == Self.supportedModel else {
            throw McpOrchestratorError.unsupportedModel(actual: model)
        }

        let firmware = try await transactCat(.firmwareVersion)
        guard case .firmwareVersion(let version) = firmware else {
            throw McpOrchestratorError.unexpectedCatResponse(
                command: "FV", actual: String(describing: firmware)
            )
        }
        guard Self.supportedFirmwareWireValues.contains(version) else {
            throw McpOrchestratorError.unsupportedFirmware(actual: version)
        }

        log.info("MCP target qualified: TH-D75 firmware 1.03")
    }

    private func transactCat(_ command: CatCommand) async throws -> CatResponse {
        try await writeWithDeadline(
            encodeCat(command: command),
            operation: "CAT \(String(describing: command))",
            timeoutNanoseconds: UInt64(catTimeoutSeconds * 1_000_000_000)
        )

        var buffer: [UInt8] = []
        let deadline = ContinuousClock.now.advanced(
            by: .seconds(catTimeoutSeconds)
        )
        while !buffer.contains(0x0D) {
            if ContinuousClock.now >= deadline {
                throw McpOrchestratorError.catResponseTimeout(
                    command: String(describing: command),
                    receivedSoFar: buffer
                )
            }
            guard let chunk = try await readRacingDeadline(
                maxBytes: 256, deadline: deadline
            ) else {
                continue
            }
            if chunk.isEmpty {
                throw McpOrchestratorError.catTransportClosed(
                    command: String(describing: command)
                )
            }
            buffer.append(contentsOf: chunk)
            if buffer.count > 512 {
                throw McpOrchestratorError.catResponseTooLong(
                    command: String(describing: command),
                    received: buffer
                )
            }
        }

        let end = buffer.firstIndex(of: 0x0D) ?? buffer.endIndex
        return parseCatLine(line: Array(buffer[..<end]))
    }

    private func isCompletedQualificationRejection(_ error: Error) -> Bool {
        guard let error = error as? McpOrchestratorError else { return false }
        switch error {
        case .unexpectedCatResponse, .unsupportedModel, .unsupportedFirmware:
            return true
        default:
            return false
        }
    }

    private func readPageWithRetry(_ page: UInt16) async throws -> Data {
        do {
            return try await readPageAttempt(page)
        } catch {
            guard Self.isRetryablePageRead(error) else {
                // Any other failure after R may leave a partial frame or
                // delayed ACK. It is not safe to retry or trust a later
                // 0x06 as proof of E.
                mcpDesynchronized = true
                throw error
            }
            log.warning(
                "MCP read page 0x\(String(page, radix: 16, uppercase: true)) returned a fully ACKed stale page; retrying once: \(error.displayMessage)"
            )
            do {
                return try await readPageAttempt(page)
            } catch {
                if !Self.isRetryablePageRead(error) {
                    mcpDesynchronized = true
                }
                throw error
            }
        }
    }

    private func readPageAttempt(_ page: UInt16) async throws -> Data {
        log.info("MCP read page 0x\(String(page, radix: 16, uppercase: true))")

        try await writeWithDeadline(
            Array(buildReadPageCmd(page: page)),
            operation: "MCP page read request",
            timeoutNanoseconds: 5_000_000_000
        )
        try await Task.sleep(nanoseconds: 10_000_000)

        let frame = try await readExact(
            count: 261, timeoutSeconds: pageReadTimeoutSeconds
        )
        let parsed = try parseWFrame(bytes: frame)

        // A complete W frame always requires the host ACK before the
        // radio emits its trailing ACK. Finish that exchange even when
        // the echoed page is stale; only a fully realigned exchange may
        // be retried.
        try await writeWithDeadline(
            [0x06],
            operation: "MCP page read ACK",
            timeoutNanoseconds: 1_000_000_000
        )
        try await Task.sleep(nanoseconds: 10_000_000)
        do {
            let ack = try await readExact(count: 1, timeoutSeconds: 1)
            guard let actual = ack.first, actual == 0x06 else {
                let got = ack.first ?? 0
                mcpDesynchronized = true
                throw McpOrchestratorError.badPageReadAck(
                    page: parsed.page, actual: got
                )
            }
        } catch {
            mcpDesynchronized = true
            if let error = error as? McpOrchestratorError,
               case .badPageReadAck = error {
                throw error
            }
            throw McpOrchestratorError.pageReadAckNotProved(
                page: parsed.page, detail: error.displayMessage
            )
        }

        guard parsed.page == page else {
            throw McpOrchestratorError.pageEchoMismatch(
                requested: page, actual: parsed.page
            )
        }

        log.info("MCP read page 0x\(String(page, radix: 16, uppercase: true)) complete")
        return parsed.data
    }

    private func sendExitOnce() async throws {
        guard phase == .active || phase == .entering else {
            throw McpOrchestratorError.invalidPhase(
                operation: "send MCP exit",
                expected: "active or entering",
                actual: phase.rawValue
            )
        }

        // This transition happens before the awaited write. From here
        // onward an ambiguous transport result must never trigger a
        // second E.
        phase = .exitSent
        defer { phase = .terminal }

        do {
            log.info("MCP exit")
            try await writeWithDeadline(
                Array(buildExitCmd()),
                operation: "MCP exit",
                timeoutNanoseconds: 1_000_000_000
            )

            let ack = try await readExact(count: 1, timeoutSeconds: 1)
            guard let actual = ack.first, actual == 0x06 else {
                let got = ack.first ?? 0
                log.error("MCP exit: bad ACK 0x\(String(got, radix: 16))")
                throw McpOrchestratorError.badExitAck(actual: got)
            }
            if mcpDesynchronized {
                throw McpOrchestratorError.exitNotProved(
                    detail: "a prior page exchange was desynchronized, so this ACK may be stale"
                )
            }
            exitAcknowledged = true
            log.info("MCP exit ACKed")
        } catch let error as McpOrchestratorError {
            switch error {
            case .badExitAck, .exitNotProved:
                throw error
            default:
                throw McpOrchestratorError.exitNotProved(detail: error.displayMessage)
            }
        } catch {
            throw McpOrchestratorError.exitNotProved(detail: error.displayMessage)
        }
    }

    private func terminateAfterOperationFailure(_ operation: Error) async -> Error {
        guard phase != .inactive else { return operation }

        var cleanupError: Error?
        if phase == .active || phase == .entering {
            do {
                try await sendExitOnce()
            } catch {
                cleanupError = error
            }
        } else if phase == .exitSent {
            cleanupError = McpOrchestratorError.exitNotProved(
                detail: "the one-shot exit write was already started"
            )
            phase = .terminal
        }

        await transport.close()

        if let cleanupError {
            return McpOrchestratorError.operationAndCleanupFailed(
                operation: operation.displayMessage,
                cleanup: cleanupError.displayMessage
            )
        }
        return operation
    }

    private func finishHighLevelFailure(_ error: Error) async -> Error {
        if phase == .entering || phase == .active || phase == .exitSent {
            return await terminateAfterOperationFailure(error)
        }
        if phase == .terminal {
            await transport.close()
        }
        return error
    }

    private func requirePhase(_ expected: Phase, operation: String) throws {
        guard phase == expected else {
            throw McpOrchestratorError.invalidPhase(
                operation: operation,
                expected: expected.rawValue,
                actual: phase.rawValue
            )
        }
    }

    private func beginActiveOperation(_ operation: String) throws {
        try requirePhase(.active, operation: operation)
        guard !operationInFlight else {
            throw McpOrchestratorError.operationInProgress(operation: operation)
        }
        operationInFlight = true
    }

    private func requireNoExclusiveFlow(operation: String) throws {
        guard !exclusiveFlowInProgress else {
            throw McpOrchestratorError.operationInProgress(operation: operation)
        }
    }

    private func beginExclusiveFlow(operation: String) throws {
        try requireNoExclusiveFlow(operation: operation)
        try requirePhase(.inactive, operation: operation)
        guard !operationInFlight else {
            throw McpOrchestratorError.operationInProgress(operation: operation)
        }
        exclusiveFlowInProgress = true
    }

    private static func isRetryablePageRead(_ error: Error) -> Bool {
        guard let error = error as? McpOrchestratorError else { return false }
        switch error {
        case .pageEchoMismatch:
            return true
        default:
            return false
        }
    }

    private static func isKnownAlignedFailure(_ error: Error) -> Bool {
        guard let error = error as? McpOrchestratorError else { return false }
        switch error {
        case .pageEchoMismatch,
             .writeVerificationMismatch,
             .writeVerificationLengthMismatch:
            return true
        default:
            return false
        }
    }

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
                continue // timed out; top of loop throws readTimeout
            }
            if chunk.isEmpty {
                try await Task.sleep(nanoseconds: 50_000_000)
                continue
            }
            buffer.append(contentsOf: chunk)
        }
        return buffer
    }

    private func writeWithDeadline(
        _ bytes: [UInt8],
        operation: String,
        timeoutNanoseconds: UInt64
    ) async throws {
        let transport = self.transport
        try await withThrowingTaskGroup(of: Void.self) { group in
            group.addTask {
                try await transport.write(bytes)
            }
            group.addTask {
                try await Task.sleep(nanoseconds: timeoutNanoseconds)
                throw McpOrchestratorError.writeTimeout(operation: operation)
            }
            defer { group.cancelAll() }
            _ = try await group.next()
        }
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

/// Errors from `McpSession`.
public enum McpOrchestratorError: Error, Equatable, Sendable {
    /// The platform cannot guarantee time for a bounded one-shot MCP cleanup.
    case platformCleanupNotGuaranteed
    /// An MCP step was invoked before entry or after the terminal exit attempt.
    case invalidPhase(operation: String, expected: String, actual: String)
    /// Another primitive already owns the session's transport exchange.
    case operationInProgress(operation: String)
    /// CAT did not return a complete line before qualification timed out.
    case catResponseTimeout(command: String, receivedSoFar: [UInt8])
    /// CAT transport closed while qualifying the radio.
    case catTransportClosed(command: String)
    /// CAT returned an unbounded line while qualifying the radio.
    case catResponseTooLong(command: String, received: [UInt8])
    /// A transport write did not complete within its MCP safety deadline.
    case writeTimeout(operation: String)
    /// A CAT response did not match the command used for qualification.
    case unexpectedCatResponse(command: String, actual: String)
    /// Firmware-offset MCP access is limited to the exact TH-D75 model.
    case unsupportedModel(actual: String)
    /// Firmware-offset MCP access is limited to firmware 1.03.
    case unsupportedFirmware(actual: String)
    /// Did not receive `0M\r` from the radio within the timeout.
    case enterTimeout(receivedSoFar: [UInt8])
    /// Received something other than `0M\r` during entry.
    case enterUnexpectedReply(received: [UInt8])
    /// Expected `count` bytes, only got `got` before the timeout.
    case readTimeout(expected: Int, got: Int)
    /// A page response echoed a different address than the request.
    case pageEchoMismatch(requested: UInt16, actual: UInt16)
    /// The radio's trailing ACK after a page read was the wrong byte.
    case badPageReadAck(page: UInt16, actual: UInt8)
    /// The radio's trailing ACK after a page read could not be proved.
    case pageReadAckNotProved(page: UInt16, detail: String)
    /// Radio replied with a non-0x06 byte after a page write.
    case badWriteAck(actual: UInt8)
    /// Page read-back differed from the 256 bytes sent.
    case writeVerificationMismatch(
        page: UInt16, offset: Int, expected: UInt8, actual: UInt8
    )
    /// Page read-back had an unexpected length.
    case writeVerificationLengthMismatch(page: UInt16, expected: Int, actual: Int)
    /// Radio replied with a non-0x06 byte after the programming-mode exit.
    case badExitAck(actual: UInt8)
    /// The one-shot MCP exit could not be proved.
    case exitNotProved(detail: String)
    /// An operation failed and its one-shot MCP cleanup also could not be proved.
    case operationAndCleanupFailed(operation: String, cleanup: String)
}

extension McpOrchestratorError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .platformCleanupNotGuaranteed:
            return "Radio programming is disabled on this platform because app suspension "
                + "can interrupt MCP cleanup. No radio setting was changed."
        case .invalidPhase(let operation, let expected, let actual):
            return "Cannot \(operation): MCP session is \(actual), expected \(expected)."
        case .operationInProgress(let operation):
            return "Cannot \(operation): another MCP transport exchange is already in progress."
        case .catResponseTimeout(let command, let received):
            return "CAT \(command) timed out during MCP target qualification "
                + "(\(received.count) byte(s) received)."
        case .catTransportClosed(let command):
            return "Radio transport closed while waiting for CAT \(command)."
        case .catResponseTooLong(let command, let received):
            return "CAT \(command) returned an invalid \(received.count)-byte line."
        case .writeTimeout(let operation):
            return "\(operation) write did not complete before its safety deadline."
        case .unexpectedCatResponse(let command, let actual):
            return "CAT \(command) returned \(actual); refusing firmware-offset MCP access."
        case .unsupportedModel(let actual):
            return "Refusing firmware-offset MCP access to model \(actual); "
                + "the validated target is exactly TH-D75."
        case .unsupportedFirmware(let actual):
            return "Refusing firmware-offset MCP access to firmware \(actual); "
                + "the validated TH-D75 firmware 1.03 wire forms are "
                + "1.03 and 1.03.000."
        case .enterTimeout(let received):
            return "MCP entry timed out after receiving \(received.count) byte(s)."
        case .enterUnexpectedReply(let received):
            return "MCP entry returned an unexpected \(received.count)-byte reply."
        case .readTimeout(let expected, let got):
            return "MCP read timed out: expected \(expected) byte(s), got \(got)."
        case .pageEchoMismatch(let requested, let actual):
            return "MCP page response was stale: requested 0x"
                + String(requested, radix: 16, uppercase: true)
                + ", received 0x" + String(actual, radix: 16, uppercase: true) + "."
        case .badPageReadAck(let page, let actual):
            return "MCP page 0x" + String(page, radix: 16, uppercase: true)
                + " read handshake returned 0x" + String(format: "%02X", actual)
                + " instead of ACK."
        case .pageReadAckNotProved(let page, let detail):
            return "MCP page 0x" + String(page, radix: 16, uppercase: true)
                + " read handshake was not completed (\(detail))."
        case .badWriteAck(let actual):
            return "Radio rejected the MCP page write with ACK byte 0x"
                + String(format: "%02X", actual) + "."
        case .writeVerificationMismatch(let page, let offset, let expected, let actual):
            return "MCP write verification failed on page 0x"
                + String(page, radix: 16, uppercase: true)
                + " at byte \(offset): wrote 0x" + String(format: "%02X", expected)
                + ", read 0x" + String(format: "%02X", actual) + "."
        case .writeVerificationLengthMismatch(let page, let expected, let actual):
            return "MCP write verification on page 0x"
                + String(page, radix: 16, uppercase: true)
                + " returned \(actual) bytes; expected \(expected)."
        case .badExitAck(let actual):
            return "MCP exit returned 0x" + String(format: "%02X", actual)
                + " instead of ACK. Exit is not proved; close the link and power-cycle "
                + "the radio before retrying."
        case .exitNotProved(let detail):
            return "MCP exit is not proved (\(detail)). The link was closed; power-cycle "
                + "the radio before retrying."
        case .operationAndCleanupFailed(let operation, let cleanup):
            return "MCP operation failed: \(operation) Cleanup also failed: \(cleanup) "
                + "The link was closed; power-cycle the radio before retrying."
        }
    }
}
