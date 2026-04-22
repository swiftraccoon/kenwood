// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// Curated starter set — the ~15 reflectors most newcomers actually
/// use. Surfaced in the picker sheet by default; the full ~200 list
/// is a "Show all" click away.
///
/// Selection favours:
/// - English-speaking, globally-active REF/XRF/DCS endpoints.
/// - At least one per protocol so DPlus auth failures don't leave a
///   user with nothing that works.
/// - XRF/DCS first because they don't require callsign registration.
enum FeaturedReflectors {
    /// Names only — resolved against the full list at display time so
    /// host/port drift in the bundled Pi-Star files flows through.
    static let names: [String] = [
        // --- DExtra (XRF) — no auth, easy wins -------------
        "XRF757", // North America multi-mode hub
        "XRF030", // Austrian, bridged to REF030
        "XRF012", // UK
        "XRF310", // Italy
        // --- DCS — no auth ---------------------------------
        "DCS001", // Flagship EU DCS
        "DCS003", // EU DCS
        "DCS006", // EU DCS
        // --- DPlus (REF) — requires registration -----------
        "REF001", // USA global
        "REF030", // USA east
        "REF038", // Germany
        "REF068", // UK
        "REF078", // Canada
    ]

    /// Resolve featured names against the live list returned by
    /// `defaultReflectors()`. Preserves the curated ordering above.
    static func resolve(from all: [Reflector]) -> [Reflector] {
        let byName = Dictionary(uniqueKeysWithValues: all.map { ($0.name, $0) })
        return names.compactMap { byName[$0] }
    }
}
