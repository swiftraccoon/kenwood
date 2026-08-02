// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import OSLog

private let azimuthRadioCoreLog = Logger(
    subsystem: "org.swiftraccoon.azimuth",
    category: "radio-core"
)

private func azimuthRadioErrorIdentity(_ error: Error) -> String {
    let bridged = error as NSError
    return "\(String(reflecting: type(of: error))) domain=\(bridged.domain) code=\(bridged.code)"
}

/// Live product controller. It owns one qualified automation core, one optimistic
/// settings snapshot, and one authenticated screen lease at a time.
@MainActor
final class AzimuthLiveRadioController: RadioControlling, APRSControlling, IFDSPModeControlling {
    typealias CoreConnector = @Sendable (ByteTransport) async throws -> any AutomationControllerProtocol
    typealias RadioModePreflight = @Sendable (
        any AzimuthRadioTransport
    ) async throws -> AzimuthRadioWireMode

    private(set) var currentState: RadioWorkspaceState
    let updates: AsyncStream<RadioWorkspaceState>
    private(set) var currentAPRSState: APRSOperationalState
    let aprsUpdates: AsyncStream<APRSOperationalState>
    private(set) var ifDSPModeState: IFDSPRadioModeState = .inactive

    private let transport: any AzimuthRadioTransport
    private let coreTransport: AzimuthCoreByteTransport
    private let schema: AzimuthCoreSettingSchema
    private let connectCore: CoreConnector
    private let prepareRadioForAutomation: RadioModePreflight
    private let updateContinuation: AsyncStream<RadioWorkspaceState>.Continuation
    private let aprsUpdateContinuation: AsyncStream<APRSOperationalState>.Continuation

    private var core: (any AutomationControllerProtocol)?
    private var settingSnapshotID: UInt64?
    private var pollingTask: Task<Void, Never>?
    private var aprsPollingTask: Task<Void, Never>?
    private var captureSlot: CaptureSlot?
    private var nextCaptureID: UInt64 = 0
    private var sessionEpoch: UInt64 = 0
    private var screenPauseDepth = 0
    private var exclusiveOperation: String?
    private var consecutiveScreenFailures = 0

    private struct CaptureSlot {
        let id: UInt64
        let task: Task<RemoteScreenFrame, Error>
    }

