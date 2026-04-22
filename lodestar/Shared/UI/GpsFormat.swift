// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// Formatting helpers for `GpsPosition`. Centralised so inline rows,
/// expanded rows, and the now-playing card share a single rendering.
enum GpsFormat {
    /// Short coordinate string: e.g. `36.1699°N 115.1398°W`. Four
    /// fractional digits is ~11 m precision — plenty for APRS-grade
    /// GPS without overstating accuracy.
    static func coordinate(_ pos: GpsPosition) -> String {
        let lat = formatAxis(pos.latitude, positive: "N", negative: "S")
        let lon = formatAxis(pos.longitude, positive: "E", negative: "W")
        return "\(lat) \(lon)"
    }

    /// One-line summary combining callsign (optional) + coordinate.
    static func summary(_ pos: GpsPosition) -> String {
        let call = pos.callsign.trimmingCharacters(in: .whitespaces)
        if call.isEmpty {
            return coordinate(pos)
        }
        return "\(call) · \(coordinate(pos))"
    }

    private static func formatAxis(
        _ value: Double,
        positive: String,
        negative: String
    ) -> String {
        let hemisphere = value >= 0 ? positive : negative
        return String(format: "%.4f°%@", abs(value), hemisphere)
    }
}
