// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import OSLog

private let azimuthRadioCoreLog = Logger(
    subsystem: "org.swiftraccoon.azimuth",
    category: "radio-core"
)

private let azimuthPlatformSupportsAutomaticCATRecovery: Bool = {
    #if os(macOS)
    true
    #else
    false
    #endif
}()

private func azimuthRadioErrorIdentity(_ error: Error) -> String {
    let bridged = error as NSError
    return "\(String(reflecting: type(of: error))) domain=\(bridged.domain) code=\(bridged.code)"
}

protocol AzimuthCATRecoveryOperation: AnyObject, Sendable {
    func cancel()
    func run() async throws -> DvGatewayRecoveryOutcome
}

extension DvGatewayRecoveryOperation: AzimuthCATRecoveryOperation {}

protocol AzimuthDvGatewayUsbRoutingOperation: AnyObject, Sendable {
    func cancel()
    func run() async throws -> DvGatewayUsbRoutingResult
}

extension DvGatewayUsbRoutingOperation: AzimuthDvGatewayUsbRoutingOperation {}

protocol AzimuthDvGatewayCatDisableOperation: AnyObject, Sendable {
    func cancel()
    func run() async throws -> DvGatewayCatDisableResult
}

protocol AzimuthAPRSCurrentModeRecoveryOperation: AnyObject, Sendable {
    func cancel()
    func run() async throws -> AprsCurrentModeRecoveryResult
}

private final class AzimuthAutomationDVGatewayCatDisableOperation:
    AzimuthDvGatewayCatDisableOperation,
    @unchecked Sendable
{
    private let core: any AutomationControllerProtocol
    private let expectedRadioSerialNumber: String

    init(
        core: any AutomationControllerProtocol,
        expectedRadioSerialNumber: String
    ) {
        self.core = core
        self.expectedRadioSerialNumber = expectedRadioSerialNumber
    }

    func cancel() {
        core.cancelDvGatewayDisable()
    }

    func run() async throws -> DvGatewayCatDisableResult {
        try await core.disableDvGateway(
            expectedRadioSerialNumber: expectedRadioSerialNumber
        )
    }
}

private final class AzimuthAutomationAPRSCurrentModeRecoveryOperation:
    AzimuthAPRSCurrentModeRecoveryOperation,
    @unchecked Sendable
{
    private let core: any AutomationControllerProtocol
    private let expectedRadioSerialNumber: String
    private let expectedKISSInterfaceRawValue: UInt8

    init(
        core: any AutomationControllerProtocol,
        expectedRadioSerialNumber: String,
        expectedKISSInterfaceRawValue: UInt8
    ) {
        self.core = core
        self.expectedRadioSerialNumber = expectedRadioSerialNumber
        self.expectedKISSInterfaceRawValue = expectedKISSInterfaceRawValue
    }

    func cancel() {
        core.cancelAprsCurrentModeRecovery()
    }

    func run() async throws -> AprsCurrentModeRecoveryResult {
        try await core.recoverAprsCurrentMode(
            expectedRadioSerialNumber: expectedRadioSerialNumber,
            expectedKissInterfaceRawValue: expectedKISSInterfaceRawValue
        )
    }
}

private struct ExpectedCATRadioIdentityMismatch: LocalizedError, Sendable {
    let expected: String
    let actual: String

    var errorDescription: String? {
        "The automation core proved CAT radio \(actual), but this connection was bound to radio \(expected). Azimuth closed the connection."
    }
}

private struct IFDSPUSBRetryObservation: Error, Sendable {}

private struct IFDSPUSBInputProofUnavailable: LocalizedError, Sendable {
    let expectedRadioSerialNumber: String

    var errorDescription: String? {
        "USB-C CAT proved radio \(expectedRadioSerialNumber), but the current USB enumeration did not provide the physical input identity required for IF-DSP."
    }
}

/// Live product controller. It owns one qualified automation core, one optimistic
/// settings snapshot, and one authenticated screen lease at a time.
@MainActor
final class AzimuthLiveRadioController: RadioControlling, APRSControlling, IFDSPModeControlling {
    typealias CoreConnector = @Sendable (ByteTransport) async throws -> any AutomationControllerProtocol
    typealias RadioModePreflight = @Sendable (
        any AzimuthRadioTransport
    ) async throws -> AzimuthRadioWireMode
    typealias USBMMDVMRecoveryFactory = @Sendable (
        _ expectedRadioSerialNumber: String,
        _ qualifiedBluetoothAddress: String?
    ) throws -> any AzimuthCATRecoveryOperation
    typealias BluetoothMMDVMUSBRoutingFactory = @Sendable (
        _ transport: ByteTransport
    ) throws -> any AzimuthDvGatewayUsbRoutingOperation
    typealias ConnectedCATDVGatewayDisableFactory = @Sendable (
        _ core: any AutomationControllerProtocol,
        _ expectedRadioSerialNumber: String
    ) throws -> any AzimuthDvGatewayCatDisableOperation
    typealias ConnectedCATAPRSCurrentModeRecoveryFactory = @Sendable (
        _ core: any AutomationControllerProtocol,
        _ expectedRadioSerialNumber: String,
        _ expectedKISSInterfaceRawValue: UInt8
    ) throws -> any AzimuthAPRSCurrentModeRecoveryOperation
    typealias BluetoothRecoveryAuthorization = @Sendable () async throws -> Void

    private(set) var currentState: RadioWorkspaceState
    let updates: AsyncStream<RadioWorkspaceState>
    private(set) var currentRadioSerialNumber: String?
    private(set) var currentIFDSPUSBInputProof: IFDSPUSBInputProof?
    private(set) var currentAPRSState: APRSOperationalState
    let aprsUpdates: AsyncStream<APRSOperationalState>
    private(set) var ifDSPModeState: IFDSPRadioModeState = .inactive

    private let transport: any AzimuthRadioTransport
    private let sameRadioBluetoothSelector: (any AzimuthSameRadioBluetoothSelecting)?
    private let sameRadioUSBSelector: (any AzimuthSameRadioUSBRefreshing)?
    private let bluetoothMmdvmUSBSelector: (any AzimuthBluetoothMMDVMUSBSelecting)?
    private let ifDSPUSBSelector: (any AzimuthIFDSPUSBSelecting)?
    private let coreTransport: AzimuthCoreByteTransport
    private let schema: AzimuthCoreSettingSchema
    private let connectCore: CoreConnector
    private let prepareRadioForAutomation: RadioModePreflight
    private let proveRadioCATWithoutPacketModeRecovery: RadioModePreflight
    private let authorizeBluetoothRecovery: BluetoothRecoveryAuthorization
    private let makeUSBMMDVMRecovery: USBMMDVMRecoveryFactory
    private let makeBluetoothMMDVMUSBRouting: BluetoothMMDVMUSBRoutingFactory
    private let makeConnectedCATDVGatewayDisable: ConnectedCATDVGatewayDisableFactory
    private let makeConnectedCATAPRSCurrentModeRecovery:
        ConnectedCATAPRSCurrentModeRecoveryFactory
    private let supportsAutomaticCATRecovery: Bool
    private let catRecoveryWindow: Duration
    private let catRecoveryPollInterval: Duration
    private let updateContinuation: AsyncStream<RadioWorkspaceState>.Continuation
    private let aprsUpdateContinuation: AsyncStream<APRSOperationalState>.Continuation

    private var core: (any AutomationControllerProtocol)?
    private var settingSnapshotID: UInt64?
    private var pollingTask: Task<Void, Never>?
    private var aprsPollingTask: Task<Void, Never>?
    private var captureSlot: CaptureSlot?
    private var nextCaptureID: UInt64 = 0
    private var connectionSlot: ConnectionSlot?
    private var nextConnectionID: UInt64 = 0
    private var preflightSlot: RadioModePreflightSlot?
    private var nextPreflightID: UInt64 = 0
    private var disconnectInProgress = false
    private var sessionEpoch: UInt64 = 0
    private var screenPauseDepth = 0
    private var exclusiveOperation: String?
    private var consecutiveScreenFailures = 0
    private var usbMmdvmRecoveryPending = false
    private var usbMmdvmExpectedRadioSerialNumber: String?
    private var usbMmdvmRecoverySlot: USBMMDVMRecoverySlot?
    private var nextUSBMMDVMRecoveryID: UInt64 = 0
    private var bluetoothMmdvmRoutingSlot: BluetoothMMDVMRoutingSlot?
    private var nextBluetoothMMDVMRoutingID: UInt64 = 0
    private var bluetoothMmdvmUSBHandoffPending = false
    private var bluetoothMmdvmUSBHandoffAvailable = false
    private var connectedCatCoreCloseSlot: ConnectedCATCoreCloseSlot?
    private var nextConnectedCATCoreCloseID: UInt64 = 0
    private var connectedCatDisableSlot: ConnectedCATDisableSlot?
    private var nextConnectedCATDisableID: UInt64 = 0
    private var connectedCatAPRSRecoverySlot: ConnectedCATAPRSRecoverySlot?
    private var nextConnectedCATAPRSRecoveryID: UInt64 = 0
    private var ifDSPUSBHandoffGeneration: UInt64 = 0
    private var aprsRecoveryGeneration: UInt64 = 0
    private var pendingAPRSDVGatewayRecovery: APRSDVGatewayRecoveryProof?

    var automaticCATRecoveryAvailable: Bool {
        supportsAutomaticCATRecovery && usbMmdvmExpectedRadioSerialNumber != nil
    }

    var bluetoothCATFallbackAvailable: Bool {
        sameRadioBluetoothSelector != nil && usbMmdvmExpectedRadioSerialNumber != nil
    }

    var usbCATFallbackAvailable: Bool {
        bluetoothMmdvmUSBHandoffPending && bluetoothMmdvmUSBHandoffAvailable
    }

    var automaticBluetoothCATRoutingAvailable: Bool {
        bluetoothMmdvmUSBHandoffPending && bluetoothMmdvmUSBHandoffAvailable
    }

    var automaticIFDSPDVGatewayRecoveryAvailable: Bool {
        supportsAutomaticCATRecovery
            && transport.device.connectionKind == .bluetooth
            && currentState.connection.isConnected
            && ifDSPUSBSelector != nil
            && core != nil
            && currentRadioSerialNumber != nil
            && !ifDSPModeState.reservesRadioState
            && !currentAPRSState.status.phase.ownsSerialLink
            && connectedCatCoreCloseSlot == nil
            && connectedCatDisableSlot == nil
            && !disconnectInProgress
    }

    var automaticAPRSDVGatewayRecoveryAvailable: Bool {
        guard let proof = pendingAPRSDVGatewayRecovery else { return false }
        return supportsAutomaticCATRecovery
            && currentState.connection.isConnected
            && core != nil
            && currentRadioSerialNumber == proof.radioSerialNumber
            && sessionEpoch == proof.sessionEpoch
            && transport.device == proof.device
            && !ifDSPModeState.reservesRadioState
            && !currentAPRSState.status.phase.ownsSerialLink
            && connectedCatCoreCloseSlot == nil
            && connectedCatDisableSlot == nil
            && connectedCatAPRSRecoverySlot == nil
            && !disconnectInProgress
            && (proof.device.connectionKind == .usb
                ? sameRadioUSBSelector != nil
                : sameRadioBluetoothSelector != nil)
    }

    private struct CaptureSlot {
        let id: UInt64
        let task: Task<RemoteScreenFrame, Error>
    }

    private struct ConnectionSlot {
        let id: UInt64
        let task: Task<Void, Error>
    }

    private struct RadioModePreflightSlot {
        let id: UInt64
        let task: Task<AzimuthRadioWireMode, Error>
    }

    private struct USBMMDVMRecoverySlot {
        let id: UInt64
        let operation: any AzimuthCATRecoveryOperation
        let task: Task<DvGatewayRecoveryOutcome, Error>
    }

    private struct BluetoothMMDVMRoutingSlot {
        let id: UInt64
        let operation: any AzimuthDvGatewayUsbRoutingOperation
        let task: Task<DvGatewayUsbRoutingResult, Error>
    }

    private struct ConnectedCATDisableSlot {
        let id: UInt64
        let operation: any AzimuthDvGatewayCatDisableOperation
        let task: Task<DvGatewayCatDisableResult, Error>
    }

    private struct ConnectedCATAPRSRecoverySlot {
        let id: UInt64
        let operation: any AzimuthAPRSCurrentModeRecoveryOperation
        let task: Task<AprsCurrentModeRecoveryResult, Error>
    }

    private struct ConnectedCATCoreCloseSlot {
        let id: UInt64
        let task: Task<Void, Error>
    }

    private struct APRSDVGatewayRecoveryProof: Equatable, Sendable {
        let configuration: APRSSessionConfiguration
        let kissInterfaceRawValue: UInt8
        let radioSerialNumber: String
        let device: AzimuthRadioDevice
        let sessionEpoch: UInt64
    }

    init(
        transport: any AzimuthRadioTransport,
        records: [SettingRecord] = settingCatalog(),
        connectCore: @escaping CoreConnector = { transport in
            try await connectAutomation(transport: transport)
        },
        prepareRadioForAutomation: @escaping RadioModePreflight = { transport in
            try await AzimuthRadioModePreflight(transport: transport).prepareForAutomation()
        },
        proveRadioCATWithoutPacketModeRecovery:
            @escaping RadioModePreflight = { transport in
                try await AzimuthRadioModePreflight(transport: transport)
                    .proveCATWithoutPacketModeRecovery()
            },
        authorizeBluetoothRecovery: @escaping BluetoothRecoveryAuthorization = {},
        recoverUSBMMDVM: @escaping USBMMDVMRecoveryFactory = {
            expectedRadioSerialNumber,
            qualifiedBluetoothAddress in
            try DvGatewayRecoveryOperation(
                expectedRadioSerialNumber: expectedRadioSerialNumber,
                bluetoothSelector: qualifiedBluetoothAddress.map {
                    .exactAddress(address: $0)
                }
            )
        },
        routeBluetoothMMDVMToUSB: @escaping BluetoothMMDVMUSBRoutingFactory = {
            transport in
            DvGatewayUsbRoutingOperation(transport: transport)
        },
        disableDVGatewayOverConnectedCAT:
            @escaping ConnectedCATDVGatewayDisableFactory = {
                core,
                expectedRadioSerialNumber in
                AzimuthAutomationDVGatewayCatDisableOperation(
                    core: core,
                    expectedRadioSerialNumber: expectedRadioSerialNumber
                )
            },
        recoverAPRSCurrentModeOverConnectedCAT:
            @escaping ConnectedCATAPRSCurrentModeRecoveryFactory = {
                core,
                expectedRadioSerialNumber,
                expectedKISSInterfaceRawValue in
                AzimuthAutomationAPRSCurrentModeRecoveryOperation(
                    core: core,
                    expectedRadioSerialNumber: expectedRadioSerialNumber,
                    expectedKISSInterfaceRawValue: expectedKISSInterfaceRawValue
                )
            },
        automaticCATRecoveryAvailable: Bool = azimuthPlatformSupportsAutomaticCATRecovery,
        catRecoveryWindow: Duration = .seconds(90),
        catRecoveryPollInterval: Duration = .seconds(3)
    ) throws {
        self.transport = transport
        sameRadioBluetoothSelector =
            transport as? any AzimuthSameRadioBluetoothSelecting
        sameRadioUSBSelector =
            transport as? any AzimuthSameRadioUSBRefreshing
        bluetoothMmdvmUSBSelector =
            transport as? any AzimuthBluetoothMMDVMUSBSelecting
        ifDSPUSBSelector = transport as? any AzimuthIFDSPUSBSelecting
        coreTransport = AzimuthCoreByteTransport(radioTransport: transport)
        schema = try AzimuthCoreSettingSchema(records: records)
        self.connectCore = connectCore
        self.prepareRadioForAutomation = prepareRadioForAutomation
        self.proveRadioCATWithoutPacketModeRecovery =
            proveRadioCATWithoutPacketModeRecovery
        self.authorizeBluetoothRecovery = authorizeBluetoothRecovery
        makeUSBMMDVMRecovery = recoverUSBMMDVM
        makeBluetoothMMDVMUSBRouting = routeBluetoothMMDVMToUSB
        makeConnectedCATDVGatewayDisable = disableDVGatewayOverConnectedCAT
        makeConnectedCATAPRSCurrentModeRecovery =
            recoverAPRSCurrentModeOverConnectedCAT
        supportsAutomaticCATRecovery = automaticCATRecoveryAvailable
        self.catRecoveryWindow = catRecoveryWindow
        self.catRecoveryPollInterval = catRecoveryPollInterval
        currentState = .disconnected
        currentAPRSState = .unavailable(
            "Connect the TH-D75 before starting an APRS KISS session."
        )
        let pair = AsyncStream<RadioWorkspaceState>.makeStream(
            bufferingPolicy: .bufferingNewest(16)
        )
        updates = pair.stream
        updateContinuation = pair.continuation
        let aprsPair = AsyncStream<APRSOperationalState>.makeStream(
            bufferingPolicy: .bufferingNewest(32)
        )
        aprsUpdates = aprsPair.stream
        aprsUpdateContinuation = aprsPair.continuation
        updateContinuation.yield(currentState)
        aprsUpdateContinuation.yield(currentAPRSState)
    }

    func connect() async throws {
        try await connect(
            expectedRadioSerialNumber: nil,
            allowsPacketModeRecovery: true
        )
    }

    private func connect(
        expectedRadioSerialNumber: String?,
        allowsPacketModeRecovery: Bool
    ) async throws {
        guard !disconnectInProgress else {
            throw RadioControllerError.capabilityUnavailable(
                "Wait for the current disconnect to finish."
            )
        }
        guard !currentState.connection.isConnected else { return }
        guard connectionSlot == nil else {
            throw RadioControllerError.capabilityUnavailable(
                "Wait for the current connection attempt to finish."
            )
        }

        let connectionID = nextConnectionID
        nextConnectionID &+= 1
        let task = Task { @MainActor [weak self] in
            guard let self else { throw CancellationError() }
            try await self.runConnectionAttempt(
                expectedRadioSerialNumber: expectedRadioSerialNumber,
                allowsPacketModeRecovery: allowsPacketModeRecovery
            )
        }
        connectionSlot = ConnectionSlot(id: connectionID, task: task)
        defer { clearConnectionSlot(id: connectionID) }

        try await withTaskCancellationHandler {
            try await task.value
        } onCancel: {
            task.cancel()
        }
    }

