// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import Observation
import OSLog

private let log = Logger(subsystem: "org.swiftraccoon.lodestar", category: "transport")

/// UI-facing state store for the active `RadioTransport`.
///
/// Owns the currently selected device, the transport instance, and the
/// last received CAT response text. SwiftUI views observe this via
/// `@Observable`.
@Observable
@MainActor
public final class TransportCoordinator {
    public private(set) var availableDevices: [BluetoothDevice] = []
    public private(set) var selectedDevice: BluetoothDevice?
    public private(set) var state: RadioTransportState = .disconnected
    public private(set) var lastResponseText: String = ""
    public private(set) var isBusy: Bool = false
    public private(set) var mcpStatus: McpStatus = .idle
    public private(set) var radioMode: RadioMode = .unknown
    public private(set) var isProbingMode: Bool = false

    /// When `true`, `tryAutoConnect()` will reconnect on launch to the
    /// last-used radio (by Bluetooth address). Persisted.
    public var autoConnectRadio: Bool {
        didSet { UserDefaults.standard.set(autoConnectRadio, forKey: Self.autoConnectKey) }
    }

    /// Bluetooth address of the most recently connected radio. Captured
    /// on every successful `connect()`. Persisted so `tryAutoConnect()`
    /// can find it on the next launch.
    public private(set) var rememberedRadioAddress: String? {
        didSet { UserDefaults.standard.set(rememberedRadioAddress, forKey: Self.rememberedAddressKey) }
    }

    /// Display name of the most recently connected radio (persisted
    /// alongside the address so the UI can render "last used: TH-D75"
    /// without re-scanning when the device isn't paired currently).
    public private(set) var rememberedRadioName: String? {
        didSet { UserDefaults.standard.set(rememberedRadioName, forKey: Self.rememberedNameKey) }
    }

    /// Handle to the underlying transport — exposed only so
    /// `RelayCoordinator` can run an `MmdvmReader`/`MmdvmWriter`
    /// alongside the coordinator's own calls. All I/O still serialises
    /// through the transport actor.
    public var relayTransport: RadioTransport? { transport }

    private var transport: RadioTransport?
    private var stateObserver: Task<Void, Never>?

    private static let autoConnectKey = "lodestar.autoConnectRadio"
    private static let rememberedAddressKey = "lodestar.rememberedRadioAddress"
    private static let rememberedNameKey = "lodestar.rememberedRadioName"

    public init() {
        let defaults = UserDefaults.standard
        self.autoConnectRadio = defaults.bool(forKey: Self.autoConnectKey)
        self.rememberedRadioAddress = defaults.string(forKey: Self.rememberedAddressKey)
        self.rememberedRadioName = defaults.string(forKey: Self.rememberedNameKey)
    }

    /// Status of the current/most-recent MCP programming-mode operation.
    public enum McpStatus: Equatable, Sendable {
        case idle
        case running(String)      // human-readable progress message
        case succeededRebooting   // radio dropped the connection; user must reconnect
        case failed(String)
    }

    public func refreshPairedDevices() {
        #if os(macOS)
        availableDevices = IOBluetoothTransport.pairedDevices()
        #else
        availableDevices = PrivateBluetoothTransport.pairedDevices()
        #endif
    }

    public func select(_ device: BluetoothDevice) {
        selectedDevice = device
    }

    public func connect() async {
        guard let device = selectedDevice else { return }
        isBusy = true
        defer { isBusy = false }
        radioMode = .unknown

        #if os(macOS)
        let t: RadioTransport = IOBluetoothTransport(device: device)
        #else
        let t: RadioTransport = PrivateBluetoothTransport(device: device)
        #endif
        transport = t
        observeState(of: t)
        do {
            try await t.open()
            // Remember this radio so `tryAutoConnect()` can find it on
            // the next launch. Captured unconditionally — the user's
            // `autoConnectRadio` toggle controls whether we act on it.
            rememberedRadioAddress = device.address
            rememberedRadioName = device.name
            // Once open, fire off a mode probe so the UI can show the
            // right affordances (MCP button only if still in CAT mode).
            await probeRadioMode()
        } catch {
            state = .failed(message: String(describing: error))
        }
    }

    /// Auto-reconnect to the remembered radio on launch, if enabled and
    /// the remembered device is still paired. Idempotent and silent when
    /// conditions aren't met — safe to call unconditionally from app
    /// startup.
    public func tryAutoConnect() async {
        guard autoConnectRadio, transport == nil else { return }
        guard let address = rememberedRadioAddress else { return }
        refreshPairedDevices()
        guard let device = availableDevices.first(where: { $0.address == address }) else {
            log.info("Auto-connect: remembered radio \(address) not in paired list; skipping")
            return
        }
        log.info("Auto-connect: reconnecting to \(device.name) (\(address))")
        select(device)
        await connect()
    }

    public func disconnect() async {
        await transport?.close()
        transport = nil
        stateObserver?.cancel()
        stateObserver = nil
        state = .disconnected
        radioMode = .unknown
    }

    /// Re-run the MMDVM GetVersion probe against the current transport.
    /// Safe to call any time a transport exists — don't gate on
    /// `state == .connected` because that's set asynchronously by the
    /// state-observer task, which races with the probe kicked off from
    /// `connect()` and causes the first-launch probe to silently bail.
    public func probeRadioMode() async {
        guard let t = transport else { return }
        isProbingMode = true
        defer { isProbingMode = false }
        let prober = RadioModeProber(transport: t)
        do {
            radioMode = try await prober.probe()
            log.info("radio mode: \(String(describing: self.radioMode))")
        } catch {
            log.error("radio mode probe failed: \(error)")
            radioMode = .unknown
        }
    }

