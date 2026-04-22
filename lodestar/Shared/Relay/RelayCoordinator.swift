// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import Observation
import OSLog

private let log = Logger(subsystem: "org.swiftraccoon.lodestar", category: "relay")

/// MMDVM command byte constants from `mmdvm-core::command`. Copied
/// here because UniFFI doesn't ship plain Rust `const` values to
/// Swift — they're protocol constants, not crate-version-sensitive.
private enum MmdvmCmd {
    static let dstarHeader: UInt8 = 0x10
    static let dstarData: UInt8 = 0x11
    static let dstarLost: UInt8 = 0x12
    static let dstarEot: UInt8 = 0x13
}

/// Bridges MMDVM D-STAR frames between a TH-D75 in Reflector Terminal
/// Mode and an active `ReflectorSession`. When started:
///
/// - **Radio → Reflector:** reads MMDVM frames off the BT transport,
///   filters for `DStarHeader` / `DStarData` / `DStarEot`, and calls
///   `ReflectorSession.sendHeader` / `sendVoice` / `sendEot`.
/// - **Reflector → Radio:** hooks into `ReflectorCoordinator` so every
///   reflector voice event is re-wrapped as an MMDVM frame and sent
///   back down the transport.
///
/// Both directions run only while the radio mode is `.mmdvm` and the
/// reflector session is connected; `stop()` is idempotent.
@Observable
@MainActor
public final class RelayCoordinator {
    public enum RelayState: Equatable, Sendable {
        case stopped
        case starting
        case running
        case failed(String)
    }

    public private(set) var state: RelayState = .stopped
    public private(set) var framesFromRadio: Int = 0
    public private(set) var framesFromReflector: Int = 0
    public private(set) var lastError: String?

    private let transportCoordinator: TransportCoordinator
    private let reflectorCoordinator: ReflectorCoordinator

    private var mmdvmReader: MmdvmReader?
    private var readerTask: Task<Void, Never>?

    /// Outbound (radio→reflector) stream state. `nil` between streams.
    private var outboundStreamId: UInt16?
    private var outboundSeq: UInt8 = 0

    /// Summary of the current local TX, captured on header and flushed
    /// on EOT so we can insert a recently-heard entry for our own
    /// relayed stream (the reflector won't echo it back to us).
    private struct LocalTxTracker {
        let mycall: String
        let suffix: String
        let urcall: String
        let startedAt: Date
        var frames: UInt32
    }
    private var localTx: LocalTxTracker?

    public init(
        transport: TransportCoordinator,
        reflector: ReflectorCoordinator
    ) {
        self.transportCoordinator = transport
        self.reflectorCoordinator = reflector
    }

    // MARK: - Public API

    /// Start bidirectional relay. Requires: transport connected, radio
    /// in MMDVM mode, reflector session connected.
    public func start() async {
        guard state != .running, state != .starting else { return }
        state = .starting

        // Gate on preconditions.
        guard transportCoordinator.radioMode == .mmdvm else {
            failWith("radio is not in MMDVM mode (current: \(transportCoordinator.radioMode))")
            return
        }
        guard let session = reflectorCoordinator.activeSession else {
            failWith("no active reflector session")
            return
        }
        guard let transport = transportCoordinator.relayTransport else {
            failWith("transport is not available")
            return
        }

        // Reflector→radio: install the hook and remember to clear it on stop.
        reflectorCoordinator.relayHook = { [weak self] event in
            guard let self else { return }
            Task { @MainActor in
                await self.handleReflectorEvent(event, transport: transport)
            }
        }

        // Radio→reflector: start the MMDVM reader.
        let reader = MmdvmReader(transport: transport)
        self.mmdvmReader = reader
        await reader.start()
        let frames = await reader.frames
        readerTask = Task { [weak self] in
            for await frame in frames {
                await self?.handleRadioFrame(frame, session: session, transport: transport)
            }
            // Stream ended (transport closed or reader stopped).
            await self?.markStopped()
        }

        state = .running
        log.info("Relay: running")
    }

    /// Stop the relay. Idempotent.
    public func stop() async {
        reflectorCoordinator.relayHook = nil
        readerTask?.cancel()
        readerTask = nil
        if let r = mmdvmReader {
            await r.stop()
        }
        mmdvmReader = nil
        outboundStreamId = nil
        outboundSeq = 0
        // Flush any in-progress TX so it still shows up in Recently heard.
        if let tx = localTx {
            reflectorCoordinator.logLocalTransmission(
                mycall: tx.mycall,
                suffix: tx.suffix,
                urcall: tx.urcall,
                startedAt: tx.startedAt,
                frames: tx.frames,
                text: nil
            )
            localTx = nil
        }
        state = .stopped
        log.info("Relay: stopped")
    }