    private func runRadioModePreflight(
        allowsPacketModeRecovery: Bool
    ) async throws -> AzimuthRadioWireMode {
        guard preflightSlot == nil else {
            throw RadioControllerError.capabilityUnavailable(
                "Wait for the current radio mode check to finish."
            )
        }

        let preflightID = nextPreflightID
        nextPreflightID &+= 1
        let task = Task { @MainActor [weak self] in
            guard let self else { throw CancellationError() }
            if allowsPacketModeRecovery {
                return try await self.prepareRadioForAutomation(self.transport)
            }
            return try await self.proveRadioCATWithoutPacketModeRecovery(
                self.transport
            )
        }
        preflightSlot = RadioModePreflightSlot(id: preflightID, task: task)
        defer { clearPreflightSlot(id: preflightID) }

        return try await withTaskCancellationHandler {
            try await task.value
        } onCancel: {
            task.cancel()
        }
    }

    private func runConnectionAttempt(
        expectedRadioSerialNumber: String?,
        allowsPacketModeRecovery: Bool
    ) async throws {
        guard !currentState.connection.isConnected else { return }
        let connectionDescription = transport.device.connection
        let connectionKind = transport.device.connectionKind
        azimuthRadioCoreLog.notice("[Azimuth Radio] Connection started")
        try beginExclusive("connect")
        usbMmdvmRecoveryPending = false
        usbMmdvmExpectedRadioSerialNumber = nil
        bluetoothMmdvmUSBHandoffPending = false
        bluetoothMmdvmUSBHandoffAvailable = false
        pendingAPRSDVGatewayRecovery = nil
        currentRadioSerialNumber = nil
        currentIFDSPUSBInputProof = nil
        sessionEpoch &+= 1
        let epoch = sessionEpoch
        stopScreenPolling()
        stopAPRSPolling()
        ifDSPModeState = .inactive
        settingSnapshotID = nil
        publishAPRS(
            .unavailable(
                "Finishing the \(connectionDescription) radio and automation connection first."
            )
        )
        publish(
            RadioWorkspaceState(
                connection: .connecting,
                capabilities: RadioCapabilities(
                    screenStreaming: .preparing,
                    frontPanelControl: .preparing,
                    settingRead: .preparing,
                    settingWrite: .preparing
                ),
                screenFrame: nil,
                telemetry: .unavailable,
                settingValues: [:],
                lastScreenError: nil
            )
        )

        var establishedCore: (any AutomationControllerProtocol)?
        var connectionStage = "\(connectionDescription) transport open"
        defer {
            if epoch == sessionEpoch {
                endExclusive("connect")
            }
        }
        do {
            azimuthRadioCoreLog.info(
                "[Azimuth Radio] \(connectionDescription, privacy: .public) transport open started"
            )
            try await transport.open()
            try requireEpoch(epoch)
            try await requireExpectedRadioIdentity(
                expectedRadioSerialNumber,
                stage: "the initial \(connectionDescription) open"
            )
            azimuthRadioCoreLog.notice(
                "[Azimuth Radio] \(connectionDescription, privacy: .public) transport open succeeded"
            )

            connectionStage = "radio mode preflight"
            var wireMode = try await runRadioModePreflight(
                allowsPacketModeRecovery: allowsPacketModeRecovery
            )
            try requireEpoch(epoch)
            if wireMode == .unresponsive, allowsPacketModeRecovery {
                connectionStage = "\(connectionDescription) session recovery"
                azimuthRadioCoreLog.notice(
                    "[Azimuth Radio] \(connectionDescription, privacy: .public) session was unresponsive; reopening once"
                )
                await transport.close()
                try requireEpoch(epoch)
                try await transport.open()
                try requireEpoch(epoch)
                try await requireExpectedRadioIdentity(
                    expectedRadioSerialNumber,
                    stage: "the \(connectionDescription) recovery reopen"
                )
                azimuthRadioCoreLog.notice(
                    "[Azimuth Radio] \(connectionDescription, privacy: .public) session reopened; retrying mode probe"
                )
                connectionStage = "radio mode preflight retry"
                wireMode = try await runRadioModePreflight(
                    allowsPacketModeRecovery: allowsPacketModeRecovery
                )
                try requireEpoch(epoch)
            }
            if allowsPacketModeRecovery, wireMode == .mmdvm {
                if connectionKind == .usb {
                    usbMmdvmExpectedRadioSerialNumber = await transport.hardwareSerialNumber
                    throw AzimuthRadioModePreflightError.usbMmdvmMode
                }
                throw AzimuthRadioModePreflightError.bluetoothMmdvmMode
            }
            guard wireMode == .cat else {
                if !allowsPacketModeRecovery {
                    throw RadioControllerError.operationFailed(
                        "The selected endpoint opened, but ordinary CAT was not ready during this recovery poll. Azimuth sent no packet-mode recovery command and will wait before probing the same endpoint again."
                    )
                }
                if connectionKind == .usb {
                    throw AzimuthRadioModePreflightError.cdcUnresponsive
                }
                throw RadioControllerError.operationFailed(
                    "Azimuth reopened the TH-D75 Bluetooth control link once and repeated the packet-mode recovery sequence, but the radio did not answer with an isolated CAT identity or a valid MMDVM response. Confirm the radio's Bluetooth connection is enabled, then reconnect."
                )
            }

            connectionStage = "AzimuthCore connectAutomation"
            azimuthRadioCoreLog.notice("[Azimuth Core] connectAutomation started")
            let connectedCore: any AutomationControllerProtocol
            do {
                connectedCore = try await connectCore(coreTransport)
            } catch {
                azimuthRadioCoreLog.error(
                    "[Azimuth Core] connectAutomation failed type=\(azimuthRadioErrorIdentity(error), privacy: .public) detail=\(error.localizedDescription, privacy: .private)"
                )
                throw error
            }
            establishedCore = connectedCore
            try requireEpoch(epoch)
            let abi = connectedCore.abi()
            guard !abi.radioSerialNumber.isEmpty else {
                throw RadioControllerError.operationFailed(
                    "The connected automation core did not retain the CAT AE radio identity."
                )
            }
            if let expectedRadioSerialNumber,
               abi.radioSerialNumber != expectedRadioSerialNumber {
                throw ExpectedCATRadioIdentityMismatch(
                    expected: expectedRadioSerialNumber,
                    actual: abi.radioSerialNumber
                )
            }
            try requireEpoch(epoch)
            core = connectedCore
            currentRadioSerialNumber = abi.radioSerialNumber
            installAPRSSnapshot(
                connectedCore.aprsSnapshot(afterSequence: nil),
                retainingHistory: false
            )
            azimuthRadioCoreLog.notice(
                "[Azimuth Core] connectAutomation succeeded (ABI \(abi.version, privacy: .public))"
            )
            var connectedState = currentState
            connectedState.connection = .connected(
                device: transport.device.name,
                transport: transport.device.connection
            )
            connectedState.telemetry.firmware = "V1.03.AZM"
            connectedState.telemetry.operatingMode = "Automation ABI \(abi.version)"
            publish(connectedState)

            connectionStage = "initial screen refresh"
            azimuthRadioCoreLog.notice("[Azimuth Core] Initial screen refresh started")
            pauseScreenStream()
            defer { resumeScreenStream() }
            do {
                let frame = try await captureFresh(core: connectedCore, epoch: epoch)
                azimuthRadioCoreLog.notice(
                    "[Azimuth Core] Initial screen refresh succeeded (\(frame.width, privacy: .public)x\(frame.height, privacy: .public), \(frame.rgba8888.count, privacy: .public) RGBA bytes)"
                )
            } catch {
                azimuthRadioCoreLog.error(
                    "[Azimuth Core] Initial screen refresh failed type=\(azimuthRadioErrorIdentity(error), privacy: .public) detail=\(error.localizedDescription, privacy: .private)"
                )
                throw error
            }

            connectionStage = "ready-state publication"
            try requireEpoch(epoch)

            // Bind audio to the currently open USB enumeration while retaining
            // CAT `AE` as the authoritative radio identity.
            await refreshCurrentIFDSPUSBInputProof(
                core: connectedCore,
                epoch: epoch
            )

            var ready = currentState
            ready.capabilities = RadioCapabilities(
                screenStreaming: .available,
                frontPanelControl: .available,
                settingRead: .available,
                settingWrite: .unavailable(
                    reason: "Read the radio settings before writing."
                )
            )
            publish(ready)
            startScreenPolling(epoch: epoch)
            azimuthRadioCoreLog.notice("[Azimuth Radio] Connection ready")
        } catch {
            let detectedUSBMMDVM =
                (error as? AzimuthRadioModePreflightError) == .usbMmdvmMode
            let detectedBluetoothMMDVM =
                (error as? AzimuthRadioModePreflightError) == .bluetoothMmdvmMode
            azimuthRadioCoreLog.error(
                "[Azimuth Radio] Connection failed during \(connectionStage, privacy: .public) type=\(azimuthRadioErrorIdentity(error), privacy: .public) detail=\(error.localizedDescription, privacy: .private)"
            )
            if let establishedCore,
               epoch == sessionEpoch || core !== establishedCore {
                try? await establishedCore.close()
            }
            guard epoch == sessionEpoch else {
                throw CancellationError()
            }
            await transport.close()
            core = nil
            currentRadioSerialNumber = nil
            currentIFDSPUSBInputProof = nil
            settingSnapshotID = nil
            ifDSPModeState = .inactive
            usbMmdvmRecoveryPending = detectedUSBMMDVM
            if !detectedUSBMMDVM {
                usbMmdvmExpectedRadioSerialNumber = nil
            }
            bluetoothMmdvmUSBHandoffPending = detectedBluetoothMMDVM
            if detectedBluetoothMMDVM {
                bluetoothMmdvmUSBHandoffAvailable =
                    (try? await bluetoothMmdvmUSBSelector?
                        .hasSoleVerifiedUSBEndpoint()) == true
            } else {
                bluetoothMmdvmUSBHandoffAvailable = false
            }
            var failed = RadioWorkspaceState.disconnected
            failed.connection = .failed(message: Self.describe(error))
            publish(failed)
            publishAPRSUnavailable(
                "APRS is unavailable because the radio connection failed."
            )
            if error is CancellationError { throw error }
            if detectedUSBMMDVM {
                throw RadioControllerError.usbMmdvmMode
            }
            if detectedBluetoothMMDVM {
                throw RadioControllerError.bluetoothMmdvmMode
            }
            if let identityMismatch = error as? ExpectedCATRadioIdentityMismatch {
                throw identityMismatch
            }
            throw RadioControllerError.operationFailed(Self.describe(error))
        }
    }

    /// Switch a Bluetooth MMDVM connection to the sole exact USB-C endpoint.
    ///
    /// This path is non-destructive. It changes only the host-side transport
    /// selection, then runs the ordinary CAT model and serial qualification.
    func connectViaUSBFromBluetoothMMDVM() async throws {
        guard bluetoothMmdvmUSBHandoffPending,
              bluetoothMmdvmUSBHandoffAvailable,
              case .failed = currentState.connection,
              let bluetoothMmdvmUSBSelector else {
            throw RadioControllerError.capabilityUnavailable(
                "USB-C CAT handoff requires a validated Bluetooth MMDVM response and exactly one verified TH-D75 USB-C endpoint."
            )
        }

        let authorizedEpoch = sessionEpoch
        await transport.close()
        do {
            try requireEpoch(authorizedEpoch)
            try await bluetoothMmdvmUSBSelector.selectSoleUSBForBluetoothMMDVM()
            try requireEpoch(authorizedEpoch)
            bluetoothMmdvmUSBHandoffPending = false
            bluetoothMmdvmUSBHandoffAvailable = false
            try await connect(
                expectedRadioSerialNumber: nil,
                allowsPacketModeRecovery: true
            )
        } catch {
            if error is CancellationError || disconnectInProgress {
                throw CancellationError()
            }
            guard sessionEpoch == authorizedEpoch else { throw error }
            bluetoothMmdvmUSBHandoffPending = true
            bluetoothMmdvmUSBHandoffAvailable =
                (try? await bluetoothMmdvmUSBSelector
                    .hasSoleVerifiedUSBEndpoint()) == true
            throw error
        }
    }

    /// Use the sole exact USB-C CAT endpoint to route Reflector Terminal traffic
    /// to USB-C, then reopen the originally selected Bluetooth address and prove
    /// it belongs to the radio identified by CAT over that USB endpoint.
    ///
    /// The caller owns the explicit mutation consent. Nothing is written until
    /// exact USB endpoint, CAT mode, CAT serial, model, firmware, and MCP schema
    /// have all been proved. The Bluetooth address is never replaced by a name
    /// match or a different paired device.
    func routeDVGatewayToUSBCAndReconnectBluetooth() async throws {
        guard bluetoothMmdvmUSBHandoffPending,
              bluetoothMmdvmUSBHandoffAvailable,
              case .failed = currentState.connection,
              let bluetoothMmdvmUSBSelector else {
            throw RadioControllerError.capabilityUnavailable(
                "Automatic Bluetooth CAT routing requires a validated Bluetooth MMDVM response and exactly one verified TH-D75 USB-C endpoint connected to this Mac."
            )
        }

        let operation = "DV Gateway routing to USB-C"
        try beginExclusive(operation)
        bluetoothMmdvmUSBHandoffPending = false
        bluetoothMmdvmUSBHandoffAvailable = false
        sessionEpoch &+= 1
        let epoch = sessionEpoch
        stopScreenPolling()
        stopAPRSPolling()
        captureSlot?.task.cancel()
        captureSlot = nil
        settingSnapshotID = nil
        core = nil
        currentRadioSerialNumber = nil
        currentIFDSPUSBInputProof = nil
        ifDSPModeState = .inactive
        publish(
            RadioWorkspaceState(
                connection: .connecting,
                capabilities: RadioCapabilities(
                    screenStreaming: .preparing,
                    frontPanelControl: .preparing,
                    settingRead: .preparing,
                    settingWrite: .preparing
                ),
                screenFrame: nil,
                telemetry: .unavailable,
                settingValues: [:],
                lastScreenError: nil
            )
        )
        publishAPRS(
            .unavailable(
                "Routing DV Gateway to USB-C and waiting for Bluetooth CAT control."
            )
        )

        var expectedRadioSerialNumber: String?
        var routingOperationStarted = false
        var routingOutcome: DvGatewayUsbRoutingOutcome?
        do {
            await transport.close()
            try requireEpoch(epoch)
            try await bluetoothMmdvmUSBSelector.selectSoleUSBForBluetoothMMDVM()
            try requireEpoch(epoch)

            try await transport.open()
            try requireEpoch(epoch)
            let usbMode = try await runRadioModePreflight(
                allowsPacketModeRecovery: true
            )
            try requireEpoch(epoch)
            guard usbMode == .cat else {
                throw RadioControllerError.operationFailed(
                    "The alternate USB-C endpoint did not prove CAT mode, so Azimuth did not change Menu 985 or Menu 650."
                )
            }

            let nativeOperation = try makeBluetoothMMDVMUSBRouting(coreTransport)
            let routingID = nextBluetoothMMDVMRoutingID
            nextBluetoothMMDVMRoutingID &+= 1
            let routingTask = Task { try await nativeOperation.run() }
            bluetoothMmdvmRoutingSlot = BluetoothMMDVMRoutingSlot(
                id: routingID,
                operation: nativeOperation,
                task: routingTask
            )
            routingOperationStarted = true
            let routingResult: DvGatewayUsbRoutingResult
            do {
                routingResult = try await withTaskCancellationHandler {
                    try await routingTask.value
                } onCancel: {
                    nativeOperation.cancel()
                }
            } catch {
                clearBluetoothMMDVMRoutingSlot(id: routingID)
                if case DvGatewayUsbRoutingError.Cancelled = error {
                    throw CancellationError()
                }
                throw error
            }
            clearBluetoothMMDVMRoutingSlot(id: routingID)
            let outcome = routingResult.outcome
            let provedRadioSerialNumber = routingResult.radioSerialNumber
            expectedRadioSerialNumber = provedRadioSerialNumber
            routingOutcome = outcome
            switch outcome {
            case .changedRadioRebooting:
                azimuthRadioCoreLog.notice(
                    "[Azimuth Radio] Menu 985 routed DV Gateway to USB-C; waiting for Bluetooth CAT after restart"
                )
            case .alreadyRouted:
                azimuthRadioCoreLog.notice(
                    "[Azimuth Radio] Menu 985 already routed DV Gateway to USB-C; proving Bluetooth CAT"
                )
            }
            if Task.isCancelled || epoch != sessionEpoch {
                throw Self.completedBluetoothRoutingInterruptedError(outcome)
            }

            await transport.close()
            try requireEpoch(epoch)
            try await bluetoothMmdvmUSBSelector
                .selectOriginalBluetoothAfterUSBRouting(
                    expectedSerialNumber: provedRadioSerialNumber
                )
            try requireEpoch(epoch)
            do {
                try await waitForBluetoothCATRecovery(
                    epoch: epoch,
                    outcome: outcome,
                    expectedRadioSerialNumber: provedRadioSerialNumber
                )
            } catch is CancellationError {
                throw Self.completedBluetoothRoutingInterruptedError(outcome)
            }
        } catch {
            if epoch == sessionEpoch {
                await transport.close()
                let canRetryWithoutInspectingRadio = routingOutcome == nil
                    && (!routingOperationStarted
                        || Self.isPreMutationBluetoothRoutingResult(error))
                if canRetryWithoutInspectingRadio {
                    try? await bluetoothMmdvmUSBSelector
                        .restoreOriginalBluetoothAfterUSBRoutingFailure()
                    bluetoothMmdvmUSBHandoffPending = true
                    bluetoothMmdvmUSBHandoffAvailable =
                        (try? await bluetoothMmdvmUSBSelector
                            .hasSoleVerifiedUSBEndpoint()) == true
                }
                endExclusive(operation)
                var failed = RadioWorkspaceState.disconnected
                failed.connection = .failed(message: Self.describe(error))
                publish(failed)
                publishAPRSUnavailable(
                    "APRS is unavailable because DV Gateway routing failed."
                )
            }
            if error is CancellationError { throw error }
            throw RadioControllerError.operationFailed(Self.describe(error))
        }

        endExclusive(operation)
        guard let expectedRadioSerialNumber else {
            throw RadioControllerError.operationFailed(
                "DV Gateway routing completed without retaining the USB radio identity."
            )
        }
        try await connect(
            expectedRadioSerialNumber: expectedRadioSerialNumber,
            allowsPacketModeRecovery: true
        )
    }

