// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

enum RadioConnectionState: Equatable, Sendable {
    case disconnected
    case connecting
    case connected(device: String, transport: String)
    case failed(message: String)

    var isConnected: Bool {
        if case .connected = self { return true }
        return false
    }
}

/// One user-selectable path to a physical TH-D75.
///
/// The identifier is opaque to the UI. A platform selector can use a stable
/// USB registry identity or an exact Bluetooth address without exposing either
/// as connection policy in the scene model.
struct RadioEndpoint: Identifiable, Equatable, Sendable {
    let id: String
    let name: String
    let transport: AzimuthRadioConnectionKind
    let detail: String?

    init(
        id: String,
        name: String,
        transport: AzimuthRadioConnectionKind,
        detail: String? = nil
    ) {
        self.id = id
        self.name = name
        self.transport = transport
        self.detail = detail
    }

    static let defaultUSBC = RadioEndpoint(
        id: "usb:2166:9023",
        name: "Kenwood TH-D75",
        transport: .usb
    )

    var displayName: String {
        if let detail, !detail.isEmpty {
            return "\(name) (\(transport.title), \(detail))"
        }
        return "\(name) (\(transport.title))"
    }
}

enum RadioEndpointRefreshState: Equatable, Sendable {
    case idle
    case refreshing
    case ready
    case failed(message: String)

    var isRefreshing: Bool { self == .refreshing }
}

struct RadioEndpointDiscoverySnapshot: Equatable, Sendable {
    let endpoints: [RadioEndpoint]
    let warning: String?
    /// Total paired macOS Bluetooth devices observed by the same fresh native
    /// inventory, including devices omitted from the likely-radio picker.
    /// `nil` means Bluetooth discovery was unavailable, not that the count was
    /// known to be zero.
    let pairedBluetoothDeviceCount: UInt32?
    /// Count of ordinary hint-matched Bluetooth rows from the same inventory.
    /// This excludes exact radios added by explicit CAT qualification.
    let likelyBluetoothRadioCount: UInt32?

    init(
        endpoints: [RadioEndpoint],
        warning: String? = nil,
        pairedBluetoothDeviceCount: UInt32? = nil,
        likelyBluetoothRadioCount: UInt32? = nil
    ) {
        self.endpoints = endpoints
        self.warning = warning
        self.pairedBluetoothDeviceCount = pairedBluetoothDeviceCount
        self.likelyBluetoothRadioCount = likelyBluetoothRadioCount
    }
}

struct RadioEndpointBluetoothSearchResult: Equatable, Sendable {
    let snapshot: RadioEndpointDiscoverySnapshot
    let probedCandidateCount: UInt32
    let totalUnhintedCandidateCount: UInt32
    let isComplete: Bool
    let wasCancelled: Bool
}

enum BluetoothRadioSearchState: Equatable, Sendable {
    case idle
    case searching
    case completed(probed: UInt32, total: UInt32, radiosFound: Int)
    case incomplete(probed: UInt32, total: UInt32, radiosFound: Int)
    case stopped(probed: UInt32, total: UInt32, radiosFound: Int)
    case failed(message: String)

    var isSearching: Bool { self == .searching }
}

enum RadioEndpointSelectionError: LocalizedError, Equatable, Sendable {
    case noEndpoints
    case noSelection
    case refreshInProgress
    case malformedEndpoint
    case invalidEndpoint(id: String)
    case duplicateEndpoint(id: String)
    case customBluetoothSearchUnavailable

    var errorDescription: String? {
        switch self {
        case .noEndpoints:
            return "No TH-D75 USB or Bluetooth connection is currently available."
        case .noSelection:
            return "Choose a TH-D75 USB or Bluetooth connection before connecting."
        case .refreshInProgress:
            return "Wait for the USB and Bluetooth connection refresh to finish before connecting."
        case .malformedEndpoint:
            return "Radio discovery returned a connection without a stable identifier or display name. No selection was changed."
        case .invalidEndpoint(let id):
            return "The selected radio connection \(id) is no longer available. Refresh the connection list and choose again."
        case .duplicateEndpoint(let id):
            return "Radio discovery returned the connection identifier \(id) more than once. No selection was changed."
        case .customBluetoothSearchUnavailable:
            return "This radio connection provider cannot search for custom-named Bluetooth radios."
        }
    }
}

