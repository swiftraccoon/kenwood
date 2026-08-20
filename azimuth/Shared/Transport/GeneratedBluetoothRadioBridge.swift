// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

#if os(macOS)

import Foundation

/// macOS factory backed by Azimuth Core's signed native Bluetooth helper.
struct AzimuthGeneratedBluetoothLinkFactory: AzimuthBluetoothLinkFactory {
    private let authorization: any AzimuthBluetoothAuthorizationProviding

    init(
        authorization: any AzimuthBluetoothAuthorizationProviding =
            AzimuthMacBluetoothAuthorizationProvider.shared
    ) {
        self.authorization = authorization
    }

    func pairedDeviceDiscovery() async throws -> AzimuthBluetoothDiscoverySnapshot {
        try await authorization.ensureBluetoothAuthorization()
        let discovery = try await discoverPairedBluetoothDevices()
        return AzimuthBluetoothDiscoverySnapshot(
            pairedEndpoints: discovery.devices.map {
                AzimuthBluetoothEndpoint(
                    address: $0.address,
                    displayName: $0.displayName
                )
            }
        )
    }

    func makeLink(exactAddress: String) async throws -> any AzimuthBluetoothByteLink {
        try await authorization.ensureBluetoothAuthorization()
        return try AzimuthGeneratedBluetoothByteLink(
            target: .exactAddress(address: exactAddress),
            authorization: authorization
        )
    }

    func makeLink(
        exactAddress: String,
        matchingExpectedUSBSerialNumber serialNumber: String
    ) async throws -> any AzimuthBluetoothByteLink {
        try await authorization.ensureBluetoothAuthorization()
        return try AzimuthGeneratedBluetoothByteLink(
            target: .exactAddressExpectedUsbSerial(
                address: exactAddress,
                serialNumber: serialNumber
            ),
            authorization: authorization
        )
    }

    func makeLink(
        matchingExpectedUSBSerialNumber serialNumber: String
    ) async throws -> any AzimuthBluetoothByteLink {
        try await authorization.ensureBluetoothAuthorization()
        return try AzimuthGeneratedBluetoothByteLink(
            target: .expectedUsbSerial(serialNumber: serialNumber),
            authorization: authorization
        )
    }
}

/// Adapts the generated Rust object to Azimuth's transport-neutral byte seam.
final class AzimuthGeneratedBluetoothByteLink: AzimuthBluetoothByteLink,
    @unchecked Sendable
{
    private let core: any BluetoothByteTransportProtocol
    private let authorization: any AzimuthBluetoothAuthorizationProviding

    init(
        target: BluetoothLinkTarget,
        authorization: any AzimuthBluetoothAuthorizationProviding
    ) throws {
        core = try BluetoothByteTransport(target: target)
        self.authorization = authorization
    }

    init(
        core: any BluetoothByteTransportProtocol,
        authorization: any AzimuthBluetoothAuthorizationProviding =
            AzimuthBluetoothAuthorizationBridge {}
    ) {
        self.core = core
        self.authorization = authorization
    }

    var hardwareSerialNumber: String? {
        get async { try? core.matchedCatSerial() }
    }

    var matchedAddress: String? {
        get async { try? core.matchedAddress() }
    }

    func open() async throws {
        do {
            try await authorization.ensureBluetoothAuthorization()
            try await withTaskCancellationHandler {
                try Task.checkCancellation()
                do {
                    try await core.open()
                } catch {
                    try Task.checkCancellation()
                    throw error
                }
                try Task.checkCancellation()
            } onCancel: {
                core.cancelPendingOpen()
            }
        } catch is CancellationError {
            // Rust close both reaps any opened helper and consumes a sticky
            // cancel which raced ahead of native open registration.
            try? await core.close()
            throw CancellationError()
        }
    }

    func close() async {
        try? await core.close()
    }

    func write(_ bytes: [UInt8]) async throws {
        try await core.write(bytes: Data(bytes))
    }

    func read(maxBytes: Int) async throws -> [UInt8] {
        guard let maximum = UInt32(exactly: maxBytes) else {
            throw AzimuthRadioTransportError.readFailed(
                reason: "Bluetooth read length does not fit UInt32"
            )
        }
        let bytes = try await withTaskCancellationHandler {
            while true {
                try Task.checkCancellation()
                do {
                    return try await core.read(maxLength: maximum)
                } catch BluetoothLinkError.ReadInterrupted {
                    // Writes and close also wake the single blocking native
                    // read. Retry only when this Swift owner remains live.
                    try Task.checkCancellation()
                }
            }
        } onCancel: {
            core.cancelPendingRead()
        }
        return [UInt8](bytes)
    }
}

#endif