    /// Inspect and, if needed, turn Menu 650 off over the currently approved
    /// Bluetooth CAT endpoint, then move control to the retained USB-C endpoint.
    ///
    /// The caller owns the explicit persistent-setting consent. Merely
    /// attempting IF-DSP never enters the persistent mutation lifecycle. A sole
    /// qualified USB endpoint is retained before the existing automation owner
    /// is closed. USB descriptor serials are not identity; the post-reboot CAT
    /// core must prove the same `AE` serial before USB control is published.
    func disableDVGatewayAndReconnectForIFDSP() async throws {
        guard automaticIFDSPDVGatewayRecoveryAvailable,
              let ifDSPUSBSelector,
              let closingCore = core,
              let approvedRadioSerialNumber = currentRadioSerialNumber else {
            throw RadioControllerError.capabilityUnavailable(
                "Automatic IF-DSP recovery requires an approved Bluetooth CAT session and an attached TH-D75 USB-C endpoint."
            )
        }

        let retainedUSBAvailable: Bool
        do {
            retainedUSBAvailable = try await ifDSPUSBSelector
                .retainSoleIFDSPUSBEndpoint()
        } catch {
            throw RadioControllerError.operationFailed(
                "Azimuth could not retain the attached TH-D75 USB-C endpoint before inspecting Menu 650. \(Self.describe(error)) No radio setting was changed."
            )
        }
        do {
            try Task.checkCancellation()
        } catch {
            await ifDSPUSBSelector.finishRetainedIFDSPUSBHandoff()
            throw error
        }
        guard retainedUSBAvailable else {
            throw RadioControllerError.capabilityUnavailable(
                "Automatic IF-DSP recovery needs exactly one attached, qualified TH-D75 USB-C endpoint. Connect the radio to this Mac, refresh Connections, and retry. No radio setting was changed."
            )
        }
        guard automaticIFDSPDVGatewayRecoveryAvailable,
              core === closingCore,
              currentRadioSerialNumber == approvedRadioSerialNumber else {
            await ifDSPUSBSelector.finishRetainedIFDSPUSBHandoff()
            throw CancellationError()
        }

        // Construct the typed actor command before relinquishing the healthy
        // connected state. A factory/validation failure performs no radio I/O,
        // so it must not orphan or tear down the authenticated CAT owner.
        let nativeOperation: any AzimuthDvGatewayCatDisableOperation
        do {
            nativeOperation = try makeConnectedCATDVGatewayDisable(
                closingCore,
                approvedRadioSerialNumber
            )
        } catch {
            await ifDSPUSBSelector.finishRetainedIFDSPUSBHandoff()
            throw RadioControllerError.operationFailed(Self.describe(error))
        }

        let operation = "IF-DSP DV Gateway recovery"
        do {
            try beginExclusive(operation)
        } catch {
            await ifDSPUSBSelector.finishRetainedIFDSPUSBHandoff()
            throw error
        }
        ifDSPUSBHandoffGeneration &+= 1
        let handoffGeneration = ifDSPUSBHandoffGeneration
        sessionEpoch &+= 1
        let epoch = sessionEpoch
        stopScreenPolling()
        stopAPRSPolling()
        captureSlot?.task.cancel()
        captureSlot = nil
        settingSnapshotID = nil
        core = nil
        currentRadioSerialNumber = nil
        currentIFDSPUSBInputProof = nil
        ifDSPModeState = .inactive
        publish(
            RadioWorkspaceState(
                connection: .connecting,
                capabilities: RadioCapabilities(
                    screenStreaming: .preparing,
                    frontPanelControl: .preparing,
                    settingRead: .preparing,
                    settingWrite: .preparing
                ),
                screenFrame: nil,
                telemetry: .unavailable,
                settingValues: [:],
                lastScreenError: nil
            )
        )
        publishAPRS(
            .unavailable(
                "Inspecting Menu 650 over approved Bluetooth CAT, then waiting for same-radio USB-C CAT."
            )
        )

        var completedOutcome: DvGatewayRecoveryOutcome?
        var expectedRadioSerialNumber: String?
        do {
            let disableID = nextConnectedCATDisableID
            nextConnectedCATDisableID &+= 1
            // This uncancelled worker owns both the approved operation and the
            // old actor's terminal shutdown. The operation may consume the
            // actor itself; in that case `close()` returns ControllerClosed,
            // which still proves there is no live receiver left on the byte
            // stream. Pre-dispatch failures instead close and join that actor
            // here before any Swift transport close or state publication.
            let disableTask = Task {
                do {
                    let result = try await nativeOperation.run()
                    _ = try? await closingCore.close()
                    return result
                } catch {
                    _ = try? await closingCore.close()
                    throw error
                }
            }
            connectedCatDisableSlot = ConnectedCATDisableSlot(
                id: disableID,
                operation: nativeOperation,
                task: disableTask
            )
            let result: DvGatewayCatDisableResult
            do {
                result = try await withTaskCancellationHandler {
                    try await disableTask.value
                } onCancel: {
                    nativeOperation.cancel()
                }
            } catch {
                clearConnectedCATDisableSlot(id: disableID)
                if case DvGatewayCatDisableError.Cancelled = error {
                    throw CancellationError()
                }
                throw error
            }
            clearConnectedCATDisableSlot(id: disableID)
            completedOutcome = result.outcome
            guard result.radioSerialNumber == approvedRadioSerialNumber else {
                throw RadioControllerError.operationFailed(
                    "\(Self.connectedCATDisableOutcomeDescription(result.outcome)) for CAT radio \(result.radioSerialNumber), but the approved automation session belonged to radio \(approvedRadioSerialNumber). USB-C handoff was stopped."
                )
            }
            expectedRadioSerialNumber = result.radioSerialNumber

            guard !Task.isCancelled, epoch == sessionEpoch else {
                throw Self.completedConnectedCATDisableInterruptedError(result.outcome)
            }

            await transport.close()
            try requireEpoch(epoch)
        } catch {
            if let disableSlot = connectedCatDisableSlot {
                disableSlot.operation.cancel()
                _ = try? await disableSlot.task.value
                clearConnectedCATDisableSlot(id: disableSlot.id)
            }
            if epoch == sessionEpoch {
                await transport.close()
                endExclusive(operation)
                let detail = Self.describe(error)
                var failed = RadioWorkspaceState.disconnected
                failed.connection = .failed(message: detail)
                publish(failed)
                publishAPRSUnavailable(
                    "APRS is unavailable because IF-DSP DV Gateway recovery did not finish."
                )
            }
            await ifDSPUSBSelector.finishRetainedIFDSPUSBHandoff()
            if let completedOutcome, error is CancellationError {
                throw Self.completedConnectedCATDisableInterruptedError(completedOutcome)
            }
            if error is CancellationError { throw error }
            throw RadioControllerError.operationFailed(Self.describe(error))
        }

        endExclusive(operation)
        guard let outcome = completedOutcome,
              let expectedRadioSerialNumber else {
            throw RadioControllerError.operationFailed(
                "Menu 650 recovery finished without retaining the proved Bluetooth radio identity."
            )
        }
        guard !Task.isCancelled else {
            throw Self.completedConnectedCATDisableInterruptedError(outcome)
        }

        do {
            try await waitForRetainedUSBCAfterDVGatewayDisable(
                selector: ifDSPUSBSelector,
                handoffGeneration: handoffGeneration,
                outcome: outcome,
                expectedRadioSerialNumber: expectedRadioSerialNumber
            )
            await ifDSPUSBSelector.finishRetainedIFDSPUSBHandoff()
        } catch is CancellationError {
            throw Self.completedConnectedCATDisableInterruptedError(outcome)
        } catch {
            await ifDSPUSBSelector.finishRetainedIFDSPUSBHandoff()
            throw RadioControllerError.operationFailed(
                "\(Self.connectedCATDisableOutcomeDescription(outcome)), but Azimuth could not prove same-radio USB-C CAT control. \(Self.describe(error))"
            )
        }
    }

    func restoreCATFromUSBMMDVM() async throws {
        guard supportsAutomaticCATRecovery else {
            throw RadioControllerError.capabilityUnavailable(
                "Automatic USB MMDVM-to-CAT recovery requires Azimuth for macOS and a paired TH-D75 Bluetooth connection."
            )
        }
        guard usbMmdvmRecoveryPending,
              let expectedRadioSerialNumber = usbMmdvmExpectedRadioSerialNumber,
              case .failed = currentState.connection else {
            throw RadioControllerError.capabilityUnavailable(
                "Automatic CAT recovery requires a validated USB MMDVM response and a stable radio serial number from that USB device."
            )
        }

        // Bluetooth privacy consent belongs to Azimuth's foreground process,
        // not the short-lived sandboxed helper which performs the serial-
        // qualified recovery. Keep this before every state mutation so a
        // denial or cancelled prompt leaves the proved USB-MMDVM recovery
        // offer intact and never constructs or launches the helper operation.
        try await authorizeBluetoothRecovery()
        try Task.checkCancellation()
        guard usbMmdvmRecoveryPending,
              usbMmdvmExpectedRadioSerialNumber == expectedRadioSerialNumber,
              case .failed = currentState.connection else {
            throw CancellationError()
        }

        let operation = "USB MMDVM recovery"
        try beginExclusive(operation)
        usbMmdvmRecoveryPending = false
        usbMmdvmExpectedRadioSerialNumber = nil
        bluetoothMmdvmUSBHandoffPending = false
        bluetoothMmdvmUSBHandoffAvailable = false
        sessionEpoch &+= 1
        let epoch = sessionEpoch
        stopScreenPolling()
        stopAPRSPolling()
        captureSlot?.task.cancel()
        captureSlot = nil
        settingSnapshotID = nil
        core = nil
        currentRadioSerialNumber = nil
        currentIFDSPUSBInputProof = nil
        ifDSPModeState = .inactive
        publish(
            RadioWorkspaceState(
                connection: .connecting,
                capabilities: RadioCapabilities(
                    screenStreaming: .preparing,
                    frontPanelControl: .preparing,
                    settingRead: .preparing,
                    settingWrite: .preparing
                ),
                screenFrame: nil,
                telemetry: .unavailable,
                settingValues: [:],
                lastScreenError: nil
            )
        )
        publishAPRS(
            .unavailable("Inspecting Menu 650 over Bluetooth and waiting for USB-C CAT control.")
        )
        do {
            await transport.close()
            try requireEpoch(epoch)
            try Task.checkCancellation()
            azimuthRadioCoreLog.notice(
                "[Azimuth Radio] Opening alternate Bluetooth link for USB MMDVM recovery"
            )
            let recoveryID = nextUSBMMDVMRecoveryID
            nextUSBMMDVMRecoveryID &+= 1
            let qualifiedBluetoothAddress = try await sameRadioBluetoothSelector?
                .knownQualifiedBluetoothAddress(
                    expectedSerialNumber: expectedRadioSerialNumber
                )
            let recoveryOperation = try makeUSBMMDVMRecovery(
                expectedRadioSerialNumber,
                qualifiedBluetoothAddress
            )
            // The worker itself is never Swift-cancelled. Explicit native
            // cancellation lets Rust finish any in-flight radio operation and
            // cleanup before this task resolves.
            let recoveryTask = Task {
                try await recoveryOperation.run()
            }
            usbMmdvmRecoverySlot = USBMMDVMRecoverySlot(
                id: recoveryID,
                operation: recoveryOperation,
                task: recoveryTask
            )
            let outcome: DvGatewayRecoveryOutcome
            do {
                outcome = try await withTaskCancellationHandler {
                    try await recoveryTask.value
                } onCancel: {
                    recoveryOperation.cancel()
                }
            } catch {
                clearUSBMMDVMRecoverySlot(id: recoveryID)
                if case DvGatewayRecoveryError.Cancelled = error {
                    throw CancellationError()
                }
                if epoch != sessionEpoch,
                   !Self.isPostMutationRecoveryResult(error) {
                    throw CancellationError()
                }
                throw error
            }
            clearUSBMMDVMRecoverySlot(id: recoveryID)
            switch outcome {
            case .changedRadioRebooting:
                azimuthRadioCoreLog.notice(
                    "[Azimuth Radio] Menu 650 changed to Off; waiting for USB CAT after restart"
                )
            case .alreadyOffCatReady:
                azimuthRadioCoreLog.notice(
                    "[Azimuth Radio] Menu 650 was already Off; waiting for USB to leave MMDVM mode"
                )
            }
            if Task.isCancelled || epoch != sessionEpoch {
                throw Self.completedRecoveryInterruptedError(outcome)
            }
            try requireEpoch(epoch)
            do {
                try await waitForUSBCATRecovery(
                    epoch: epoch,
                    outcome: outcome,
                    expectedRadioSerialNumber: expectedRadioSerialNumber
                )
            } catch is CancellationError {
                // The Menu 650 outcome is already known at this point. Do not
                // let stopping the subsequent USB-C poll erase that material
                // radio state change from the error presented to the user.
                throw Self.completedRecoveryInterruptedError(outcome)
            }
            try Task.checkCancellation()
            try requireEpoch(epoch)
        } catch {
            if epoch == sessionEpoch {
                await transport.close()
                endExclusive(operation)
                let detail = Self.describe(error)
                var failed = RadioWorkspaceState.disconnected
                failed.connection = .failed(message: detail)
                publish(failed)
                publishAPRSUnavailable(
                    "APRS is unavailable because CAT-mode recovery failed."
                )
            }
            if error is CancellationError { throw error }
            throw RadioControllerError.operationFailed(Self.describe(error))
        }

        endExclusive(operation)
        try await connect(
            expectedRadioSerialNumber: expectedRadioSerialNumber,
            allowsPacketModeRecovery: true
        )
    }

    func connectViaBluetoothFromUSBMMDVM() async throws {
        guard usbMmdvmRecoveryPending,
              let expectedRadioSerialNumber = usbMmdvmExpectedRadioSerialNumber,
              case .failed = currentState.connection,
              let sameRadioBluetoothSelector else {
            throw RadioControllerError.capabilityUnavailable(
                "Bluetooth CAT handoff requires a validated USB MMDVM response and a stable same-radio serial identity."
            )
        }

        let authorizedEpoch = sessionEpoch
        await transport.close()
        do {
            try requireEpoch(authorizedEpoch)
            try await sameRadioBluetoothSelector.selectBluetoothForSameRadio(
                expectedSerialNumber: expectedRadioSerialNumber
            )
            try requireEpoch(authorizedEpoch)
            try await connect(
                expectedRadioSerialNumber: expectedRadioSerialNumber,
                allowsPacketModeRecovery: true
            )
        } catch {
            await transport.close()
            if error is CancellationError || disconnectInProgress {
                throw CancellationError()
            }
            let failedConnectionEpoch = authorizedEpoch &+ 1
            guard sessionEpoch == authorizedEpoch
                    || sessionEpoch == failedConnectionEpoch else {
                throw CancellationError()
            }
            try? await sameRadioBluetoothSelector.selectUSBForRecovery(
                expectedSerialNumber: expectedRadioSerialNumber
            )
            guard !disconnectInProgress,
                  sessionEpoch == authorizedEpoch
                    || sessionEpoch == failedConnectionEpoch else {
                throw CancellationError()
            }
            usbMmdvmRecoveryPending = true
            usbMmdvmExpectedRadioSerialNumber = expectedRadioSerialNumber
            throw error
        }
    }

    func disconnect() async {
        guard !disconnectInProgress else { return }
        azimuthRadioCoreLog.notice("[Azimuth Radio] Disconnect started")
        disconnectInProgress = true
        defer { disconnectInProgress = false }
        sessionEpoch &+= 1
        ifDSPUSBHandoffGeneration &+= 1
        aprsRecoveryGeneration &+= 1
        pendingAPRSDVGatewayRecovery = nil
        currentRadioSerialNumber = nil
        currentIFDSPUSBInputProof = nil
        stopScreenPolling()
        stopAPRSPolling()
        captureSlot?.task.cancel()
        captureSlot = nil
        if let connectionSlot {
            connectionSlot.task.cancel()
            _ = try? await connectionSlot.task.value
            clearConnectionSlot(id: connectionSlot.id)
        }
        if let preflightSlot {
            preflightSlot.task.cancel()
            _ = try? await preflightSlot.task.value
            clearPreflightSlot(id: preflightSlot.id)
        }
        if let closeSlot = connectedCatCoreCloseSlot {
            // The old automation actor remains the transport owner until this
            // uncancelled close completes. Do not publish disconnected or let
            // another owner open the byte stream while it is still closing.
            _ = try? await closeSlot.task.value
            clearConnectedCATCoreCloseSlot(id: closeSlot.id)
        }
        if let recoverySlot = usbMmdvmRecoverySlot {
            // Native cancel is synchronous, while the uncancelled worker owns
            // Rust cleanup. Do not publish disconnected until it returns.
            recoverySlot.operation.cancel()
            _ = try? await recoverySlot.task.value
            clearUSBMMDVMRecoverySlot(id: recoverySlot.id)
        }
        if let routingSlot = bluetoothMmdvmRoutingSlot {
            // As with Menu 650 recovery, native cancellation is synchronous
            // but the Rust worker retains cleanup ownership until completion.
            routingSlot.operation.cancel()
            _ = try? await routingSlot.task.value
            clearBluetoothMMDVMRoutingSlot(id: routingSlot.id)
        }
        if let disableSlot = connectedCatDisableSlot {
            // A late cancellation must not drop the MCP setter after its
            // mutation gate. The native worker retains cleanup ownership and
            // publishes its completed outcome before disconnect continues.
            disableSlot.operation.cancel()
            _ = try? await disableSlot.task.value
            clearConnectedCATDisableSlot(id: disableSlot.id)
        }
        if let recoverySlot = connectedCatAPRSRecoverySlot {
            // The current-actor APRS recovery owns its mutation result and
            // terminal actor shutdown until this uncancelled worker returns.
            recoverySlot.operation.cancel()
            _ = try? await recoverySlot.task.value
            clearConnectedCATAPRSRecoverySlot(id: recoverySlot.id)
        }
        screenPauseDepth = 0
        settingSnapshotID = nil
        consecutiveScreenFailures = 0
        let closingCore = core
        core = nil
        if let closingCore {
            do {
                try await closingCore.close()
                azimuthRadioCoreLog.info("[Azimuth Core] Controller closed")
            } catch {
                azimuthRadioCoreLog.error(
                    "[Azimuth Core] Close failed type=\(azimuthRadioErrorIdentity(error), privacy: .public) detail=\(error.localizedDescription, privacy: .private)"
                )
            }
        }
        await transport.close()
        await ifDSPUSBSelector?.finishRetainedIFDSPUSBHandoff()
        exclusiveOperation = nil
        usbMmdvmRecoveryPending = false
        usbMmdvmExpectedRadioSerialNumber = nil
        bluetoothMmdvmUSBHandoffPending = false
        bluetoothMmdvmUSBHandoffAvailable = false
        ifDSPModeState = .inactive
        publish(.disconnected)
        publishAPRSUnavailable(
            "Connect the TH-D75 before starting an APRS KISS session."
        )
        azimuthRadioCoreLog.notice("[Azimuth Radio] Disconnected")
    }