/// Platform-owned discovery and routing for the endpoint used by the radio
/// controller's next connection attempt.
///
/// Selection does not open the radio. A later concrete selector can swap the
/// controller's transport factory only after this method succeeds, while the
/// scene model continues to serialize the actual connection through
/// [`RadioControlling.connect()`].
@MainActor
protocol RadioEndpointSelecting: AnyObject {
    /// Immediately available endpoints shown before the first refresh.
    var initialEndpoints: [RadioEndpoint] { get }

    /// Paired-device count belonging to `initialEndpoints`, when the selector
    /// already owns a complete Bluetooth inventory.
    var initialPairedBluetoothDeviceCount: UInt32? { get }

    /// Whether this selector can CAT-probe unhinted paired devices and return
    /// only exact endpoints proved to be TH-D75 radios.
    var supportsCustomNamedBluetoothSearch: Bool { get }

    /// Return a fresh, complete endpoint snapshot.
    func refreshEndpoints() async throws -> RadioEndpointDiscoverySnapshot

    func findCustomNamedBluetoothRadios() async throws
        -> RadioEndpointBluetoothSearchResult

    /// Prepare one listed endpoint for the next controller connection.
    func selectEndpoint(id: String) async throws

    /// Stable endpoint actually retained by the platform router, when known.
    ///
    /// A same-radio serial-qualified Bluetooth handoff can resolve to an exact
    /// paired address only after open, so this may differ from the endpoint
    /// selected before the connection attempt.
    func selectedEndpoint() async -> RadioEndpoint?
}

extension RadioEndpointSelecting {
    var initialPairedBluetoothDeviceCount: UInt32? { nil }
    var supportsCustomNamedBluetoothSearch: Bool { false }
    func findCustomNamedBluetoothRadios() async throws
        -> RadioEndpointBluetoothSearchResult {
        throw RadioEndpointSelectionError.customBluetoothSearchUnavailable
    }
    func selectedEndpoint() async -> RadioEndpoint? { nil }
}

/// Existing product behavior until a platform-specific endpoint selector is
/// injected: one fixed USB-C endpoint and a no-op routing step.
@MainActor
final class FixedUSBRadioEndpointSelector: RadioEndpointSelecting {
    let initialEndpoints = [RadioEndpoint.defaultUSBC]
    let initialPairedBluetoothDeviceCount: UInt32? = 0

    func refreshEndpoints() async throws -> RadioEndpointDiscoverySnapshot {
        RadioEndpointDiscoverySnapshot(
            endpoints: initialEndpoints,
            pairedBluetoothDeviceCount: 0,
            likelyBluetoothRadioCount: 0
        )
    }

    func selectEndpoint(id: String) async throws {
        guard id == RadioEndpoint.defaultUSBC.id else {
            throw RadioEndpointSelectionError.invalidEndpoint(id: id)
        }
    }

    func selectedEndpoint() async -> RadioEndpoint? {
        .defaultUSBC
    }
}

enum RadioCapabilityState: Equatable, Sendable {
    case available
    case unavailable(reason: String)
    case preparing

    var isAvailable: Bool { self == .available }
}

struct RadioCapabilities: Equatable, Sendable {
    var screenStreaming: RadioCapabilityState
    var frontPanelControl: RadioCapabilityState
    var settingRead: RadioCapabilityState
    var settingWrite: RadioCapabilityState

    static let disconnected = RadioCapabilities(
        screenStreaming: .unavailable(reason: "Connect a radio to start screen capture."),
        frontPanelControl: .unavailable(reason: "Connect a radio to enable controls."),
        settingRead: .unavailable(reason: "Connect a radio to read its settings."),
        settingWrite: .unavailable(reason: "Connect and read the radio before writing."),
    )
}

/// A platform-neutral RGBA8888 frame supplied by the radio adapter.
/// The UI never fabricates pixels: when this value is nil it renders an
/// explicit no-live-frame state.
struct RadioScreenFrame: Equatable, Sendable {
    let width: Int
    let height: Int
    let rgba8888: Data
    let capturedAt: Date

    var isValid: Bool {
        width > 0 && height > 0 && rgba8888.count == width * height * 4
    }
}

struct RadioTelemetry: Equatable, Sendable {
    var firmware: String?
    var operatingMode: String?
    var activeBand: String?
    var primaryFrequency: String?
    var secondaryFrequency: String?
    var signalStrength: Double?
    var batteryFraction: Double?

    static let unavailable = RadioTelemetry()
}

struct RadioWorkspaceState: Equatable, Sendable {
    var connection: RadioConnectionState
    var capabilities: RadioCapabilities
    var screenFrame: RadioScreenFrame?
    var telemetry: RadioTelemetry
    var settingValues: [String: ProposedSettingValue]
    var lastScreenError: String?

