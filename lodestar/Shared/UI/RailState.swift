// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// Pure derivation of what the wide-layout rail shows and with what
/// emphasis. Every product opinion about pane precedence lives here
/// (error > setup-needed > now-transmitting > quiet), so the rules are
/// table-testable without SwiftUI.
///
/// The narrow layout ignores `chain` (it always renders the expanded
/// card, preserving the classic single-column behavior) but shares
/// `showsMcpCard` / `showsNowTransmitting` / `showsHeardList`.
struct RailState: Equatable {
    /// How the Radio → Reflector → Relay chain renders in the rail.
    enum ChainDisplay: Equatable {
        /// Full card: per-stage rows, connect buttons, inline errors.
        case expanded
        /// One-line status summary; tap to expand manually.
        case strip
    }

    var chain: ChainDisplay
    var showsMcpCard: Bool
    var showsNowTransmitting: Bool
    var showsHeardList: Bool
    /// Map renders dimmed with an explanatory hint until a reflector
    /// link is live (first run, link lost, …).
    var mapDimmed: Bool

    /// Everything the derivation reads, captured as plain values so
    /// tests never need coordinators.
    struct Inputs: Equatable {
        var transport: RadioTransportState
        var radioMode: RadioMode
        var mcpStatus: TransportCoordinator.McpStatus
        var hasProbeError: Bool
        var reflector: ReflectorCoordinator.State
        var relay: RelayCoordinator.RelayState
        var streamActive: Bool
        var hasHeardHistory: Bool
        /// Strip tapped open by the user. Only ever *adds* expansion;
        /// forced expansion (error/setup) wins when this is false.
        var manualChainExpanded: Bool
    }

    static func derive(_ i: Inputs) -> RailState {
        let transportFailed: Bool
        if case .failed = i.transport { transportFailed = true } else { transportFailed = false }
        let reflectorFailed: Bool
        if case .failed = i.reflector { reflectorFailed = true } else { reflectorFailed = false }
        let relayFailed: Bool
        if case .failed = i.relay { relayFailed = true } else { relayFailed = false }
        let hasError = transportFailed || reflectorFailed || relayFailed || i.hasProbeError

        let radioConnected = i.transport == .connected
        let modeNeedsSetup: Bool
        switch i.radioMode {
        case .cat, .unrecognized: modeNeedsSetup = true
        case .mmdvm, .unknown: modeNeedsSetup = false
        }
        let setupNeeded = radioConnected && (i.mcpStatus != .idle || modeNeedsSetup)

        let reflectorConnected = i.reflector == .connected

        let chainExpanded = hasError
            || setupNeeded
            || i.manualChainExpanded
            || !reflectorConnected
            || i.transport == .connecting

        return RailState(
            chain: chainExpanded ? .expanded : .strip,
            showsMcpCard: setupNeeded,
            showsNowTransmitting: i.streamActive && reflectorConnected,
            showsHeardList: reflectorConnected || i.hasHeardHistory,
            mapDimmed: !reflectorConnected
        )
    }
}
