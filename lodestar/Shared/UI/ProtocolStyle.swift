// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

/// Visual treatment for each D-STAR protocol — consistent color + SF
/// Symbol + short label across every view that displays reflectors.
extension ReflectorProtocol {
    var accentColor: Color {
        switch self {
        case .dPlus:  return .blue
        case .dExtra: return .purple
        case .dcs:    return .orange
        }
    }

    /// Short human-facing label: `DPlus` / `DExtra` / `DCS`.
    var displayName: String {
        switch self {
        case .dPlus:  return "DPlus"
        case .dExtra: return "DExtra"
        case .dcs:    return "DCS"
        }
    }

    /// One-line description of the protocol family — used as a caption.
    var shortTagline: String {
        switch self {
        case .dPlus:
            return "REF reflectors. Requires callsign registration at dstargateway.org."
        case .dExtra:
            return "XRF reflectors. No auth required."
        case .dcs:
            return "DCS reflectors. No auth required."
        }
    }

    /// SF Symbol name suited to the protocol.
    var sfSymbol: String {
        switch self {
        case .dPlus:  return "personalhotspot"
        case .dExtra: return "dot.radiowaves.forward"
        case .dcs:    return "waveform.path"
        }
    }
}
