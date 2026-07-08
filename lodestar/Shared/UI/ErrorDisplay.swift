// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

extension Error {
    /// Operator-facing message: prefers the localized description a
    /// `LocalizedError` (including UniFFI-generated errors) provides,
    /// falling back to the debug rendering.
    var displayMessage: String {
        if let localized = (self as? LocalizedError)?.errorDescription {
            return localized
        }
        return String(describing: self)
    }
}
