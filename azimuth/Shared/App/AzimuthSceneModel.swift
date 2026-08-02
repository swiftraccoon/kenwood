// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import Observation

enum AzimuthRoute: String, CaseIterable, Identifiable, Hashable, Sendable {
    case radio
    case aprs
    case ifDSP = "if-dsp"
    case settings
    case assistant
    case learn

    var id: String { rawValue }

    var title: String {
        switch self {
        case .radio: return "Radio"
        case .aprs: return "APRS"
        case .ifDSP: return "IF-DSP"
        case .settings: return "Settings"
        case .assistant: return "Assistant"
        case .learn: return "Learn"
        }
    }

    var symbol: String {
        switch self {
        case .radio: return "radio"
        case .aprs: return "point.3.connected.trianglepath.dotted"
        case .ifDSP: return "waveform.path.ecg.rectangle"
        case .settings: return "slider.horizontal.3"
        case .assistant: return "apple.intelligence"
        case .learn: return "book.pages"
        }
    }
}

enum CatalogLoadState: Equatable, Sendable {
    case loading
    case ready
    case failed(String)
}

enum AssistantWorkflowState: Equatable, Sendable {
    case idle
    case proposing(request: String)
    case review(AssistantPlan)
    case applying(plan: AssistantPlan, progress: RadioSettingApplyProgress)
    case applied(plan: AssistantPlan, report: RadioSettingApplyReport)
    case failed(plan: AssistantPlan?, report: RadioSettingApplyReport?, message: String)
}

enum ManualSettingApplyState: Equatable, Sendable {
    case idle
    case applying(progress: RadioSettingApplyProgress)
    case applied(RadioSettingApplyReport)
    case failed(report: RadioSettingApplyReport?, message: String)
}

/// The single observable model consumed by the independent SwiftUI shell.
/// Generated bindings and transport implementations are injected exclusively
/// through the domain protocols below.
@Observable
@MainActor
final class AzimuthSceneModel {
    var route: AzimuthRoute = .radio
    private(set) var radioState: RadioWorkspaceState
    private(set) var catalog: RadioSettingCatalog
    private(set) var catalogLoadState: CatalogLoadState
    private(set) var isRadioOperationInFlight = false
    private(set) var assistantWorkflow: AssistantWorkflowState = .idle
    private(set) var manualSettingApplyState: ManualSettingApplyState = .idle
    private(set) var aprsState: APRSOperationalState
    private(set) var isAPRSOperationInFlight = false
    private(set) var ifDSPState: IFDSPLiveStreamState
    private(set) var ifDSPConfiguration: IFDSPConfiguration
    private(set) var ifDSPMonitoringState: IFDSPMonitoringState
    private(set) var ifDSPModeState: IFDSPRadioModeState
    private(set) var isIFDSPOperationInFlight = false
    private(set) var operationError: String?

    @ObservationIgnored private let radioController: any RadioControlling
    @ObservationIgnored private let catalogProvider: any RadioSettingCatalogProviding
    @ObservationIgnored private let assistantPlanner: any AssistantPlanning
    @ObservationIgnored private let aprsController: any APRSControlling
    @ObservationIgnored private let ifDSPStream: any IFDSPLiveStreaming
    @ObservationIgnored private let ifDSPModeController: any IFDSPModeControlling
    @ObservationIgnored private var radioUpdatesTask: Task<Void, Never>?
    @ObservationIgnored private var aprsUpdatesTask: Task<Void, Never>?
    @ObservationIgnored private var ifDSPUpdatesTask: Task<Void, Never>?
    @ObservationIgnored private var catalogTask: Task<Void, Never>?
    @ObservationIgnored private var sceneLifecycleTask: Task<Void, Never>?
    @ObservationIgnored private var sceneLifecycleSequence: UInt64 = 0
    @ObservationIgnored private var sceneIsBackgrounded = false
    @ObservationIgnored private var reconnectRadioAfterBackground = false