    func refreshScreen() async throws {
        azimuthRadioCoreLog.notice("[Azimuth Core] Screen refresh started")
        let context: ConnectedContext
        do {
            context = try beginConnectedOperation("screen refresh")
        } catch {
            azimuthRadioCoreLog.error(
                "[Azimuth Core] Screen refresh failed to start type=\(azimuthRadioErrorIdentity(error), privacy: .public) detail=\(error.localizedDescription, privacy: .private)"
            )
            throw error
        }
        defer { endConnectedOperation("screen refresh") }
        await settleCapture()
        do {
            let frame = try await captureFresh(core: context.core, epoch: context.epoch)
            await refreshCurrentIFDSPUSBInputProof(
                core: context.core,
                epoch: context.epoch
            )
            azimuthRadioCoreLog.notice(
                "[Azimuth Core] Screen refresh succeeded (\(frame.width, privacy: .public)x\(frame.height, privacy: .public), \(frame.rgba8888.count, privacy: .public) RGBA bytes)"
            )
        } catch {
            azimuthRadioCoreLog.error(
                "[Azimuth Core] Screen refresh failed type=\(azimuthRadioErrorIdentity(error), privacy: .public) detail=\(error.localizedDescription, privacy: .private)"
            )
            await handleScreenFailure(error, epoch: context.epoch)
            throw RadioControllerError.operationFailed(Self.describe(error))
        }
    }

    func refreshSettings() async throws {
        azimuthRadioCoreLog.notice("[Azimuth Core] Settings refresh started")
        let context: ConnectedContext
        do {
            context = try beginConnectedOperation("settings refresh")
        } catch {
            azimuthRadioCoreLog.error(
                "[Azimuth Core] Settings refresh failed to start type=\(azimuthRadioErrorIdentity(error), privacy: .public) detail=\(error.localizedDescription, privacy: .private)"
            )
            throw error
        }
        defer { endConnectedOperation("settings refresh") }
        await settleCapture()
        settingSnapshotID = nil
        setSettingCapabilities(.preparing, .preparing)

        do {
            let read = try await context.core.readSettingValues(settingIds: nil)
            try requireEpoch(context.epoch)
            try installSettings(read)
            setSettingCapabilities(.available, .available)
            await recaptureAfterSettings(core: context.core, epoch: context.epoch)
            await refreshCurrentIFDSPUSBInputProof(
                core: context.core,
                epoch: context.epoch
            )
            azimuthRadioCoreLog.notice(
                "[Azimuth Core] Settings refresh succeeded (\(read.values.count, privacy: .public) values)"
            )
        } catch {
            if context.epoch == sessionEpoch,
               transport.device.connectionKind == .usb {
                currentIFDSPUSBInputProof = nil
            }
            azimuthRadioCoreLog.error(
                "[Azimuth Core] Settings refresh failed type=\(azimuthRadioErrorIdentity(error), privacy: .public) detail=\(error.localizedDescription, privacy: .private)"
            )
            if context.epoch == sessionEpoch {
                setSettingCapabilities(
                    .unavailable(reason: Self.describe(error)),
                    .unavailable(reason: "Refresh the radio settings before writing.")
                )
            }
            throw RadioControllerError.operationFailed(Self.describe(error))
        }
    }

    func press(_ key: RadioFrontPanelKey) async throws {
        let context = try beginConnectedOperation("front-panel input")
        defer { endConnectedOperation("front-panel input") }
        await settleCapture()

        do {
            // Always capture after the stream has fully stopped. A previously
            // displayed frame is never trusted as a lease for new input.
            let screen = try await captureFresh(core: context.core, epoch: context.epoch)
            let result = try await context.core.guardedTap(
                leaseId: screen.leaseId,
                key: Self.coreKey(key)
            )
            try requireEpoch(context.epoch)
            try installScreen(result.screen)
            switch result.disposition {
            case .dispatched, .dispatchedAfterDeadline:
                invalidateSettingsSnapshot(
                    writeReason: "A front-panel key was sent after the last settings read. Refresh settings before writing."
                )
                return
            case .contextChanged:
                throw RadioControllerError.operationFailed(
                    "The radio screen changed before the key could be sent. No input was dispatched; try again from the new screen."
                )
            }
        } catch let error as RadioControllerError {
            throw error
        } catch {
            await handleScreenFailure(error, epoch: context.epoch)
            throw RadioControllerError.operationFailed(Self.describe(error))
        }
    }

    func applySettings(
        _ changes: [ValidatedRadioSettingChange],
        progress: @escaping @MainActor @Sendable (RadioSettingApplyProgress) -> Void
    ) async throws -> RadioSettingApplyReport {
        guard !changes.isEmpty else {
            throw RadioControllerError.operationFailed("An approved setting batch cannot be empty.")
        }
        let context = try beginConnectedOperation("setting apply")
        defer { endConnectedOperation("setting apply") }
        await settleCapture()

        guard let snapshotID = settingSnapshotID else {
            throw RadioControllerError.capabilityUnavailable(
                "Azimuth has no live setting snapshot. Refresh settings before applying changes."
            )
        }
        guard Set(changes.map(\.settingID)).count == changes.count else {
            throw RadioControllerError.operationFailed(
                "The approved batch contains the same setting more than once."
            )
        }

        let coreChanges = try changes.map { change -> SettingChange in
            guard let record = schema.recordsByID[change.settingID] else {
                throw RadioControllerError.operationFailed(
                    "Unknown setting ID \(change.settingID)."
                )
            }
            guard let reviewed = change.previousValue,
                  currentState.settingValues[change.settingID] == reviewed else {
                throw RadioControllerError.operationFailed(
                    "\(change.settingID) no longer matches the value shown for approval. Refresh and review a new plan."
                )
            }
            return SettingChange(
                settingId: change.settingID,
                snapshotId: snapshotID,
                expectedValue: try AzimuthCoreSettingValueBridge.coreValue(reviewed, record: record),
                desiredValue: try AzimuthCoreSettingValueBridge.coreValue(change.targetValue, record: record)
            )
        }
        let validation = validateSettingChanges(changes: coreChanges)
        guard validation.accepted else {
            throw RadioControllerError.operationFailed(
                validation.batchError ?? "The generated schema rejected the approved setting batch."
            )
        }

        progress(
            RadioSettingApplyProgress(
                completedCount: 0,
                totalCount: changes.count,
                currentSettingID: changes.first?.settingID
            )
        )
        setSettingCapabilities(.available, .preparing)
        var invokedCoreBatch = false
        do {
            invokedCoreBatch = true
            // Exactly one automated, stale-safe core transaction is issued
            // for the complete user-approved list.
            let coreReport = try await context.core.applySettingChanges(changes: coreChanges)
            try requireEpoch(context.epoch)
            try installSettings(coreReport.refreshedValues)
            setSettingCapabilities(.available, .available)

            let reported = Dictionary(
                coreReport.changes.map { ($0.settingId, $0) },
                uniquingKeysWith: { first, _ in first }
            )
            let results = try changes.map { approved -> RadioSettingApplyResult in
                guard let coreResult = reported[approved.settingID],
                      let record = schema.recordsByID[approved.settingID] else {
                    return RadioSettingApplyResult(
                        settingID: approved.settingID,
                        previousValue: approved.previousValue,
                        targetValue: approved.targetValue,
                        outcome: .failed(reason: "The core omitted this setting from its verification report.")
                    )
                }
                let verified = try AzimuthCoreSettingValueBridge.productValue(
                    coreResult.value,
                    record: record
                )
                guard verified == approved.targetValue else {
                    return RadioSettingApplyResult(
                        settingID: approved.settingID,
                        previousValue: approved.previousValue,
                        targetValue: approved.targetValue,
                        outcome: .failed(reason: "Read-back did not match the approved value.")
                    )
                }
                return RadioSettingApplyResult(
                    settingID: approved.settingID,
                    previousValue: approved.previousValue,
                    targetValue: approved.targetValue,
                    outcome: .applied
                )
            }
            progress(
                RadioSettingApplyProgress(
                    completedCount: changes.count,
                    totalCount: changes.count,
                    currentSettingID: nil
                )
            )
            await recaptureAfterSettings(core: context.core, epoch: context.epoch)
            await refreshCurrentIFDSPUSBInputProof(
                core: context.core,
                epoch: context.epoch
            )
            return RadioSettingApplyReport(results: results)
        } catch {
            if invokedCoreBatch, context.epoch == sessionEpoch {
                await refreshAfterFailedApply(core: context.core, epoch: context.epoch)
            }
            throw RadioControllerError.operationFailed(Self.describe(error))
        }
    }

    // MARK: - USB IF-DSP radio ownership

    /// Retain the sole attached USB-C endpoint and surface the explicit
    /// Menu 650 inspection/change consent while the authenticated Bluetooth
    /// CAT actor and its exact transport remain untouched.
    ///
    /// A Bluetooth-started IF-DSP session must ultimately own USB CAT/audio.
    /// Inspecting Menu 650 requires MCP. Its `E` exit resets CAT and USB and
    /// takes roughly five seconds to reconnect even when no setting write is
    /// needed; a changed value uses the detached reboot path. Therefore the
    /// inspection and transport handoff belong after explicit consent rather
    /// than behind a speculative USB probe.
    private func handoffBluetoothCATToUSBForIFDSPIfNeeded() async throws {
        guard currentState.connection.isConnected,
              transport.device.connectionKind == .bluetooth,
              currentIFDSPUSBInputProof == nil else { return }
        guard supportsAutomaticCATRecovery,
              let ifDSPUSBSelector,
              let approvedCore = core,
              let approvedRadioSerialNumber = currentRadioSerialNumber else {
            throw RadioControllerError.capabilityUnavailable(
                "IF-DSP needs USB-C CAT control from the connected TH-D75. Connect the radio to this Mac over USB-C and retry."
            )
        }

        let retainedUSBAvailable: Bool
        do {
            retainedUSBAvailable = try await ifDSPUSBSelector
                .retainSoleIFDSPUSBEndpoint()
        } catch {
            throw RadioControllerError.operationFailed(
                "Azimuth could not retain the attached USB-C endpoint for IF-DSP recovery. \(Self.describe(error)) No radio setting was changed."
            )
        }
        guard retainedUSBAvailable else {
            throw RadioControllerError.capabilityUnavailable(
                "IF-DSP needs exactly one attached, qualified TH-D75 USB-C endpoint. No radio setting was changed."
            )
        }
        do {
            try Task.checkCancellation()
        } catch {
            await ifDSPUSBSelector.finishRetainedIFDSPUSBHandoff()
            throw error
        }
        guard core === approvedCore,
              currentRadioSerialNumber == approvedRadioSerialNumber,
              currentState.connection.isConnected,
              transport.device.connectionKind == .bluetooth else {
            await ifDSPUSBSelector.finishRetainedIFDSPUSBHandoff()
            throw CancellationError()
        }
        throw RadioControllerError.ifDspDVGatewayRecoveryRequired
    }

    @discardableResult
    func prepareIFDSPMode() async throws -> IFDSPRadioModeStatus {
        guard !ifDSPModeState.reservesRadioState else {
            throw RadioControllerError.capabilityUnavailable(
                "Restore the current IF-DSP radio session before preparing another one."
            )
        }
        guard !currentAPRSState.status.phase.ownsSerialLink else {
            throw RadioControllerError.capabilityUnavailable(
                "Stop APRS before preparing the radio for IF-DSP."
            )
        }
        try await handoffBluetoothCATToUSBForIFDSPIfNeeded()

        let operation = "IF-DSP prepare"
        try beginExclusive(operation)
        guard currentState.connection.isConnected,
              transport.device.connectionKind == .usb,
              currentIFDSPUSBInputProof != nil,
              let core else {
            endExclusive(operation)
            throw RadioControllerError.capabilityUnavailable(
                "Connect and prove the TH-D75 over USB-C before starting IF-DSP."
            )
        }
        let epoch = sessionEpoch
        stopScreenPolling()
        pauseScreenStream()
        await settleCapture()
        settingSnapshotID = nil
        ifDSPModeState = .preparing
        publishCATUnavailableForIFDSP(
            mode: "Preparing USB IF",
            reason: "Saving the radio state and verifying Band B USB IF output."
        )

        do {
            let coreStatus = try await core.prepareIfDsp()
            if Task.isCancelled { throw CancellationError() }
            try requireEpoch(epoch)
            await refreshCurrentIFDSPUSBInputProof(core: core, epoch: epoch)
            let status = try Self.activeIFDSPStatus(coreStatus)
            ifDSPModeState = .active(status)
            publishCATUnavailableForIFDSP(
                mode: "USB IF-DSP",
                reason: "IF-DSP owns a saved radio state. Stop IF-DSP to restore CAT controls."
            )
            endExclusive(operation)
            return status
        } catch {
            let restorationPending = await reconcileIFDSPReservation(
                core: core,
                after: error
            )
            let presentationError = Self.ifDSPPresentationError(error)
            if error is CancellationError, !restorationPending {
                ifDSPModeState = .inactive
            } else {
                ifDSPModeState = .failed(
                    message: presentationError.localizedDescription,
                    restorationPending: restorationPending
                )
            }
            if !restorationPending {
                try? await restoreCATWorkspace(core: core, epoch: epoch)
                resumeScreenStream()
            }
            endExclusive(operation)
            if error is CancellationError { throw error }
            throw presentationError
        }
    }

    @discardableResult
    func retuneIFDSP(to frequencyHz: UInt32) async throws -> IFDSPRadioModeStatus {
        guard case .active(let previous) = ifDSPModeState else {
            throw RadioControllerError.capabilityUnavailable(
                "Prepare IF-DSP before changing the Band B center frequency."
            )
        }
        let operation = "IF-DSP retune"
        try beginExclusive(operation)
        guard currentState.connection.isConnected, let core else {
            endExclusive(operation)
            throw RadioControllerError.capabilityUnavailable(
                "The radio disconnected before IF-DSP could tune."
            )
        }
        let epoch = sessionEpoch
        ifDSPModeState = .tuning(
            previous: previous,
            requestedFrequencyHz: frequencyHz
        )

        do {
            let coreStatus = try await core.retuneIfDsp(frequencyHz: frequencyHz)
            if Task.isCancelled { throw CancellationError() }
            try requireEpoch(epoch)
            let status = try Self.activeIFDSPStatus(coreStatus)
            ifDSPModeState = .active(status)
            endExclusive(operation)
            return status
        } catch {
            let restorationPending = await reconcileIFDSPReservation(
                core: core,
                after: error
            )
            if error is CancellationError, !restorationPending {
                ifDSPModeState = .inactive
            } else {
                ifDSPModeState = .failed(
                    message: Self.describe(error),
                    restorationPending: restorationPending
                )
            }
            if !restorationPending {
                try? await restoreCATWorkspace(core: core, epoch: epoch)
                resumeScreenStream()
            }
            endExclusive(operation)
            if error is CancellationError { throw error }
            throw RadioControllerError.operationFailed(Self.describe(error))
        }
    }

    func restoreIFDSPMode() async throws {
        guard ifDSPModeState.reservesRadioState else {
            ifDSPModeState = .inactive
            return
        }
        let operation = "IF-DSP restore"
        try beginExclusive(operation)
        guard currentState.connection.isConnected, let core else {
            endExclusive(operation)
            throw RadioControllerError.capabilityUnavailable(
                "The radio disconnected before its saved IF-DSP state could be restored."
            )
        }
        let epoch = sessionEpoch
        ifDSPModeState = .restoring(ifDSPModeState.activeStatus)
        var radioRestored = false
        var screenPauseReleased = false
        publishCATUnavailableForIFDSP(
            mode: "Restoring radio",
            reason: "Restoring and readback-verifying every value saved before IF-DSP."
        )

        do {
            let status = try await core.restoreIfDsp()
            try requireEpoch(epoch)
            guard status.phase == .inactive else {
                throw RadioControllerError.operationFailed(
                    "The core did not confirm that IF-DSP released the radio state."
                )
            }
            radioRestored = true
            ifDSPModeState = .inactive
            resumeScreenStream()
            screenPauseReleased = true
            try await restoreCATWorkspace(core: core, epoch: epoch)
            endExclusive(operation)
        } catch {
            if radioRestored {
                ifDSPModeState = .inactive
                if !screenPauseReleased { resumeScreenStream() }
            } else {
                ifDSPModeState = .failed(
                    message: Self.describe(error),
                    restorationPending: true
                )
            }
            endExclusive(operation)
            if error is CancellationError { throw error }
            throw RadioControllerError.operationFailed(Self.describe(error))
        }
    }

    // MARK: - APRS KISS operations

    func startAPRS(_ configuration: APRSSessionConfiguration) async throws {
        pendingAPRSDVGatewayRecovery = nil
        try await startAPRS(
            configuration,
            retainedRouteProof: nil,
            recoveredDataBand: nil,
            permitsRecoveryOffer: true
        )
    }

