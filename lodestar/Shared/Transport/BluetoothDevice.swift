// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// A paired Bluetooth device the user can select as a radio.
public struct BluetoothDevice: Identifiable, Sendable, Hashable {
    /// Stable identifier — the device's Bluetooth address string.
    public let id: String
    /// Display name (e.g. "TH-D75").
    public let name: String
    /// Bluetooth address in `XX-XX-XX-XX-XX-XX` form.
    public let address: String

    public init(id: String, name: String, address: String) {
        self.id = id
        self.name = name
        self.address = address
    }
}