    /// Root integration point. Supply adapters that conform to the domain
    /// protocols; no UI file needs to import AzimuthCore or USB types.
    init(
        radioController: any RadioControlling,
        catalogProvider: any RadioSettingCatalogProviding,
        assistantPlanner: any AssistantPlanning,
        aprsController: (any APRSControlling)? = nil,
        ifDSPStream: (any IFDSPLiveStreaming)? = nil,
        ifDSPModeController: (any IFDSPModeControlling)? = nil,
        initialCatalog: RadioSettingCatalog? = nil
    ) {
        let resolvedAPRSController = aprsController ?? UnavailableAPRSController()
        let resolvedIFDSPStream = ifDSPStream ?? UnavailableIFDSPLiveStream()
        let resolvedIFDSPModeController = ifDSPModeController ?? UnavailableIFDSPModeController()
        self.radioController = radioController
        self.catalogProvider = catalogProvider
        self.assistantPlanner = assistantPlanner
        self.aprsController = resolvedAPRSController
        self.ifDSPStream = resolvedIFDSPStream
        self.ifDSPModeController = resolvedIFDSPModeController
        radioState = radioController.currentState
        aprsState = resolvedAPRSController.currentAPRSState
        ifDSPState = resolvedIFDSPStream.currentState
        ifDSPConfiguration = resolvedIFDSPStream.configuration
        ifDSPMonitoringState = resolvedIFDSPStream.monitoringState
        ifDSPModeState = resolvedIFDSPModeController.ifDSPModeState
        catalog = initialCatalog ?? .designPreview
        catalogLoadState = initialCatalog == nil ? .loading : .ready
    }

    convenience init() {
        self.init(
            radioController: DisconnectedRadioController(),
            catalogProvider: PreviewRadioSettingCatalogProvider(),
            assistantPlanner: OnDeviceAssistantPlanner()
        )
    }

    var assistantAvailability: AssistantAvailability {
        assistantPlanner.availability
    }

    var assistantCanAccept: Bool {
        guard case .review(let plan) = assistantWorkflow else { return false }
        return !plan.needsClarification
            && plan.isFullyValidated
            && radioState.connection.isConnected
            && radioState.capabilities.settingWrite.isAvailable
    }

    func activate() {
        guard radioUpdatesTask == nil else { return }
        radioState = radioController.currentState
        radioUpdatesTask = Task { @MainActor [weak self, radioController] in
            for await update in radioController.updates {
                guard !Task.isCancelled else { return }
                // A buffered stream value can arrive after an awaited command
                // has already advanced the controller. Never let that stale
                // value roll the observable workspace backward.
                guard update == radioController.currentState else { continue }
                guard let self else { return }
                self.radioState = update
                self.ifDSPModeState = self.ifDSPModeController.ifDSPModeState
            }
        }
        aprsState = aprsController.currentAPRSState
        aprsUpdatesTask = Task { @MainActor [weak self, aprsController] in
            for await update in aprsController.aprsUpdates {
                guard !Task.isCancelled else { return }
                self?.aprsState = update
            }
        }
        ifDSPState = ifDSPStream.currentState
        ifDSPConfiguration = ifDSPStream.configuration
        ifDSPMonitoringState = ifDSPStream.monitoringState
        ifDSPModeState = ifDSPModeController.ifDSPModeState
        ifDSPUpdatesTask = Task { @MainActor [weak self, ifDSPStream] in
            for await update in ifDSPStream.updates {
                guard !Task.isCancelled else { return }
                guard let self else { return }
                // Permission and route setup can publish several states while
                // the command is suspended. Ignore buffered values which no
                // longer describe the stream at the time they are observed.
                guard update == ifDSPStream.currentState else { continue }
                self.ifDSPState = update
                self.ifDSPConfiguration = ifDSPStream.configuration
                self.ifDSPMonitoringState = ifDSPStream.monitoringState
                if self.shouldRestoreIFDSP(after: update) {
                    await self.restoreIFDSPAfterUnexpectedStreamEnd(update)
                }
            }
        }
        reloadCatalog()
    }

    /// DriverKit user-client connections are not kept logically live while an
    /// iPad app is suspended. The shell calls this for `.background` on iOS;
    /// `.inactive` deliberately does not tear down a radio for brief system UI.
    @discardableResult
    func handleScenePhaseBackground() -> Task<Void, Never> {
        enqueueSceneLifecycle { model in
            await model.suspendRadioForBackground()
        }
    }

