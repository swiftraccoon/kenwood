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

enum RadioConnectionActivity: Equatable, Sendable {
    /// Ordinary connection to the endpoint selected in the picker.
    case connection
    /// Non-destructive same-radio USB-to-Bluetooth CAT handoff.
    case bluetoothHandoff
    /// Non-destructive Bluetooth-MMDVM to USB-C CAT handoff.
    case usbHandoff
    /// Explicitly approved Menu 650 change followed by USB CAT recovery.
    case menu650Recovery
    /// Explicitly approved Menu 985 route to USB-C followed by Bluetooth CAT recovery.
    case gatewayRoutingRecovery
    /// Explicitly approved Menu 650 inspection followed by exact USB-C recovery for IF-DSP.
    case ifDSPGatewayRecovery
    /// Explicitly approved APRS Menu 983/Menu 506/Menu 650 inspection followed by exact-endpoint recovery.
    case aprsGatewayRecovery

    var mayChangePersistentRadioConfiguration: Bool {
        switch self {
        case .menu650Recovery, .gatewayRoutingRecovery, .ifDSPGatewayRecovery,
             .aprsGatewayRecovery:
            true
        case .connection, .bluetoothHandoff, .usbHandoff:
            false
        }
    }
}

/// The single observable model consumed by the independent SwiftUI shell.
/// Generated bindings and transport implementations are injected exclusively
/// through the domain protocols below.
@Observable
@MainActor
final class AzimuthSceneModel {
    var route: AzimuthRoute = .radio
    private(set) var radioState: RadioWorkspaceState
    private(set) var radioEndpoints: [RadioEndpoint]
    private(set) var selectedRadioEndpointID: String?
    private(set) var radioEndpointRefreshState: RadioEndpointRefreshState
    private(set) var radioEndpointDiscoveryWarning: String?
    private(set) var pairedBluetoothDeviceCount: UInt32?
    private(set) var catalog: RadioSettingCatalog
    private(set) var catalogLoadState: CatalogLoadState
    private(set) var isRadioOperationInFlight = false
    private(set) var radioConnectionActivity: RadioConnectionActivity?
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
    private(set) var catRecoveryAlert: RadioCATRecoveryAlert?
    private(set) var ifDSPDVGatewayRecoveryAlert: IFDSPDVGatewayRecoveryAlert?
    private(set) var aprsDVGatewayRecoveryAlert: APRSDVGatewayRecoveryAlert?

