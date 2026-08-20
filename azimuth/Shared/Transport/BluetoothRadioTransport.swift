// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// One paired Bluetooth Classic SPP endpoint returned by the platform core.
///
/// The address, rather than the display name, is the connection identity.
/// Several paired radios may retain the same factory name.
public struct AzimuthBluetoothEndpoint: Identifiable, Sendable, Equatable {
    public let address: String
    public let displayName: String
    /// CAT serial proved during same-radio USB-to-Bluetooth recovery. When
    /// present, every later exact-address open re-proves this identity.
    public let verifiedCATSerialNumber: String?

    public var id: String {
        Self.stableID(for: address)
    }

    public static func stableID(for address: String) -> String {
        "bluetooth:\(address.replacingOccurrences(of: "-", with: ":").uppercased())"
    }

    public var device: AzimuthRadioDevice {
        AzimuthRadioDevice(
            id: id,
            name: displayName,
            connectionKind: .bluetooth,
            connection: "Bluetooth"
        )
    }

    public init(
        address: String,
        displayName: String,
        verifiedCATSerialNumber: String? = nil
    ) {
        self.address = address
        self.displayName = displayName
        self.verifiedCATSerialNumber = verifiedCATSerialNumber
    }
}

/// Byte stream opened for one exact paired Bluetooth address.
///
/// The generated Rust bridge implements this seam. Keeping the bridge behind
/// a protocol makes exact-address selection and transport ownership testable
/// without launching the native helper.
public protocol AzimuthBluetoothByteLink: AnyObject, Sendable {
    /// CAT serial identity read from the currently opened radio.
    var hardwareSerialNumber: String? { get async }
    /// Exact paired address selected by the currently opened link.
    var matchedAddress: String? { get async }

    func open() async throws
    func close() async
    func write(_ bytes: [UInt8]) async throws
    func read(maxBytes: Int) async throws -> [UInt8]
}

public extension AzimuthBluetoothByteLink {
    var matchedAddress: String? { get async { nil } }
}

/// One native paired-device inventory.
///
/// Every endpoint is shown in the picker. Its exact address is selection
/// identity and its display name is presentation only; neither claims that the
/// device is a TH-D75.
public struct AzimuthBluetoothDiscoverySnapshot: Sendable, Equatable {
    public let pairedEndpoints: [AzimuthBluetoothEndpoint]

    public init(
        pairedEndpoints: [AzimuthBluetoothEndpoint]
    ) {
        self.pairedEndpoints = pairedEndpoints
    }
}

/// Discovers paired devices and creates a link for an exact address.
public protocol AzimuthBluetoothLinkFactory: Sendable {
    func pairedDeviceDiscovery() async throws -> AzimuthBluetoothDiscoverySnapshot
    func makeLink(exactAddress: String) async throws -> any AzimuthBluetoothByteLink
    func makeLink(
        exactAddress: String,
        matchingExpectedUSBSerialNumber serialNumber: String
    ) async throws -> any AzimuthBluetoothByteLink
    func makeLink(
        matchingExpectedUSBSerialNumber serialNumber: String
    ) async throws -> any AzimuthBluetoothByteLink
}

/// Closure-backed bridge used to adapt generated Bluetooth core objects.
///
/// Production composition supplies closures backed by the generated core;
/// tests can supply deterministic in-memory links through the same boundary.
public struct AzimuthBluetoothCoreBridge: AzimuthBluetoothLinkFactory, Sendable {
    public typealias EndpointDiscovery = @Sendable () async throws
        -> AzimuthBluetoothDiscoverySnapshot
    public typealias ExactLinkBuilder = @Sendable (String) async throws
        -> any AzimuthBluetoothByteLink
    public typealias QualifiedLinkBuilder = @Sendable (String, String) async throws
        -> any AzimuthBluetoothByteLink
    private let discover: EndpointDiscovery
    private let buildExactLink: ExactLinkBuilder
    private let buildQualifiedLink: QualifiedLinkBuilder
    private let buildMatchingLink: ExactLinkBuilder

    public init(
        discover: @escaping EndpointDiscovery,
        buildExactLink: @escaping ExactLinkBuilder,
        buildQualifiedLink: @escaping QualifiedLinkBuilder,
        buildMatchingLink: @escaping ExactLinkBuilder
    ) {
        self.discover = discover
        self.buildExactLink = buildExactLink
        self.buildQualifiedLink = buildQualifiedLink
        self.buildMatchingLink = buildMatchingLink
    }

    public func pairedDeviceDiscovery() async throws -> AzimuthBluetoothDiscoverySnapshot {
        try await discover()
    }

    public func makeLink(exactAddress: String) async throws -> any AzimuthBluetoothByteLink {
        try await buildExactLink(exactAddress)
    }