    /// Reopens only a previously live, idle connection after the matching
    /// background teardown has completed. A failed/manual/in-flight session is
    /// never turned into a surprise automatic connection.
    @discardableResult
    func handleScenePhaseActive() -> Task<Void, Never> {
        activate()
        return enqueueSceneLifecycle { model in
            await model.restoreRadioAfterBackground()
        }
    }

    func reloadCatalog() {
        catalogTask?.cancel()
        catalogLoadState = .loading
        catalogTask = Task { @MainActor [weak self, catalogProvider] in
            do {
                let loaded = try await catalogProvider.catalog()
                guard !Task.isCancelled else { return }
                self?.catalog = loaded
                self?.catalogLoadState = .ready
            } catch is CancellationError {
                return
            } catch {
                self?.catalogLoadState = .failed(error.localizedDescription)
            }
        }
    }

    func connectRadio() async {
        guard !isRadioOperationInFlight else { return }
        isRadioOperationInFlight = true
        operationError = nil
        defer { isRadioOperationInFlight = false }
        do {
            try await radioController.connect()
            radioState = radioController.currentState
            reloadCatalog()
        } catch {
            operationError = error.localizedDescription
            radioState = radioController.currentState
        }
    }

    func disconnectRadio() async {
        guard !isRadioOperationInFlight, !isIFDSPOperationInFlight else { return }
        isRadioOperationInFlight = true
        defer { isRadioOperationInFlight = false }
        if let restorationError = await stopIFDSPStreamAndRestoreRadio() {
            operationError = restorationError
            if ifDSPModeState.reservesRadioState { return }
        }
        await radioController.disconnect()
        radioState = radioController.currentState
        synchronizeIFDSPModeState()
    }

    func startAPRS(_ configuration: APRSSessionConfiguration) async {
        guard !isAPRSOperationInFlight, !isRadioOperationInFlight else { return }
        isAPRSOperationInFlight = true
        operationError = nil
        defer { isAPRSOperationInFlight = false }
        do {
            try await aprsController.startAPRS(configuration)
            aprsState = aprsController.currentAPRSState
            radioState = radioController.currentState
        } catch {
            operationError = error.localizedDescription
            aprsState = aprsController.currentAPRSState
            radioState = radioController.currentState
        }
    }

    func stopAPRS() async {
        guard !isAPRSOperationInFlight else { return }
        isAPRSOperationInFlight = true
        operationError = nil
        defer { isAPRSOperationInFlight = false }
        do {
            try await aprsController.stopAPRS()
            aprsState = aprsController.currentAPRSState
            radioState = radioController.currentState
        } catch {
            operationError = error.localizedDescription
            aprsState = aprsController.currentAPRSState
            radioState = radioController.currentState
        }
    }

    func sendAPRSMessage(
        addressee: String,
        text: String,
        messageID: String?
    ) async throws -> APRSActivity {
        guard !isAPRSOperationInFlight else {
            throw RadioControllerError.operationFailed("Another APRS operation is still running.")
        }
        isAPRSOperationInFlight = true
        operationError = nil
        defer { isAPRSOperationInFlight = false }
        do {
            let activity = try await aprsController.sendAPRSMessage(
                addressee: addressee,
                text: text,
                messageID: messageID
            )
            aprsState = aprsController.currentAPRSState
            return activity
        } catch {
            operationError = error.localizedDescription
            aprsState = aprsController.currentAPRSState
            throw error
        }
    }

    func sendAPRSPosition(
        latitude: Double,
        longitude: Double,
        comment: String
    ) async throws -> APRSActivity {
        guard !isAPRSOperationInFlight else {
            throw RadioControllerError.operationFailed("Another APRS operation is still running.")
        }
        isAPRSOperationInFlight = true
        operationError = nil
        defer { isAPRSOperationInFlight = false }
        do {
            let activity = try await aprsController.sendAPRSPosition(
                latitude: latitude,
                longitude: longitude,
                comment: comment
            )
            aprsState = aprsController.currentAPRSState
            return activity
        } catch {
            operationError = error.localizedDescription
            aprsState = aprsController.currentAPRSState
            throw error
        }
    }