    static let disconnected = RadioWorkspaceState(
        connection: .disconnected,
        capabilities: .disconnected,
        screenFrame: nil,
        telemetry: .unavailable,
        settingValues: [:],
        lastScreenError: nil,
    )
}

enum RadioFrontPanelKey: String, CaseIterable, Hashable, Sendable {
    case mode
    case menu
    case ab
    case function
    case monitor
    case up
    case down
    case left
    case right
    case enter
    case mark0
    case vfo1
    case mr2
    case call3
    case msg4
    case list5
    case beacon6
    case reverse7
    case tone8
    case pf1_9
    case mhzStar
    case pf2Hash
    case micPf1
    case micPf2
    case micPf3
}

enum RadioControllerError: LocalizedError, Equatable, Sendable {
    case adapterUnavailable
    case capabilityUnavailable(String)
    case usbMmdvmMode
    case operationFailed(String)

    var errorDescription: String? {
        switch self {
        case .adapterUnavailable:
            return "No TH-D75 control adapter is installed in this build."
        case .usbMmdvmMode:
            return "After Azimuth sent the TH-D75 packet-mode exit sequence, the USB-C interface returned a valid MMDVM response, so CAT control is unavailable on that interface after recovery."
        case .capabilityUnavailable(let reason), .operationFailed(let reason):
            return reason
        }
    }
}

enum RadioCATRecoveryAlert: Equatable, Sendable {
    case usbMmdvmMode(
        automaticRecoveryAvailable: Bool,
        bluetoothFallbackAvailable: Bool
    )
    case recoveryFailed(
        message: String,
        automaticRecoveryAvailable: Bool = false,
        bluetoothFallbackAvailable: Bool = false
    )

    var title: String {
        switch self {
        case .usbMmdvmMode:
            return "USB-C Remains in MMDVM Mode"
        case .recoveryFailed:
            return "CAT Recovery Failed"
        }
    }

    var message: String {
        switch self {
        case .usbMmdvmMode(true, true):
            return "Azimuth already sent the transient packet-mode exit sequence, but the TH-D75 USB-C interface returned a validated MMDVM version response instead of CAT. Azimuth found paired Bluetooth devices and can try to locate and verify this same radio without changing Menu 650, or you can explicitly try automatic recovery to turn Menu 650 off and restore USB-C CAT. Turning Menu 650 off restarts the radio and can take more than a minute."
        case .usbMmdvmMode(false, true):
            return "Azimuth already sent the transient packet-mode exit sequence, but the TH-D75 USB-C interface returned a validated MMDVM version response instead of CAT. Azimuth found paired Bluetooth devices and can try to locate and verify this same radio without changing Menu 650. USB-C remains available to the gateway session."
        case .usbMmdvmMode(true, false):
            return "Azimuth already sent the transient packet-mode exit sequence, but the TH-D75 USB-C interface returned a validated MMDVM version response instead of CAT. Azimuth found paired Bluetooth devices and can try to locate and verify this same radio, turn Menu 650 off if needed, and reconnect only after USB-C proves CAT. The radio restarts if Menu 650 changes, and recovery can take more than a minute."
        case .usbMmdvmMode(false, false):
            #if os(iOS)
            return "Azimuth already sent the transient packet-mode exit sequence, but the TH-D75 USB-C interface returned a validated MMDVM version response instead of CAT. The gateway-owned USB interface does not route the programming commands needed to change Menu 650, and Azimuth has no second TH-D75 CAT control path on iPadOS, so it cannot safely change Menu 650 automatically. End any active MMDVM or DV Gateway session. If no host session is active, set Menu 650 (DV Gateway) to Off and power-cycle the radio, then reconnect."
            #else
            return "Azimuth already sent the transient packet-mode exit sequence, but the TH-D75 USB-C interface returned a validated MMDVM version response instead of CAT. Automatic recovery needs a configured, paired TH-D75 Bluetooth control link because the gateway-owned USB interface does not route the programming commands needed to change Menu 650. End any active MMDVM or DV Gateway session. If no host session is active, set Menu 650 (DV Gateway) to Off and power-cycle the radio, then reconnect."
            #endif
        case .recoveryFailed(let message, _, _):
            return message
        }
    }

    var automaticRecoveryAvailable: Bool {
        switch self {
        case .usbMmdvmMode(let available, _): return available
        case .recoveryFailed(_, let available, _): return available
        }
    }