    @ObservationIgnored private let radioController: any RadioControlling
    @ObservationIgnored private let radioEndpointSelector: any RadioEndpointSelecting
    @ObservationIgnored private let catalogProvider: any RadioSettingCatalogProviding
    @ObservationIgnored private let assistantPlanner: any AssistantPlanning
    @ObservationIgnored private let aprsController: any APRSControlling
    @ObservationIgnored private let ifDSPStream: any IFDSPLiveStreaming
    @ObservationIgnored private let ifDSPModeController: any IFDSPModeControlling
    @ObservationIgnored private var radioUpdatesTask: Task<Void, Never>?
    @ObservationIgnored private var radioEndpointRefreshTask: Task<Void, Never>?
    @ObservationIgnored private var radioEndpointRefreshSequence: UInt64 = 0
    @ObservationIgnored private var userSelectedRadioEndpointID: String?
    @ObservationIgnored private var aprsUpdatesTask: Task<Void, Never>?
    @ObservationIgnored private var ifDSPUpdatesTask: Task<Void, Never>?
    @ObservationIgnored private var catalogTask: Task<Void, Never>?
    @ObservationIgnored private var radioConnectionTask: Task<Void, Error>?
    @ObservationIgnored private var radioConnectionCancellationGeneration: UInt64 = 0
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
        radioEndpointSelector: (any RadioEndpointSelecting)? = nil,
        initialCatalog: RadioSettingCatalog? = nil
    ) {
        let resolvedAPRSController = aprsController ?? UnavailableAPRSController()
        let resolvedIFDSPStream = ifDSPStream ?? UnavailableIFDSPLiveStream()
        let resolvedIFDSPModeController = ifDSPModeController ?? UnavailableIFDSPModeController()
        let resolvedEndpointSelector = radioEndpointSelector ?? FixedUSBRadioEndpointSelector()
        let initialEndpoints = Self.validInitialEndpoints(
            resolvedEndpointSelector.initialEndpoints
        )
        self.radioController = radioController
        self.radioEndpointSelector = resolvedEndpointSelector
        self.catalogProvider = catalogProvider
        self.assistantPlanner = assistantPlanner
        self.aprsController = resolvedAPRSController
        self.ifDSPStream = resolvedIFDSPStream
        self.ifDSPModeController = resolvedIFDSPModeController
        radioState = radioController.currentState
        radioEndpoints = initialEndpoints
        selectedRadioEndpointID = Self.automaticRadioEndpointID(
            in: initialEndpoints
        )
        radioEndpointRefreshState = initialEndpoints.isEmpty ? .idle : .ready
        radioEndpointDiscoveryWarning = nil
        pairedBluetoothDeviceCount = resolvedEndpointSelector
            .initialPairedBluetoothDeviceCount
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

    var isCATRecoveryInFlight: Bool {
        radioConnectionActivity?.mayChangePersistentRadioConfiguration == true
    }

    var isBluetoothHandoffInFlight: Bool {
        radioConnectionActivity == .bluetoothHandoff
    }

    private var hasFreshPairedBluetoothDevice: Bool {
        radioEndpointRefreshState == .ready
            && (pairedBluetoothDeviceCount ?? 0) > 0
    }

    private var hasFreshUSBEndpoint: Bool {
        radioEndpointRefreshState == .ready
            && radioEndpoints.contains { $0.transport == .usb }
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
        refreshRadioEndpoints()
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

    var selectedRadioEndpoint: RadioEndpoint? {
        guard let selectedRadioEndpointID else { return nil }
        return radioEndpoints.first { $0.id == selectedRadioEndpointID }
    }

    var canSelectRadioEndpoint: Bool {
        guard !isRadioOperationInFlight else {
            return false
        }
        switch radioState.connection {
        case .disconnected, .failed:
            if radioEndpointRefreshState.isRefreshing {
                return radioEndpoints.contains { $0.transport == .usb }
            }
            return true
        case .connecting, .connected:
            return false
        }
    }

    /// During optional Bluetooth inventory, already validated USB rows remain
    /// safe to select. Bluetooth rows wait for the fresh paired snapshot so a
    /// stale address cannot race helper discovery.
    func canSelectRadioEndpoint(id: String) -> Bool {
        guard canSelectRadioEndpoint,
              let endpoint = radioEndpoints.first(where: { $0.id == id }) else {
            return false
        }
        return !radioEndpointRefreshState.isRefreshing
            || endpoint.transport == .usb
    }

    /// A previously validated exact endpoint remains connectable while an
    /// optional discovery refresh is still inventorying Bluetooth devices.
    /// Discovery may enrich the picker later, but never delays USB control.
    var canConnectSelectedRadioEndpoint: Bool {
        guard !isRadioOperationInFlight,
              let selectedRadioEndpointID,
              let endpoint = radioEndpoints.first(
                where: { $0.id == selectedRadioEndpointID }
              ) else {
            return false
        }
        if radioEndpointRefreshState.isRefreshing,
           endpoint.transport == .bluetooth {
            return false
        }
        switch radioState.connection {
        case .disconnected, .failed:
            return true
        case .connecting, .connected:
            return false
        }
    }

    var radioEndpointRefreshError: String? {
        guard case .failed(let message) = radioEndpointRefreshState else { return nil }
        return message
    }

    func selectRadioEndpoint(id: String) {
        guard canSelectRadioEndpoint(id: id) else {
            return
        }
        userSelectedRadioEndpointID = id
        selectedRadioEndpointID = id
    }

    /// Replace the endpoint list from one complete platform snapshot.
    ///
    /// A refresh preserves the current stable identifier when it remains
    /// available. Failed or malformed snapshots leave the last usable list
    /// and selection intact.
    @discardableResult
    func refreshRadioEndpoints() -> Task<Void, Never> {
        guard !isRadioOperationInFlight else { return Task {} }
        switch radioState.connection {
        case .disconnected, .failed:
            break
        case .connecting, .connected:
            return Task {}
        }

        radioEndpointRefreshTask?.cancel()
        radioEndpointRefreshSequence &+= 1
        let sequence = radioEndpointRefreshSequence
        radioEndpointRefreshState = .refreshing
        let task = Task { @MainActor [weak self, radioEndpointSelector] in
            do {
                let snapshot = try await radioEndpointSelector.refreshEndpoints()
                try Task.checkCancellation()
                guard let self, self.radioEndpointRefreshSequence == sequence else { return }
                let validated = try Self.validateRadioEndpoints(snapshot.endpoints)
                let retainedUserSelection = self.userSelectedRadioEndpointID.flatMap { selected in
                    validated.contains(where: { $0.id == selected }) ? selected : nil
                }
                self.userSelectedRadioEndpointID = retainedUserSelection
                self.radioEndpoints = validated
                self.selectedRadioEndpointID = retainedUserSelection
                    ?? Self.automaticRadioEndpointID(in: validated)
                self.radioEndpointRefreshState = .ready
                self.radioEndpointDiscoveryWarning = snapshot.warning
                self.pairedBluetoothDeviceCount = snapshot.pairedBluetoothDeviceCount
                self.refreshCATRecoveryOfferAvailability()
                self.refreshIFDSPDVGatewayRecoveryOfferAvailability()
                self.radioEndpointRefreshTask = nil
            } catch is CancellationError {
                guard let self, self.radioEndpointRefreshSequence == sequence else { return }
                self.radioEndpointRefreshState = self.radioEndpoints.isEmpty ? .idle : .ready
                self.radioEndpointRefreshTask = nil
                return
            } catch {
                guard let self, self.radioEndpointRefreshSequence == sequence else { return }
                self.radioEndpointRefreshState = .failed(message: error.localizedDescription)
                self.radioEndpointDiscoveryWarning = nil
                self.pairedBluetoothDeviceCount = nil
                self.radioEndpointRefreshTask = nil
            }
        }
        radioEndpointRefreshTask = task
        return task
    }

    func connectRadio() async {
        guard !isRadioOperationInFlight else { return }
        isRadioOperationInFlight = true
        operationError = nil
        catRecoveryAlert = nil
        ifDSPDVGatewayRecoveryAlert = nil
        aprsDVGatewayRecoveryAlert = nil
        aprsController.discardAPRSDVGatewayRecovery()
        radioConnectionActivity = .connection
        let connectionTask = Task { @MainActor [weak self] in
            guard let self else { throw CancellationError() }
            guard let selectedRadioEndpointID = self.selectedRadioEndpointID else {
                throw self.radioEndpoints.isEmpty
                    ? RadioEndpointSelectionError.noEndpoints
                    : RadioEndpointSelectionError.noSelection
            }
            guard let endpoint = self.radioEndpoints.first(
                where: { $0.id == selectedRadioEndpointID }
            ) else {
                throw RadioEndpointSelectionError.invalidEndpoint(
                    id: selectedRadioEndpointID
                )
            }
            if self.radioEndpointRefreshState.isRefreshing,
               endpoint.transport == .bluetooth {
                throw RadioEndpointSelectionError.refreshInProgress
            }
            try await self.radioEndpointSelector.selectEndpoint(
                id: selectedRadioEndpointID
            )
            try Task.checkCancellation()
            try await self.radioController.connect()
        }
        radioConnectionTask = connectionTask
        defer {
            radioConnectionTask = nil
            radioConnectionActivity = nil
            isRadioOperationInFlight = false
        }
        do {
            try await withTaskCancellationHandler {
                try await connectionTask.value
            } onCancel: {
                connectionTask.cancel()
            }
            await synchronizeSelectedRadioEndpoint()
            radioState = radioController.currentState
            reloadCatalog()
        } catch RadioControllerError.usbMmdvmMode {
            radioState = radioController.currentState
            catRecoveryAlert = .usbMmdvmMode(
                automaticRecoveryAvailable:
                    radioController.automaticCATRecoveryAvailable
                    && hasFreshPairedBluetoothDevice,
                bluetoothFallbackAvailable:
                    radioController.bluetoothCATFallbackAvailable
                    && hasFreshPairedBluetoothDevice
            )
        } catch RadioControllerError.bluetoothMmdvmMode {
            radioState = radioController.currentState
            catRecoveryAlert = .bluetoothMmdvmMode(
                usbFallbackAvailable:
                    radioController.usbCATFallbackAvailable
                    && hasFreshUSBEndpoint,
                automaticBluetoothCATRoutingAvailable:
                    radioController.automaticBluetoothCATRoutingAvailable
                    && hasFreshUSBEndpoint
            )
        } catch is CancellationError {
            operationError = nil
            catRecoveryAlert = nil
            radioState = radioController.currentState
        } catch {
            operationError = error.localizedDescription
            radioState = radioController.currentState
        }
    }

    /// Runs the explicitly approved Menu 650 inspection, changes it only when
    /// needed, proves the same radio over USB-C, and resumes the interrupted
    /// IF-DSP start exactly once.
    func inspectDVGatewayAndStartIFDSP() async {
        guard !isRadioOperationInFlight, !isIFDSPOperationInFlight else { return }
        guard radioController.automaticIFDSPDVGatewayRecoveryAvailable else {
            ifDSPDVGatewayRecoveryAlert = .failed(
                message: "Automatic IF-DSP recovery needs the connected Bluetooth CAT radio and its verified TH-D75 USB-C endpoint on this Mac. No radio setting was changed."
            )
            return
        }

        ifDSPDVGatewayRecoveryAlert = nil
        operationError = nil
        isRadioOperationInFlight = true
        radioConnectionActivity = .ifDSPGatewayRecovery
        let cancellationGeneration = radioConnectionCancellationGeneration
        let recoveryTask = Task { @MainActor [radioController] in
            try await radioController.disableDVGatewayAndReconnectForIFDSP()
        }
        radioConnectionTask = recoveryTask

        do {
            try await withTaskCancellationHandler {
                try await recoveryTask.value
            } onCancel: {
                recoveryTask.cancel()
            }
            await synchronizeSelectedRadioEndpoint()
            radioState = radioController.currentState
            reloadCatalog()
            radioConnectionTask = nil
            radioConnectionActivity = nil
            isRadioOperationInFlight = false
            await startIFDSP()
        } catch is CancellationError {
            radioConnectionTask = nil
            radioConnectionActivity = nil
            isRadioOperationInFlight = false
            ifDSPDVGatewayRecoveryAlert = nil
            radioState = radioController.currentState
        } catch {
            radioConnectionTask = nil
            radioConnectionActivity = nil
            isRadioOperationInFlight = false
            radioState = radioController.currentState
            if radioConnectionCancellationGeneration == cancellationGeneration {
                ifDSPDVGatewayRecoveryAlert = .failed(
                    message: error.localizedDescription
                )
            }
        }
    }

    private static func validInitialEndpoints(
        _ endpoints: [RadioEndpoint]
    ) -> [RadioEndpoint] {
        (try? validateRadioEndpoints(endpoints)) ?? []
    }

    /// Choose a useful endpoint only when the user has not made a valid
    /// selection. USB remains the first automatic choice when attached. On a
    /// Bluetooth-only Mac, prefer the radio's stock paired name instead of an
    /// unrelated device which happens to sort first. This is presentation
    /// policy only; connection-time CAT qualification still proves TH-D75
    /// identity before the endpoint is accepted.
    private static func automaticRadioEndpointID(
        in endpoints: [RadioEndpoint]
    ) -> String? {
        if let usb = endpoints.first(where: { $0.transport == .usb }) {
            return usb.id
        }
        if let stockNamedBluetooth = endpoints.first(where: {
            $0.transport == .bluetooth
                && $0.name.caseInsensitiveCompare("TH-D75") == .orderedSame
        }) {
            return stockNamedBluetooth.id
        }
        return endpoints.first?.id
    }

    private static func validateRadioEndpoints(
        _ endpoints: [RadioEndpoint]
    ) throws -> [RadioEndpoint] {
        var identifiers: Set<String> = []
        for endpoint in endpoints {
            guard !endpoint.id.isEmpty, !endpoint.name.isEmpty else {
                throw RadioEndpointSelectionError.malformedEndpoint
            }
            guard identifiers.insert(endpoint.id).inserted else {
                throw RadioEndpointSelectionError.duplicateEndpoint(id: endpoint.id)
            }
        }
        return endpoints
    }

    func restoreCATFromUSBMMDVM() async {
        guard !isRadioOperationInFlight else { return }
        guard radioController.automaticCATRecoveryAvailable,
              hasFreshPairedBluetoothDevice else {
            catRecoveryAlert = .recoveryFailed(
                message: "Automatic CAT recovery is unavailable for this radio connection."
            )
            return
        }

        catRecoveryAlert = nil
        operationError = nil
        isRadioOperationInFlight = true
        radioConnectionActivity = .menu650Recovery
        let cancellationGeneration = radioConnectionCancellationGeneration
        let recoveryTask = Task { @MainActor [radioController] in
            try await radioController.restoreCATFromUSBMMDVM()
        }
        radioConnectionTask = recoveryTask
        defer {
            radioConnectionTask = nil
            radioConnectionActivity = nil
            isRadioOperationInFlight = false
        }
        do {
            try await withTaskCancellationHandler {
                try await recoveryTask.value
            } onCancel: {
                recoveryTask.cancel()
            }
            await synchronizeSelectedRadioEndpoint()
            radioState = radioController.currentState
            reloadCatalog()
        } catch is CancellationError {
            catRecoveryAlert = radioConnectionCancellationGeneration
                == cancellationGeneration
                ? pendingUSBMMDVMRecoveryOffer()
                : nil
            radioState = radioController.currentState
        } catch {
            catRecoveryAlert = usbMMDVMRecoveryFailureAlert(error: error)
            if error is AzimuthBluetoothAuthorizationError {
                radioEndpointDiscoveryWarning =
                    "Bluetooth connections unavailable: \(error.localizedDescription)"
            }
            radioState = radioController.currentState
        }
    }

    func connectViaBluetoothFromUSBMMDVM() async {
        guard !isRadioOperationInFlight else { return }
        guard radioController.bluetoothCATFallbackAvailable,
              hasFreshPairedBluetoothDevice else {
            catRecoveryAlert = .recoveryFailed(
                message: "A same-radio Bluetooth CAT connection is unavailable for this radio connection."
            )
            return
        }

        catRecoveryAlert = nil
        operationError = nil
        isRadioOperationInFlight = true
        radioConnectionActivity = .bluetoothHandoff
        let cancellationGeneration = radioConnectionCancellationGeneration
        let recoveryTask = Task { @MainActor [radioController] in
            try await radioController.connectViaBluetoothFromUSBMMDVM()
        }
        radioConnectionTask = recoveryTask
        defer {
            radioConnectionTask = nil
            radioConnectionActivity = nil
            isRadioOperationInFlight = false
        }
        do {
            try await withTaskCancellationHandler {
                try await recoveryTask.value
            } onCancel: {
                recoveryTask.cancel()
            }
            await synchronizeSelectedRadioEndpoint()
            radioState = radioController.currentState
            reloadCatalog()
        } catch is CancellationError {
            catRecoveryAlert = radioConnectionCancellationGeneration
                == cancellationGeneration
                ? pendingUSBMMDVMRecoveryOffer()
                : nil
            radioState = radioController.currentState
        } catch {
            catRecoveryAlert = usbMMDVMRecoveryFailureAlert(error: error)
            if error is AzimuthBluetoothAuthorizationError {
                radioEndpointDiscoveryWarning =
                    "Bluetooth connections unavailable: \(error.localizedDescription)"
            }
            radioState = radioController.currentState
        }
    }

    func connectViaUSBFromBluetoothMMDVM() async {
        guard !isRadioOperationInFlight else { return }
        guard radioController.usbCATFallbackAvailable,
              hasFreshUSBEndpoint else {
            catRecoveryAlert = .recoveryFailed(
                message: "An exact USB-C CAT connection is unavailable for this radio connection."
            )
            return
        }

        catRecoveryAlert = nil
        operationError = nil
        isRadioOperationInFlight = true
        radioConnectionActivity = .usbHandoff
        let cancellationGeneration = radioConnectionCancellationGeneration
        let handoffTask = Task { @MainActor [radioController] in
            try await radioController.connectViaUSBFromBluetoothMMDVM()
        }
        radioConnectionTask = handoffTask
        defer {
            radioConnectionTask = nil
            radioConnectionActivity = nil
            isRadioOperationInFlight = false
        }
        do {
            try await withTaskCancellationHandler {
                try await handoffTask.value
            } onCancel: {
                handoffTask.cancel()
            }
            await synchronizeSelectedRadioEndpoint()
            radioState = radioController.currentState
            reloadCatalog()
        } catch is CancellationError {
            catRecoveryAlert = radioConnectionCancellationGeneration
                == cancellationGeneration
                ? pendingBluetoothMMDVMRecoveryOffer()
                : nil
            radioState = radioController.currentState
        } catch {
            catRecoveryAlert = bluetoothMMDVMRecoveryFailureAlert(error: error)
            radioState = radioController.currentState
        }
    }

    func routeDVGatewayToUSBCAndReconnectBluetooth() async {
        guard !isRadioOperationInFlight else { return }
        guard radioController.automaticBluetoothCATRoutingAvailable,
              hasFreshUSBEndpoint else {
            catRecoveryAlert = .recoveryFailed(
                message: "Automatic DV Gateway routing needs exactly one verified TH-D75 USB-C endpoint connected to this Mac."
            )
            return
        }

        catRecoveryAlert = nil
        operationError = nil
        isRadioOperationInFlight = true
        radioConnectionActivity = .gatewayRoutingRecovery
        let cancellationGeneration = radioConnectionCancellationGeneration
        let routingTask = Task { @MainActor [radioController] in
            try await radioController.routeDVGatewayToUSBCAndReconnectBluetooth()
        }
        radioConnectionTask = routingTask
        defer {
            radioConnectionTask = nil
            radioConnectionActivity = nil
            isRadioOperationInFlight = false
        }
        do {
            try await withTaskCancellationHandler {
                try await routingTask.value
            } onCancel: {
                routingTask.cancel()
            }
            await synchronizeSelectedRadioEndpoint()
            radioState = radioController.currentState
            reloadCatalog()
        } catch is CancellationError {
            catRecoveryAlert = radioConnectionCancellationGeneration
                == cancellationGeneration
                ? pendingBluetoothMMDVMRecoveryOffer()
                : nil
            radioState = radioController.currentState
        } catch {
            catRecoveryAlert = bluetoothMMDVMRecoveryFailureAlert(error: error)
            radioState = radioController.currentState
        }
    }

    /// Stop the current connection, handoff, or approved recovery and wait
    /// until its transport/native cleanup has finished before publishing the
    /// disconnected state.
    func cancelRadioConnection() async {
        guard let activity = radioConnectionActivity else { return }
        radioConnectionCancellationGeneration &+= 1
        let connectionTask = radioConnectionTask
        connectionTask?.cancel()
        await radioController.disconnect()
        do {
            try await connectionTask?.value
            catRecoveryAlert = nil
            aprsDVGatewayRecoveryAlert = nil
        } catch is CancellationError {
            catRecoveryAlert = nil
            aprsDVGatewayRecoveryAlert = nil
        } catch {
            if activity == .ifDSPGatewayRecovery {
                ifDSPDVGatewayRecoveryAlert = .failed(
                    message: error.localizedDescription
                )
            } else if activity == .aprsGatewayRecovery {
                aprsDVGatewayRecoveryAlert = .failed(
                    message: error.localizedDescription
                )
            } else if activity.mayChangePersistentRadioConfiguration {
                // A late cancellation can finish an approved Menu 650 or
                // Menu 985 operation before it stops reconnect. Preserve that
                // truthful outcome.
                catRecoveryAlert = .recoveryFailed(message: error.localizedDescription)
            }
        }
        operationError = nil
        radioState = radioController.currentState
        synchronizeIFDSPModeState()
    }

    func cancelCATRecovery() async {
        guard isCATRecoveryInFlight else { return }
        await cancelRadioConnection()
    }

    func dismissCATRecoveryAlert() {
        catRecoveryAlert = nil
    }

    func dismissIFDSPDVGatewayRecoveryAlert() {
        ifDSPDVGatewayRecoveryAlert = nil
    }

    private func refreshCATRecoveryOfferAvailability() {
        let automatic = radioController.automaticCATRecoveryAvailable
            && hasFreshPairedBluetoothDevice
        let bluetooth = radioController.bluetoothCATFallbackAvailable
            && hasFreshPairedBluetoothDevice
        let usb = radioController.usbCATFallbackAvailable
            && hasFreshUSBEndpoint
        let bluetoothRouting = radioController.automaticBluetoothCATRoutingAvailable
            && hasFreshUSBEndpoint
        switch catRecoveryAlert {
        case .usbMmdvmMode:
            catRecoveryAlert = .usbMmdvmMode(
                automaticRecoveryAvailable: automatic,
                bluetoothFallbackAvailable: bluetooth
            )
        case .bluetoothMmdvmMode:
            catRecoveryAlert = .bluetoothMmdvmMode(
                usbFallbackAvailable: usb,
                automaticBluetoothCATRoutingAvailable: bluetoothRouting
            )
        case .recoveryFailed(let message, _, _, _, _):
            catRecoveryAlert = .recoveryFailed(
                message: message,
                automaticRecoveryAvailable: automatic,
                bluetoothFallbackAvailable: bluetooth,
                usbFallbackAvailable: usb,
                automaticBluetoothCATRoutingAvailable: bluetoothRouting
            )
        case nil:
            break
        }
    }

    private func refreshIFDSPDVGatewayRecoveryOfferAvailability() {
        guard case .offer = ifDSPDVGatewayRecoveryAlert else { return }
        ifDSPDVGatewayRecoveryAlert = .offer(
            automaticRecoveryAvailable:
                radioController.automaticIFDSPDVGatewayRecoveryAvailable
        )
    }

    private func pendingBluetoothMMDVMRecoveryOffer() -> RadioCATRecoveryAlert? {
        let usb = radioController.usbCATFallbackAvailable
            && hasFreshUSBEndpoint
        let routing = radioController.automaticBluetoothCATRoutingAvailable
            && hasFreshUSBEndpoint
        guard usb || routing else { return nil }
        return .bluetoothMmdvmMode(
            usbFallbackAvailable: usb,
            automaticBluetoothCATRoutingAvailable: routing
        )
    }

    private func pendingUSBMMDVMRecoveryOffer() -> RadioCATRecoveryAlert? {
        let automatic = radioController.automaticCATRecoveryAvailable
            && hasFreshPairedBluetoothDevice
        let bluetooth = radioController.bluetoothCATFallbackAvailable
            && hasFreshPairedBluetoothDevice
        guard automatic || bluetooth else { return nil }
        return .usbMmdvmMode(
            automaticRecoveryAvailable: automatic,
            bluetoothFallbackAvailable: bluetooth
        )
    }

    private func usbMMDVMRecoveryFailureAlert(
        error: Error
    ) -> RadioCATRecoveryAlert {
        let automatic = radioController.automaticCATRecoveryAvailable
            && hasFreshPairedBluetoothDevice
        let bluetooth = radioController.bluetoothCATFallbackAvailable
            && hasFreshPairedBluetoothDevice
        return .recoveryFailed(
            message: error.localizedDescription,
            automaticRecoveryAvailable: automatic,
            bluetoothFallbackAvailable: bluetooth
        )
    }

    private func bluetoothMMDVMRecoveryFailureAlert(
        error: Error
    ) -> RadioCATRecoveryAlert {
        let usb = radioController.usbCATFallbackAvailable
            && hasFreshUSBEndpoint
        let routing = radioController.automaticBluetoothCATRoutingAvailable
            && hasFreshUSBEndpoint
        return .recoveryFailed(
            message: error.localizedDescription,
            usbFallbackAvailable: usb,
            automaticBluetoothCATRoutingAvailable: routing
        )
    }

    func disconnectRadio() async {
        guard !isRadioOperationInFlight, !isIFDSPOperationInFlight else { return }
        catRecoveryAlert = nil
        ifDSPDVGatewayRecoveryAlert = nil
        aprsDVGatewayRecoveryAlert = nil
        aprsController.discardAPRSDVGatewayRecovery()
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

    private func synchronizeSelectedRadioEndpoint() async {
        guard let endpoint = await radioEndpointSelector.selectedEndpoint(),
              !endpoint.id.isEmpty,
              !endpoint.name.isEmpty else {
            return
        }
        let synchronizedUserSelection = userSelectedRadioEndpointID
            == selectedRadioEndpointID
        if let index = radioEndpoints.firstIndex(where: { $0.id == endpoint.id }) {
            var synchronized = radioEndpoints
            synchronized[index] = endpoint
            guard let validated = try? Self.validateRadioEndpoints(synchronized) else {
                return
            }
            radioEndpoints = validated
            selectedRadioEndpointID = endpoint.id
            if synchronizedUserSelection {
                userSelectedRadioEndpointID = endpoint.id
            }
            return
        }
        guard let validated = try? Self.validateRadioEndpoints(
            radioEndpoints + [endpoint]
        ) else {
            return
        }
        radioEndpoints = validated
        selectedRadioEndpointID = endpoint.id
        if synchronizedUserSelection {
            userSelectedRadioEndpointID = endpoint.id
        }
    }

    func startAPRS(_ configuration: APRSSessionConfiguration) async {
        guard !isAPRSOperationInFlight, !isRadioOperationInFlight else { return }
        isAPRSOperationInFlight = true
        operationError = nil
        aprsDVGatewayRecoveryAlert = nil
        defer { isAPRSOperationInFlight = false }
        do {
            try await aprsController.startAPRS(configuration)
            aprsState = aprsController.currentAPRSState
            radioState = radioController.currentState
        } catch RadioControllerError.aprsDVGatewayRecoveryRequired {
            operationError = nil
            aprsState = aprsController.currentAPRSState
            radioState = radioController.currentState
            aprsDVGatewayRecoveryAlert = .offer(
                connectionName: aprsControlConnectionName,
                automaticRecoveryAvailable:
                    aprsController.automaticAPRSDVGatewayRecoveryAvailable
            )
        } catch {
            operationError = error.localizedDescription
            aprsState = aprsController.currentAPRSState
            radioState = radioController.currentState
        }
    }

    /// Runs the one approved, AE-bound recovery on the current CAT actor,
    /// reconnects the exact selected endpoint, and retries the retained APRS
    /// configuration once. A second current-mode refusal becomes a dismiss-only
    /// failure and cannot recreate the consent offer.
    func inspectDVGatewayAndRetryAPRS() async {
        guard !isRadioOperationInFlight, !isAPRSOperationInFlight else { return }
        guard aprsController.automaticAPRSDVGatewayRecoveryAvailable else {
            aprsDVGatewayRecoveryAlert = .failed(
                message: "Automatic APRS recovery no longer has the original authenticated CAT owner, selected endpoint, radio identity, and KISS route proof. No radio setting was changed. Start APRS again from a current radio connection."
            )
            return
        }

        aprsDVGatewayRecoveryAlert = nil
        operationError = nil
        isRadioOperationInFlight = true
        isAPRSOperationInFlight = true
        radioConnectionActivity = .aprsGatewayRecovery
        let cancellationGeneration = radioConnectionCancellationGeneration
        let recoveryTask = Task { @MainActor [aprsController] in
            try await aprsController.recoverDVGatewayAndRetryAPRS()
        }
        radioConnectionTask = recoveryTask
        defer {
            radioConnectionTask = nil
            radioConnectionActivity = nil
            isRadioOperationInFlight = false
            isAPRSOperationInFlight = false
        }

        do {
            try await withTaskCancellationHandler {
                try await recoveryTask.value
            } onCancel: {
                recoveryTask.cancel()
            }
            await synchronizeSelectedRadioEndpoint()
            aprsState = aprsController.currentAPRSState
            radioState = radioController.currentState
            reloadCatalog()
        } catch is CancellationError {
            aprsDVGatewayRecoveryAlert = nil
            aprsState = aprsController.currentAPRSState
            radioState = radioController.currentState
        } catch {
            aprsState = aprsController.currentAPRSState
            radioState = radioController.currentState
            if radioConnectionCancellationGeneration == cancellationGeneration {
                aprsDVGatewayRecoveryAlert = .failed(
                    message: error.localizedDescription
                )
            }
        }
    }

    func dismissAPRSDVGatewayRecoveryAlert() {
        aprsController.discardAPRSDVGatewayRecovery()
        aprsDVGatewayRecoveryAlert = nil
    }

    /// SwiftUI closes an alert after either button action. Hiding that
    /// presentation must not discard the one-use proof before an approved
    /// recovery task gets scheduled; only the explicit cancel action discards
    /// the proof.
    func hideAPRSDVGatewayRecoveryAlertPresentation() {
        aprsDVGatewayRecoveryAlert = nil
    }

    private var aprsControlConnectionName: String {
        if case .connected(_, let transport) = radioState.connection {
            return transport
        }
        return selectedRadioEndpoint?.transport.title ?? "radio"
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

    /// Enters one coherent IF operating mode: prove the exact USB-C CAT/audio
    /// device, save and verify the radio, then accept samples only from it.
    /// A capture which cannot actually start never leaves the radio in IF mode.
    func startIFDSP() async {
        guard !isIFDSPOperationInFlight, !isRadioOperationInFlight else { return }
        guard radioState.connection.isConnected else {
            operationError = "Connect the TH-D75 over Bluetooth or USB-C before starting IF-DSP."
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

        var audioPreflightCompleted = false
        do {
            let preparedAudioInput: IFDSPPreparedAudioInput
            if let inputProof = radioController.currentIFDSPUSBInputProof {
                guard let prepared = await ifDSPStream.preflight(
                    inputProof: inputProof
                ) else {
                    synchronizeIFDSPStreamState()
                    let audioFailure = ifDSPStartFailureDescription(ifDSPState)
                    ifDSPStream.stop()
                    synchronizeIFDSPStreamState()
                    guard !Task.isCancelled else { return }
                    operationError = audioFailure
                    return
                }
                preparedAudioInput = prepared
                audioPreflightCompleted = true
                synchronizeIFDSPStreamState()
                guard !Task.isCancelled else {
                    ifDSPStream.stop()
                    synchronizeIFDSPStreamState()
                    return
                }
            } else {
                // A Bluetooth connection has no USB audio proof. The
                // controller uses this call only to retain the exact USB
                // endpoint and surface the typed, pre-mutation consent offer.
                // It must not return successfully without a USB proof.
                _ = try await ifDSPModeController.prepareIFDSPMode()
                synchronizeIFDSPModeState()
                let proofFailure = "Azimuth could not prove that the USB-C CAT and audio interfaces belong to the connected TH-D75, so audio capture was not started."
                do {
                    try await ifDSPModeController.restoreIFDSPMode()
                    synchronizeIFDSPModeState()
                    operationError = "\(proofFailure) Azimuth restored and verified the saved radio state."
                } catch {
                    synchronizeIFDSPModeState()
                    operationError = ifDSPModeState.reservesRadioState
                        ? "\(proofFailure) Azimuth still could not verify the saved radio state. \(error.localizedDescription) Return the radio to normal dual-band VFO operation, then choose Retry Restore before starting IF-DSP again."
                        : "\(proofFailure) Azimuth restored and verified the saved radio state, but the CAT settings/screen workspace could not be refreshed: \(error.localizedDescription) Reconnect before trying again."
                }
                return
            }

            _ = try await ifDSPModeController.prepareIFDSPMode()
            synchronizeIFDSPModeState()
            try Task.checkCancellation()

            await ifDSPStream.start(preparedInput: preparedAudioInput)
            synchronizeIFDSPStreamState()
            try Task.checkCancellation()
            guard ifDSPState.isStreaming else {
                let audioFailure = ifDSPStartFailureDescription(ifDSPState)
                ifDSPStream.stop()
                synchronizeIFDSPStreamState()
                do {
                    try await ifDSPModeController.restoreIFDSPMode()
                    synchronizeIFDSPModeState()
                    operationError = "\(audioFailure) Azimuth restored and verified the saved radio state."
                } catch {
                    synchronizeIFDSPModeState()
                    operationError = ifDSPModeState.reservesRadioState
                        ? "\(audioFailure) Azimuth still could not verify the saved radio state. \(error.localizedDescription) Return the radio to normal dual-band VFO operation, then choose Retry Restore before starting IF-DSP again."
                        : "\(audioFailure) Azimuth restored and verified the saved radio state, but the CAT settings/screen workspace could not be refreshed: \(error.localizedDescription) Reconnect before trying again."
                }
                return
            }
        } catch RadioControllerError.ifDspDVGatewayRecoveryRequired {
            if audioPreflightCompleted {
                ifDSPStream.stop()
                synchronizeIFDSPStreamState()
            }
            synchronizeIFDSPModeState()
            operationError = nil
            ifDSPDVGatewayRecoveryAlert = .offer(
                automaticRecoveryAvailable:
                    radioController.automaticIFDSPDVGatewayRecoveryAvailable
            )
        } catch {
            let startFailure = error.localizedDescription
            ifDSPStream.stop()
            synchronizeIFDSPStreamState()
            synchronizeIFDSPModeState()
            let restorationWasPending = ifDSPModeState.reservesRadioState
            do {
                // A partial prepare keeps its saved snapshot in the core. This
                // second restoration attempt is intentional and safe when the
                // failed prepare had already cleaned itself up.
                try await ifDSPModeController.restoreIFDSPMode()
                synchronizeIFDSPModeState()
                operationError = restorationWasPending
                    ? "IF-DSP couldn’t start, but Azimuth restored and verified the saved radio state. Correct the radio mode or connection problem, then try again."
                    : startFailure
            } catch {
                synchronizeIFDSPModeState()
                operationError = ifDSPModeState.reservesRadioState
                    ? "IF-DSP couldn’t start, and Azimuth still could not verify the saved radio state. \(error.localizedDescription) Return the radio to normal dual-band VFO operation, then choose Retry Restore before starting IF-DSP again."
                    : "IF-DSP couldn’t start, but Azimuth restored and verified the saved radio state. The CAT settings/screen workspace could not be refreshed: \(error.localizedDescription) Reconnect before trying again."
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
            return "The TH-D75 USB audio input was not available. \(visible)"
        case .paused(let reason, _):
            return "IF audio stopped during startup: \(reason)"
        case .failed(let message, _):
            return "IF audio could not start: \(message)"
        case .idle:
            return "IF audio did not start."
        case .requestingPermission:
            return "Audio-input permission did not complete."
        case .starting(let routeName):
            return "The \(routeName) audio route did not begin streaming."
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
