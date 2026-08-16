// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// A physical radio endpoint that Azimuth can open.
public struct AzimuthRadioDevice: Identifiable, Sendable, Equatable {
    public let id: String
    public let name: String
    public let connection: String

    public init(id: String, name: String, connection: String) {
        self.id = id
        self.name = name
        self.connection = connection
    }

    /// The directly attached TH-D75. The VID/PID are encoded in the ID
    /// so a future multi-radio picker can retain stable identities.
    public static let thD75USBC = AzimuthRadioDevice(
        id: "usb:2166:9023",
        name: "Kenwood TH-D75",
        connection: "USB-C"
    )
}

public enum AzimuthRadioTransportState: Sendable, Equatable {
    case disconnected
    case connecting
    case connected
    case failed(message: String)
}

/// Byte-oriented radio connection used below the CAT/MCP automation layer.
public protocol AzimuthRadioTransport: Sendable {
    var device: AzimuthRadioDevice { get }
    var state: AzimuthRadioTransportState { get async }
    var stateStream: AsyncStream<AzimuthRadioTransportState> { get }
    /// Stable serial identifier reported by the physical USB device, when the
    /// platform can prove one for the currently opened endpoint.
    var hardwareSerialNumber: String? { get async }

    func open() async throws
    func close() async
    /// Synchronous by design: the core programming callback changes line
    /// coding before its async close/reopen sequence continues.
    func setBaudRate(baud: UInt32) throws
    func write(_ bytes: [UInt8]) async throws

    /// Suspends until at least one byte is available. An empty result means
    /// the connection closed (or this individual read was cancelled).
    func read(maxBytes: Int) async throws -> [UInt8]
}

public extension AzimuthRadioTransport {
    var hardwareSerialNumber: String? { get async { nil } }
}

public enum AzimuthRadioTransportError: LocalizedError, Sendable, Equatable {
    case notConnected
    case openFailed(reason: String)
    case writeFailed(reason: String)
    case readFailed(reason: String)

    public var errorDescription: String? {
        switch self {
        case .notConnected:
            return "The TH-D75 USB connection is not open."
        case .openFailed(let reason), .writeFailed(let reason), .readFailed(let reason):
            return reason
        }
    }
}

/// Deterministic Simulator link. It prevents the iOS simulator from looking
/// like a physical iPad whose approved driver simply has not started.
public final class AzimuthUnavailableUSBSerialLink: AzimuthUSBSerialLink, @unchecked Sendable {
    private let reason: String

    public init(reason: String) {
        self.reason = reason
    }

    public func servicePresent() -> Bool { false }
    public func open() throws { throw AzimuthUSBLinkError.unsupportedEnvironment(reason) }
    public func close() {}
    public func setBaudRate(baud: UInt32) throws {
        _ = baud
        throw AzimuthUSBLinkError.unsupportedEnvironment(reason)
    }
    public func write(_ bytes: [UInt8]) throws {
        _ = bytes
        throw AzimuthUSBLinkError.unsupportedEnvironment(reason)
    }
    public func drain(maxBytes: Int) throws -> [UInt8] {
        _ = maxBytes
        throw AzimuthUSBLinkError.unsupportedEnvironment(reason)
    }
    public func armDoorbell(onFire: @escaping @Sendable (Bool) -> Void) throws {
        _ = onFire
        throw AzimuthUSBLinkError.unsupportedEnvironment(reason)
    }
}