    private func startAPRS(
        _ configuration: APRSSessionConfiguration,
        retainedRouteProof: APRSDVGatewayRecoveryProof?,
        recoveredDataBand: TncDataBand?,
        permitsRecoveryOffer: Bool
    ) async throws {
        guard !ifDSPModeState.reservesRadioState else {
            throw RadioControllerError.capabilityUnavailable(
                "Restore the IF-DSP radio session before starting APRS."
            )
        }
        let context = try beginAPRSOperation("APRS start")
        defer { endConnectedOperation("APRS start") }
        guard !currentAPRSState.status.phase.ownsSerialLink else {
            throw RadioControllerError.capabilityUnavailable(
                "Stop the current APRS KISS session before starting another one."
            )
        }

        let kissInterfaceRawValue: UInt8
        let startAuthority: AprsStartAuthority
        if let retainedRouteProof {
            guard retainedRouteProof.configuration == configuration,
                  retainedRouteProof.device == transport.device,
                  currentRadioSerialNumber == retainedRouteProof.radioSerialNumber,
                  context.core.abi().radioSerialNumber
                    == retainedRouteProof.radioSerialNumber,
                  retainedRouteProof.kissInterfaceRawValue
                    == Self.kissInterfaceRawValue(for: transport.device) else {
                throw RadioControllerError.capabilityUnavailable(
                    "The one-use APRS recovery proof no longer matches the pending configuration, selected endpoint, KISS route, and CAT radio identity. Start APRS again from current radio settings."
                )
            }
            guard let recoveredDataBand else {
                throw RadioControllerError.capabilityUnavailable(
                    "The one-use APRS recovery proof did not retain the freshly verified Menu 506 data band. Start APRS again from current radio settings."
                )
            }
            kissInterfaceRawValue = retainedRouteProof.kissInterfaceRawValue
            startAuthority = .currentModeRecovery(
                expectedRadioSerialNumber: retainedRouteProof.radioSerialNumber,
                expectedDataBand: recoveredDataBand
            )
        } else {
            guard recoveredDataBand == nil else {
                throw RadioControllerError.capabilityUnavailable(
                    "A recovered Menu 506 data band cannot authorize a new APRS start without its same-endpoint recovery proof. Refresh radio settings and start again."
                )
            }
            let settingsAuthority = try requireAPRSSettingsSnapshotAuthority()
            kissInterfaceRawValue = settingsAuthority.kissInterfaceRawValue
            startAuthority = .settingsSnapshot(
                snapshotId: settingsAuthority.snapshotID,
                expectedKissInterfaceRawValue: settingsAuthority.kissInterfaceRawValue
            )
        }
        guard let radioSerialNumber = currentRadioSerialNumber,
              !radioSerialNumber.isEmpty,
              context.core.abi().radioSerialNumber == radioSerialNumber else {
            throw RadioControllerError.capabilityUnavailable(
                "APRS start requires the current CAT actor and its exact AE radio identity. Reconnect the selected endpoint before trying again."
            )
        }
        let candidateRecoveryProof = APRSDVGatewayRecoveryProof(
            configuration: configuration,
            kissInterfaceRawValue: kissInterfaceRawValue,
            radioSerialNumber: radioSerialNumber,
            device: transport.device,
            sessionEpoch: context.epoch
        )

        stopScreenPolling()
        stopAPRSPolling()
        await settleCapture()
        invalidateSettingsSnapshot(
            writeReason: "Read the radio settings again before writing."
        )
        var starting = currentAPRSState
        starting.status.phase = .starting
        starting.status.startedAt = Date()
        starting.status.configuration = configuration
        starting.status.lastError = nil
        publishAPRS(starting)
        publishCATUnavailableForAPRS(
            mode: "Entering APRS KISS",
            reason: "APRS is taking ownership of the radio control link."
        )

        do {
            _ = try await context.core.startAprs(
                config: AzimuthCoreAPRSAdapter.coreConfiguration(configuration),
                authority: startAuthority
            )
            try requireEpoch(context.epoch)
            installAPRSSnapshot(
                context.core.aprsSnapshot(
                    afterSequence: currentAPRSState.latestSequence
                ),
                retainingHistory: true
            )
            publishCATUnavailableForAPRS(
                mode: "APRS KISS",
                reason: "APRS KISS owns the radio control link. Stop APRS to restore CAT control."
            )
            pendingAPRSDVGatewayRecovery = nil
            startAPRSPolling(epoch: context.epoch)
        } catch {
            let currentModeDetail = Self.aprsCurrentModeDetail(error)
            if context.epoch == sessionEpoch {
                installAPRSSnapshot(
                    context.core.aprsSnapshot(
                        afterSequence: currentAPRSState.latestSequence
                    ),
                    retainingHistory: true
                )
                await recoverCATAfterAPRSError(
                    core: context.core,
                    epoch: context.epoch,
                    originalError: error
                )
            }
            if error is CancellationError { throw error }
            if let currentModeDetail {
                azimuthRadioCoreLog.notice(
                    "APRS KISS was refused in the current radio mode after exact CAT restoration: \(currentModeDetail, privacy: .private)"
                )
                let recoveredSameCATOwner = context.epoch == sessionEpoch
                    && currentState.connection.isConnected
                    && core === context.core
                    && currentRadioSerialNumber
                        == candidateRecoveryProof.radioSerialNumber
                    && transport.device == candidateRecoveryProof.device
                    && currentAPRSState.status.phase == .inactive
                if permitsRecoveryOffer,
                   recoveredSameCATOwner,
                   supportsAutomaticCATRecovery {
                    pendingAPRSDVGatewayRecovery = candidateRecoveryProof
                    throw RadioControllerError.aprsDVGatewayRecoveryRequired
                }
                pendingAPRSDVGatewayRecovery = nil
                if !permitsRecoveryOffer, recoveredSameCATOwner {
                    throw RadioControllerError.operationFailed(
                        "The TH-D75 still refused KISS after Azimuth verified Menu 983, Menu 506, and Menu 650 together, reset CAT, reconnected the same radio endpoint, and retried this APRS configuration once with the fresh TNC band. The radio remains in CAT mode. Dismiss this message and inspect the radio's current operating mode before starting a new attempt."
                    )
                }
                if recoveredSameCATOwner {
                    throw RadioControllerError.operationFailed(
                        "The TH-D75 refused KISS in its current mode. Automatic inspection is unavailable for this connection, so Azimuth left the radio unchanged. Inspect Menu 983, Menu 506, Menu 650, and the current operating mode on the radio before reconnecting and trying APRS again."
                    )
                }
                throw RadioControllerError.operationFailed(
                    "The TH-D75 refused KISS in its current mode, and Azimuth could not retain the same authenticated CAT owner for automatic recovery. Reconnect the selected endpoint before trying APRS again."
                )
            }
            throw RadioControllerError.operationFailed(Self.describe(error))
        }
    }

    /// Fail before KISS takes ownership unless one reviewed settings snapshot
    /// contains a valid Menu 506 TNC band and routes Menu 983 packet bytes to
    /// the physical interface this controller currently owns. The Rust actor
    /// independently consumes and validates that same snapshot before `TN`.
    /// Persistent radio settings are never changed implicitly here.
    private func requireAPRSSettingsSnapshotAuthority() throws -> (
        snapshotID: UInt64,
        kissInterfaceRawValue: UInt8
    ) {
        guard let snapshotID = settingSnapshotID, snapshotID != 0 else {
            throw RadioControllerError.capabilityUnavailable(
                "Azimuth has no current settings snapshot for APRS. Refresh radio settings, then try APRS again."
            )
        }
        let settingID = "radio.KissModeInterface"
        guard let stored = currentState.settingValues[settingID] else {
            throw RadioControllerError.capabilityUnavailable(
                "Azimuth has not read Menu 983 (KISS interface) from this radio. Refresh radio settings, then try APRS again."
            )
        }
        guard case .choice(let rawValue) = stored,
              rawValue == 0 || rawValue == 1 else {
            throw RadioControllerError.capabilityUnavailable(
                "Menu 983 (KISS interface) returned an unsupported value. Refresh radio settings and verify the menu on the radio before starting APRS."
            )
        }

        let dataBandSettingID = "aprs.TncDataBand"
        guard let storedDataBand = currentState.settingValues[dataBandSettingID] else {
            throw RadioControllerError.capabilityUnavailable(
                "Azimuth has not read Menu 506 (TNC data band) from this radio. Refresh radio settings, then try APRS again."
            )
        }
        guard case .choice(let rawDataBand) = storedDataBand,
              rawDataBand == 0 || rawDataBand == 1 else {
            throw RadioControllerError.capabilityUnavailable(
                "Menu 506 (TNC data band) returned an unsupported value. Refresh radio settings and verify the menu on the radio before starting APRS."
            )
        }

        let selectedKind = transport.device.connectionKind
        let expectedRawValue = selectedKind == .usb ? 0 : 1
        guard rawValue == expectedRawValue else {
            let selectedName = selectedKind.title
            let configuredName = rawValue == 0
                ? AzimuthRadioConnectionKind.usb.title
                : AzimuthRadioConnectionKind.bluetooth.title
            throw RadioControllerError.capabilityUnavailable(
                "Menu 983 routes KISS to \(configuredName), but Azimuth is connected over \(selectedName). Set Menu 983 (KISS) to \(selectedName), refresh radio settings, then start APRS again."
            )
        }
        return (snapshotID, UInt8(rawValue))
    }

    private static func kissInterfaceRawValue(
        for device: AzimuthRadioDevice
    ) -> UInt8 {
        device.connectionKind == .usb ? 0 : 1
    }

    private static func aprsCurrentModeDetail(_ error: Error) -> String? {
        if case AutomationError.AprsCurrentModeUnavailable(let detail) = error {
            return detail
        }
        return nil
    }

    func discardAPRSDVGatewayRecovery() {
        pendingAPRSDVGatewayRecovery = nil
    }

    func recoverDVGatewayAndRetryAPRS() async throws {
        guard automaticAPRSDVGatewayRecoveryAvailable,
              let proof = pendingAPRSDVGatewayRecovery,
              let closingCore = core,
              currentRadioSerialNumber == proof.radioSerialNumber else {
            throw RadioControllerError.capabilityUnavailable(
                "Automatic APRS recovery requires the original authenticated CAT actor, selected endpoint, radio identity, and KISS route proof. No radio setting was changed."
            )
        }
        pendingAPRSDVGatewayRecovery = nil

        let nativeOperation: any AzimuthAPRSCurrentModeRecoveryOperation
        do {
            nativeOperation = try makeConnectedCATAPRSCurrentModeRecovery(
                closingCore,
                proof.radioSerialNumber,
                proof.kissInterfaceRawValue
            )
        } catch {
            throw RadioControllerError.operationFailed(Self.describe(error))
        }

        let operation = "APRS current-mode recovery"
        try beginExclusive(operation)
        aprsRecoveryGeneration &+= 1
        let recoveryGeneration = aprsRecoveryGeneration
        sessionEpoch &+= 1
        stopScreenPolling()
        stopAPRSPolling()
        captureSlot?.task.cancel()
        captureSlot = nil
        settingSnapshotID = nil
        core = nil
        currentRadioSerialNumber = nil
        currentIFDSPUSBInputProof = nil
        ifDSPModeState = .inactive
        publish(
            RadioWorkspaceState(
                connection: .connecting,
                capabilities: RadioCapabilities(
                    screenStreaming: .preparing,
                    frontPanelControl: .preparing,
                    settingRead: .preparing,
                    settingWrite: .preparing
                ),
                screenFrame: nil,
                telemetry: .unavailable,
                settingValues: [:],
                lastScreenError: nil
            )
        )
        publishAPRS(
            .unavailable(
                "Verifying Menu 983, Menu 506, and Menu 650, then waiting for the same radio endpoint."
            )
        )

        var completedOutcome: DvGatewayRecoveryOutcome?
        var completedDataBand: TncDataBand?
        do {
            let recoveryID = nextConnectedCATAPRSRecoveryID
            nextConnectedCATAPRSRecoveryID &+= 1
            let recoveryTask = Task {
                do {
                    let result = try await nativeOperation.run()
                    _ = try? await closingCore.close()
                    return result
                } catch {
                    _ = try? await closingCore.close()
                    throw error
                }
            }
            connectedCatAPRSRecoverySlot = ConnectedCATAPRSRecoverySlot(
                id: recoveryID,
                operation: nativeOperation,
                task: recoveryTask
            )

            let result: AprsCurrentModeRecoveryResult
            do {
                result = try await withTaskCancellationHandler {
                    try await recoveryTask.value
                } onCancel: {
                    nativeOperation.cancel()
                }
            } catch {
                clearConnectedCATAPRSRecoverySlot(id: recoveryID)
                if case AprsCurrentModeRecoveryError.Cancelled = error {
                    throw CancellationError()
                }
                if case AprsCurrentModeRecoveryError.CompletedButReleaseFailed(
                    let completed,
                    _
                ) = error {
                    completedOutcome = completed.outcome
                    completedDataBand = completed.dataBand
                }
                throw error
            }
            clearConnectedCATAPRSRecoverySlot(id: recoveryID)
            completedOutcome = result.outcome
            completedDataBand = result.dataBand

            guard result.radioSerialNumber == proof.radioSerialNumber else {
                throw RadioControllerError.operationFailed(
                    "\(Self.connectedCATDisableOutcomeDescription(result.outcome)) for CAT radio \(result.radioSerialNumber), but the approved APRS session belonged to radio \(proof.radioSerialNumber). Same-endpoint reconnect was stopped."
                )
            }
            guard result.kissInterfaceRawValue == proof.kissInterfaceRawValue else {
                throw RadioControllerError.operationFailed(
                    "\(Self.connectedCATDisableOutcomeDescription(result.outcome)), but the approved operation returned KISS route \(result.kissInterfaceRawValue) while the selected endpoint requires route \(proof.kissInterfaceRawValue). Same-endpoint reconnect was stopped."
                )
            }
            try requireAPRSRecovery(recoveryGeneration)

            await transport.close()
            try requireAPRSRecovery(recoveryGeneration)
            guard transport.device == proof.device else {
                throw RadioControllerError.operationFailed(
                    "The selected radio endpoint changed after the approved Menu 650 operation. Azimuth refused to reconnect a different endpoint."
                )
            }
            if proof.device.connectionKind == .bluetooth {
                guard let sameRadioBluetoothSelector else {
                    throw RadioControllerError.capabilityUnavailable(
                        "The selected Bluetooth transport cannot retain its exact paired address for same-radio recovery."
                    )
                }
                try await sameRadioBluetoothSelector
                    .qualifySelectedBluetoothForReconnect(
                        expectedSerialNumber: proof.radioSerialNumber
                    )
                try requireAPRSRecovery(recoveryGeneration)
                guard transport.device == proof.device else {
                    throw RadioControllerError.operationFailed(
                        "The selected Bluetooth endpoint changed while it was being bound to the approved CAT serial. Azimuth refused to reconnect it."
                    )
                }
            }
        } catch {
            if let recoverySlot = connectedCatAPRSRecoverySlot {
                recoverySlot.operation.cancel()
                _ = try? await recoverySlot.task.value
                clearConnectedCATAPRSRecoverySlot(id: recoverySlot.id)
            }
            let detail = Self.aprsRecoveryFailureDescription(
                error,
                completedOutcome: completedOutcome
            )
            if recoveryGeneration == aprsRecoveryGeneration {
                await transport.close()
                endExclusive(operation)
                var failed = RadioWorkspaceState.disconnected
                failed.connection = .failed(message: detail)
                publish(failed)
                publishAPRSUnavailable(
                    "APRS is unavailable because the approved radio-mode recovery did not finish."
                )
            }
            if let completedOutcome, error is CancellationError {
                throw Self.completedAPRSRecoveryInterruptedError(completedOutcome)
            }
            if error is CancellationError { throw error }
            throw RadioControllerError.operationFailed(detail)
        }

        endExclusive(operation)
        guard let outcome = completedOutcome else {
            throw RadioControllerError.operationFailed(
                "APRS radio-mode recovery finished without retaining its verified Menu 650 outcome."
            )
        }
        guard let dataBand = completedDataBand else {
            throw RadioControllerError.operationFailed(
                "APRS radio-mode recovery finished without retaining the verified Menu 506 data band."
            )
        }
        guard !Task.isCancelled else {
            throw Self.completedAPRSRecoveryInterruptedError(outcome)
        }

        let reconnectedProof: APRSDVGatewayRecoveryProof
        do {
            reconnectedProof = try await waitForSameEndpointAfterAPRSRecovery(
                proof: proof,
                recoveryGeneration: recoveryGeneration,
                outcome: outcome
            )
        } catch is CancellationError {
            throw Self.completedAPRSRecoveryInterruptedError(outcome)
        } catch {
            throw RadioControllerError.operationFailed(
                Self.aprsRecoveryFailureDescription(
                    error,
                    completedOutcome: outcome
                )
            )
        }

        try await startAPRS(
            reconnectedProof.configuration,
            retainedRouteProof: reconnectedProof,
            recoveredDataBand: dataBand,
            permitsRecoveryOffer: false
        )
    }

    func stopAPRS() async throws {
        let context = try beginAPRSOperation("APRS stop")
        defer { endConnectedOperation("APRS stop") }
        guard currentAPRSState.status.phase == .active else {
            throw RadioControllerError.capabilityUnavailable(
                "Start an APRS KISS session before trying to stop it."
            )
        }

        stopAPRSPolling()
        var restoring = currentAPRSState
        restoring.status.phase = .restoring
        publishAPRS(restoring)
        publishCATUnavailableForAPRS(
            mode: "Restoring automation",
            reason: "Stopping KISS and proving automation control before controls resume."
        )

        do {
            _ = try await context.core.stopAprs()
            try requireEpoch(context.epoch)
            installAPRSSnapshot(
                context.core.aprsSnapshot(
                    afterSequence: currentAPRSState.latestSequence
                ),
                retainingHistory: true
            )
            try await restoreCATWorkspace(core: context.core, epoch: context.epoch)
        } catch {
            if context.epoch == sessionEpoch {
                installAPRSSnapshot(
                    context.core.aprsSnapshot(
                        afterSequence: currentAPRSState.latestSequence
                    ),
                    retainingHistory: true
                )
                await recoverCATAfterAPRSError(
                    core: context.core,
                    epoch: context.epoch,
                    originalError: error
                )
            }
            if error is CancellationError { throw error }
            throw RadioControllerError.operationFailed(Self.describe(error))
        }
    }

    func sendAPRSMessage(
        addressee: String,
        text: String,
        messageID: String?
    ) async throws -> APRSActivity {
        let context = try beginAPRSOperation("APRS message transmit")
        defer { endConnectedOperation("APRS message transmit") }
        try requireActiveAPRS()
        do {
            let record = try await context.core.sendAprsMessage(
                addressee: addressee,
                text: text,
                messageId: messageID
            )
            try requireEpoch(context.epoch)
            installAPRSSnapshot(
                context.core.aprsSnapshot(
                    afterSequence: currentAPRSState.latestSequence
                ),
                retainingHistory: true
            )
            return AzimuthCoreAPRSAdapter.activity(record)
        } catch {
            if context.epoch == sessionEpoch {
                installAPRSSnapshot(
                    context.core.aprsSnapshot(
                        afterSequence: currentAPRSState.latestSequence
                    ),
                    retainingHistory: true
                )
            }
            if error is CancellationError { throw error }
            throw RadioControllerError.operationFailed(Self.describe(error))
        }
    }