    /// Enters one coherent IF operating mode: save and verify the radio first,
    /// then accept samples only from the selected physical USB audio input.
    /// A capture which cannot actually start never leaves the radio in IF mode.
    func startIFDSP() async {
        guard !isIFDSPOperationInFlight, !isRadioOperationInFlight else { return }
        guard radioState.connection.isConnected else {
            operationError = "Connect the TH-D75 over USB-C before starting IF-DSP."
            return
        }
        guard !ifDSPModeState.reservesRadioState else {
            operationError = "Restore the current IF-DSP radio state before starting another session."
            return
        }

        isIFDSPOperationInFlight = true
        operationError = nil
        defer {
            isIFDSPOperationInFlight = false
            radioState = radioController.currentState
        }

        do {
            _ = try await ifDSPModeController.prepareIFDSPMode()
            synchronizeIFDSPModeState()

            await ifDSPStream.start()
            synchronizeIFDSPStreamState()
            guard ifDSPState.isStreaming else {
                let audioFailure = ifDSPStartFailureDescription(ifDSPState)
                ifDSPStream.stop()
                synchronizeIFDSPStreamState()
                do {
                    try await ifDSPModeController.restoreIFDSPMode()
                    synchronizeIFDSPModeState()
                    operationError = audioFailure
                } catch {
                    synchronizeIFDSPModeState()
                    operationError = "\(audioFailure) Radio restoration also failed: \(error.localizedDescription)"
                }
                return
            }
        } catch {
            let startFailure = error.localizedDescription
            ifDSPStream.stop()
            synchronizeIFDSPStreamState()
            synchronizeIFDSPModeState()
            do {
                // A partial prepare keeps its saved snapshot in the core. This
                // second restoration attempt is intentional and safe when the
                // failed prepare had already cleaned itself up.
                try await ifDSPModeController.restoreIFDSPMode()
                synchronizeIFDSPModeState()
                operationError = startFailure
            } catch {
                synchronizeIFDSPModeState()
                operationError = "\(startFailure) Radio restoration also failed: \(error.localizedDescription)"
            }
        }
    }

    /// Stops physical capture before restoring every radio field saved by the
    /// IF session. This ordering prevents the audio engine from presenting a
    /// stale stream while the radio returns to its previous mode.
    func stopIFDSP() async {
        guard !isIFDSPOperationInFlight else { return }
        isIFDSPOperationInFlight = true
        operationError = nil
        defer {
            isIFDSPOperationInFlight = false
            radioState = radioController.currentState
        }
        if let restorationError = await stopIFDSPStreamAndRestoreRadio() {
            operationError = restorationError
        }
    }

    func retuneIFDSP(to frequencyHz: UInt32) async {
        guard !isIFDSPOperationInFlight else { return }
        guard case .active = ifDSPModeState else {
            operationError = "Start IF-DSP before changing the Band B center frequency."
            return
        }

        isIFDSPOperationInFlight = true
        operationError = nil
        defer {
            isIFDSPOperationInFlight = false
            radioState = radioController.currentState
        }
        do {
            _ = try await ifDSPModeController.retuneIFDSP(to: frequencyHz)
            synchronizeIFDSPModeState()
            synchronizeIFDSPStreamState()
            if !ifDSPState.isStreaming {
                let interruption = ifDSPStreamInterruptionDescription(ifDSPState)
                if let restorationError = await stopIFDSPStreamAndRestoreRadio() {
                    operationError = "\(interruption) \(restorationError)"
                } else {
                    operationError = "\(interruption) IF-DSP was stopped and the saved radio state was restored."
                }
            }
        } catch {
            let tuneFailure = error.localizedDescription
            synchronizeIFDSPModeState()
            if let restorationError = await stopIFDSPStreamAndRestoreRadio() {
                operationError = "\(tuneFailure) \(restorationError)"
            } else {
                operationError = "\(tuneFailure) IF-DSP was stopped and the saved radio state was restored."
            }
        }
    }

