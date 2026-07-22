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

    /// Why the last mode probe failed (nil after a successful probe).
    /// Surfaced by the diagnostics card: a probe failure must never
    /// hide behind a bare "Mode unknown".
    public private(set) var lastProbeErrorText: String?

    /// Result of the last automated USB-relay setup (which radio
    /// settings were found and changed). Displayed so the operator sees
    /// exactly what the app read and did, with no menu spelunking.
    public private(set) var lastRelaySetup: UsbRelaySetupReport?

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

    /// Handle to the underlying transport, exposed only so
    /// `RelayCoordinator` can run an `MmdvmReader`/`MmdvmWriter`
    /// alongside the coordinator's own calls. All I/O still serialises
    /// through the transport actor.
    public var relayTransport: RadioTransport? { transport }

    /// Builds the concrete transport for a device. Overridable so tests
    /// can substitute `MockRadioTransport`; defaults to the platform
    /// transport (IOBluetooth on macOS, USB serial on iPad).
    public var transportFactory: @MainActor (BluetoothDevice) -> RadioTransport = { device in
        #if os(macOS)
        IOBluetoothTransport(device: device)
        #else
        USBSerialTransport(device: device, link: IOKitUSBSerialLink())
        #endif
    }

    /// Backoff schedule for post-drop reconnects. Overridable in tests.
    public var reconnectDelaysNs: [UInt64] = [3_000_000_000, 10_000_000_000, 30_000_000_000]

    private var transport: RadioTransport?
    private var stateObserver: Task<Void, Never>?
    private var radioReconnectTask: Task<Void, Never>?

    /// True when `handleScenePhaseBackground()` tore down a live
    /// connection that `handleScenePhaseActive()` should restore.
    private var resumeRadioOnForeground = false

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
        availableDevices = USBSerialTransport.availableDevices()
        #endif
    }

    public func select(_ device: BluetoothDevice) {
        selectedDevice = device
    }

    /// User-driven connect. Cancels any pending post-drop reconnect (the
    /// user's explicit action takes precedence) then performs the open.
    public func connect() async {
        radioReconnectTask?.cancel()
        radioReconnectTask = nil
        await performConnect()
    }

    /// The actual open/observe/probe sequence, shared by the public
    /// `connect()` and the reconnect task. Kept free of the reconnect
    /// cancel so the reconnect task can call it without cancelling itself.
    private func performConnect() async {
        guard let device = selectedDevice else { return }
        isBusy = true
        defer { isBusy = false }
        radioMode = .unknown

        let t = transportFactory(device)
        transport = t
        observeState(of: t)
        do {
            try await t.open()
            // Remember this radio so `tryAutoConnect()` can find it on
            // the next launch. Captured unconditionally; the user's
            // `autoConnectRadio` toggle controls whether we act on it.
            rememberedRadioAddress = device.address
            rememberedRadioName = device.name
            // Once open, fire off a mode probe so the UI can show the
            // right affordances (MCP button only if still in CAT mode).
            await probeRadioMode()
        } catch {
            state = .failed(message: error.displayMessage)
            stateObserver?.cancel()
            stateObserver = nil
            transport = nil
        }
    }

    /// Auto-reconnect to the remembered radio on launch, if enabled and
    /// the remembered device is still paired. Idempotent and silent when
    /// conditions aren't met, so it is safe to call unconditionally from
    /// app startup.
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
        if case .failed(let message) = state {
            NotificationManager.shared.autoConnectFailed(what: "Radio", reason: message)
        }
    }

    public func disconnect() async {
        radioReconnectTask?.cancel()
        radioReconnectTask = nil
        stateObserver?.cancel()
        stateObserver = nil
        // Detach BEFORE the await: `close()` is a cross-actor suspension
        // point, and a concurrent connect() interleaving there would have
        // its fresh transport clobbered by a post-await `transport = nil`.
        let t = transport
        transport = nil
        state = .disconnected
        radioMode = .unknown
        await t?.close()
    }

    /// Backgrounding teardown. DriverKit user-client connections don't
    /// usefully survive app suspension (a suspended app receives no
    /// IOKit notifications, and pending mach messages may be dropped),
    /// so the recommended pattern is: disconnect before suspending,
    /// rescan + reopen on wake. Called from the scenePhase observer on
    /// iOS only; macOS Bluetooth connections survive fine.
    public func handleScenePhaseBackground() async {
        resumeRadioOnForeground = transport != nil
        guard transport != nil else { return }
        await disconnect()
    }

    /// Foreground restore: refresh the device list (also the mitigation
    /// for radios plugged in while suspended, whose arrival
    /// notifications never fire) and reconnect if backgrounding tore a
    /// live connection down.
    public func handleScenePhaseActive() async {
        refreshPairedDevices()
        guard resumeRadioOnForeground else { return }
        resumeRadioOnForeground = false
        if selectedDevice == nil, let first = availableDevices.first {
            select(first)
        }
        guard selectedDevice != nil else { return }
        await connect()
    }

    /// Re-run the MMDVM GetVersion probe against the current transport.
    /// Safe to call any time a transport exists. Don't gate on
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
            lastProbeErrorText = nil
            log.info("radio mode: \(String(describing: self.radioMode))")
        } catch {
            log.error("radio mode probe failed: \(error)")
            lastProbeErrorText = error.displayMessage
            radioMode = .unknown
        }
    }

    /// Assemble everything needed to debug the radio link into one
    /// pasteable blob: coordinator state, last errors, and (USB) the
    /// dext's counters + event ring fetched through the user client.
    public func diagnosticsText() async -> String {
        var lines: [String] = []
        lines.append("=== Lodestar radio diagnostics ===")
        lines.append("state=\(state) mode=\(radioMode) busy=\(isBusy) probing=\(isProbingMode)")
        lines.append("device=\(selectedDevice?.name ?? "none") (\(selectedDevice?.address ?? "-"))")
        if let probeError = lastProbeErrorText {
            lines.append("last probe error: \(probeError)")
        }
        if !lastResponseText.isEmpty {
            lines.append("last CAT response: \(lastResponseText)")
        }
        if let usb = transport as? USBSerialTransport {
            lines.append(await usb.diagnosticsReport())
        } else if transport == nil {
            lines.append("no transport (disconnected)")
        }
        return lines.joined(separator: "\n")
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
            lastResponseText = "Error: \(error.displayMessage)"
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
        // Prove the CAT path FIRST. `0M PROGRAM` sent at a radio that
        // isn't answering leaves it half-entered in programming mode,
        // mute to everything until a power cycle (hardware-verified
        // 2026-07-19, and it poisons all subsequent debugging).
        await sendIdentify()
        guard lastResponseText.hasPrefix("Identify:") else {
            mcpStatus = .failed(
                "Radio is not answering CAT (last response: \(lastResponseText)). "
                + "Not entering programming mode, because that would wedge the radio. "
                + "Power-cycle the radio, reconnect, and retry once Send ID works.")
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

            // Radio will drop the connection; force our local state to
            // match. Detach before the await (same reentrancy rule as
            // `disconnect()`).
            stateObserver?.cancel()
            stateObserver = nil
            let t = transport
            transport = nil
            state = .disconnected
            radioMode = .unknown
            await t?.close()
            mcpStatus = .succeededRebooting
            isBusy = false
            log.info("MCP: enable Reflector Terminal Mode succeeded")
            // The radio reboots itself on programming-mode exit (its
            // protocol, same as over Bluetooth). Reconnect automatically
            // as it re-enumerates, with no manual reconnect step.
            scheduleRadioReconnect()
        } catch {
            log.error("MCP: enable Reflector Terminal Mode failed: \(error)")
            mcpStatus = .failed(error.displayMessage)
            isBusy = false
        }
    }

    public func acknowledgeMcpStatus() {
        mcpStatus = .idle
    }

    /// One-tap, fully automated path to a USB-relay-ready radio.
    ///
    /// The app reads and fixes BOTH settings the relay depends on
    /// (Menu 650 Reflector Terminal Mode and Menu 985 DV Gateway
    /// Interface = USB), reboots the radio only if something changed,
    /// then reconnects and POLLS for MMDVM (terminal mode engages
    /// ~50 s after the reboot, so a single probe would always miss it).
    /// No radio keypresses, no manual reconnect.
    public func setUpUsbRelay() async {
        guard let t = transport, case .connected = state else {
            mcpStatus = .failed("Connect the radio first.")
            return
        }
        isBusy = true

        // 1. Cheap readiness check: if MMDVM already answers over USB,
        // the radio is set up correctly and no reboot is needed.
        mcpStatus = .running("Checking whether the radio is already relay-ready…")
        await probeRadioMode()
        if radioMode == .mmdvm {
            lastResponseText = "Radio is already in Terminal Mode over USB, ready to relay."
            mcpStatus = .idle
            isBusy = false
            return
        }

        // 2. Reprogramming needs a live CAT parser.
        await sendIdentify()
        guard lastResponseText.hasPrefix("Identify:") else {
            mcpStatus = .failed(
                "Radio isn't answering CAT (\(lastResponseText)). Can't reprogram. "
                + "Power-cycle the radio, reconnect, and try again.")
            isBusy = false
            return
        }

        // 3. Read + fix the relay settings in one programming pass.
        mcpStatus = .running("Reading and updating radio settings…")
        let session = McpSession(transport: t)
        let report: UsbRelaySetupReport
        do {
            report = try await session.prepareForUsbRelay()
        } catch {
            mcpStatus = .failed("Couldn't program the radio: \(error.displayMessage)")
            isBusy = false
            return
        }
        lastRelaySetup = report
        log.info("USB relay setup: \(report.summary)")

        // 4. Reconnect + poll for terminal mode to come up.
        if report.rebooted {
            mcpStatus = .running("Applied changes; radio is rebooting.\n\(report.summary)")
        } else {
            mcpStatus = .running("Settings were already correct; waiting for MMDVM…")
        }
        await reconnectAndPollForMmdvm()
        isBusy = false
    }

    /// After a settings-change reboot, reconnect to the radio and poll
    /// the MMDVM probe until terminal mode answers (or a 2-minute
    /// ceiling). Manages the transport directly so the standard
    /// unexpected-drop reconnect logic doesn't race this.
    private func reconnectAndPollForMmdvm() async {
        radioReconnectTask?.cancel()
        radioReconnectTask = nil
        stateObserver?.cancel()
        stateObserver = nil
        await transport?.close()
        transport = nil
        state = .disconnected
        radioMode = .unknown

        guard let device = selectedDevice else {
            mcpStatus = .failed("No radio selected to reconnect to.")
            return
        }

        // Let the radio actually drop and begin rebooting.
        try? await Task.sleep(nanoseconds: 3_000_000_000)
        let deadline = ContinuousClock.now.advanced(by: .seconds(120))

        while ContinuousClock.now < deadline {
            if transport == nil {
                refreshPairedDevices()
                if availableDevices.contains(where: { $0.address == device.address }) {
                    mcpStatus = .running("Reconnecting to the radio…")
                    let fresh = transportFactory(device)
                    do {
                        try await fresh.open()
                        transport = fresh
                    } catch {
                        // Radio not back yet; wait and retry.
                        try? await Task.sleep(nanoseconds: 3_000_000_000)
                        continue
                    }
                } else {
                    try? await Task.sleep(nanoseconds: 3_000_000_000)
                    continue
                }
            }

            if let t = transport {
                mcpStatus = .running("Connected; waiting for Terminal Mode (MMDVM)…")
                let prober = RadioModeProber(transport: t)
                if let mode = try? await prober.probe(), mode == .mmdvm {
                    radioMode = .mmdvm
                    lastProbeErrorText = nil
                    state = .connected
                    observeState(of: t)   // hand back to normal drop-handling
                    rememberedRadioAddress = device.address
                    rememberedRadioName = device.name
                    lastResponseText = "Radio is in Terminal Mode over USB, ready to relay."
                    mcpStatus = .idle
                    return
                }
                // Still CAT (radio mid-boot); the transport may also
                // drop as the radio reboots; detect and re-acquire.
                if await t.state == .disconnected {
                    await t.close()
                    transport = nil
                }
            }
            try? await Task.sleep(nanoseconds: 3_000_000_000)
        }

        // Timed out. Leave whatever transport we have connected for CAT.
        if let t = transport {
            state = .connected
            observeState(of: t)
        }
        mcpStatus = .failed(
            "The radio didn't come up in Terminal Mode within 2 minutes. "
            + "If its screen shows TERM, tap Set up USB relay again; otherwise "
            + "it may still be booting.")
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
        stateObserver = Task { @MainActor [weak self] in
            for await s in stream {
                guard let self else { return }
                self.applyTransportState(s)
            }
        }
    }

    /// Apply a state yielded by the transport's own stream. A
    /// `.connected → .disconnected` transition seen HERE is always
    /// unexpected: user-initiated paths cancel the observer before
    /// closing the transport.
    private func applyTransportState(_ s: RadioTransportState) {
        let previous = state
        state = s
        switch s {
        case .failed:
            radioMode = .unknown
        case .disconnected:
            guard case .connected = previous else { return }
            log.warning("Transport dropped unexpectedly")
            radioMode = .unknown
            transport = nil
            NotificationManager.shared.radioDisconnected()
            scheduleRadioReconnect()
        case .connecting, .connected:
            break
        }
    }

    /// Best-effort reconnect after an unexpected drop. Mirrors the
    /// reflector coordinator's backoff so a BT blip mid-QSO heals
    /// without user action. Cancelled by `disconnect()` and by any
    /// fresh manual `connect()`.
    private func scheduleRadioReconnect() {
        guard selectedDevice != nil else { return }
        radioReconnectTask?.cancel()
        radioReconnectTask = Task { @MainActor [weak self] in
            guard let delays = self?.reconnectDelaysNs else { return }
            for delay in delays {
                try? await Task.sleep(nanoseconds: delay)
                guard let self, !Task.isCancelled else { return }
                // The real serialization point: cancellation is cooperative and an in-flight attempt runs to completion, but a live transport always makes the next attempt bail here.
                guard self.transport == nil else { return }
                await self.performConnect()
                if case .connected = self.state { return }
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
