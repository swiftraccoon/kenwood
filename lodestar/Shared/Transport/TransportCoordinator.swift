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
    /// Synchronous fence for state observers. Task cancellation is
    /// cooperative, so a cancelled observer may still have buffered stream
    /// elements ready to consume unless every element also proves it belongs
    /// to the current observation generation.
    private var stateObservationGeneration: UInt64 = 0
    private var radioReconnectTask: Task<Void, Never>?
    private var mcpOperationInFlight = false
    /// Synchronous MainActor lease for every coordinator-owned transport
    /// transaction. Acquiring this before the first await prevents a parked
    /// read or opening transport from overlapping MCP or another coordinator
    /// operation.
    private var coordinatorIOInFlight = false
    /// Generation fence for transport opens. Background teardown advances
    /// this while an uncancellable open is suspended so that attempt can
    /// close itself without restoring or clobbering coordinator state.
    private var transportGeneration: UInt64 = 0

    /// True when `handleScenePhaseBackground()` tore down a live
    /// connection that `handleScenePhaseActive()` should restore.
    private var resumeRadioOnForeground = false
    /// Foreground arrived before an old coordinator transaction released
    /// its lease. `endCoordinatorIO()` schedules the deferred reconnect.
    private var foregroundReconnectPending = false

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
        guard !mcpOperationInFlight else {
            log.warning("Connect ignored while MCP owns the radio transport")
            return
        }
        radioReconnectTask?.cancel()
        radioReconnectTask = nil
        guard beginCoordinatorIO(operation: "connect") else { return }
        defer { endCoordinatorIO() }
        await performConnectOwned()
    }

    /// The actual open/observe/probe sequence, shared by the public
    /// `connect()` and the reconnect task. Kept free of the reconnect
    /// cancel so the reconnect task can call it without cancelling itself.
    private func performConnect() async {
        guard beginCoordinatorIO(operation: "reconnect") else { return }
        defer { endCoordinatorIO() }
        await performConnectOwned()
    }

    private func performConnectOwned() async {
        guard let device = selectedDevice else { return }
        guard transport == nil else {
            log.warning("Connect ignored because a transport already exists")
            return
        }
        transportGeneration &+= 1
        let attemptGeneration = transportGeneration
        isBusy = true
        defer { isBusy = false }
        radioMode = .unknown

        let t = transportFactory(device)
        transport = t
        observeState(of: t)
        do {
            try await t.open()
            guard attemptGeneration == transportGeneration else {
                log.info("Discarding transport opened after background teardown")
                await t.close()
                return
            }
            guard case .connected = await t.state else {
                throw RadioTransportError.openFailed(
                    reason: "transport closed before connection setup completed"
                )
            }
            // `open()` has a connected-state postcondition, but the observer
            // task may not have consumed its buffered `.connected` event yet.
            // Publish the postcondition synchronously before `connect()` can
            // return; a later buffered `.connecting` event is ignored below.
            state = .connected
            // Remember this radio so `tryAutoConnect()` can find it on
            // the next launch. Captured unconditionally; the user's
            // `autoConnectRadio` toggle controls whether we act on it.
            rememberedRadioAddress = device.address
            rememberedRadioName = device.name
            // Once open, fire off a mode probe so the UI can show the
            // right affordances (MCP button only if still in CAT mode).
            try Task.checkCancellation()
            await probeRadioModeOwned()
            try Task.checkCancellation()
            guard attemptGeneration == transportGeneration else {
                log.info("Discarding transport probed after background teardown")
                await t.close()
                return
            }
        } catch {
            if attemptGeneration == transportGeneration {
                state = .failed(message: error.displayMessage)
                stopObservingState()
                transport = nil
            } else {
                log.info("Ignoring stale connect failure after background teardown")
            }
            await t.close()
        }
    }

    /// Auto-reconnect to the remembered radio on launch, if enabled and
    /// the remembered device is still paired. Idempotent and silent when
    /// conditions aren't met, so it is safe to call unconditionally from
    /// app startup.
    public func tryAutoConnect() async {
        guard !mcpOperationInFlight else { return }
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
        guard !mcpOperationInFlight else {
            log.warning("Disconnect ignored while MCP cleanup owns the radio transport")
            return
        }
        guard beginCoordinatorIO(operation: "disconnect") else { return }
        defer { endCoordinatorIO() }
        radioReconnectTask?.cancel()
        radioReconnectTask = nil
        stopObservingState()
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
        if mcpOperationInFlight {
            log.warning("Background transport teardown deferred during MCP cleanup")
            return
        }
        foregroundReconnectPending = false
        resumeRadioOnForeground = transport != nil
        guard transport != nil else { return }
        await detachTransportForBackground()
    }

    /// Foreground restore: refresh the device list (also the mitigation
    /// for radios plugged in while suspended, whose arrival
    /// notifications never fire) and reconnect if backgrounding tore a
    /// live connection down.
    public func handleScenePhaseActive() async {
        guard !mcpOperationInFlight else { return }
        refreshPairedDevices()
        guard resumeRadioOnForeground else { return }
        guard !coordinatorIOInFlight else {
            foregroundReconnectPending = true
            return
        }
        await reconnectAfterBackground()
    }

    /// Re-run the MMDVM GetVersion probe against the current transport.
    /// Safe to call any time a transport exists. Don't gate on
    /// `state == .connected` because that's set asynchronously by the
    /// state-observer task, which races with the probe kicked off from
    /// `connect()` and causes the first-launch probe to silently bail.
    public func probeRadioMode() async {
        guard !mcpOperationInFlight else {
            log.warning("Mode probe ignored while MCP owns the radio transport")
            return
        }
        guard beginCoordinatorIO(operation: "mode probe") else { return }
        defer { endCoordinatorIO() }
        await probeRadioModeOwned()
    }

    private func probeRadioModeOwned() async {
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
        guard !mcpOperationInFlight else {
            log.warning("CAT identify ignored while MCP owns the radio transport")
            return
        }
        guard beginCoordinatorIO(operation: "CAT identify") else { return }
        defer { endCoordinatorIO() }
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
        guard !mcpOperationInFlight else {
            log.warning("Ignoring overlapping MCP setup request")
            return
        }
        #if os(iOS)
        mcpStatus = .failed(
            "Radio programming is disabled on iPad because app suspension can "
                + "interrupt MCP cleanup. No radio setting was changed."
        )
        return
        #else
        guard let t = transport, case .connected = state else {
            mcpStatus = .failed("Not connected to the radio.")
            return
        }
        guard !coordinatorIOInFlight else {
            mcpStatus = .failed(
                "Another radio operation is still in progress; wait for it to finish "
                    + "before entering programming mode."
            )
            return
        }
        // MainActor makes both ownership transitions atomic with respect to
        // every public coordinator operation and reconnect attempt.
        mcpOperationInFlight = true
        coordinatorIOInFlight = true
        isBusy = true
        defer {
            mcpOperationInFlight = false
            coordinatorIOInFlight = false
            isBusy = false
        }
        mcpStatus = .running("Entering programming mode…")
        log.info("MCP: enable Reflector Terminal Mode starting")

        let session = McpSession(transport: t)
        guard await proveCatModeForMcp() else { return }
        quarantineTransportForMcp()
        do {
            // `enterProgramming` performs exact typed ID/FV target
            // qualification immediately before its entry wire write.
            mcpStatus = .running("Qualifying TH-D75 firmware 1.03…")
            try await session.enterProgramming()
            mcpStatus = .running("Reading page 0x1C…")
            let page = pageOf(offset: 0x1CA0)
            let byte = byteOf(offset: 0x1CA0)
            let currentData = try await session.readPage(page)
            let patched = try patchPageByte(
                pageData: currentData, offset: byte, value: 1
            )
            if currentData == patched {
                log.info("MCP: radio already in Reflector Terminal Mode; skipping write")
                mcpStatus = .running("Already enabled; exiting programming mode…")
            } else {
                mcpStatus = .running("Writing page 0x1C…")
                try await session.writePage(page, data: patched)
                mcpStatus = .running("Exiting programming mode…")
            }
            try await session.exitProgramming()

            await detachAfterMcpAttempt(t)
            mcpStatus = .succeededRebooting
            log.info("MCP: enable Reflector Terminal Mode succeeded")
            // The radio reboots itself on programming-mode exit (its
            // protocol, same as over Bluetooth). Reconnect automatically
            // as it re-enumerates, with no manual reconnect step.
            scheduleRadioReconnect()
        } catch {
            log.error("MCP: enable Reflector Terminal Mode failed: \(error)")
            let mustDetach = await session.requiresTransportDetach()
            let exitProved = await session.exitWasProved()
            if mustDetach {
                await detachAfterMcpAttempt(t)
                if exitProved {
                    scheduleRadioReconnect()
                }
            } else {
                await restoreObservationAfterSafeQualificationFailure(t)
            }
            mcpStatus = .failed(error.displayMessage)
        }
        #endif
    }

    public func acknowledgeMcpStatus() {
        mcpStatus = .idle
    }

    /// Remove every coordinator reference to a transport that may have
    /// entered MCP or begun its reset. Detach before awaiting close so
    /// the observable state can never claim this link is still usable.
    private func detachAfterMcpAttempt(_ attemptedTransport: RadioTransport) async {
        radioReconnectTask?.cancel()
        radioReconnectTask = nil
        stopObservingState()
        transport = nil
        state = .disconnected
        radioMode = .unknown
        await attemptedTransport.close()
    }

    /// Give one McpSession exclusive logical ownership before its first
    /// await. This hides the transport from RelayCoordinator and every
    /// other public coordinator I/O path while the wire mode is unknown.
    private func quarantineTransportForMcp() {
        radioReconnectTask?.cancel()
        radioReconnectTask = nil
        stopObservingState()
        transport = nil
        state = .disconnected
        radioMode = .unknown
    }

    /// Require a positive CAT-mode classification before MCP takes the
    /// transport. A live MMDVM classification means a relay may already
    /// own captured reader/writer handles, so no CAT or MCP byte is sent.
    private func proveCatModeForMcp() async -> Bool {
        if radioMode == .cat {
            return true
        }
        if radioMode == .mmdvm {
            lastResponseText = "Radio is already in Terminal Mode."
            mcpStatus = .idle
            return false
        }

        mcpStatus = .running("Confirming CAT mode…")
        await probeRadioModeOwned()
        switch radioMode {
        case .cat:
            return true
        case .mmdvm:
            lastResponseText = "Radio is already in Terminal Mode."
            mcpStatus = .idle
            return false
        case .unknown, .unrecognized:
            mcpStatus = .failed(
                "Radio mode could not be proved as CAT; refusing to enter programming mode."
            )
            return false
        }
    }

    /// Qualification failures happen before any MCP entry byte and leave
    /// a live CAT transport safe to keep. Restore observation only if the
    /// transport itself still agrees that it is connected.
    private func restoreObservationAfterSafeQualificationFailure(
        _ attemptedTransport: RadioTransport
    ) async {
        if await attemptedTransport.state == .connected {
            transport = attemptedTransport
            state = .connected
            observeState(of: attemptedTransport)
        } else {
            await detachAfterMcpAttempt(attemptedTransport)
        }
    }

    /// Background teardown is intentionally allowed to interrupt an ordinary
    /// coordinator transaction. Detach synchronously, invalidate any suspended
    /// open, then close so parked reads are resumed without exposing the link.
    private func detachTransportForBackground() async {
        radioReconnectTask?.cancel()
        radioReconnectTask = nil
        stopObservingState()
        transportGeneration &+= 1
        let detached = transport
        transport = nil
        state = .disconnected
        radioMode = .unknown
        await detached?.close()
    }

    private func reconnectAfterBackground() async {
        guard resumeRadioOnForeground else { return }
        guard !coordinatorIOInFlight, !mcpOperationInFlight else {
            foregroundReconnectPending = true
            return
        }
        foregroundReconnectPending = false
        resumeRadioOnForeground = false
        if selectedDevice == nil, let first = availableDevices.first {
            select(first)
        }
        guard selectedDevice != nil else { return }
        await connect()
    }

    /// Acquire the coordinator's transport transaction lease synchronously,
    /// before the caller reaches its first suspension point.
    private func beginCoordinatorIO(operation: String) -> Bool {
        guard !mcpOperationInFlight else {
            log.warning("\(operation, privacy: .public) ignored while MCP owns the transport")
            return false
        }
        guard !coordinatorIOInFlight else {
            log.warning(
                "\(operation, privacy: .public) ignored while another radio operation is in progress"
            )
            return false
        }
        coordinatorIOInFlight = true
        return true
    }

    private func endCoordinatorIO() {
        coordinatorIOInFlight = false
        guard foregroundReconnectPending, resumeRadioOnForeground else { return }
        Task { @MainActor [weak self] in
            await self?.reconnectAfterBackground()
        }
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
        stopObservingState()
        let observationGeneration = stateObservationGeneration
        let stream = transport.stateStream
        stateObserver = Task { @MainActor [weak self] in
            for await s in stream {
                guard let self,
                      !Task.isCancelled,
                      self.stateObservationGeneration == observationGeneration else { return }
                self.applyTransportState(s)
            }
        }
    }

    /// Invalidate the observer synchronously before detaching or closing its
    /// transport. The generation change is the correctness fence; cancellation
    /// only lets the old task retire promptly.
    private func stopObservingState() {
        stateObservationGeneration &+= 1
        stateObserver?.cancel()
        stateObserver = nil
    }

    /// Apply a state yielded by the transport's own stream. A
    /// `.connected → terminal` transition seen HERE is always unexpected:
    /// user-initiated paths cancel the observer before closing the transport.
    private func applyTransportState(_ s: RadioTransportState) {
        let previous = state
        switch s {
        case .failed(let message):
            state = s
            radioMode = .unknown
            guard case .connected = previous else { return }
            handleUnexpectedTransportEnd(
                reason: "Transport failed unexpectedly: \(message)"
            )
        case .disconnected:
            state = s
            guard case .connected = previous else { return }
            handleUnexpectedTransportEnd(reason: "Transport dropped unexpectedly")
        case .connecting:
            // `performConnectOwned()` publishes `.connected` from the
            // transport's post-open snapshot. A buffered pre-open event must
            // never regress that current state after `connect()` returns.
            guard case .connected = previous else {
                state = s
                return
            }
        case .connected:
            state = s
        }
    }

    /// Detach a terminal transport before exposing reconnect affordances. In
    /// particular, the Bluetooth helper reports EOF/write ambiguity as
    /// `.failed`; retaining that poisoned object makes both manual and
    /// automatic reconnect bail out because they require `transport == nil`.
    private func handleUnexpectedTransportEnd(reason: String) {
        log.warning("\(reason)")
        radioMode = .unknown
        transport = nil
        stopObservingState()
        NotificationManager.shared.radioDisconnected()
        scheduleRadioReconnect()
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
        case .firmwareVersion(let version):
            return "Firmware: \(version)"
        case .unknown:
            return "? (unknown command)"
        case .notAvailableInMode:
            return "N (not available in current mode)"
        case .raw(let line):
            return "Raw: \(line)"
        }
    }
}