    func configureIFDSP(_ configuration: IFDSPConfiguration) async {
        operationError = nil
        await ifDSPStream.setConfiguration(configuration)
        ifDSPState = ifDSPStream.currentState
        ifDSPConfiguration = ifDSPStream.configuration
        ifDSPMonitoringState = ifDSPStream.monitoringState
    }

    func refreshRadioScreen() async {
        guard !isRadioOperationInFlight else { return }
        isRadioOperationInFlight = true
        operationError = nil
        defer { isRadioOperationInFlight = false }
        do {
            try await radioController.refreshScreen()
            radioState = radioController.currentState
        } catch {
            operationError = error.localizedDescription
        }
    }

    func refreshRadioSettings() async {
        guard !isRadioOperationInFlight else { return }
        isRadioOperationInFlight = true
        operationError = nil
        defer { isRadioOperationInFlight = false }
        do {
            try await radioController.refreshSettings()
            radioState = radioController.currentState
        } catch {
            operationError = error.localizedDescription
            radioState = radioController.currentState
        }
    }

    func press(_ key: RadioFrontPanelKey) async {
        guard !isRadioOperationInFlight else { return }
        isRadioOperationInFlight = true
        operationError = nil
        defer { isRadioOperationInFlight = false }
        do {
            try await radioController.press(key)
            radioState = radioController.currentState
        } catch {
            operationError = error.localizedDescription
        }
    }

    func proposeAssistantPlan(request: String) async {
        let trimmed = request.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        assistantWorkflow = .proposing(request: trimmed)
        do {
            let plan = try await assistantPlanner.propose(
                request: trimmed,
                catalog: catalog,
                currentValues: radioState.settingValues
            )
            assistantWorkflow = .review(plan)
        } catch is CancellationError {
            assistantWorkflow = .idle
        } catch {
            assistantWorkflow = .failed(plan: nil, report: nil, message: error.localizedDescription)
        }
    }

    func acceptAssistantPlan() async {
        guard case .review(let plan) = assistantWorkflow else { return }
        guard !plan.needsClarification, plan.isFullyValidated else {
            assistantWorkflow = .failed(
                plan: plan,
                report: nil,
                message: "The proposal contains changes that did not pass catalog validation."
            )
            return
        }
        guard radioState.connection.isConnected else {
            assistantWorkflow = .failed(
                plan: plan,
                report: nil,
                message: "Connect the TH-D75 before accepting this proposal."
            )
            return
        }
        guard radioState.capabilities.settingWrite.isAvailable else {
            let reason: String
            if case .unavailable(let detail) = radioState.capabilities.settingWrite {
                reason = detail
            } else {
                reason = "Radio setting writes are not ready."
            }
            assistantWorkflow = .failed(plan: plan, report: nil, message: reason)
            return
        }

        let changes = plan.changes.compactMap { change -> ValidatedRadioSettingChange? in
            guard change.validation == .validated,
                  let definition = change.definition,
                  let target = change.proposedValue else { return nil }
            return ValidatedRadioSettingChange(
                settingID: definition.id,
                previousValue: change.previousValue,
                targetValue: target
            )
        }
        let latestState = radioController.currentState
        radioState = latestState
        if let stale = changes.first(where: { change in
            guard let expected = change.previousValue else { return false }
            return latestState.settingValues[change.settingID] != expected
        }) {
            assistantWorkflow = .failed(
                plan: plan,
                report: nil,
                message: "\(stale.settingID) changed on the radio after this proposal was built. "
                    + "Generate a fresh proposal before applying."
            )
            return
        }
        let initialProgress = RadioSettingApplyProgress(
            completedCount: 0,
            totalCount: changes.count,
            currentSettingID: changes.first?.settingID
        )
        assistantWorkflow = .applying(plan: plan, progress: initialProgress)

        do {
            let report = try await radioController.applySettings(changes) { [weak self] progress in
                self?.assistantWorkflow = .applying(plan: plan, progress: progress)
            }
            radioState = radioController.currentState
            reloadCatalog()
            if report.succeeded {
                assistantWorkflow = .applied(plan: plan, report: report)
            } else {
                assistantWorkflow = .failed(
                    plan: plan,
                    report: report,
                    message: "\(report.appliedCount) of \(report.results.count) changes applied. "
                        + "Review the failures below."
                )
            }
        } catch {
            radioState = radioController.currentState
            assistantWorkflow = .failed(
                plan: plan,
                report: nil,
                message: error.localizedDescription
            )
        }
    }

