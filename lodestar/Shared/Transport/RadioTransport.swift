// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// Connection state for a `RadioTransport`.
public enum RadioTransportState: Sendable, Equatable {
    case disconnected
    case connecting
    case connected
    case failed(message: String)
}

/// Abstract transport to a radio. Bytes in, bytes out.
///
/// Implementations:
/// - `IOBluetoothTransport` (Mac Catalyst, via IOBluetooth RFCOMM).
/// - `MockRadioTransport` (unit tests, in-memory).
/// - Future: `USBCDCTransport` (iPadOS + iPhone 15+ + Mac, via IOUSBHost).
public protocol RadioTransport: Sendable {
    /// The device this transport talks to.
    var device: BluetoothDevice { get }

    /// Current state. Kept in sync with `stateStream`.
    var state: RadioTransportState { get async }

    /// Async stream of state transitions. Finishes when the transport is deallocated.
    var stateStream: AsyncStream<RadioTransportState> { get }

    /// Open the connection. Throws on failure.
    func open() async throws

    /// Close the connection cleanly. Safe to call even if already closed.
    func close() async

    /// Write `bytes` to the radio. Throws on failure.
    func write(_ bytes: [UInt8]) async throws

    /// Read at most `maxBytes` bytes. Blocks until at least 1 byte is available
    /// or the transport closes. Returns an empty array on clean close.
    func read(maxBytes: Int) async throws -> [UInt8]
}

/// Errors surfaced by any `RadioTransport`.
public enum RadioTransportError: Error, Sendable, Equatable {
    case notAvailableOnPlatform(reason: String)
    case notConnected
    case openFailed(reason: String)
    case writeFailed(reason: String)
    case readFailed(reason: String)
    case deviceNotFound(address: String)
}
