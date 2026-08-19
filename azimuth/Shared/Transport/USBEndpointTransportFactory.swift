// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// One exact macOS USB CDC endpoint returned by verified TH-D75 discovery.
///
/// The device path is an open target and endpoint detail, not user-facing
/// transport copy. A connected controller therefore reports `USB-C` while the
/// picker can still distinguish two attached radios by their exact paths.
public struct AzimuthUSBEndpoint: Identifiable, Sendable, Equatable {
    public let id: String
    public let displayName: String
    public let devicePath: String
    /// Stable IORegistry USB serial used to follow the same radio when its CDC
    /// tty path changes after a reboot.
    public let usbSerialNumber: String?

    public var device: AzimuthRadioDevice {
        AzimuthRadioDevice(
            id: id,
            name: displayName,
            connectionKind: .usb,
            connection: "USB-C"
        )
    }

    public static func stableID(
        devicePath: String,
        usbSerialNumber: String?
    ) -> String {
        usbSerialNumber.map { "usb:serial:\($0)" }
            ?? "tty:\(devicePath)"
    }

    public init(
        id: String,
        displayName: String,
        devicePath: String,
        usbSerialNumber: String? = nil
    ) {
        self.id = id
        self.displayName = displayName
        self.devicePath = devicePath
        self.usbSerialNumber = usbSerialNumber
    }
}

/// Discovers exact USB endpoints and constructs a transport pinned to one.
public protocol AzimuthUSBTransportFactory: Sendable {
    func availableEndpoints() -> [AzimuthUSBEndpoint]
    func makeTransport(
        endpoint: AzimuthUSBEndpoint
    ) throws -> any AzimuthRadioTransport
}

/// Closure-backed USB factory for deterministic endpoint-routing tests.
public struct AzimuthUSBCoreBridge: AzimuthUSBTransportFactory, Sendable {
    public typealias EndpointDiscovery = @Sendable () -> [AzimuthUSBEndpoint]
    public typealias TransportBuilder = @Sendable (
        AzimuthUSBEndpoint
    ) throws -> any AzimuthRadioTransport

    private let discover: EndpointDiscovery
    private let buildTransport: TransportBuilder

    public init(
        discover: @escaping EndpointDiscovery,
        buildTransport: @escaping TransportBuilder
    ) {
        self.discover = discover
        self.buildTransport = buildTransport
    }

    public func availableEndpoints() -> [AzimuthUSBEndpoint] {
        discover()
    }

    public func makeTransport(
        endpoint: AzimuthUSBEndpoint
    ) throws -> any AzimuthRadioTransport {
        try buildTransport(endpoint)
    }
}

#if os(macOS)

/// Production macOS USB discovery and exact-path transport construction.
public struct AzimuthPlatformUSBTransportFactory: AzimuthUSBTransportFactory {
    public init() {}

    public func availableEndpoints() -> [AzimuthUSBEndpoint] {
        POSIXAzimuthUSBSerialLink.availableDeviceDescriptors().map { descriptor in
            return AzimuthUSBEndpoint(
                id: AzimuthUSBEndpoint.stableID(
                    devicePath: descriptor.path,
                    usbSerialNumber: descriptor.serialNumber
                ),
                displayName: "Kenwood TH-D75",
                devicePath: descriptor.path,
                usbSerialNumber: descriptor.serialNumber
            )
        }
    }

    public func makeTransport(
        endpoint: AzimuthUSBEndpoint
    ) throws -> any AzimuthRadioTransport {
        let expectedID = AzimuthUSBEndpoint.stableID(
            devicePath: endpoint.devicePath,
            usbSerialNumber: endpoint.usbSerialNumber
        )
        guard endpoint.id == expectedID, !endpoint.devicePath.isEmpty else {
            throw RadioEndpointSelectionError.malformedEndpoint
        }
        return AzimuthUSBSerialTransport(
            device: endpoint.device,
            link: POSIXAzimuthUSBSerialLink(
                devicePath: endpoint.devicePath,
                expectedSerialNumber: endpoint.usbSerialNumber
            )
        )
    }
}

#endif