    func declineAssistantPlan() {
        guard case .applying = assistantWorkflow else {
            assistantWorkflow = .idle
            return
        }
    }

    func resetAssistantWorkflow() {
        guard case .applying = assistantWorkflow else {
            assistantWorkflow = .idle
            return
        }
    }

    func applyManualSetting(id: String, targetValue: ProposedSettingValue) async {
        guard let definition = catalog.definition(id: id) else {
            manualSettingApplyState = .failed(
                report: nil,
                message: "The setting is no longer present in the loaded catalog."
            )
            return
        }
        guard !definition.isSpecializedEditor, definition.domain.accepts(targetValue) else {
            manualSettingApplyState = .failed(
                report: nil,
                message: "The staged value does not match \(definition.domain.summary)."
            )
            return
        }
        guard radioState.connection.isConnected,
              radioState.capabilities.settingWrite.isAvailable else {
            manualSettingApplyState = .failed(
                report: nil,
                message: "Connect and read the TH-D75 before applying this setting."
            )
            return
        }
        guard let previous = radioState.settingValues[id] else {
            manualSettingApplyState = .failed(
                report: nil,
                message: "Azimuth has not read a live value for this setting. Refresh the radio snapshot first."
            )
            return
        }
        guard previous != targetValue else {
            manualSettingApplyState = .failed(
                report: nil,
                message: "The staged value already matches the radio."
            )
            return
        }

        let change = ValidatedRadioSettingChange(
            settingID: id,
            previousValue: previous,
            targetValue: targetValue
        )
        manualSettingApplyState = .applying(
            progress: RadioSettingApplyProgress(
                completedCount: 0,
                totalCount: 1,
                currentSettingID: id
            )
        )
        do {
            let report = try await radioController.applySettings([change]) { [weak self] progress in
                self?.manualSettingApplyState = .applying(progress: progress)
            }
            radioState = radioController.currentState
            reloadCatalog()
            manualSettingApplyState = report.succeeded
                ? .applied(report)
                : .failed(report: report, message: "The radio rejected this setting change.")
        } catch {
            radioState = radioController.currentState
            manualSettingApplyState = .failed(report: nil, message: error.localizedDescription)
        }
    }

    func resetManualSettingApplyState() {
        guard case .applying = manualSettingApplyState else {
            manualSettingApplyState = .idle
            return
        }
    }

    func dismissOperationError() {
        operationError = nil
    }

    private func synchronizeIFDSPStreamState() {
        ifDSPState = ifDSPStream.currentState
        ifDSPConfiguration = ifDSPStream.configuration
        ifDSPMonitoringState = ifDSPStream.monitoringState
    }

    private func synchronizeIFDSPModeState() {
        ifDSPModeState = ifDSPModeController.ifDSPModeState
        radioState = radioController.currentState
    }

    private func stopIFDSPStreamAndRestoreRadio() async -> String? {
        ifDSPStream.stop()
        synchronizeIFDSPStreamState()
        synchronizeIFDSPModeState()
        guard ifDSPModeState.reservesRadioState else { return nil }

        do {
            try await ifDSPModeController.restoreIFDSPMode()
            synchronizeIFDSPModeState()
            return nil
        } catch {
            synchronizeIFDSPModeState()
            if ifDSPModeState.reservesRadioState {
                return "The USB IF stream stopped, but the saved radio state was not fully "
                    + "restored: \(error.localizedDescription)"
            }
            return "The saved radio state was restored, but the CAT settings/screen workspace "
                + "could not be refreshed: \(error.localizedDescription)"
        }
    }

    private func shouldRestoreIFDSP(after state: IFDSPLiveStreamState) -> Bool {
        guard !isIFDSPOperationInFlight,
              !sceneIsBackgrounded,
              ifDSPModeState.reservesRadioState else { return false }
        switch state {
        case .paused, .failed:
            return true
        case .idle, .requestingPermission, .waitingForUSBAudio, .starting, .streaming:
            return false
        }
    }