    // MARK: - Internal

    private func handleRadioFrame(
        _ frame: MmdvmFrame,
        session: ReflectorSession,
        transport _: RadioTransport
    ) async {
        framesFromRadio += 1
        do {
            switch frame.command {
            case MmdvmCmd.dstarHeader:
                // New inbound stream from the radio. Generate a fresh
                // stream ID (non-zero) and forward the 41-byte header.
                let streamId = Self.freshStreamId()
                outboundStreamId = streamId
                outboundSeq = 0
                // Decode the radio's header so we can synthesise a
                // recently-heard entry for our own TX on EOT — the
                // reflector won't echo it back.
                if let decoded = decodeRadioHeader(bytes: frame.payload) {
                    localTx = LocalTxTracker(
                        mycall: decoded.mycall,
                        suffix: decoded.suffix,
                        urcall: decoded.urcall,
                        startedAt: Date(),
                        frames: 0
                    )
                }
                log.info("Relay: radio → reflector HEADER, streamId=0x\(String(streamId, radix: 16))")
                try await session.sendHeader(
                    headerBytes: frame.payload,
                    streamId: streamId
                )
            case MmdvmCmd.dstarData:
                guard let sid = outboundStreamId else {
                    log.warning("Relay: ignoring DStarData with no header yet")
                    return
                }
                let seq = outboundSeq
                outboundSeq &+= 1
                if localTx != nil {
                    localTx?.frames &+= 1
                }
                try await session.sendVoice(streamId: sid, seq: seq, voiceBytes: frame.payload)
            case MmdvmCmd.dstarEot, MmdvmCmd.dstarLost:
                guard let sid = outboundStreamId else { return }
                let seq = outboundSeq
                // send_eot returns the assembled TX message (Kenwood
                // 4-block text) the radio transmitted during this
                // stream, so we can include it in the local heard entry.
                let txText = try await session.sendEot(streamId: sid, seq: seq)
                outboundStreamId = nil
                outboundSeq = 0
                if let tx = localTx {
                    reflectorCoordinator.logLocalTransmission(
                        mycall: tx.mycall,
                        suffix: tx.suffix,
                        urcall: tx.urcall,
                        startedAt: tx.startedAt,
                        frames: tx.frames,
                        text: txText
                    )
                }
                localTx = nil
                log.info("Relay: radio → reflector EOT")
            default:
                // Any other MMDVM command (status, version, etc.) is
                // informational at the relay layer — skip.
                break
            }
        } catch {
            log.error("Relay: radio → reflector failed: \(error)")
            lastError = String(describing: error)
        }
    }

    private func handleReflectorEvent(_ event: ReflectorEvent, transport: RadioTransport) async {
        let writer = MmdvmWriter(transport: transport)
        do {
            switch event {
            case .voiceStart(_, _, _, _, _, _, let headerBytes):
                framesFromReflector += 1
                try await writer.send(command: MmdvmCmd.dstarHeader, payload: headerBytes)
                log.info("Relay: reflector → radio HEADER (\(headerBytes.count) bytes)")
            case .voiceFrame(_, _, let voiceBytes):
                framesFromReflector += 1
                try await writer.send(command: MmdvmCmd.dstarData, payload: voiceBytes)
            case .voiceEnd:
                try await writer.send(command: MmdvmCmd.dstarEot, payload: Data())
                log.info("Relay: reflector → radio EOT")
            case .connected, .disconnected, .pollEcho, .slowDataUpdate, .ended:
                // Slow-data text / GPS is surfaced by the reflector
                // coordinator for the UI; it isn't relayed to the
                // radio as a standalone frame (the text/GPS bytes
                // already ride inside every .voiceFrame).
                break
            }
        } catch {
            log.error("Relay: reflector → radio failed: \(error)")
            lastError = String(describing: error)
        }
    }

    private func markStopped() {
        if state == .running {
            state = .stopped
            log.info("Relay: reader stream ended — stopping")
        }
    }

    private func failWith(_ msg: String) {
        state = .failed(msg)
        lastError = msg
        log.error("Relay: \(msg)")
    }

    /// Generate a non-zero `StreamId` for an outbound stream. We use a
    /// random u16 and ensure it's not zero (Rust-side `StreamId::new`
    /// rejects zero).
    private static func freshStreamId() -> UInt16 {
        var sid: UInt16
        repeat {
            sid = UInt16.random(in: 1...UInt16.max)
        } while sid == 0
        return sid
    }
}
