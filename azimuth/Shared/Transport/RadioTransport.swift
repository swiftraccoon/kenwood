// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

public enum AzimuthRadioConnectionKind: String, Sendable, Equatable {
    case usb
    case bluetooth

    var title: String {
        switch self {
        case .usb: return "USB-C"
        case .bluetooth: return "Bluetooth"
        }
    }
}

/// A physical radio endpoint that Azimuth can open.
public struct AzimuthRadioDevice: Identifiable, Sendable, Equatable {
    public let id: String
    public let name: String
    public let connectionKind: AzimuthRadioConnectionKind
    public let connection: String

    public init(
        id: String,
        name: String,
        connectionKind: AzimuthRadioConnectionKind,
        connection: String
    ) {
        self.id = id
        self.name = name
        self.connectionKind = connectionKind
        self.connection = connection
    }

    /// The directly attached TH-D75. The VID/PID are encoded in the ID
    /// so a future multi-radio picker can retain stable identities.
    public static let thD75USBC = AzimuthRadioDevice(
        id: "usb:2166:9023",
        name: "Kenwood TH-D75",
        connectionKind: .usb,
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
    /// Stable serial identifier for the physical radio, when the selected
    /// connection can prove one for its currently opened endpoint.
    var hardwareSerialNumber: String? { get async }
    /// Current macOS IORegistry ID for the physical USB-device ancestor shared
    /// by the opened CDC and CoreAudio interfaces. Nil on other platforms and
    /// for every non-USB or closed transport.
    var macOSUSBDeviceRegistryEntryID: UInt64? { get async }

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

/// Optional handoff supported by a transport router that can replace a failed
/// USB control session with the paired Bluetooth link for the same radio.
///
/// Selection is non-destructive and does not open the new link. The next
/// ordinary controller connection performs the bounded Bluetooth serial proof.
protocol AzimuthSameRadioBluetoothSelecting: Sendable {
    func selectBluetoothForSameRadio(
        expectedSerialNumber: String
    ) async throws

    /// Exact address already CAT-qualified for this serial, when unique.
    func knownQualifiedBluetoothAddress(
        expectedSerialNumber: String
    ) async throws -> String?

    /// Refresh USB discovery and select the current tty path for this serial.
    func selectUSBForRecovery(
        expectedSerialNumber: String
    ) async throws

    /// Retain the currently selected exact Bluetooth address and require its
    /// next open to prove the supplied CAT serial. This is used after a
    /// consented setting change reboots the same radio.
    func qualifySelectedBluetoothForReconnect(
        expectedSerialNumber: String
    ) async throws
}

/// Host-side continuity for the currently selected USB endpoint after a radio
/// reset. USB descriptor fields and registry IDs are only re-enumeration hints;
/// the controller must still prove the expected radio with CAT `AE` after open.
protocol AzimuthSameRadioUSBRefreshing: Sendable {
    /// Refresh discovery and select the unique candidate which follows the
    /// current USB endpoint. Returns false while that candidate is absent or
    /// ambiguous and never treats a USB descriptor serial as CAT identity.
    func refreshSelectedUSBForSameRadioRecovery() async throws -> Bool
}

/// Optional non-destructive handoff from a Bluetooth interface that answered
/// MMDVM to the sole attached, verified TH-D75 USB-C endpoint.
///
/// Availability never changes selection. Selection occurs only after the user
/// accepts the recovery prompt. USB descriptors are not treated as radio
/// identity because a conforming TH-D75 may expose no USB serial string; the
/// subsequent CAT operation obtains the authoritative unit serial with `AE`.
protocol AzimuthBluetoothMMDVMUSBSelecting: Sendable {
    func hasSoleVerifiedUSBEndpoint() async throws -> Bool
    func selectSoleUSBForBluetoothMMDVM() async throws

    /// Restore the exact Bluetooth address retained before the USB routing
    /// operation, requiring its CAT serial to match the serial proved over CAT
    /// on the selected USB endpoint. This never scans or selects by display
    /// name.
    func selectOriginalBluetoothAfterUSBRouting(
        expectedSerialNumber: String
    ) async throws

    /// Restore the exact raw Bluetooth selection after a pre-mutation failure.
    /// No serial qualification is attached because the MMDVM link could not
    /// prove one before the failed USB operation.
    func restoreOriginalBluetoothAfterUSBRoutingFailure() async throws
}

/// Retained host-side routing context for IF-DSP handoff from one exact
/// Bluetooth endpoint to the sole attached TH-D75 USB CDC endpoint.
///
/// USB descriptor serials are never radio identity. Selection retains physical
/// enumeration hints only so the same candidate can be followed across a radio
/// restart. The controller accepts the handoff only after CAT `AE` from the
/// opened USB transport exactly matches the approved Bluetooth CAT session.
protocol AzimuthIFDSPUSBSelecting: Sendable {
    /// Refresh USB discovery and retain the sole qualified endpoint without
    /// changing or closing the current Bluetooth selection.
    func retainSoleIFDSPUSBEndpoint() async throws -> Bool

    /// Refresh discovery and select the retained USB candidate if it is
    /// currently present. Returns false while it is absent or ambiguous.
    func selectRetainedIFDSPUSBEndpoint() async throws -> Bool

    /// Restore the exact Bluetooth address retained by the pre-handoff check,
    /// binding its next open to the approved CAT `AE` serial.
    func restoreRetainedIFDSPBluetoothEndpoint(
        expectedSerialNumber: String
    ) async throws

    /// Discard completed or failed handoff authority without changing the
    /// selected endpoint.
    func finishRetainedIFDSPUSBHandoff() async
}

public extension AzimuthRadioTransport {
    var hardwareSerialNumber: String? { get async { nil } }
    var macOSUSBDeviceRegistryEntryID: UInt64? { get async { nil } }
}

public enum AzimuthRadioTransportError: LocalizedError, Sendable, Equatable {
    case notConnected
    case openFailed(reason: String)
    case writeFailed(reason: String)
    case readFailed(reason: String)

    public var errorDescription: String? {
        switch self {
        case .notConnected:
            return "The TH-D75 radio connection is not open."
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