    func sendAPRSPosition(
        latitude: Double,
        longitude: Double,
        comment: String
    ) async throws -> APRSActivity {
        let context = try beginAPRSOperation("APRS position transmit")
        defer { endConnectedOperation("APRS position transmit") }
        try requireActiveAPRS()
        do {
            let record = try await context.core.sendAprsPosition(
                latitude: latitude,
                longitude: longitude,
                comment: comment
            )
            try requireEpoch(context.epoch)
            installAPRSSnapshot(
                context.core.aprsSnapshot(
                    afterSequence: currentAPRSState.latestSequence
                ),
                retainingHistory: true
            )
            return AzimuthCoreAPRSAdapter.activity(record)
        } catch {
            if context.epoch == sessionEpoch {
                installAPRSSnapshot(
                    context.core.aprsSnapshot(
                        afterSequence: currentAPRSState.latestSequence
                    ),
                    retainingHistory: true
                )
            }
            if error is CancellationError { throw error }
            throw RadioControllerError.operationFailed(Self.describe(error))
        }
    }

    // MARK: - Core state installation

    private func installSettings(_ read: SettingReadResult) throws {
        var values: [String: ProposedSettingValue] = [:]
        values.reserveCapacity(read.values.count)
        for item in read.values {
            guard let record = schema.recordsByID[item.settingId] else {
                throw AzimuthCoreIntegrationError.unsupportedSetting(
                    item.settingId,
                    reason: "the live value is absent from the generated catalog"
                )
            }
            values[item.settingId] = try AzimuthCoreSettingValueBridge.productValue(
                item.value,
                record: record
            )
        }
        settingSnapshotID = read.snapshotId
        var state = currentState
        state.settingValues = values
        publish(state)
    }

    private func installScreen(_ frame: RemoteScreenFrame) throws {
        guard let width = Int(exactly: frame.width),
              let height = Int(exactly: frame.height),
              let rowBytes = Int(exactly: frame.rowBytes),
              width == 240,
              height == 180,
              rowBytes == width * 4,
              frame.rgba8888.count == rowBytes * height else {
            throw AzimuthCoreIntegrationError.invalidScreen(
                "expected 240×180 RGBA8888 with a 960-byte row"
            )
        }
        let productFrame = RadioScreenFrame(
            width: width,
            height: height,
            rgba8888: frame.rgba8888,
            capturedAt: Date()
        )
        guard productFrame.isValid else {
            throw AzimuthCoreIntegrationError.invalidScreen("RGBA byte count is inconsistent")
        }
        consecutiveScreenFailures = 0
        var state = currentState
        state.screenFrame = productFrame
        state.lastScreenError = nil
        state.capabilities.screenStreaming = .available
        state.capabilities.frontPanelControl = .available
        publish(state)
    }

    private func installAPRSSnapshot(
        _ snapshot: AprsOperationalSnapshot,
        retainingHistory: Bool
    ) {
        publishAPRS(
            AzimuthCoreAPRSAdapter.operationalState(
                snapshot,
                retaining: retainingHistory ? currentAPRSState : nil
            )
        )
    }

    private func publishAPRS(_ state: APRSOperationalState) {
        guard state != currentAPRSState else { return }
        currentAPRSState = state
        aprsUpdateContinuation.yield(state)
    }

    private func publishAPRSUnavailable(_ reason: String) {
        var state = currentAPRSState
        state.status.phase = .unavailable(reason: reason)
        state.status.configuration = nil
        publishAPRS(state)
    }

    private func publishCATUnavailableForAPRS(mode: String, reason: String) {
        var state = currentState
        let unavailable = RadioCapabilityState.unavailable(reason: reason)
        state.capabilities = RadioCapabilities(
            screenStreaming: unavailable,
            frontPanelControl: unavailable,
            settingRead: unavailable,
            settingWrite: unavailable
        )
        state.telemetry.operatingMode = mode
        publish(state)
    }

    private func publishCATUnavailableForIFDSP(mode: String, reason: String) {
        var state = currentState
        let unavailable = RadioCapabilityState.unavailable(reason: reason)
        state.capabilities = RadioCapabilities(
            screenStreaming: unavailable,
            frontPanelControl: unavailable,
            settingRead: unavailable,
            settingWrite: unavailable
        )
        state.telemetry.operatingMode = mode
        publish(state)
    }

    // MARK: - Authenticated capture serialization

    private func captureFresh(
        core: any AutomationControllerProtocol,
        epoch: UInt64
    ) async throws -> RemoteScreenFrame {
        await settleCapture()
        return try await captureShared(core: core, epoch: epoch)
    }

    private func captureShared(
        core: any AutomationControllerProtocol,
        epoch: UInt64
    ) async throws -> RemoteScreenFrame {
        if let captureSlot {
            return try await captureSlot.task.value
        }
        nextCaptureID &+= 1
        let captureID = nextCaptureID
        let task = Task { @MainActor [weak self, core] () throws -> RemoteScreenFrame in
            let frame = try await core.captureScreen()
            guard let self else { throw CancellationError() }
            try self.requireEpoch(epoch)
            try self.installScreen(frame)
            return frame
        }
        captureSlot = CaptureSlot(id: captureID, task: task)
        do {
            let frame = try await task.value
            clearCapture(id: captureID)
            return frame
        } catch {
            clearCapture(id: captureID)
            throw error
        }
    }

    private func settleCapture() async {
        guard let captureSlot else { return }
        _ = try? await captureSlot.task.value
        clearCapture(id: captureSlot.id)
    }

    private func clearCapture(id: UInt64) {
        guard captureSlot?.id == id else { return }
        captureSlot = nil
    }

