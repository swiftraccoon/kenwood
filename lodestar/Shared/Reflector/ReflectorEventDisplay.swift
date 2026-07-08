// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

extension DisconnectCause {
    /// Operator-facing description of why the link ended.
    var displayText: String {
        switch self {
        case .rejected: return "link rejected by reflector"
        case .unlinkAcked: return "unlinked"
        case .keepaliveTimeout: return "keepalive timeout"
        case .disconnectTimeout: return "disconnect not acknowledged"
        case .unknown: return "unknown reason"
        }
    }
}

extension VoiceEndCause {
    /// Operator-facing description of why a stream ended.
    var displayText: String {
        switch self {
        case .eot: return "eot"
        case .inactivity: return "inactivity"
        case .unknown: return "unknown"
        }
    }
}