    var bluetoothFallbackAvailable: Bool {
        switch self {
        case .usbMmdvmMode(_, let available): return available
        case .recoveryFailed(_, _, let available): return available
        }
    }

    var isRecoveryOffer: Bool {
        switch self {
        case .usbMmdvmMode:
            true
        case .recoveryFailed(_, let automatic, let bluetooth):
            automatic || bluetooth
        }
    }
}

struct ValidatedRadioSettingChange: Identifiable, Equatable, Sendable {
    let settingID: String
    let previousValue: ProposedSettingValue?
    let targetValue: ProposedSettingValue

    var id: String { settingID }
}

struct RadioSettingApplyProgress: Equatable, Sendable {
    let completedCount: Int
    let totalCount: Int
    let currentSettingID: String?

    var fractionCompleted: Double {
        guard totalCount > 0 else { return 0 }
        return Double(completedCount) / Double(totalCount)
    }
}

struct RadioSettingApplyResult: Identifiable, Equatable, Sendable {
    let settingID: String
    let previousValue: ProposedSettingValue?
    let targetValue: ProposedSettingValue
    let outcome: Outcome

    var id: String { settingID }

    enum Outcome: Equatable, Sendable {
        case applied
        case failed(reason: String)
        case rolledBack(reason: String)
    }
}

struct RadioSettingApplyReport: Equatable, Sendable {
    let results: [RadioSettingApplyResult]

    var appliedCount: Int {
        results.filter { $0.outcome == .applied }.count
    }

    var failedCount: Int { results.count - appliedCount }
    var succeeded: Bool { !results.isEmpty && failedCount == 0 }
}

/// Integration boundary between the independent SwiftUI product and the
/// generated Azimuth core / USB transport adapter.
///
/// An adapter should turn the core's 240×180 RGBA8888 screen frame into
/// `RadioScreenFrame`, map all 25 core front-panel keys, and publish each
/// state transition through `updates`. UI code never imports generated bindings.
@MainActor
protocol RadioControlling: AnyObject {
    var currentState: RadioWorkspaceState { get }
    var updates: AsyncStream<RadioWorkspaceState> { get }
    var automaticCATRecoveryAvailable: Bool { get }
    var bluetoothCATFallbackAvailable: Bool { get }

    func connect() async throws
    func restoreCATFromUSBMMDVM() async throws
    func connectViaBluetoothFromUSBMMDVM() async throws
    func disconnect() async
    func refreshScreen() async throws
    func refreshSettings() async throws
    func press(_ key: RadioFrontPanelKey) async throws
    func applySettings(
        _ changes: [ValidatedRadioSettingChange],
        progress: @escaping @MainActor @Sendable (RadioSettingApplyProgress) -> Void
    ) async throws -> RadioSettingApplyReport
}

extension RadioControlling {
    var automaticCATRecoveryAvailable: Bool { false }
    var bluetoothCATFallbackAvailable: Bool { false }

    func restoreCATFromUSBMMDVM() async throws {
        throw RadioControllerError.capabilityUnavailable(
            "Automatic USB MMDVM-to-CAT recovery is unavailable in this build."
        )
    }

    func connectViaBluetoothFromUSBMMDVM() async throws {
        throw RadioControllerError.capabilityUnavailable(
            "A same-radio Bluetooth CAT handoff is unavailable in this build."
        )
    }
}

/// Honest standalone default. It makes the complete UI previewable without
/// implying a USB device or live radio exists.
@MainActor
final class DisconnectedRadioController: RadioControlling {
    let currentState = RadioWorkspaceState.disconnected

    var updates: AsyncStream<RadioWorkspaceState> {
        let state = currentState
        return AsyncStream { continuation in
            continuation.yield(state)
            continuation.finish()
        }
    }

    func connect() async throws {
        throw RadioControllerError.adapterUnavailable
    }

    func disconnect() async {}

    func refreshScreen() async throws {
        throw RadioControllerError.adapterUnavailable
    }

    func refreshSettings() async throws {
        throw RadioControllerError.adapterUnavailable
    }

    func press(_ key: RadioFrontPanelKey) async throws {
        throw RadioControllerError.adapterUnavailable
    }

    func applySettings(
        _ changes: [ValidatedRadioSettingChange],
        progress: @escaping @MainActor @Sendable (RadioSettingApplyProgress) -> Void
    ) async throws -> RadioSettingApplyReport {
        throw RadioControllerError.adapterUnavailable
    }
}
