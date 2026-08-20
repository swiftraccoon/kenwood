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
    public var relayTransport: RadioTransport? {
        state == .connected ? transport : nil
    }

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

    /// Metadata source for paired-device UI. Internal so tests can supply a
    /// deterministic list without consulting the host's Bluetooth cache.
    var bluetoothPairedDevicesProvider: @MainActor () -> [BluetoothDevice] = {
        IOBluetoothTransport.pairedDevices()
    }

    /// Notification seam for launch auto-connect failures. Tests replace this
    /// instead of posting into the developer's Notification Center.
    var autoConnectFailureNotifier: @MainActor (String) -> Void = { reason in
        NotificationManager.shared.autoConnectFailed(what: "Radio", reason: reason)
    }

    /// Backoff schedule for post-drop reconnects. Overridable in tests.
    public var reconnectDelaysNs: [UInt64] = [3_000_000_000, 10_000_000_000, 30_000_000_000]

    /// Reboot-to-MMDVM polling interval and bound. Internal tests shorten both;
    /// production follows the hardware-verified three-second/90-second policy.
    var terminalModePollDelayNs: UInt64 = 3_000_000_000
    var terminalModeTransitionWindow: Duration = .seconds(90)

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
        case succeeded            // same exact link positively answered MMDVM
        case failed(String)
    }

    public func refreshPairedDevices() {
        #if os(macOS)
        availableDevices = bluetoothPairedDevicesProvider()
        #else
        availableDevices = USBSerialTransport.availableDevices()
        #endif
    }

    public func select(_ device: BluetoothDevice) {
        selectedDevice = device
    }

    /// Invalidate a picker-owned connection attempt before its suspended open
    /// or protocol proof can publish state. A completed connection is not an
    /// attempt and is deliberately left intact when the picker dismisses.
    public func cancelConnectionAttempt() {
        guard state == .connecting, !mcpOperationInFlight else { return }
        transportGeneration &+= 1
        stopObservingState()
        let openingTransport = transport
        transport = nil
        state = .disconnected
        radioMode = .unknown
        Task {
            await openingTransport?.close()
        }
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
        state = .connecting

        let t = transportFactory(device)
        transport = t
        do {
            guard Self.canonicalBluetoothAddress(t.device.address)
                    == Self.canonicalBluetoothAddress(device.address) else {
                throw RadioConnectionQualificationError.transportIdentityMismatch(
                    selected: device.address,
                    opened: t.device.address
                )
            }
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
            try Task.checkCancellation()
            let provedMode = try await qualifyOpenedRadio(t)
            try Task.checkCancellation()
            guard attemptGeneration == transportGeneration else {
                log.info("Discarding transport qualified after background teardown")
                await t.close()
                return
            }

            // Do not expose the byte stream to the relay or publish connected
            // until either exact CAT identity or a complete MMDVM GetVersion
            // frame has proved the selected endpoint is the radio.
            radioMode = provedMode
            lastProbeErrorText = nil
            observeState(of: t)
            state = .connected
            // Remember this radio so `tryAutoConnect()` can find it on
            // the next launch. Captured unconditionally; the user's
            // `autoConnectRadio` toggle controls whether we act on it.
            rememberedRadioAddress = device.address
            rememberedRadioName = device.name
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

    /// Prove one freshly opened exact endpoint before publishing it.
    ///
    /// A complete MMDVM GetVersion reply positively identifies an already
    /// terminal-mode radio. Every CAT-like result, including silence, must
    /// then return the exact `ID TH-D75` response.
    private func qualifyOpenedRadio(_ openedTransport: RadioTransport) async throws -> RadioMode {
        isProbingMode = true
        defer { isProbingMode = false }
        let mode = try await RadioModeProber(transport: openedTransport).probe()
        switch mode {
        case .mmdvm:
            log.info("Connection qualified by complete MMDVM GetVersion framing")
            return .mmdvm
        case .cat:
            let expectedModel = mcpD75SchemaTarget().model
            let model = try await identifyModel(on: openedTransport)
            guard model == expectedModel else {
                throw RadioConnectionQualificationError.wrongModel(
                    expected: expectedModel,
                    actual: model
                )
            }
            lastResponseText = "Identify: \(model)"
            log.info("Connection qualified by exact CAT ID \(expectedModel, privacy: .public)")
            return .cat
        case .unknown:
            throw RadioConnectionQualificationError.modeNotProved
        case .unrecognized(let firstByte):
            throw RadioConnectionQualificationError.unrecognizedFraming(firstByte: firstByte)
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
        guard let device = availableDevices.first(where: {
            Self.canonicalBluetoothAddress($0.address)
                == Self.canonicalBluetoothAddress(address)
        }) else {
            log.info("Auto-connect: remembered radio \(address) not in paired list; skipping")
            return
        }
        log.info("Auto-connect: reconnecting to \(device.name) (\(address))")
        select(device)
        await connect()
        if case .failed(let message) = state {
            autoConnectFailureNotifier(message)
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

    /// Route DV Gateway to the connected transport's physical interface and enable
    /// Reflector Terminal Mode in one verified MCP programming session.
    /// Success is published only after the same exact device address reopens
    /// and returns a complete MMDVM GetVersion frame.
    public func enableReflectorTerminalMode() async {
        guard !mcpOperationInFlight else {
            log.warning("Ignoring overlapping MCP setup request")
            return
        }
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
        let interface = t.pcOutputInterface
        let settings = reflectorTerminalSettings(interface: interface)
        log.info(
            "MCP: binding Menu 985 to interface value \(settings.interfaceValue) and enabling Menu 650"
        )

        let session = McpSession(transport: t)
        guard await proveCatModeForMcp() else { return }
        quarantineTransportForMcp()
        do {
            // The session performs exact typed ID/FV qualification, reads
            // both pages before any write, and read-back verifies Menu 985
            // and Menu 650 before its one-shot programming-mode exit.
            mcpStatus = .running("Configuring Menu 985 and Menu 650…")
            try await session.enableReflectorTerminalMode(on: interface)

            await detachAfterMcpAttempt(t)
            state = .connecting
            mcpStatus = .running("Radio is rebooting; waiting for Terminal Mode…")
            log.info("MCP: settings verified; quarantining early CAT until MMDVM answers")
            try await awaitTerminalMode(on: t.device)
            mcpStatus = .succeeded
            log.info("MCP: same exact device positively answered MMDVM GetVersion")
        } catch {
            log.error("MCP: enable Reflector Terminal Mode failed: \(error)")
            let mustDetach = await session.requiresTransportDetach()
            if mustDetach {
                await detachAfterMcpAttempt(t)
            } else {
                await restoreObservationAfterSafeQualificationFailure(t)
            }
            mcpStatus = .failed(error.displayMessage)
        }
    }

    /// Reopen one exact device until the rebooted gateway application proves
    /// complete MMDVM framing. Silence, open failures, malformed 0xE0 prefixes,
    /// and early-boot CAT are all retryable transition observations.
    private func awaitTerminalMode(on device: BluetoothDevice) async throws {
        let deadline = ContinuousClock.now.advanced(by: terminalModeTransitionWindow)
        var lastObservation = "no probe completed"

        while ContinuousClock.now < deadline {
            try Task.checkCancellation()
            if terminalModePollDelayNs > 0 {
                try await Task.sleep(nanoseconds: terminalModePollDelayNs)
            }
            try Task.checkCancellation()
            guard ContinuousClock.now < deadline else { break }

            let candidate = transportFactory(device)
            guard Self.canonicalBluetoothAddress(candidate.device.address)
                    == Self.canonicalBluetoothAddress(device.address) else {
                await candidate.close()
                throw RadioConnectionQualificationError.transportIdentityMismatch(
                    selected: device.address,
                    opened: candidate.device.address
                )
            }

            do {
                try await candidate.open()
                guard case .connected = await candidate.state else {
                    throw RadioTransportError.openFailed(
                        reason: "transport closed before terminal-mode probing"
                    )
                }
                let mode = try await RadioModeProber(transport: candidate).probe()
                guard mode == .mmdvm else {
                    lastObservation = "radio answered \(String(describing: mode))"
                    log.info("Terminal transition not ready: \(lastObservation, privacy: .public)")
                    await candidate.close()
                    continue
                }

                transport = candidate
                radioMode = .mmdvm
                lastProbeErrorText = nil
                observeState(of: candidate)
                state = .connected
                rememberedRadioAddress = device.address
                rememberedRadioName = device.name
                return
            } catch is CancellationError {
                await candidate.close()
                throw CancellationError()
            } catch {
                lastObservation = error.displayMessage
                log.info("Terminal transition probe failed: \(lastObservation, privacy: .public)")
                await candidate.close()
            }
        }

        throw RadioConnectionQualificationError.terminalModeNotEngaged(
            window: terminalModeTransitionWindow,
            lastObservation: lastObservation
        )
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

    /// Read one typed CAT identity response from an already-open transport.
    private func identifyModel(on transport: RadioTransport) async throws -> String {
        try await transport.write(encodeCat(command: .identify))
        let deadline = ContinuousClock.now.advanced(by: .seconds(2))
        var buffer: [UInt8] = []
        while !buffer.contains(0x0D), buffer.count < 256 {
            guard let chunk = try await readChunkWithTimeout(
                transport: transport,
                maxBytes: 256 - buffer.count,
                deadline: deadline
            ) else {
                throw RadioConnectionQualificationError.noCatIdentityResponse
            }
            guard !chunk.isEmpty else {
                throw RadioConnectionQualificationError.transportClosed
            }
            buffer.append(contentsOf: chunk)
        }
        guard let carriageReturn = buffer.firstIndex(of: 0x0D) else {
            throw RadioConnectionQualificationError.invalidCatIdentityResponse
        }
        let response = parseCatLine(line: Array(buffer[..<carriageReturn]))
        guard case .identify(let model) = response else {
            throw RadioConnectionQualificationError.unexpectedCatResponse(
                String(describing: response)
            )
        }
        return model
    }

    private static func canonicalBluetoothAddress(_ address: String) -> String {
        address.lowercased()
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

private enum RadioConnectionQualificationError: Error, Sendable {
    case transportIdentityMismatch(selected: String, opened: String)
    case transportClosed
    case noCatIdentityResponse
    case invalidCatIdentityResponse
    case unexpectedCatResponse(String)
    case wrongModel(expected: String, actual: String)
    case modeNotProved
    case unrecognizedFraming(firstByte: UInt8)
    case terminalModeNotEngaged(window: Duration, lastObservation: String)
}

extension RadioConnectionQualificationError: LocalizedError {
    var errorDescription: String? {
        switch self {
        case .transportIdentityMismatch(let selected, let opened):
            return "The transport factory opened \(opened), not the selected exact address "
                + "\(selected). The connection was closed."
        case .transportClosed:
            return "The radio connection closed before identity was proved."
        case .noCatIdentityResponse:
            return "The selected device did not answer CAT ID TH-D75 or a complete MMDVM "
                + "GetVersion probe, so it was not accepted as the radio."
        case .invalidCatIdentityResponse:
            return "The selected device returned an invalid CAT identity line."
        case .unexpectedCatResponse(let response):
            return "The selected device returned \(response) instead of CAT identity."
        case .wrongModel(let expected, let actual):
            return "CAT identified \(actual), not exact model \(expected). The connection was closed."
        case .modeNotProved:
            return "The selected device's radio protocol could not be proved."
        case .unrecognizedFraming(let firstByte):
            return "The selected device returned unrecognized framing beginning with 0x"
                + String(format: "%02X", firstByte) + "."
        case .terminalModeNotEngaged(let window, let lastObservation):
            return "Menu 985 and Menu 650 were verified, but the same radio did not return a "
                + "complete MMDVM GetVersion frame within \(window): \(lastObservation)."
        }
    }
}