    private func restoreIFDSPAfterUnexpectedStreamEnd(
        _ state: IFDSPLiveStreamState
    ) async {
        guard shouldRestoreIFDSP(after: state) else { return }
        isIFDSPOperationInFlight = true
        defer { isIFDSPOperationInFlight = false }

        let streamFailure: String
        switch state {
        case .paused(let reason, _):
            streamFailure = reason
        case .failed(let message, _):
            streamFailure = message
        default:
            return
        }
        if let restorationError = await stopIFDSPStreamAndRestoreRadio() {
            operationError = "\(streamFailure) \(restorationError)"
        } else {
            operationError = "\(streamFailure) Azimuth stopped IF-DSP and restored the saved radio state."
        }
    }

    private func ifDSPStartFailureDescription(
        _ state: IFDSPLiveStreamState
    ) -> String {
        switch state {
        case .waitingForUSBAudio(let inputs):
            let visible = inputs.isEmpty
                ? "No audio inputs were visible."
                : "Visible inputs: \(inputs.joined(separator: ", "))."
            return "The TH-D75 USB audio input was not available. \(visible) The saved radio state was restored."
        case .paused(let reason, _):
            return "IF audio stopped during startup: \(reason) The saved radio state was restored."
        case .failed(let message, _):
            return "IF audio could not start: \(message) The saved radio state was restored."
        case .idle:
            return "IF audio did not start. The saved radio state was restored."
        case .requestingPermission:
            return "Audio-input permission did not complete. The saved radio state was restored."
        case .starting(let routeName):
            return "The \(routeName) audio route did not begin streaming. The saved radio state was restored."
        case .streaming:
            return ""
        }
    }

    private func ifDSPStreamInterruptionDescription(
        _ state: IFDSPLiveStreamState
    ) -> String {
        switch state {
        case .paused(let reason, _):
            return reason
        case .failed(let message, _):
            return message
        case .waitingForUSBAudio:
            return "The TH-D75 USB audio input is no longer available."
        case .idle:
            return "The USB IF audio stream stopped during the radio operation."
        case .requestingPermission, .starting:
            return "The USB IF audio stream was no longer live when the radio operation completed."
        case .streaming:
            return ""
        }
    }

    private func enqueueSceneLifecycle(
        _ operation: @escaping @MainActor (AzimuthSceneModel) async -> Void
    ) -> Task<Void, Never> {
        let predecessor = sceneLifecycleTask
        sceneLifecycleSequence &+= 1
        let sequence = sceneLifecycleSequence
        let task = Task { @MainActor [weak self] in
            await predecessor?.value
            guard let self else { return }
            await operation(self)
            if self.sceneLifecycleSequence == sequence {
                self.sceneLifecycleTask = nil
            }
        }
        sceneLifecycleTask = task
        return task
    }

    private func suspendRadioForBackground() async {
        guard !sceneIsBackgrounded else { return }
        sceneIsBackgrounded = true

        let stateBeforeSuspension = radioController.currentState
        reconnectRadioAfterBackground = stateBeforeSuspension.connection.isConnected
            && !hasUserRadioOperationInFlight

        if let restorationError = await stopIFDSPStreamAndRestoreRadio() {
            operationError = restorationError
            reconnectRadioAfterBackground = false
        }

        guard stateBeforeSuspension.connection != .disconnected else {
            radioState = stateBeforeSuspension
            return
        }
        await radioController.disconnect()
        radioState = radioController.currentState
        synchronizeIFDSPModeState()
    }

    private func restoreRadioAfterBackground() async {
        guard sceneIsBackgrounded else { return }
        sceneIsBackgrounded = false
        guard reconnectRadioAfterBackground else { return }
        reconnectRadioAfterBackground = false
        guard !hasUserRadioOperationInFlight else { return }
        await connectRadio()
    }

    private var hasUserRadioOperationInFlight: Bool {
        if isRadioOperationInFlight { return true }
        if isAPRSOperationInFlight { return true }
        if isIFDSPOperationInFlight { return true }
        if case .applying = assistantWorkflow { return true }
        if case .applying = manualSettingApplyState { return true }
        return false
    }
}