    init(
        transport: any AzimuthRadioTransport,
        records: [SettingRecord] = settingCatalog(),
        connectCore: @escaping CoreConnector = { transport in
            try await connectAutomation(transport: transport)
        },
        prepareRadioForAutomation: @escaping RadioModePreflight = { transport in
            try await AzimuthRadioModePreflight(transport: transport).prepareForAutomation()
        }
    ) throws {
        self.transport = transport
        coreTransport = AzimuthCoreByteTransport(radioTransport: transport)
        schema = try AzimuthCoreSettingSchema(records: records)
        self.connectCore = connectCore
        self.prepareRadioForAutomation = prepareRadioForAutomation
        currentState = .disconnected
        currentAPRSState = .unavailable(
            "Connect the TH-D75 over USB-C before starting an APRS KISS session."
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
        guard !currentState.connection.isConnected else { return }
        azimuthRadioCoreLog.notice("[Azimuth Radio] Connection started")
        try beginExclusive("connect")
        sessionEpoch &+= 1
        let epoch = sessionEpoch
        stopScreenPolling()
        stopAPRSPolling()
        ifDSPModeState = .inactive
        settingSnapshotID = nil
        publishAPRS(
            .unavailable("Finishing the USB-C and automation connection first.")
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
        var connectionStage = "USB transport open"
        defer { endExclusive("connect") }
        do {
            azimuthRadioCoreLog.info("[Azimuth Radio] USB transport open started")
            try await transport.open()
            try requireEpoch(epoch)
            azimuthRadioCoreLog.notice("[Azimuth Radio] USB transport open succeeded")

            connectionStage = "radio mode preflight"
            var wireMode = try await prepareRadioForAutomation(transport)
            try requireEpoch(epoch)
            if wireMode == .unresponsive {
                connectionStage = "USB CDC session recovery"
                azimuthRadioCoreLog.notice(
                    "[Azimuth Radio] CDC session was unresponsive; reopening once"
                )
                await transport.close()
                try requireEpoch(epoch)
                try await transport.open()
                try requireEpoch(epoch)
                azimuthRadioCoreLog.notice(
                    "[Azimuth Radio] USB CDC session reopened; retrying mode probe"
                )
                connectionStage = "radio mode preflight retry"
                wireMode = try await prepareRadioForAutomation(transport)
                try requireEpoch(epoch)
            }
            if wireMode == .mmdvm {
                throw AzimuthRadioModePreflightError.dvGatewayMode
            }
            guard wireMode == .cat else {
                throw AzimuthRadioModePreflightError.cdcUnresponsive
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
            core = connectedCore
            installAPRSSnapshot(
                connectedCore.aprsSnapshot(afterSequence: nil),
                retainingHistory: false
            )

            let abi = connectedCore.abi()
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

            connectionStage = "initial settings refresh"
            azimuthRadioCoreLog.notice("[Azimuth Core] Initial settings refresh started")
            let settings: SettingReadResult
            do {
                settings = try await connectedCore.readSettingValues(settingIds: nil)
                try requireEpoch(epoch)
                try installSettings(settings)
            } catch {
                azimuthRadioCoreLog.error(
                    "[Azimuth Core] Initial settings refresh failed type=\(azimuthRadioErrorIdentity(error), privacy: .public) detail=\(error.localizedDescription, privacy: .private)"
                )
                throw error
            }
            let settingsSummary =
                "[Azimuth Core] Initial settings refresh succeeded "
                + "(liveScalarValues=\(settings.values.count)/\(schema.scalarSettingCount) "
                + "deferredBlobs=\(schema.deferredBlobCount) "
                + "catalog=\(schema.records.count))"
            azimuthRadioCoreLog.notice("\(settingsSummary, privacy: .public)")

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

            var ready = currentState
            ready.capabilities = RadioCapabilities(
                screenStreaming: .available,
                frontPanelControl: .available,
                settingRead: .available,
                settingWrite: .available
            )
            publish(ready)
            startScreenPolling(epoch: epoch)
            azimuthRadioCoreLog.notice("[Azimuth Radio] Connection ready")
        } catch {
            azimuthRadioCoreLog.error(
                "[Azimuth Radio] Connection failed during \(connectionStage, privacy: .public) type=\(azimuthRadioErrorIdentity(error), privacy: .public) detail=\(error.localizedDescription, privacy: .private)"
            )
            if let establishedCore {
                try? await establishedCore.close()
            }
            await transport.close()
            core = nil
            settingSnapshotID = nil
            ifDSPModeState = .inactive
            if epoch == sessionEpoch {
                var failed = RadioWorkspaceState.disconnected
                failed.connection = .failed(message: Self.describe(error))
                publish(failed)
                publishAPRSUnavailable(
                    "APRS is unavailable because the radio connection failed."
                )
            }
            if error is CancellationError { throw error }
            throw RadioControllerError.operationFailed(Self.describe(error))
        }
    }

    func disconnect() async {
        azimuthRadioCoreLog.notice("[Azimuth Radio] Disconnect started")
        sessionEpoch &+= 1
        stopScreenPolling()
        stopAPRSPolling()
        captureSlot?.task.cancel()
        captureSlot = nil
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
        exclusiveOperation = nil
        ifDSPModeState = .inactive
        publish(.disconnected)
        publishAPRSUnavailable(
            "Connect the TH-D75 over USB-C before starting an APRS KISS session."
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
            azimuthRadioCoreLog.notice(
                "[Azimuth Core] Settings refresh succeeded (\(read.values.count, privacy: .public) values)"
            )
        } catch {
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
            return RadioSettingApplyReport(results: results)
        } catch {
            if invokedCoreBatch, context.epoch == sessionEpoch {
                await refreshAfterFailedApply(core: context.core, epoch: context.epoch)
            }
            throw RadioControllerError.operationFailed(Self.describe(error))
        }
    }

    // MARK: - USB IF-DSP radio ownership

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

        let operation = "IF-DSP prepare"
        try beginExclusive(operation)
        guard currentState.connection.isConnected, let core else {
            endExclusive(operation)
            throw RadioControllerError.capabilityUnavailable(
                "Connect the TH-D75 over USB-C before starting IF-DSP."
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

        stopScreenPolling()
        stopAPRSPolling()
        await settleCapture()
        settingSnapshotID = nil
        var starting = currentAPRSState
        starting.status.phase = .starting
        starting.status.startedAt = Date()
        starting.status.configuration = configuration
        starting.status.lastError = nil
        publishAPRS(starting)
        publishCATUnavailableForAPRS(
            mode: "Entering APRS KISS",
            reason: "APRS is taking ownership of the USB serial link."
        )

        do {
            _ = try await context.core.startAprs(
                config: AzimuthCoreAPRSAdapter.coreConfiguration(configuration)
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
                reason: "APRS KISS owns the USB serial link. Stop APRS to restore CAT control."
            )
            startAPRSPolling(epoch: context.epoch)
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
        let fresh = try await core.readSettingValues(settingIds: nil)
        try requireEpoch(epoch)
        try installSettings(fresh)

        var restoring = currentState
        restoring.capabilities = RadioCapabilities(
            screenStreaming: .preparing,
            frontPanelControl: .preparing,
            settingRead: .available,
            settingWrite: .available
        )
        restoring.telemetry.operatingMode = "Automation ABI \(core.abi().version)"
        publish(restoring)

        do {
            _ = try await captureFresh(core: core, epoch: epoch)
        } catch {
            await handleScreenFailure(error, epoch: epoch)
        }
        if epoch == sessionEpoch, currentState.connection.isConnected {
            startScreenPolling(epoch: epoch)
        }
    }

    private func recoverCATAfterAPRSError(
        core: any AutomationControllerProtocol,
        epoch: UInt64,
        originalError: Error
    ) async {
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
            // A failed KISS entry can return its error before the actor's
            // follow-up automation qualification finishes. This read is serialized
            // behind that proof and therefore cannot re-enable CAT early.
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
        stopScreenPolling()
        stopAPRSPolling()
        captureSlot?.task.cancel()
        captureSlot = nil
        screenPauseDepth = 0
        let failedCore = core
        core = nil
        settingSnapshotID = nil
        ifDSPModeState = .inactive
        if let failedCore {
            try? await failedCore.close()
        }
        await transport.close()
        var failed = RadioWorkspaceState.disconnected
        failed.connection = .failed(message: message)
        publish(failed)
        if currentAPRSState.status.phase != .failed {
            publishAPRSUnavailable(
                "APRS is unavailable because the USB radio connection was lost."
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
                "Connect the TH-D75 over USB-C before using \(name)."
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
                "Connect the TH-D75 over USB-C before using \(name)."
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

    private func setSettingCapabilities(
        _ read: RadioCapabilityState,
        _ write: RadioCapabilityState
    ) {
        var state = currentState
        state.capabilities.settingRead = read
        state.capabilities.settingWrite = write
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

    private static func describe(_ error: Error) -> String {
        switch error {
        case AutomationError.UsbTransport(let detail): return "USB transport failed: \(detail)"
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
            return "APRS KISS owns the USB serial link. Stop APRS before using CAT controls."
        case AutomationError.AprsModeInactive:
            return "Start an APRS KISS session before using this packet operation."
        case AutomationError.InvalidAprsConfiguration(let detail):
            return "Invalid APRS configuration: \(detail)"
        case AutomationError.AprsOperation(let detail):
            return "APRS operation failed: \(detail)"
        case AutomationError.IfDspModeActive:
            return "IF-DSP owns a saved radio state. Stop IF-DSP before using conflicting controls."
        case AutomationError.IfDspModeInactive:
            return "Prepare the radio for IF-DSP before using this operation."
        case AutomationError.IfDspOperation(let detail):
            return "IF-DSP radio operation failed: \(detail)"
        case AutomationError.IfDspRestoration(let detail):
            return "IF-DSP could not restore and verify every saved radio value: \(detail)"
        case AutomationError.Shutdown(let detail): return "Radio shutdown failed: \(detail)"
        case let localized as LocalizedError:
            return localized.errorDescription ?? String(describing: error)
        default:
            return String(describing: error)
        }
    }
}