    public func sendIdentify() async {
        guard let t = transport else { return }
        isBusy = true
        defer { isBusy = false }
        do {
            let cmd = encodeCat(command: .identify)
            log.info("Send ID: writing \(cmd.count) bytes: \(Self.hexDump(cmd))")
            try await t.write(cmd)
            log.info("Send ID: write complete, waiting for response")

            // Race the reads against a 2s deadline. `readChunk` returns
            // nil when the timeout fires without the radio sending data,
            // which lets us exit the loop deterministically.
            var buffer: [UInt8] = []
            let totalDeadline = ContinuousClock.now.advanced(by: .seconds(2))
            while !buffer.contains(0x0D), ContinuousClock.now < totalDeadline {
                let chunk = try await readChunkWithTimeout(
                    transport: t, maxBytes: 256, deadline: totalDeadline
                )
                guard let chunk else {
                    log.warning("Send ID: read timed out after 2s; buffer=\(Self.hexDump(buffer))")
                    break
                }
                if chunk.isEmpty {
                    log.warning("Send ID: transport returned empty chunk (closed?)")
                    break
                }
                log.info("Send ID: got \(chunk.count) bytes: \(Self.hexDump(chunk))")
                buffer.append(contentsOf: chunk)
            }
            let crIndex = buffer.firstIndex(of: 0x0D) ?? buffer.endIndex
            let line = Array(buffer[..<crIndex])
            let response = parseCatLine(line: line)
            log.info("Send ID: parsed response=\(String(describing: response))")
            if buffer.isEmpty {
                lastResponseText = "No response in 2s. Check Menu 983 (must be USB)."
            } else {
                lastResponseText = Self.displayText(for: response)
            }
        } catch {
            log.error("Send ID failed: \(error)")
            lastResponseText = "Error: \(error)"
        }
    }

    /// Flip Menu 650 (DV Gateway) to Reflector Terminal Mode via an MCP
    /// programming-mode write. The radio drops the BT connection after
    /// the exit byte and reboots; the coordinator transitions to
    /// `.disconnected` and the user must re-pair / reconnect.
    public func enableReflectorTerminalMode() async {
        guard let t = transport, case .connected = state else {
            mcpStatus = .failed("Not connected to the radio.")
            return
        }
        isBusy = true
        mcpStatus = .running("Entering programming mode…")
        log.info("MCP: enable Reflector Terminal Mode starting")

        let session = McpSession(transport: t)
        do {
            // Surface progress as the coordinator works through the steps.
            mcpStatus = .running("Entering programming mode…")
            try await session.enterProgramming()
            mcpStatus = .running("Reading page 0x1C…")
            // `enableReflectorTerminalMode` performs read → patch → write → exit.
            // We already called `enterProgramming` above, so run the rest
            // piecewise for better progress reporting.
            let page = pageOf(offset: 0x1CA0)
            let byte = byteOf(offset: 0x1CA0)
            let currentData = try await session.readPage(page)
            let patched = try patchPageByte(pageData: currentData, offset: byte, value: 1)
            if currentData == patched {
                log.info("MCP: radio already in Reflector Terminal Mode; skipping write")
                mcpStatus = .running("Already enabled; exiting programming mode…")
            } else {
                mcpStatus = .running("Writing page 0x1C…")
                try await session.writePage(page, data: patched)
                mcpStatus = .running("Exiting programming mode…")
            }
            try await session.exitProgramming()

            // Radio will drop the connection; force our local state to match.
            await transport?.close()
            transport = nil
            stateObserver?.cancel()
            stateObserver = nil
            state = .disconnected
            radioMode = .unknown
            mcpStatus = .succeededRebooting
            isBusy = false
            log.info("MCP: enable Reflector Terminal Mode succeeded")
        } catch {
            log.error("MCP: enable Reflector Terminal Mode failed: \(error)")
            mcpStatus = .failed(String(describing: error))
            isBusy = false
        }
    }

    public func acknowledgeMcpStatus() {
        mcpStatus = .idle
    }

    /// Race `transport.read` against an absolute deadline. Returns `nil` if
    /// the deadline fires first.
    private func readChunkWithTimeout(
        transport: RadioTransport,
        maxBytes: Int,
        deadline: ContinuousClock.Instant
    ) async throws -> [UInt8]? {
        try await withThrowingTaskGroup(of: [UInt8]?.self) { group in
            group.addTask {
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

    private static func hexDump(_ bytes: [UInt8]) -> String {
        bytes.map { String(format: "%02x", $0) }.joined(separator: " ")
    }

    private func observeState(of transport: RadioTransport) {
        stateObserver?.cancel()
        let stream = transport.stateStream
        stateObserver = Task { [weak self] in
            for await s in stream {
                await MainActor.run {
                    self?.state = s
                }
            }
        }
    }

    private static func displayText(for resp: CatResponse) -> String {
        switch resp {
        case .identify(let model):
            return "Identify: \(model)"
        case .unknown:
            return "? (unknown command)"
        case .notAvailableInMode:
            return "N (not available in current mode)"
        case .raw(let line):
            return "Raw: \(line)"
        }
    }
}