    public func makeLink(
        exactAddress: String,
        matchingExpectedUSBSerialNumber serialNumber: String
    ) async throws -> any AzimuthBluetoothByteLink {
        try await buildQualifiedLink(exactAddress, serialNumber)
    }

    public func makeLink(
        matchingExpectedUSBSerialNumber serialNumber: String
    ) async throws -> any AzimuthBluetoothByteLink {
        try await buildMatchingLink(serialNumber)
    }
}

private enum AzimuthBluetoothTransportTarget: Sendable {
    case exact(AzimuthBluetoothEndpoint)
    case exactExpectedUSBSerial(AzimuthBluetoothEndpoint, String)
    case expectedUSBSerial(String)

    var device: AzimuthRadioDevice {
        switch self {
        case .exact(let endpoint):
            endpoint.device
        case .exactExpectedUSBSerial(let endpoint, _):
            endpoint.device
        case .expectedUSBSerial(let serialNumber):
            AzimuthRadioDevice(
                id: "bluetooth:serial:\(serialNumber)",
                name: "Kenwood TH-D75",
                connectionKind: .bluetooth,
                connection: "Bluetooth"
            )
        }
    }
}

/// Normal Azimuth radio transport over one paired macOS Bluetooth SPP link.
///
/// The transport never opens by display name. Its factory receives the exact
/// address retained in `endpoint`. Exact-address links remain raw until the
/// controller runs its packet-mode preflight; same-radio fallback links expose
/// the CAT serial proved by the generated core through `hardwareSerialNumber`.
public actor AzimuthBluetoothRadioTransport: AzimuthRadioTransport {
    public nonisolated let device: AzimuthRadioDevice
    public nonisolated let stateStream: AsyncStream<AzimuthRadioTransportState>

    private let target: AzimuthBluetoothTransportTarget
    private let factory: any AzimuthBluetoothLinkFactory
    private let stateContinuation: AsyncStream<AzimuthRadioTransportState>.Continuation
    private var currentState: AzimuthRadioTransportState = .disconnected
    private var link: (any AzimuthBluetoothByteLink)?
    private var currentHardwareSerialNumber: String?
    private var currentMatchedAddress: String?
    private var generation: UInt64 = 0

    public init(
        endpoint: AzimuthBluetoothEndpoint,
        factory: any AzimuthBluetoothLinkFactory
    ) {
        target = .exact(endpoint)
        self.factory = factory
        device = target.device
        var continuation: AsyncStream<AzimuthRadioTransportState>.Continuation!
        stateStream = AsyncStream { continuation = $0 }
        stateContinuation = continuation
    }

    init(
        expectedUSBSerialNumber: String,
        factory: any AzimuthBluetoothLinkFactory
    ) {
        target = .expectedUSBSerial(expectedUSBSerialNumber)
        self.factory = factory
        device = target.device
        var continuation: AsyncStream<AzimuthRadioTransportState>.Continuation!
        stateStream = AsyncStream { continuation = $0 }
        stateContinuation = continuation
    }

    init(
        endpoint: AzimuthBluetoothEndpoint,
        expectedUSBSerialNumber: String,
        factory: any AzimuthBluetoothLinkFactory
    ) {
        target = .exactExpectedUSBSerial(endpoint, expectedUSBSerialNumber)
        self.factory = factory
        device = target.device
        var continuation: AsyncStream<AzimuthRadioTransportState>.Continuation!
        stateStream = AsyncStream { continuation = $0 }
        stateContinuation = continuation
    }

    public var state: AzimuthRadioTransportState { currentState }

    public var hardwareSerialNumber: String? { currentHardwareSerialNumber }

    var resolvedEndpoint: AzimuthBluetoothEndpoint? {
        switch target {
        case .exact(let endpoint):
            endpoint
        case .exactExpectedUSBSerial(let endpoint, let serialNumber):
            AzimuthBluetoothEndpoint(
                address: endpoint.address,
                displayName: endpoint.displayName,
                verifiedCATSerialNumber: serialNumber
            )
        case .expectedUSBSerial(let serialNumber):
            currentMatchedAddress.map {
                AzimuthBluetoothEndpoint(
                    address: $0,
                    displayName: "Kenwood TH-D75",
                    verifiedCATSerialNumber: serialNumber
                )
            }
        }
    }

    public func open() async throws {
        guard currentState != .connected else { return }
        generation &+= 1
        let attempt = generation
        updateState(.connecting)
        var attemptedLink: (any AzimuthBluetoothByteLink)?
        var linkWasInstalled = false

        do {
            let openedLink = try await makeLink()
            attemptedLink = openedLink
            try ensureCurrent(attempt)
            link = openedLink
            linkWasInstalled = true
            try await openedLink.open()
            try ensureCurrent(attempt)
            let hardwareSerialNumber = await openedLink.hardwareSerialNumber
            try ensureCurrent(attempt)
            let matchedAddress = await openedLink.matchedAddress
            try ensureCurrent(attempt)
            currentHardwareSerialNumber = hardwareSerialNumber
            currentMatchedAddress = matchedAddress
            updateState(.connected)
        } catch is CancellationError {
            // A concurrent `close()` owns an installed link and has already
            // closed it. A task cancellation in the current generation, or a
            // stale factory result which was never installed, still needs
            // local cleanup.
            if attempt == generation || !linkWasInstalled {
                await attemptedLink?.close()
            }
            if attempt == generation {
                generation &+= 1
                link = nil
                currentHardwareSerialNumber = nil
                currentMatchedAddress = nil
                updateState(.disconnected)
            }
            throw CancellationError()
        } catch {
            guard attempt == generation else { throw CancellationError() }
            generation &+= 1
            let openedLink = link
            link = nil
            currentHardwareSerialNumber = nil
            currentMatchedAddress = nil
            await openedLink?.close()
            let reason = Self.describe(error)
            updateState(.failed(message: reason))
            throw AzimuthRadioTransportError.openFailed(reason: reason)
        }
    }

    public func close() async {
        guard currentState != .disconnected || link != nil else { return }
        generation &+= 1
        let openedLink = link
        link = nil
        currentHardwareSerialNumber = nil
        currentMatchedAddress = nil
        await openedLink?.close()
        updateState(.disconnected)
    }

    /// Bluetooth SPP has a fixed radio-side rate, so CDC line coding does not
    /// apply. The generated transport intentionally treats this as a no-op.
    public nonisolated func setBaudRate(baud: UInt32) throws {
        _ = baud
    }

    public func write(_ bytes: [UInt8]) async throws {
        guard currentState == .connected, let link else {
            throw AzimuthRadioTransportError.notConnected
        }
        guard !bytes.isEmpty else { return }
        let attempt = generation
        do {
            try await link.write(bytes)
            try ensureCurrent(attempt)
        } catch is CancellationError {
            throw CancellationError()
        } catch {
            await failCurrentLink(
                link,
                generation: attempt,
                reason: Self.describe(error)
            )
            throw AzimuthRadioTransportError.writeFailed(reason: Self.describe(error))
        }
    }

    public func read(maxBytes: Int) async throws -> [UInt8] {
        guard maxBytes > 0 else {
            throw AzimuthRadioTransportError.readFailed(
                reason: "maxBytes must be positive"
            )
        }
        guard currentState == .connected, let link else {
            return []
        }
        let attempt = generation
        do {
            let bytes = try await link.read(maxBytes: maxBytes)
            try ensureCurrent(attempt)
            if bytes.isEmpty {
                await failCurrentLink(
                    link,
                    generation: attempt,
                    reason: nil
                )
            }
            return bytes
        } catch is CancellationError {
            throw CancellationError()
        } catch {
            let reason = Self.describe(error)
            await failCurrentLink(link, generation: attempt, reason: reason)
            throw AzimuthRadioTransportError.readFailed(reason: reason)
        }
    }

    private func ensureCurrent(_ attempt: UInt64) throws {
        guard attempt == generation else { throw CancellationError() }
        try Task.checkCancellation()
    }

    private func makeLink() async throws -> any AzimuthBluetoothByteLink {
        switch target {
        case .exact(let endpoint):
            try await factory.makeLink(exactAddress: endpoint.address)
        case .exactExpectedUSBSerial(let endpoint, let serialNumber):
            try await factory.makeLink(
                exactAddress: endpoint.address,
                matchingExpectedUSBSerialNumber: serialNumber
            )
        case .expectedUSBSerial(let serialNumber):
            try await factory.makeLink(
                matchingExpectedUSBSerialNumber: serialNumber
            )
        }
    }

    private func failCurrentLink(
        _ failedLink: any AzimuthBluetoothByteLink,
        generation attempt: UInt64,
        reason: String?
    ) async {
        guard attempt == generation, link === failedLink else { return }
        generation &+= 1
        link = nil
        currentHardwareSerialNumber = nil
        currentMatchedAddress = nil
        await failedLink.close()
        if let reason {
            updateState(.failed(message: reason))
        } else {
            updateState(.disconnected)
        }
    }

    private func updateState(_ next: AzimuthRadioTransportState) {
        currentState = next
        stateContinuation.yield(next)
    }

    private static func describe(_ error: Error) -> String {
        if let localized = error as? LocalizedError,
           let description = localized.errorDescription {
            return description
        }
        return String(describing: error)
    }
}
