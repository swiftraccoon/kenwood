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
            return "The TH-D75 USB-C interface returned a valid MMDVM response, so CAT control is unavailable on that interface."
        case .capabilityUnavailable(let reason), .operationFailed(let reason):
            return reason
        }
    }
}

enum RadioCATRecoveryAlert: Equatable, Sendable {
    case usbMmdvmMode(automaticRecoveryAvailable: Bool)
    case recoveryFailed(message: String)

    var title: String {
        switch self {
        case .usbMmdvmMode:
            return "USB-C Is in MMDVM Mode"
        case .recoveryFailed:
            return "CAT Recovery Failed"
        }
    }

    var message: String {
        switch self {
        case .usbMmdvmMode(true):
            return "The TH-D75 USB-C interface returned a validated MMDVM version response instead of CAT. This can be caused by DV Gateway or another MMDVM session. Azimuth can use its paired Bluetooth control link, verify the same radio, turn Menu 650 off if needed, and reconnect only after USB-C proves CAT. The radio restarts if Menu 650 changes, and recovery can take more than a minute."
        case .usbMmdvmMode(false):
            return "The TH-D75 USB-C interface returned a validated MMDVM version response instead of CAT. End any active MMDVM or DV Gateway session. If no host session is active, set Menu 650 (DV Gateway) to Off and power-cycle the radio, then reconnect."
        case .recoveryFailed(let message):
            return message
        }
    }

    var automaticRecoveryAvailable: Bool {
        switch self {
        case .usbMmdvmMode(let available): return available
        case .recoveryFailed: return false
        }
    }

    var isRecoveryOffer: Bool {
        if case .usbMmdvmMode = self { return true }
        return false
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

    func connect() async throws
    func restoreCATFromUSBMMDVM() async throws
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

    func restoreCATFromUSBMMDVM() async throws {
        throw RadioControllerError.capabilityUnavailable(
            "Automatic USB MMDVM-to-CAT recovery is unavailable in this build."
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