    private func startScreenPolling(epoch: UInt64) {
        stopScreenPolling()
        pollingTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                do {
                    try await Task.sleep(nanoseconds: 300_000_000)
                } catch {
                    return
                }
                guard let self,
                      epoch == self.sessionEpoch,
                      self.currentState.connection.isConnected,
                      self.exclusiveOperation == nil,
                      self.screenPauseDepth == 0,
                      let core = self.core else { continue }
                do {
                    _ = try await self.captureShared(core: core, epoch: epoch)
                } catch is CancellationError {
                    return
                } catch {
                    azimuthRadioCoreLog.error(
                        "[Azimuth Core] Screen polling capture failed type=\(azimuthRadioErrorIdentity(error), privacy: .public) detail=\(error.localizedDescription, privacy: .private)"
                    )
                    await self.handleScreenFailure(error, epoch: epoch)
                }
            }
        }
    }

    private func stopScreenPolling() {
        pollingTask?.cancel()
        pollingTask = nil
    }

    private func waitForUSBCATRecovery(
        epoch: UInt64,
        outcome: DvGatewayRecoveryOutcome,
        expectedRadioSerialNumber: String
    ) async throws {
        let deadline = ContinuousClock.now.advanced(by: catRecoveryWindow)
        var lastObservation = "USB-C had not reopened yet."

        while ContinuousClock.now < deadline {
            try Task.checkCancellation()
            try requireEpoch(epoch)
            await transport.close()
            do {
                try requireEpoch(epoch)
                if let sameRadioBluetoothSelector {
                    do {
                        try await sameRadioBluetoothSelector.selectUSBForRecovery(
                            expectedSerialNumber: expectedRadioSerialNumber
                        )
                    } catch let error as AzimuthRadioSelectionError {
                        if case .expectedUSBRadioUnavailable = error {
                            lastObservation = "USB radio \(expectedRadioSerialNumber) had not re-enumerated yet."
                            try await Task.sleep(for: catRecoveryPollInterval)
                            continue
                        }
                        throw RadioControllerError.operationFailed(
                            error.localizedDescription
                        )
                    }
                }
                try await transport.open()
                try requireEpoch(epoch)
                let observedSerialNumber = await transport.hardwareSerialNumber
                try requireEpoch(epoch)
                if let observedSerialNumber,
                   observedSerialNumber != expectedRadioSerialNumber {
                    throw RadioControllerError.operationFailed(
                        "A different USB radio appeared during CAT recovery. Expected \(expectedRadioSerialNumber), found \(observedSerialNumber)."
                    )
                }
                guard observedSerialNumber == expectedRadioSerialNumber else {
                    lastObservation = "USB-C reopened without a stable radio serial number."
                    await transport.close()
                    try requireEpoch(epoch)
                    try await Task.sleep(for: catRecoveryPollInterval)
                    try requireEpoch(epoch)
                    continue
                }
                let mode = try await runRadioModePreflight(
                    allowsPacketModeRecovery: false
                )
                try Task.checkCancellation()
                try requireEpoch(epoch)
                switch mode {
                case .cat:
                    azimuthRadioCoreLog.notice(
                        "[Azimuth Radio] USB CAT returned after MMDVM recovery"
                    )
                    await transport.close()
                    try requireEpoch(epoch)
                    return
                case .mmdvm:
                    lastObservation = "USB-C was still carrying MMDVM data."
                case .unresponsive:
                    lastObservation = "USB-C reopened but did not answer CAT."
                }
            } catch is CancellationError {
                if epoch == sessionEpoch {
                    await transport.close()
                }
                throw CancellationError()
            } catch let error as RadioControllerError {
                if epoch == sessionEpoch {
                    await transport.close()
                }
                throw error
            } catch {
                lastObservation = Self.describe(error)
            }
            try requireEpoch(epoch)
            await transport.close()
            try requireEpoch(epoch)

            guard ContinuousClock.now < deadline else { break }
            try await Task.sleep(for: catRecoveryPollInterval)
        }

        let seconds = max(Int64(1), catRecoveryWindow.components.seconds)
        let menuState = switch outcome {
        case .changedRadioRebooting: "Menu 650 was changed to Off"
        case .alreadyOffCatReady: "Menu 650 was already Off"
        }
        throw RadioControllerError.operationFailed(
            "\(menuState), but USB CAT did not return during the \(seconds)-second recovery window. \(lastObservation) Power-cycle the radio before retrying."
        )
    }

    private func waitForBluetoothCATRecovery(
        epoch: UInt64,
        outcome: DvGatewayUsbRoutingOutcome,
        expectedRadioSerialNumber: String
    ) async throws {
        let deadline = ContinuousClock.now.advanced(by: catRecoveryWindow)
        var lastObservation = "Bluetooth had not reopened yet."

        while ContinuousClock.now < deadline {
            try Task.checkCancellation()
            try requireEpoch(epoch)
            await transport.close()
            do {
                try requireEpoch(epoch)
                try await transport.open()
                try requireEpoch(epoch)
                try await requireExpectedRadioIdentity(
                    expectedRadioSerialNumber,
                    stage: "the post-routing Bluetooth reopen"
                )
                let mode = try await runRadioModePreflight(
                    allowsPacketModeRecovery: false
                )
                try Task.checkCancellation()
                try requireEpoch(epoch)
                switch mode {
                case .cat:
                    azimuthRadioCoreLog.notice(
                        "[Azimuth Radio] Bluetooth CAT returned after DV Gateway routing"
                    )
                    await transport.close()
                    try requireEpoch(epoch)
                    return
                case .mmdvm:
                    lastObservation = "Bluetooth was still carrying MMDVM data."
                case .unresponsive:
                    lastObservation = "Bluetooth reopened but did not answer CAT."
                }
            } catch is CancellationError {
                if epoch == sessionEpoch {
                    await transport.close()
                }
                throw CancellationError()
            } catch let error as RadioControllerError {
                if epoch == sessionEpoch {
                    await transport.close()
                }
                throw error
            } catch {
                lastObservation = Self.describe(error)
            }
            try requireEpoch(epoch)
            await transport.close()
            try requireEpoch(epoch)

            guard ContinuousClock.now < deadline else { break }
            try await Task.sleep(for: catRecoveryPollInterval)
        }

        let seconds = max(Int64(1), catRecoveryWindow.components.seconds)
        let routingState = switch outcome {
        case .changedRadioRebooting:
            "Menu 985 was changed to route DV Gateway to USB-C"
        case .alreadyRouted:
            "Menu 985 was already routed to USB-C"
        }
        throw RadioControllerError.operationFailed(
            "\(routingState), but Bluetooth CAT did not return during the \(seconds)-second recovery window. \(lastObservation) Power-cycle the radio before reconnecting Bluetooth."
        )
    }

    /// Await an uncancelled core shutdown while exposing ownership to
    /// `disconnect()`. The byte transport cannot be opened by another core until
    /// this slot has completed.
    private func closeConnectedCoreForTransportHandoff(
        _ closingCore: any AutomationControllerProtocol,
        epoch: UInt64
    ) async throws {
        let closeID = nextConnectedCATCoreCloseID
        nextConnectedCATCoreCloseID &+= 1
        let closeTask = Task { try await closingCore.close() }
        connectedCatCoreCloseSlot = ConnectedCATCoreCloseSlot(
            id: closeID,
            task: closeTask
        )
        do {
            try await closeTask.value
        } catch {
            clearConnectedCATCoreCloseSlot(id: closeID)
            throw error
        }
        clearConnectedCATCoreCloseSlot(id: closeID)
        try requireEpoch(epoch)
    }

    private func requireIFDSPUSBHandoff(_ generation: UInt64) throws {
        guard generation == ifDSPUSBHandoffGeneration,
              !disconnectInProgress else {
            throw CancellationError()
        }
        try Task.checkCancellation()
    }

    private func requireAPRSRecovery(_ generation: UInt64) throws {
        guard generation == aprsRecoveryGeneration,
              !disconnectInProgress else {
            throw CancellationError()
        }
        try Task.checkCancellation()
    }

    /// Reopen only the endpoint retained by the refused APRS start. USB refreshes
    /// the selected endpoint through descriptor-neutral physical continuity
    /// hints when its tty path changes; Bluetooth is already rebound to its
    /// exact paired address and expected CAT serial before this loop begins.
    /// Every successful candidate must still prove the approved `AE` identity
    /// in the newly created automation core. Poll attempts use CAT-only
    /// preflight and never send packet-mode recovery bytes, so an early-open
    /// endpoint cannot overwrite the Menu 506 band returned by the approved
    /// MCP operation.
    private func waitForSameEndpointAfterAPRSRecovery(
        proof: APRSDVGatewayRecoveryProof,
        recoveryGeneration: UInt64,
        outcome: DvGatewayRecoveryOutcome
    ) async throws -> APRSDVGatewayRecoveryProof {
        let deadline = ContinuousClock.now.advanced(by: catRecoveryWindow)
        var lastObservation = "The selected endpoint had not returned to CAT yet."

        while ContinuousClock.now < deadline {
            try requireAPRSRecovery(recoveryGeneration)
            guard Self.aprsRecoveryDeviceMatches(
                transport.device,
                retainedDevice: proof.device
            ) else {
                throw RadioControllerError.operationFailed(
                    "The selected endpoint changed during APRS recovery. Azimuth refused to open a different endpoint."
                )
            }
            if proof.device.connectionKind == .usb {
                guard let sameRadioUSBSelector else {
                    throw RadioControllerError.capabilityUnavailable(
                        "The selected USB-C transport cannot follow the same endpoint across radio re-enumeration."
                    )
                }
                guard try await sameRadioUSBSelector
                    .refreshSelectedUSBForSameRadioRecovery() else {
                    lastObservation = "The retained USB-C endpoint had not re-enumerated yet."
                    try requireAPRSRecovery(recoveryGeneration)
                    try await Task.sleep(for: catRecoveryPollInterval)
                    continue
                }
                try requireAPRSRecovery(recoveryGeneration)
                guard transport.device.connectionKind == .usb else {
                    throw RadioControllerError.operationFailed(
                        "USB-C discovery changed the selected endpoint during APRS recovery. Azimuth refused to open it."
                    )
                }
            }
            do {
                try await connect(
                    expectedRadioSerialNumber: proof.radioSerialNumber,
                    allowsPacketModeRecovery: false
                )
                try requireAPRSRecovery(recoveryGeneration)
                guard Self.aprsRecoveryDeviceMatches(
                          transport.device,
                          retainedDevice: proof.device
                      ),
                      currentRadioSerialNumber == proof.radioSerialNumber,
                      core?.abi().radioSerialNumber == proof.radioSerialNumber else {
                    throw RadioControllerError.operationFailed(
                        "The recovered connection did not retain the exact selected endpoint and approved CAT radio identity."
                    )
                }
                azimuthRadioCoreLog.notice(
                    "[Azimuth Radio] Same endpoint and CAT radio proved after APRS current-mode recovery"
                )
                return APRSDVGatewayRecoveryProof(
                    configuration: proof.configuration,
                    kissInterfaceRawValue: proof.kissInterfaceRawValue,
                    radioSerialNumber: proof.radioSerialNumber,
                    device: transport.device,
                    sessionEpoch: sessionEpoch
                )
            } catch let identityMismatch as ExpectedCATRadioIdentityMismatch {
                throw identityMismatch
            } catch is CancellationError {
                throw CancellationError()
            } catch let error as RadioControllerError {
                switch error {
                case .usbMmdvmMode, .bluetoothMmdvmMode:
                    lastObservation = "The selected endpoint was still carrying DV Gateway packet data."
                case .operationFailed(let detail),
                     .capabilityUnavailable(let detail):
                    lastObservation = detail
                default:
                    lastObservation = error.localizedDescription
                }
            } catch {
                lastObservation = Self.describe(error)
            }

            try requireAPRSRecovery(recoveryGeneration)
            await transport.close()
            try requireAPRSRecovery(recoveryGeneration)
            guard Self.aprsRecoveryDeviceMatches(
                transport.device,
                retainedDevice: proof.device
            ) else {
                throw RadioControllerError.operationFailed(
                    "The selected endpoint changed while Azimuth was waiting for CAT to return."
                )
            }
            guard ContinuousClock.now < deadline else { break }
            try await Task.sleep(for: catRecoveryPollInterval)
        }

        let seconds = max(Int64(1), catRecoveryWindow.components.seconds)
        throw RadioControllerError.operationFailed(
            "\(Self.connectedCATDisableOutcomeDescription(outcome)), but the same selected \(proof.device.connection) endpoint and CAT radio did not return during the \(seconds)-second recovery window. \(lastObservation) Leave the radio connected and retry after it finishes restarting."
        )
    }

    private static func aprsRecoveryDeviceMatches(
        _ currentDevice: AzimuthRadioDevice,
        retainedDevice: AzimuthRadioDevice
    ) -> Bool {
        if retainedDevice.connectionKind == .usb {
            return currentDevice.connectionKind == .usb
        }
        return currentDevice == retainedDevice
    }

    /// Release a successfully connected intermediate USB core before restoring
    /// Bluetooth or reporting that a required physical USB proof is unavailable.
    private func releaseCurrentCoreForIFDSPHandoff() async throws {
        stopScreenPolling()
        stopAPRSPolling()
        captureSlot?.task.cancel()
        captureSlot = nil
        let closingCore = core
        core = nil
        currentRadioSerialNumber = nil
        currentIFDSPUSBInputProof = nil
        settingSnapshotID = nil
        ifDSPModeState = .inactive
        sessionEpoch &+= 1
        let epoch = sessionEpoch
        if let closingCore {
            try await closeConnectedCoreForTransportHandoff(
                closingCore,
                epoch: epoch
            )
        }
        await transport.close()
        try requireEpoch(epoch)
        publish(.disconnected)
    }

    /// Poll USB discovery through the full radio reboot/re-enumeration window.
    /// Selection uses only retained physical hints. Success requires the final
    /// USB automation core to prove the approved Bluetooth CAT `AE` serial and
    /// publish a current USB-device/audio ancestor proof.
    private func waitForRetainedUSBCAfterDVGatewayDisable(
        selector: any AzimuthIFDSPUSBSelecting,
        handoffGeneration: UInt64,
        outcome: DvGatewayRecoveryOutcome,
        expectedRadioSerialNumber: String
    ) async throws {
        let deadline = ContinuousClock.now.advanced(by: catRecoveryWindow)
        var lastObservation = "The retained USB-C endpoint had not re-enumerated yet."

        while ContinuousClock.now < deadline {
            try requireIFDSPUSBHandoff(handoffGeneration)
            await transport.close()
            do {
                guard try await selector.selectRetainedIFDSPUSBEndpoint() else {
                    lastObservation = "The retained USB-C endpoint had not re-enumerated yet."
                    throw IFDSPUSBRetryObservation()
                }
                try requireIFDSPUSBHandoff(handoffGeneration)
                try await connect(
                    expectedRadioSerialNumber: expectedRadioSerialNumber,
                    allowsPacketModeRecovery: false
                )
                try requireIFDSPUSBHandoff(handoffGeneration)
                guard transport.device.connectionKind == .usb,
                      currentRadioSerialNumber == expectedRadioSerialNumber else {
                    throw RadioControllerError.operationFailed(
                        "The post-reboot connection did not remain on the retained USB-C endpoint."
                    )
                }
                guard currentIFDSPUSBInputProof?.catSerialNumber
                        == expectedRadioSerialNumber else {
                    try await releaseCurrentCoreForIFDSPHandoff()
                    throw IFDSPUSBInputProofUnavailable(
                        expectedRadioSerialNumber: expectedRadioSerialNumber
                    )
                }
                azimuthRadioCoreLog.notice(
                    "[Azimuth Radio] Same-radio USB CAT proved after Menu 650 recovery"
                )
                return
            } catch is IFDSPUSBRetryObservation {
                // Absence during reboot is expected and remains retryable.
            } catch let identityMismatch as ExpectedCATRadioIdentityMismatch {
                throw identityMismatch
            } catch let proofError as IFDSPUSBInputProofUnavailable {
                throw proofError
            } catch is CancellationError {
                throw CancellationError()
            } catch let error as RadioControllerError {
                switch error {
                case .usbMmdvmMode:
                    lastObservation = "USB-C was still carrying MMDVM data."
                case .operationFailed(let detail):
                    lastObservation = detail
                default:
                    lastObservation = error.localizedDescription
                }
            } catch {
                lastObservation = Self.describe(error)
            }
            try requireIFDSPUSBHandoff(handoffGeneration)
            await transport.close()
            try requireIFDSPUSBHandoff(handoffGeneration)

            guard ContinuousClock.now < deadline else { break }
            try await Task.sleep(for: catRecoveryPollInterval)
        }

        let seconds = max(Int64(1), catRecoveryWindow.components.seconds)
        throw RadioControllerError.operationFailed(
            "\(Self.connectedCATDisableOutcomeDescription(outcome)), but same-radio USB-C CAT did not return during the \(seconds)-second recovery window. \(lastObservation) Leave the radio connected over USB-C and retry after it finishes restarting."
        )
    }

    private func requireExpectedRadioIdentity(
        _ expectedSerialNumber: String?,
        stage: String
    ) async throws {
        guard let expectedSerialNumber else { return }
        // A TH-D75 USB CDC endpoint may omit iSerial or expose a descriptor
        // value unrelated to CAT `AE`. USB identity is proved only after the
        // automation core reads `AE` from this exact opened byte stream.
        guard transport.device.connectionKind != .usb else { return }
        let actualSerialNumber = await transport.hardwareSerialNumber
        guard actualSerialNumber == expectedSerialNumber else {
            throw RadioControllerError.operationFailed(
                "The radio identity changed during CAT recovery after \(stage). Expected \(expectedSerialNumber), found \(actualSerialNumber ?? "no stable serial number")."
            )
        }
    }

    private func startAPRSPolling(epoch: UInt64) {
        stopAPRSPolling()
        aprsPollingTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                do {
                    try await Task.sleep(nanoseconds: 200_000_000)
                } catch {
                    return
                }
                guard let self,
                      epoch == self.sessionEpoch,
                      self.currentState.connection.isConnected,
                      self.currentAPRSState.status.phase == .active,
                      let core = self.core else { continue }

                self.installAPRSSnapshot(
                    core.aprsSnapshot(
                        afterSequence: self.currentAPRSState.latestSequence
                    ),
                    retainingHistory: true
                )
                if self.currentAPRSState.status.phase == .failed {
                    let detail = self.currentAPRSState.status.lastError
                        ?? "The KISS packet stream ended unexpectedly."
                    await self.transitionToLostConnection(message: detail, epoch: epoch)
                    return
                }
            }
        }
    }

    private func stopAPRSPolling() {
        aprsPollingTask?.cancel()
        aprsPollingTask = nil
    }

    private func pauseScreenStream() {
        screenPauseDepth += 1
    }

    private func resumeScreenStream() {
        screenPauseDepth = max(0, screenPauseDepth - 1)
    }

    // MARK: - Failure recovery

    private func restoreCATWorkspace(
        core: any AutomationControllerProtocol,
        epoch: UInt64
    ) async throws {
        try requireEpoch(epoch)
        settingSnapshotID = nil

        var restoring = currentState
        restoring.settingValues = [:]
        restoring.capabilities = RadioCapabilities(
            screenStreaming: .preparing,
            frontPanelControl: .preparing,
            settingRead: .available,
            settingWrite: .unavailable(
                reason: "Read the radio settings before writing."
            )
        )
        restoring.telemetry.operatingMode = "Automation ABI \(core.abi().version)"
        publish(restoring)

        do {
            _ = try await captureFresh(core: core, epoch: epoch)
        } catch {
            await handleScreenFailure(error, epoch: epoch)
        }
        await refreshCurrentIFDSPUSBInputProof(core: core, epoch: epoch)
        if epoch == sessionEpoch, currentState.connection.isConnected {
            startScreenPolling(epoch: epoch)
        }
    }

    private func recoverCATAfterAPRSError(
        core: any AutomationControllerProtocol,
        epoch: UInt64,
        originalError: Error
    ) async {
        if Self.aprsCurrentModeDetail(originalError) != nil {
            // The aligned current-mode result is an in-session refusal. The
            // same automation actor and CAT attestation remain valid, so do
            // not delay consent behind another synchronous screen capture.
            // Revoke every stale UI lease,
            // publish guarded CAT availability, and let ordinary polling obtain
            // the next authenticated screen after this operation releases its
            // exclusive owner.
            restoreCATWorkspaceAfterAlignedAPRSRefusal(
                core: core,
                epoch: epoch
            )
            return
        }
        if Self.isTerminalAutomationRestoration(originalError) {
            // A typed restoration failure is emitted only after the Rust
            // actor cannot re-establish automation control. The actor then
            // closes by design, so issuing a settings read here can only
            // replace the useful failure with a second "controller ended"
            // error.
            await transitionToLostConnection(
                message: Self.describe(originalError),
                epoch: epoch
            )
            return
        }
        do {
            // A typed current-mode refusal retains the already qualified CAT
            // actor. Other failed KISS entries may finish a follow-up CAT
            // qualification before this call returns. In either case, the
            // workspace is republished only through the actor that returned
            // the error and only while its session epoch is still current.
            try await restoreCATWorkspace(core: core, epoch: epoch)
            if currentAPRSState.status.phase == .failed {
                var recovered = currentAPRSState
                recovered.status.phase = .inactive
                publishAPRS(recovered)
            }
        } catch {
            let detail = "\(Self.describe(originalError)) CAT recovery also failed: \(Self.describe(error))"
            await transitionToLostConnection(message: detail, epoch: epoch)
        }
    }

    private func restoreCATWorkspaceAfterAlignedAPRSRefusal(
        core: any AutomationControllerProtocol,
        epoch: UInt64
    ) {
        guard epoch == sessionEpoch,
              self.core === core,
              currentState.connection.isConnected else { return }
        settingSnapshotID = nil
        var restored = currentState
        restored.screenFrame = nil
        restored.settingValues = [:]
        restored.capabilities = RadioCapabilities(
            screenStreaming: .preparing,
            frontPanelControl: .unavailable(
                reason: "Waiting for a fresh authenticated radio screen."
            ),
            settingRead: .available,
            settingWrite: .unavailable(
                reason: "Read the radio settings before writing."
            )
        )
        restored.telemetry.operatingMode = "Automation ABI \(core.abi().version)"
        restored.lastScreenError = nil
        publish(restored)
        startScreenPolling(epoch: epoch)
    }

    private static func isTerminalAutomationRestoration(_ error: Error) -> Bool {
        if case AutomationError.AutomationRestoration = error { return true }
        return false
    }

    private func recaptureAfterSettings(
        core: any AutomationControllerProtocol,
        epoch: UInt64
    ) async {
        do {
            _ = try await captureFresh(core: core, epoch: epoch)
        } catch {
            azimuthRadioCoreLog.error(
                "[Azimuth Core] Post-settings screen refresh failed type=\(azimuthRadioErrorIdentity(error), privacy: .public) detail=\(error.localizedDescription, privacy: .private)"
            )
            await handleScreenFailure(error, epoch: epoch)
        }
    }

    private func refreshAfterFailedApply(
        core: any AutomationControllerProtocol,
        epoch: UInt64
    ) async {
        settingSnapshotID = nil
        do {
            let fresh = try await core.readSettingValues(settingIds: nil)
            try requireEpoch(epoch)
            try installSettings(fresh)
            setSettingCapabilities(.available, .available)
        } catch {
            if epoch == sessionEpoch {
                setSettingCapabilities(
                    .unavailable(reason: Self.describe(error)),
                    .unavailable(reason: "Read the radio again before approving another change.")
                )
            }
        }
        await recaptureAfterSettings(core: core, epoch: epoch)
        await refreshCurrentIFDSPUSBInputProof(core: core, epoch: epoch)
    }

    /// Refresh the CAT-to-USB-audio join after any core operation which may
    /// close/reopen CDC or re-enumerate the radio. Uncertainty clears authority;
    /// it never preserves a registry ID from an earlier USB enumeration.
    private func refreshCurrentIFDSPUSBInputProof(
        core: any AutomationControllerProtocol,
        epoch: UInt64
    ) async {
        guard epoch == sessionEpoch else { return }
        guard transport.device.connectionKind == .usb,
              let currentRadioSerialNumber,
              core.abi().radioSerialNumber == currentRadioSerialNumber else {
            currentIFDSPUSBInputProof = nil
            return
        }
        let registryEntryID = await transport.macOSUSBDeviceRegistryEntryID
        guard epoch == sessionEpoch else { return }
        #if os(macOS)
        guard let registryEntryID, registryEntryID != 0 else {
            currentIFDSPUSBInputProof = nil
            return
        }
        #endif
        currentIFDSPUSBInputProof = try? IFDSPUSBInputProof(
            catSerialNumber: currentRadioSerialNumber,
            macOSUSBDeviceRegistryEntryID: registryEntryID
        )
    }

    private func handleScreenFailure(_ error: Error, epoch: UInt64) async {
        guard epoch == sessionEpoch else { return }
        let transportState = await transport.state
        let fatalCoreError: Bool
        switch error {
        case AutomationError.ControllerClosed, AutomationError.UsbTransport:
            fatalCoreError = true
        default:
            fatalCoreError = false
        }
        switch transportState {
        case .disconnected, .failed:
            await transitionToLostConnection(message: Self.describe(error), epoch: epoch)
            return
        case .connecting, .connected:
            break
        }
        consecutiveScreenFailures += 1
        if fatalCoreError || consecutiveScreenFailures >= 3 {
            await transitionToLostConnection(message: Self.describe(error), epoch: epoch)
            return
        }
        var state = currentState
        state.lastScreenError = Self.describe(error)
        state.capabilities.screenStreaming = .unavailable(reason: Self.describe(error))
        state.capabilities.frontPanelControl = .unavailable(
            reason: "A fresh authenticated screen is required before sending input."
        )
        publish(state)
    }

    private func transitionToLostConnection(message: String, epoch: UInt64) async {
        guard epoch == sessionEpoch else { return }
        azimuthRadioCoreLog.error(
            "[Azimuth Radio] Connection lost: \(message, privacy: .public)"
        )
        sessionEpoch &+= 1
        aprsRecoveryGeneration &+= 1
        pendingAPRSDVGatewayRecovery = nil
        stopScreenPolling()
        stopAPRSPolling()
        captureSlot?.task.cancel()
        captureSlot = nil
        screenPauseDepth = 0
        let failedCore = core
        core = nil
        currentRadioSerialNumber = nil
        currentIFDSPUSBInputProof = nil
        settingSnapshotID = nil
        ifDSPModeState = .inactive
        if let failedCore {
            try? await failedCore.close()
        }
        await transport.close()
        await ifDSPUSBSelector?.finishRetainedIFDSPUSBHandoff()
        var failed = RadioWorkspaceState.disconnected
        failed.connection = .failed(message: message)
        publish(failed)
        if currentAPRSState.status.phase != .failed {
            publishAPRSUnavailable(
                "APRS is unavailable because the radio connection was lost."
            )
        }
    }

    // MARK: - Operation and state helpers

    private struct ConnectedContext {
        let core: any AutomationControllerProtocol
        let epoch: UInt64
    }

    private func beginConnectedOperation(_ name: String) throws -> ConnectedContext {
        try beginExclusive(name)
        guard currentState.connection.isConnected, let core else {
            endExclusive(name)
            throw RadioControllerError.capabilityUnavailable(
                "Connect the TH-D75 before using \(name)."
            )
        }
        guard !currentAPRSState.status.phase.ownsSerialLink else {
            endExclusive(name)
            throw RadioControllerError.capabilityUnavailable(
                "Stop APRS KISS and wait for automation restoration before using \(name)."
            )
        }
        guard !ifDSPModeState.reservesRadioState else {
            endExclusive(name)
            throw RadioControllerError.capabilityUnavailable(
                "Stop IF-DSP and restore the saved radio state before using \(name)."
            )
        }
        pauseScreenStream()
        return ConnectedContext(core: core, epoch: sessionEpoch)
    }

    private func beginAPRSOperation(_ name: String) throws -> ConnectedContext {
        try beginExclusive(name)
        guard currentState.connection.isConnected, let core else {
            endExclusive(name)
            throw RadioControllerError.capabilityUnavailable(
                "Connect the TH-D75 before using \(name)."
            )
        }
        guard !ifDSPModeState.reservesRadioState else {
            endExclusive(name)
            throw RadioControllerError.capabilityUnavailable(
                "Restore the IF-DSP radio session before using \(name)."
            )
        }
        pauseScreenStream()
        return ConnectedContext(core: core, epoch: sessionEpoch)
    }

    private func requireActiveAPRS() throws {
        guard currentAPRSState.status.phase == .active else {
            throw RadioControllerError.capabilityUnavailable(
                "Start an APRS KISS session before transmitting a packet."
            )
        }
        guard currentAPRSState.status.configuration?.isReceiveOnly == false else {
            throw RadioControllerError.capabilityUnavailable(
                "Set a valid source callsign and restart APRS before transmitting."
            )
        }
    }

    private func endConnectedOperation(_ name: String) {
        resumeScreenStream()
        endExclusive(name)
    }

    private func beginExclusive(_ name: String) throws {
        guard exclusiveOperation == nil else {
            throw RadioControllerError.capabilityUnavailable(
                "Wait for \(exclusiveOperation ?? "the current radio operation") to finish."
            )
        }
        exclusiveOperation = name
    }

    private func endExclusive(_ name: String) {
        guard exclusiveOperation == name else { return }
        exclusiveOperation = nil
    }

    private func requireEpoch(_ epoch: UInt64) throws {
        guard epoch == sessionEpoch else { throw CancellationError() }
    }

    private func clearConnectionSlot(id: UInt64) {
        guard connectionSlot?.id == id else { return }
        connectionSlot = nil
    }

    private func clearPreflightSlot(id: UInt64) {
        guard preflightSlot?.id == id else { return }
        preflightSlot = nil
    }

    private func clearUSBMMDVMRecoverySlot(id: UInt64) {
        guard usbMmdvmRecoverySlot?.id == id else { return }
        usbMmdvmRecoverySlot = nil
    }

    private func clearBluetoothMMDVMRoutingSlot(id: UInt64) {
        guard bluetoothMmdvmRoutingSlot?.id == id else { return }
        bluetoothMmdvmRoutingSlot = nil
    }

    private func clearConnectedCATDisableSlot(id: UInt64) {
        guard connectedCatDisableSlot?.id == id else { return }
        connectedCatDisableSlot = nil
    }

    private func clearConnectedCATAPRSRecoverySlot(id: UInt64) {
        guard connectedCatAPRSRecoverySlot?.id == id else { return }
        connectedCatAPRSRecoverySlot = nil
    }

    private func clearConnectedCATCoreCloseSlot(id: UInt64) {
        guard connectedCatCoreCloseSlot?.id == id else { return }
        connectedCatCoreCloseSlot = nil
    }

    private func setSettingCapabilities(
        _ read: RadioCapabilityState,
        _ write: RadioCapabilityState
    ) {
        var state = currentState
        state.capabilities.settingRead = read
        state.capabilities.settingWrite = write
        publish(state)
    }

    private func invalidateSettingsSnapshot(writeReason: String) {
        settingSnapshotID = nil
        var state = currentState
        state.settingValues = [:]
        state.capabilities.settingWrite = .unavailable(reason: writeReason)
        publish(state)
    }

    private func publish(_ state: RadioWorkspaceState) {
        currentState = state
        updateContinuation.yield(state)
    }

    private static func coreKey(_ key: RadioFrontPanelKey) -> FrontPanelKey {
        switch key {
        case .mode: return .mode
        case .menu: return .menu
        case .ab: return .ab
        case .function: return .function
        case .monitor: return .monitor
        case .up: return .up
        case .down: return .down
        case .left: return .left
        case .right: return .right
        case .enter: return .enter
        case .mark0: return .mark0
        case .vfo1: return .vfo1
        case .mr2: return .mr2
        case .call3: return .call3
        case .msg4: return .msg4
        case .list5: return .list5
        case .beacon6: return .beacon6
        case .reverse7: return .reverse7
        case .tone8: return .tone8
        case .pf1_9: return .pf19
        case .mhzStar: return .mhzStar
        case .pf2Hash: return .pf2Hash
        case .micPf1: return .micPf1
        case .micPf2: return .micPf2
        case .micPf3: return .micPf3
        }
    }

    private static func activeIFDSPStatus(
        _ status: IfDspRadioStatus
    ) throws -> IFDSPRadioModeStatus {
        guard status.phase == .active,
              let frequencyHz = status.bandBFrequencyHz else {
            throw RadioControllerError.operationFailed(
                "The core did not return a verified active Band-B IF session."
            )
        }
        return IFDSPRadioModeStatus(
            bandBFrequencyHz: frequencyHz,
            ifCenterHz: status.ifCenterHz
        )
    }

    /// Reconcile actor ownership after an error whose Swift task may have
    /// been cancelled while UniFFI continued awaiting the Rust future.
    /// Cancellation never races a speculative restore ahead of prepare: the
    /// serialized status query first waits for the in-flight transition.
    private func reconcileIFDSPReservation(
        core: any AutomationControllerProtocol,
        after error: Error
    ) async -> Bool {
        var status = try? await core.ifDspStatus()
        if error is CancellationError {
            let mightReserveRadio = status.map {
                $0.phase != .inactive
            } ?? true
            if mightReserveRadio {
                _ = try? await core.restoreIfDsp()
                status = try? await core.ifDspStatus()
            }
        }
        if let status {
            return status.phase != .inactive
        }
        return Self.ifDSPRestorationIsPending(error) || error is CancellationError
    }

    private static func ifDSPRestorationIsPending(_ error: Error) -> Bool {
        switch error {
        case AutomationError.IfDspRestoration, AutomationError.IfDspModeActive:
            return true
        default:
            return false
        }
    }

    private static func ifDSPPresentationError(_ error: Error) -> RadioControllerError {
        if case AutomationError.IfDspCurrentModeUnavailable(let detail) = error {
            azimuthRadioCoreLog.notice(
                "IF-DSP was refused in the current radio mode after exact rollback: \(detail, privacy: .public)"
            )
            return .ifDspCurrentModeUnavailable
        }
        return .operationFailed(Self.describe(error))
    }

    private static func describe(_ error: Error) -> String {
        switch error {
        case AutomationError.UsbTransport(let detail): return "Radio transport failed: \(detail)"
        case AutomationError.AutomationQualification(let detail):
            return "Automation qualification failed: \(detail)"
        case AutomationError.AutomationRestoration(let operation, let detail):
            return "\(operation) completed, but automation restoration failed: \(detail)"
        case AutomationError.ControllerClosed: return "The radio controller closed."
        case AutomationError.Internal(let detail): return "Core controller error: \(detail)"
        case AutomationError.ScreenCapture(let detail): return "Screen capture failed: \(detail)"
        case AutomationError.ScreenLeaseUnavailable:
            return "A fresh authenticated screen is required before sending input."
        case AutomationError.ScreenLeaseStale:
            return "The radio screen changed before input was sent."
        case AutomationError.GuardedInput(let detail): return "Front-panel input failed: \(detail)"
        case AutomationError.PostTapCapture(_, let detail):
            return "The key was handled, but its follow-up screen failed: \(detail)"
        case AutomationError.SettingsRead(let detail): return "Setting read failed: \(detail)"
        case AutomationError.InvalidSettingsPlan(let detail): return "Invalid setting plan: \(detail)"
        case AutomationError.SettingsSnapshotUnavailable:
            return "The reviewed setting snapshot expired. Refresh and review the changes again."
        case AutomationError.SettingPreconditionFailed(let settingID, _, _):
            return "\(settingID) changed after review. Refresh and review the plan again."
        case AutomationError.SettingsSnapshotStale(let detail):
            return "The radio settings changed after review; nothing was written. \(detail)"
        case AutomationError.SettingsApply(let detail): return "Setting apply failed: \(detail)"
        case AutomationError.AprsModeActive:
            return "APRS KISS owns the radio control link. Stop APRS before using CAT controls."
        case AutomationError.AprsModeInactive:
            return "Start an APRS KISS session before using this packet operation."
        case AutomationError.InvalidAprsConfiguration(let detail):
            return "Invalid APRS configuration: \(detail)"
        case AutomationError.AprsStartAuthority(let detail):
            return "APRS start was refused because its reviewed settings authority is no longer valid. \(detail)"
        case AutomationError.AprsCurrentModeUnavailable:
            return "The TH-D75 refused KISS in its current operating mode."
        case AutomationError.AprsOperation(let detail):
            return "APRS operation failed: \(detail)"
        case AutomationError.IfDspModeActive:
            return "IF-DSP owns a saved radio state. Stop IF-DSP before using conflicting controls."
        case AutomationError.IfDspModeInactive:
            return "Prepare the radio for IF-DSP before using this operation."
        case AutomationError.IfDspCurrentModeUnavailable:
            return RadioControllerError.ifDspCurrentModeUnavailable.localizedDescription
        case AutomationError.IfDspOperation(let detail):
            return "IF-DSP radio operation failed: \(detail)"
        case AutomationError.IfDspRestoration(let detail):
            return "IF-DSP could not restore and verify every saved radio value: \(detail)"
        case AutomationError.Shutdown(let detail): return "Radio shutdown failed: \(detail)"
        case DvGatewayRecoveryError.Cancelled:
            return "Automatic CAT recovery was cancelled before Menu 650 could be changed."
        case DvGatewayRecoveryError.OperationAlreadyRun:
            return "This automatic CAT recovery operation has already finished. Start a new recovery attempt."
        case DvGatewayRecoveryError.UnsupportedPlatform:
            return "Automatic USB MMDVM-to-CAT recovery requires Azimuth for macOS."
        case DvGatewayRecoveryError.BluetoothUnavailable(let detail):
            return "Azimuth could not open the paired TH-D75 Bluetooth control link. \(detail)"
        case DvGatewayRecoveryError.UsbIdentityUnavailable(let detail):
            return "Azimuth could not prove a stable serial identity for the USB radio. \(detail)"
        case DvGatewayRecoveryError.BluetoothIdentityUnavailable(let detail):
            return "Azimuth opened Bluetooth but could not read that radio's serial identity. \(detail)"
        case DvGatewayRecoveryError.RadioIdentityMismatch(let expected, let actual):
            return "Azimuth refused to change Menu 650 because Bluetooth reached radio \(actual), but USB-C identified radio \(expected)."
        case DvGatewayRecoveryError.RadioOperation(let detail):
            return "Azimuth could not turn Menu 650 (DV Gateway) off. \(detail)"
        case DvGatewayRecoveryError.OutcomeUncertain(let detail):
            return "The Menu 650 write outcome is uncertain. \(detail) Power-cycle the radio and inspect Menu 650 before retrying."
        case DvGatewayCatDisableError.Cancelled:
            return "IF-DSP DV Gateway recovery was cancelled before Menu 650 could be changed."
        case DvGatewayCatDisableError.ControllerUnavailable(let detail):
            return "The authenticated Bluetooth radio session ended before Azimuth could inspect Menu 650. \(detail) No radio setting was changed."
        case DvGatewayCatDisableError.InvalidExpectedRadioSerial(let detail):
            return "Azimuth refused to open the Menu 650 mutation gate because the approved CAT session serial is invalid. \(detail) No radio setting was changed."
        case DvGatewayCatDisableError.CatIdentityUnavailable(let detail):
            return "Azimuth could not read the selected Bluetooth radio's CAT serial identity. \(detail)"
        case DvGatewayCatDisableError.RadioIdentityMismatch(let expected, let actual):
            return "Azimuth refused to change Menu 650 because the selected Bluetooth endpoint now identifies as radio \(actual), but the approved CAT session belonged to radio \(expected). No radio setting was changed."
        case DvGatewayCatDisableError.RadioQualification(let detail):
            return "Azimuth refused to change Menu 650 because the selected Bluetooth radio did not pass exact model, firmware, and schema qualification. \(detail)"
        case DvGatewayCatDisableError.RadioOperation(let detail):
            return "Azimuth could not finish turning Menu 650 off after the mutation gate opened. \(detail) Inspect Menu 650 before retrying."
        case DvGatewayCatDisableError.OutcomeUncertain(let detail):
            return "The Menu 650 write outcome is uncertain. \(detail) Power-cycle the radio and inspect Menu 650 before retrying."
        case AprsCurrentModeRecoveryError.Cancelled:
            return "APRS recovery was cancelled before the persistent-setting mutation gate. No radio setting was changed."
        case AprsCurrentModeRecoveryError.ControllerUnavailable(let detail):
            return "The authenticated CAT actor ended before Azimuth could verify Menu 983, Menu 506, and Menu 650 together. \(detail) No radio setting was changed."
        case AprsCurrentModeRecoveryError.InvalidExpectedRadioSerial(let detail):
            return "Azimuth refused to open APRS recovery because the approved CAT serial is invalid. \(detail) No radio setting was changed."
        case AprsCurrentModeRecoveryError.InvalidExpectedKissInterface(let value):
            return "Azimuth refused to open APRS recovery because KISS route \(value) is not USB-C or Bluetooth. No radio setting was changed."
        case AprsCurrentModeRecoveryError.CatIdentityUnavailable(let detail):
            return "Azimuth could not re-read the selected endpoint's CAT serial identity. \(detail) No radio setting was changed."
        case AprsCurrentModeRecoveryError.RadioIdentityMismatch(let expected, let actual):
            return "Azimuth refused to inspect or change Menu 650 because the selected endpoint now identifies as radio \(actual), but the approved APRS session belonged to radio \(expected). No radio setting was changed."
        case AprsCurrentModeRecoveryError.RadioQualification(let detail):
            return "Azimuth refused APRS recovery because the selected radio did not pass exact model, firmware, and schema qualification. \(detail) No radio setting was changed."
        case AprsCurrentModeRecoveryError.KissInterfaceMismatch(let expected, let actual):
            return "Menu 983 now routes KISS to \(Self.kissInterfaceDescription(actual)), but the selected endpoint requires \(Self.kissInterfaceDescription(expected)). Azimuth stopped before changing Menu 650. No radio setting was changed."
        case AprsCurrentModeRecoveryError.KissInterfaceMismatchAndCleanupFailed(
            let expected,
            let actual,
            let detail
        ):
            return "Menu 983 routes KISS to \(Self.kissInterfaceDescription(actual)), but the selected endpoint requires \(Self.kissInterfaceDescription(expected)). No radio setting was changed, but CAT cleanup failed. \(detail)"
        case AprsCurrentModeRecoveryError.InvalidTncDataBand(let actual):
            return "Menu 506 returned unsupported TNC data-band value \(actual). Azimuth stopped before changing Menu 650. No radio setting was changed."
        case AprsCurrentModeRecoveryError.InvalidTncDataBandAndCleanupFailed(
            let actual,
            let detail
        ):
            return "Menu 506 returned unsupported TNC data-band value \(actual). No radio setting was changed, but CAT cleanup failed. \(detail)"
        case AprsCurrentModeRecoveryError.RadioOperation(let detail):
            return "Azimuth could not finish the approved Menu 650 operation. \(detail) Inspect Menu 650 before retrying APRS."
        case AprsCurrentModeRecoveryError.NoSettingChanged(let detail):
            return "APRS recovery did not complete, but no persistent setting write started. \(detail) Reconnect the selected endpoint before trying again."
        case AprsCurrentModeRecoveryError.CompletedButReleaseFailed(let result, let detail):
            return "\(Self.connectedCATDisableOutcomeDescription(result.outcome)); Menu 983 and Menu 506 were also verified, but Azimuth could not release the old CAT connection. \(detail) Reconnect the selected endpoint before trying again."
        case AprsCurrentModeRecoveryError.OutcomeUncertain(let detail):
            return "The approved Menu 650 outcome is uncertain. \(detail) Power-cycle the radio and inspect Menu 650 before retrying APRS."
        case DvGatewayUsbRoutingError.Cancelled:
            return "DV Gateway routing was cancelled before Menu 985 or Menu 650 could be changed."
        case DvGatewayUsbRoutingError.OperationAlreadyRun:
            return "This DV Gateway routing operation has already finished. Start a new routing attempt."
        case DvGatewayUsbRoutingError.UsbCatIdentityUnavailable(let detail):
            return "Azimuth could not read the selected USB-C radio's CAT serial identity. \(detail)"
        case DvGatewayUsbRoutingError.RadioQualification(let detail):
            return "Azimuth refused to route DV Gateway because the selected USB-C radio did not pass exact model, firmware, and schema qualification. \(detail)"
        case DvGatewayUsbRoutingError.RadioOperation(let detail):
            return "Azimuth could not finish routing DV Gateway to USB-C after the mutation gate opened. \(detail) Inspect Menu 985 and Menu 650 before retrying."
        case DvGatewayUsbRoutingError.OutcomeUncertain(let detail):
            return "The Menu 985 and Menu 650 routing outcome is uncertain. \(detail) Power-cycle the radio and inspect both settings before retrying."
        case let localized as LocalizedError:
            return localized.errorDescription ?? String(describing: error)
        default:
            return String(describing: error)
        }
    }

    private static func isPostMutationRecoveryResult(_ error: Error) -> Bool {
        switch error {
        case DvGatewayRecoveryError.RadioOperation,
             DvGatewayRecoveryError.OutcomeUncertain:
            true
        default:
            false
        }
    }

    private static func isPreMutationBluetoothRoutingResult(_ error: Error) -> Bool {
        switch error {
        case DvGatewayUsbRoutingError.Cancelled,
             DvGatewayUsbRoutingError.UsbCatIdentityUnavailable,
             DvGatewayUsbRoutingError.RadioQualification:
            true
        default:
            false
        }
    }

    private static func completedRecoveryInterruptedError(
        _ outcome: DvGatewayRecoveryOutcome
    ) -> RadioControllerError {
        switch outcome {
        case .changedRadioRebooting:
            .operationFailed(
                "Menu 650 was changed to Off and the radio is rebooting. The USB-C CAT reconnect was stopped; reconnect after the radio finishes restarting."
            )
        case .alreadyOffCatReady:
            .operationFailed(
                "Menu 650 was already Off. The USB-C CAT reconnect was stopped; reconnect to finish restoring radio control."
            )
        }
    }

    private static func completedBluetoothRoutingInterruptedError(
        _ outcome: DvGatewayUsbRoutingOutcome
    ) -> RadioControllerError {
        switch outcome {
        case .changedRadioRebooting:
            .operationFailed(
                "Menu 985 was changed to route DV Gateway to USB-C and the radio is rebooting. The Bluetooth CAT reconnect was stopped; reconnect after the radio finishes restarting."
            )
        case .alreadyRouted:
            .operationFailed(
                "Menu 985 was already routed to USB-C and Menu 650 was already Reflector Terminal. The Bluetooth CAT proof was stopped; reconnect to finish restoring Bluetooth control."
            )
        }
    }

    private static func connectedCATDisableOutcomeDescription(
        _ outcome: DvGatewayRecoveryOutcome
    ) -> String {
        switch outcome {
        case .changedRadioRebooting:
            "Menu 650 was changed to Off"
        case .alreadyOffCatReady:
            "Menu 650 was already Off"
        }
    }

    private static func kissInterfaceDescription(_ rawValue: UInt8) -> String {
        switch rawValue {
        case 0: "USB-C"
        case 1: "Bluetooth"
        default: "unsupported route \(rawValue)"
        }
    }

    private static func completedConnectedCATDisableInterruptedError(
        _ outcome: DvGatewayRecoveryOutcome
    ) -> RadioControllerError {
        switch outcome {
        case .changedRadioRebooting:
            .operationFailed(
                "Menu 650 was changed to Off and the radio is rebooting. The same-radio USB-C handoff was stopped; retry after the radio finishes restarting."
            )
        case .alreadyOffCatReady:
            .operationFailed(
                "Menu 650 was already Off. The same-radio USB-C handoff was stopped; retry to finish USB-C CAT qualification."
            )
        }
    }

    private static func completedAPRSRecoveryInterruptedError(
        _ outcome: DvGatewayRecoveryOutcome
    ) -> RadioControllerError {
        switch outcome {
        case .changedRadioRebooting:
            .operationFailed(
                "Menu 650 was changed to Off and the radio is rebooting. The same-endpoint CAT reconnect and one-time APRS retry were stopped; reconnect after the radio finishes restarting."
            )
        case .alreadyOffCatReady:
            .operationFailed(
                "Menu 650 was already Off, and exiting MCP reset CAT without changing the setting. The same-endpoint reconnect and one-time APRS retry were stopped; reconnect before starting APRS again."
            )
        }
    }

    private static func aprsRecoveryFailureDescription(
        _ error: Error,
        completedOutcome: DvGatewayRecoveryOutcome?
    ) -> String {
        guard let completedOutcome else { return describe(error) }
        if error is CancellationError {
            return completedAPRSRecoveryInterruptedError(completedOutcome)
                .localizedDescription
        }
        let outcome = connectedCATDisableOutcomeDescription(completedOutcome)
        let detail = describe(error)
        guard !detail.hasPrefix(outcome) else { return detail }
        return "\(outcome), but exact-endpoint CAT reconnect preparation failed. \(detail)"
    }
}
