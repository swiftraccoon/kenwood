// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import OSLog

private let log = Logger(subsystem: "org.swiftraccoon.lodestar", category: "mmdvm-reader")

/// Accumulates bytes from a `RadioTransport` into complete MMDVM
/// frames and publishes them as an `AsyncStream<MmdvmFrame>`. Once
/// started, runs until `stop()` is called or the transport closes.
///
/// Used in MMDVM mode (after flipping Menu 650) when the BT channel
/// carries `[0xE0, len, cmd, payload...]` framing instead of CAT
/// ASCII. The reader is deliberately transport-agnostic — give it any
/// `RadioTransport` and it pulls bytes through `transport.read(...)`.
public actor MmdvmReader {
    public let transport: RadioTransport
    public nonisolated let frames: AsyncStream<MmdvmFrame>
    private let continuation: AsyncStream<MmdvmFrame>.Continuation
    private var runTask: Task<Void, Never>?
    private var stopped: Bool = false

    public init(transport: RadioTransport) {
        self.transport = transport
        var cont: AsyncStream<MmdvmFrame>.Continuation!
        self.frames = AsyncStream { c in cont = c }
        self.continuation = cont
    }

    /// Start pumping bytes from the transport. Emits parsed frames on
    /// `frames`; finishes the stream when `stop()` is called or the
    /// transport returns an empty read (closed).
    public func start() {
        guard runTask == nil, !stopped else { return }
        runTask = Task { [weak self] in
            await self?.runLoop()
        }
    }

    /// Stop reading and finish the stream. Idempotent.
    public func stop() {
        stopped = true
        runTask?.cancel()
        runTask = nil
        continuation.finish()
    }

    private func runLoop() async {
        log.info("MmdvmReader: starting")
        var buffer: [UInt8] = []
        while !stopped, !Task.isCancelled {
            // Read whatever's available, up to a full max-length MMDVM frame.
            let chunk: [UInt8]
            do {
                chunk = try await transport.read(maxBytes: 512)
            } catch {
                log.error("MmdvmReader: read error: \(error)")
                continuation.finish()
                return
            }
            if chunk.isEmpty {
                log.info("MmdvmReader: transport closed")
                continuation.finish()
                return
            }
            buffer.append(contentsOf: chunk)

            // Drain as many complete frames as are in the buffer.
            while !buffer.isEmpty {
                do {
                    let result = try decodeMmdvmBytes(bytes: Data(buffer))
                    guard let frame = result.frame else {
                        // Partial frame — wait for more bytes.
                        break
                    }
                    continuation.yield(frame)
                    buffer.removeFirst(Int(result.bytesConsumed))
                } catch let error as MmdvmFrameError {
                    // Resync: drop the leading byte and try again on
                    // the next `0xE0`. Anything before a start byte
                    // is garbage from a previous session.
                    log.warning("MmdvmReader: decode error \(String(describing: error)); resyncing")
                    if let next = buffer.firstIndex(of: 0xE0), next > 0 {
                        buffer.removeFirst(next)
                    } else {
                        buffer.removeAll()
                    }
                } catch {
                    log.error("MmdvmReader: unexpected error: \(error)")
                    break
                }
            }
        }
        log.info("MmdvmReader: loop ended")
        continuation.finish()
    }
}

/// Thin helper for sending MMDVM frames. Non-streaming — one
/// `send` per frame. The writer borrows the transport and doesn't
/// own it, so the same transport can be shared with `MmdvmReader`.
public struct MmdvmWriter {
    public let transport: RadioTransport

    public init(transport: RadioTransport) {
        self.transport = transport
    }

    /// Encode `frame` and write the wire bytes to the transport.
    public func send(_ frame: MmdvmFrame) async throws {
        let wire = try buildMmdvmFrame(command: frame.command, payload: frame.payload)
        try await transport.write(Array(wire))
    }

    /// Send any command with an optional payload. Shortcut.
    public func send(command: UInt8, payload: Data = Data()) async throws {
        let wire = try buildMmdvmFrame(command: command, payload: payload)
        try await transport.write(Array(wire))
    }
}
