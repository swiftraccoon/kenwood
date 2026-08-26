// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

private enum AzimuthTransportSelection: Sendable, Equatable {
    case usb(AzimuthUSBEndpoint)
    case bluetooth(AzimuthBluetoothEndpoint)
    case qualifiedBluetooth(AzimuthBluetoothEndpoint, expectedUSBSerial: String)
    case bluetoothMatchingUSBSerial(String)

    var device: AzimuthRadioDevice {
        switch self {
        case .usb(let endpoint):
            endpoint.device
        case .bluetooth(let endpoint):
            endpoint.device
        case .qualifiedBluetooth(let endpoint, _):
            endpoint.device
        case .bluetoothMatchingUSBSerial(let serialNumber):
            AzimuthRadioDevice(
                id: "bluetooth:serial:\(serialNumber)",
                name: "Kenwood TH-D75",
                connectionKind: .bluetooth,
                connection: "Bluetooth"
            )
        }
    }
}

/// Errors from endpoint discovery, selection, and same-radio qualification.
public enum AzimuthRadioSelectionError: LocalizedError, Sendable, Equatable {
    case transportIsOpen
    case ambiguousQualifiedBluetoothSerial(serialNumber: String)
    case expectedUSBRadioUnavailable(serialNumber: String)
    case differentUSBRadioAtRetainedPath(expected: String, actual: String?)
    case resolvedBluetoothAddressUnavailable
    case bluetoothMmdvmUSBFallbackUnavailable(attachedUSBCount: Int)
    case bluetoothMmdvmOriginalBluetoothUnavailable
    case bluetoothMmdvmUSBRoutingContextUnavailable
    case bluetoothReconnectContextUnavailable
    case ifDSPOriginalBluetoothUnavailable
    case ifDSPUSBHandoffContextUnavailable

    public var errorDescription: String? {
        switch self {
        case .transportIsOpen:
            "Disconnect the current radio before changing connection methods."
        case .ambiguousQualifiedBluetoothSerial(let serialNumber):
            "More than one retained Bluetooth address claims CAT serial \(serialNumber). Refresh paired devices before continuing."
        case .expectedUSBRadioUnavailable(let serialNumber):
            "USB radio \(serialNumber) has not re-enumerated yet."
        case .differentUSBRadioAtRetainedPath(let expected, let actual):
            "A different USB radio appeared at the recovery path. Expected \(expected), found \(actual ?? "no stable serial number")."
        case .resolvedBluetoothAddressUnavailable:
            "The same-radio Bluetooth link opened without reporting its exact paired address. Azimuth closed it rather than retaining an ambiguous connection."
        case .bluetoothMmdvmUSBFallbackUnavailable(let attachedUSBCount):
            "Automatic USB-C handoff requires exactly one attached TH-D75; found \(attachedUSBCount). Choose the intended USB-C endpoint in the connection picker."
        case .bluetoothMmdvmOriginalBluetoothUnavailable:
            "DV Gateway USB-C routing requires the exact Bluetooth address which positively answered MMDVM. Refresh connections and select that Bluetooth endpoint again."
        case .bluetoothMmdvmUSBRoutingContextUnavailable:
            "The exact Bluetooth endpoint retained for DV Gateway USB-C routing is no longer available. Retry from the Bluetooth MMDVM prompt."
        case .bluetoothReconnectContextUnavailable:
            "The exact Bluetooth endpoint used before the radio restart is no longer selected. Reconnect that Bluetooth endpoint before retrying."
        case .ifDSPOriginalBluetoothUnavailable:
            "IF-DSP USB-C handoff requires the exact Bluetooth endpoint from the connected CAT session."
        case .ifDSPUSBHandoffContextUnavailable:
            "The retained IF-DSP USB-C handoff context is no longer available. Retry from the connected Bluetooth CAT session."
        }
    }
}

/// Synchronous forwarding slot for the generated core's baud callback.
///
/// The router itself is an actor, but `AzimuthRadioTransport.setBaudRate` is
/// deliberately synchronous. This slot publishes exactly one active owner to
/// that callback without holding its lock during platform I/O.
private final class AzimuthActiveTransportSlot: @unchecked Sendable {
    private let lock = NSLock()
    private var active: (any AzimuthRadioTransport)?

    func replace(with transport: (any AzimuthRadioTransport)?) {
        lock.withLock { active = transport }
    }

    func setBaudRate(_ baud: UInt32) throws {
        let transport = lock.withLock { active }
        guard let transport else { throw AzimuthRadioTransportError.notConnected }
        try transport.setBaudRate(baud: baud)
    }
}

/// Locked snapshot for the protocol's synchronous device property.
private final class AzimuthSelectedDeviceSlot: @unchecked Sendable {
    private let lock = NSLock()
    private var selectedDevice: AzimuthRadioDevice

    init(_ device: AzimuthRadioDevice) {
        selectedDevice = device
    }

    var device: AzimuthRadioDevice {
        lock.withLock { selectedDevice }
    }

    func replace(with device: AzimuthRadioDevice) {
        lock.withLock { selectedDevice = device }
    }
}

/// Routes one Azimuth controller to either USB or one exact Bluetooth device.
///
/// Only one child transport is active at a time. Endpoint changes are accepted
/// only while disconnected, stale child state is generation-fenced, and the
/// same-radio fallback delegates exact serial qualification to the core.
public actor AzimuthSelectableRadioTransport: AzimuthRadioTransport,
    AzimuthSameRadioBluetoothSelecting, AzimuthSameRadioUSBRefreshing,
    AzimuthBluetoothMMDVMUSBSelecting, AzimuthIFDSPUSBSelecting
{
    private struct CleanupSlot: Sendable {
        let id: UInt64
        let task: Task<Void, Never>
        let finalState: AzimuthRadioTransportState
    }

    private struct IFDSPUSBHandoffContext: Sendable {
        let bluetoothEndpoint: AzimuthBluetoothEndpoint
        let usbEndpoint: AzimuthUSBEndpoint
    }

    public nonisolated let stateStream: AsyncStream<AzimuthRadioTransportState>

    nonisolated let initialEndpoints: [RadioEndpoint]

    private let usbFactory: any AzimuthUSBTransportFactory
    private let bluetoothFactory: any AzimuthBluetoothLinkFactory
    private let stateContinuation: AsyncStream<AzimuthRadioTransportState>.Continuation
    private let activeSlot = AzimuthActiveTransportSlot()
    private let selectedDeviceSlot: AzimuthSelectedDeviceSlot

    private var selected: AzimuthTransportSelection?
    private var retainedUSBEndpoint: AzimuthUSBEndpoint?
    private var retainedBluetoothMmdvmRoutingEndpoint: AzimuthBluetoothEndpoint?
    private var retainedBluetoothReconnectEndpoint: AzimuthBluetoothEndpoint?
    private var retainedIFDSPUSBHandoff: IFDSPUSBHandoffContext?
    private var lastBluetoothEndpoints: [AzimuthBluetoothEndpoint] = []
    private var endpointDiscoveryGeneration: UInt64 = 0
    private var activeTransport: (any AzimuthRadioTransport)?
    private var activeSelection: AzimuthTransportSelection?
    private var stateObserver: Task<Void, Never>?
    private var currentState: AzimuthRadioTransportState = .disconnected
    private var currentHardwareSerialNumber: String?
    private var currentMacOSUSBDeviceRegistryEntryID: UInt64?
    private var generation: UInt64 = 0
    private var cleanupSlot: CleanupSlot?
    private var nextCleanupID: UInt64 = 0

    public init(
        usbFactory: any AzimuthUSBTransportFactory,
        bluetoothFactory: any AzimuthBluetoothLinkFactory
    ) throws {
        let usbEndpoints = usbFactory.availableEndpoints()
        try Self.validateUSBEndpoints(usbEndpoints)
        self.usbFactory = usbFactory
        self.bluetoothFactory = bluetoothFactory
        let initialSelection = usbEndpoints.first.map(AzimuthTransportSelection.usb)
        selected = initialSelection
        retainedUSBEndpoint = usbEndpoints.first
        selectedDeviceSlot = AzimuthSelectedDeviceSlot(
            initialSelection?.device ?? .thD75USBC
        )
        initialEndpoints = usbEndpoints.map(Self.radioEndpoint(for:))
        var continuation: AsyncStream<AzimuthRadioTransportState>.Continuation!
        stateStream = AsyncStream { continuation = $0 }
        stateContinuation = continuation
    }

    /// Stable identifier prepared for the next connection.
    var selectedEndpointID: String? {
        selected?.device.id
    }

    func selectedRadioEndpoint() -> RadioEndpoint? {
        switch selected {
        case .usb(let endpoint):
            Self.radioEndpoint(for: endpoint)
        case .bluetooth(let endpoint),
             .qualifiedBluetooth(let endpoint, _):
            RadioEndpoint(
                id: endpoint.id,
                name: endpoint.displayName,
                transport: .bluetooth,
                detail: endpoint.address
            )
        case .bluetoothMatchingUSBSerial(let serialNumber):
            RadioEndpoint(
                id: "bluetooth:serial:\(serialNumber)",
                name: "Kenwood TH-D75",
                transport: .bluetooth,
                detail: "same radio as USB serial \(serialNumber)"
            )
        case nil:
            nil
        }
    }

    /// The selected radio description. The controller reads this only after
    /// `open()` has established the selected child transport.
    public nonisolated var device: AzimuthRadioDevice {
        selectedDeviceSlot.device
    }

    public var state: AzimuthRadioTransportState { currentState }

    public var hardwareSerialNumber: String? { currentHardwareSerialNumber }

    public var macOSUSBDeviceRegistryEntryID: UInt64? {
        currentMacOSUSBDeviceRegistryEntryID
    }

    /// Returns USB followed by every device in one bounded paired-device
    /// inventory. Bluetooth display names are presentation only; connection
    /// later qualifies the selected exact address over CAT.
    func availableEndpointSnapshot() async throws -> RadioEndpointDiscoverySnapshot {
        endpointDiscoveryGeneration &+= 1
        let discoveryGeneration = endpointDiscoveryGeneration
        let discoveryResult: Result<AzimuthBluetoothDiscoverySnapshot, Error>
        do {
            discoveryResult = .success(try await pairedBluetoothDiscovery())
        } catch {
            discoveryResult = .failure(error)
        }
        try ensureCurrentEndpointDiscovery(discoveryGeneration)

        let bluetoothDiscovery: AzimuthBluetoothDiscoverySnapshot
        switch discoveryResult {
        case .success(let discovery):
            bluetoothDiscovery = discovery
        case .failure(let error as RadioEndpointSelectionError):
            // Generated discovery output which violates the endpoint contract
            // is not an optional Bluetooth availability failure. Reject the
            // malformed snapshot before it can alter retained selection.
            throw error
        case .failure(let error):
            let usb = usbFactory.availableEndpoints()
            try Self.validateUSBEndpoints(usb)
            try ensureCurrentEndpointDiscovery(discoveryGeneration)
            let retainedQualified = lastBluetoothEndpoints.filter {
                $0.verifiedCATSerialNumber != nil
            }
            lastBluetoothEndpoints = retainedQualified
            return RadioEndpointDiscoverySnapshot(
                endpoints: usb.map(Self.radioEndpoint(for:)) + retainedQualified.map {
                    Self.radioEndpoint(for: $0)
                },
                warning: "Bluetooth connections unavailable: \(Self.describe(error))",
                pairedBluetoothDeviceCount: nil
            )
        }
        let usb = usbFactory.availableEndpoints()
        try Self.validateUSBEndpoints(usb)
        try ensureCurrentEndpointDiscovery(discoveryGeneration)
        let bluetooth = Self.mergeBluetoothEndpoints(
            bluetoothDiscovery.pairedEndpoints,
            retainingQualifiedFrom: lastBluetoothEndpoints
        )
        try Self.validateCombinedEndpointIDs(usb: usb, bluetooth: bluetooth)
        lastBluetoothEndpoints = bluetooth
        guard let pairedDeviceCount = UInt32(exactly: bluetooth.count) else {
            throw RadioEndpointSelectionError.malformedEndpoint
        }
        return RadioEndpointDiscoverySnapshot(
            endpoints: usb.map(Self.radioEndpoint(for:)) + bluetooth.map {
                Self.radioEndpoint(for: $0)
            },
            pairedBluetoothDeviceCount: pairedDeviceCount
        )
    }

    /// Select an endpoint from a current discovery snapshot.
    ///
    /// Bluetooth selection uses the last complete discovery snapshot; it does
    /// not launch a second helper before open. An identifier absent from that
    /// snapshot fails as stale, while the later exact-address open revalidates
    /// that the paired endpoint is still available. Display names are never
    /// used as connection identity.
    func selectEndpoint(id: String) async throws {
        guard activeTransport == nil, cleanupSlot == nil else {
            throw AzimuthRadioSelectionError.transportIsOpen
        }
        let usb = usbFactory.availableEndpoints()
        try Self.validateUSBEndpoints(usb)
        if let endpoint = usb.first(where: { $0.id == id }) {
            retainedIFDSPUSBHandoff = nil
            retainedBluetoothMmdvmRoutingEndpoint = nil
            retainedBluetoothReconnectEndpoint = nil
            selected = .usb(endpoint)
            retainedUSBEndpoint = endpoint
            selectedDeviceSlot.replace(with: endpoint.device)
            updateState(.disconnected)
            return
        }
        guard let endpoint = lastBluetoothEndpoints.first(where: { $0.id == id }) else {
            throw RadioEndpointSelectionError.invalidEndpoint(id: id)
        }
        retainedIFDSPUSBHandoff = nil
        retainedBluetoothMmdvmRoutingEndpoint = nil
        retainedBluetoothReconnectEndpoint = nil
        if let serialNumber = endpoint.verifiedCATSerialNumber {
            selected = .qualifiedBluetooth(
                endpoint,
                expectedUSBSerial: serialNumber
            )
        } else {
            selected = .bluetooth(endpoint)
        }
        selectedDeviceSlot.replace(with: endpoint.device)
        updateState(.disconnected)
    }

    /// Select the paired Bluetooth endpoint whose CAT serial matches USB.
    ///
    /// The next `open()` asks the core to enumerate exact paired addresses and
    /// retain only the link whose CAT serial matches this USB identity.
    @discardableResult
    func selectBluetooth(
        matchingSerialNumber expectedSerialNumber: String
    ) async throws -> RadioEndpoint {
        guard activeTransport == nil, cleanupSlot == nil else {
            throw AzimuthRadioSelectionError.transportIsOpen
        }
        let retainedMatches = lastBluetoothEndpoints.filter {
            $0.verifiedCATSerialNumber == expectedSerialNumber
        }
        guard retainedMatches.count <= 1 else {
            throw AzimuthRadioSelectionError.ambiguousQualifiedBluetoothSerial(
                serialNumber: expectedSerialNumber
            )
        }
        retainedIFDSPUSBHandoff = nil
        retainedBluetoothMmdvmRoutingEndpoint = nil
        retainedBluetoothReconnectEndpoint = nil
        if let retained = retainedMatches.first {
            selected = .qualifiedBluetooth(
                retained,
                expectedUSBSerial: expectedSerialNumber
            )
        } else {
            selected = .bluetoothMatchingUSBSerial(expectedSerialNumber)
        }
        guard let device = selected?.device else {
            throw RadioEndpointSelectionError.noSelection
        }
        selectedDeviceSlot.replace(with: device)
        updateState(.disconnected)
        return RadioEndpoint(
            id: device.id,
            name: device.name,
            transport: .bluetooth,
            detail: "same radio as USB serial \(expectedSerialNumber)"
        )
    }

    func selectBluetoothForSameRadio(
        expectedSerialNumber: String
    ) async throws {
        _ = try await selectBluetooth(
            matchingSerialNumber: expectedSerialNumber
        )
    }

    func qualifySelectedBluetoothForReconnect(
        expectedSerialNumber: String
    ) async throws {
        guard activeTransport == nil, cleanupSlot == nil else {
            throw AzimuthRadioSelectionError.transportIsOpen
        }
        guard let retained = retainedBluetoothReconnectEndpoint else {
            throw AzimuthRadioSelectionError.bluetoothReconnectContextUnavailable
        }
        defer { retainedBluetoothReconnectEndpoint = nil }
        let selectedEndpoint: AzimuthBluetoothEndpoint
        switch selected {
        case .bluetooth(let endpoint),
             .qualifiedBluetooth(let endpoint, _):
            selectedEndpoint = endpoint
        case .usb, .bluetoothMatchingUSBSerial, nil:
            throw AzimuthRadioSelectionError.bluetoothReconnectContextUnavailable
        }
        guard selectedEndpoint.id == retained.id else {
            throw AzimuthRadioSelectionError.bluetoothReconnectContextUnavailable
        }
        let qualified = retainQualifiedBluetoothEndpoint(
            AzimuthBluetoothEndpoint(
                address: retained.address,
                displayName: retained.displayName,
                verifiedCATSerialNumber: expectedSerialNumber
            )
        )
        selected = .qualifiedBluetooth(
            qualified,
            expectedUSBSerial: expectedSerialNumber
        )
        selectedDeviceSlot.replace(with: qualified.device)
        updateState(.disconnected)
    }

    func knownQualifiedBluetoothAddress(
        expectedSerialNumber: String
    ) async throws -> String? {
        let matches = lastBluetoothEndpoints.filter {
            $0.verifiedCATSerialNumber == expectedSerialNumber
        }
        guard matches.count <= 1 else {
            throw AzimuthRadioSelectionError.ambiguousQualifiedBluetoothSerial(
                serialNumber: expectedSerialNumber
            )
        }
        return matches.first?.address
    }

    func selectUSBForRecovery(
        expectedSerialNumber: String
    ) async throws {
        guard activeTransport == nil, cleanupSlot == nil else {
            throw AzimuthRadioSelectionError.transportIsOpen
        }
        let refreshed = usbFactory.availableEndpoints()
        try Self.validateUSBEndpoints(refreshed)
        let matches = refreshed.filter {
            $0.usbSerialNumber == expectedSerialNumber
        }
        guard matches.count <= 1 else {
            throw RadioEndpointSelectionError.duplicateEndpoint(
                id: "usb:serial:\(expectedSerialNumber)"
            )
        }
        guard let current = matches.first else {
            if let retainedUSBEndpoint,
               let replacement = refreshed.first(where: {
                   $0.devicePath == retainedUSBEndpoint.devicePath
               }) {
                throw AzimuthRadioSelectionError.differentUSBRadioAtRetainedPath(
                    expected: expectedSerialNumber,
                    actual: replacement.usbSerialNumber
                )
            }
            throw AzimuthRadioSelectionError.expectedUSBRadioUnavailable(
                serialNumber: expectedSerialNumber
            )
        }
        retainedIFDSPUSBHandoff = nil
        retainedBluetoothMmdvmRoutingEndpoint = nil
        retainedBluetoothReconnectEndpoint = nil
        retainedUSBEndpoint = current
        selected = .usb(current)
        selectedDeviceSlot.replace(with: current.device)
        updateState(.disconnected)
    }

    func refreshSelectedUSBForSameRadioRecovery() async throws -> Bool {
        guard activeTransport == nil, cleanupSlot == nil else {
            throw AzimuthRadioSelectionError.transportIsOpen
        }
        guard case .usb(let retained) = selected,
              retainedUSBEndpoint == retained else {
            return false
        }
        let refreshed = usbFactory.availableEndpoints()
        try Self.validateUSBEndpoints(refreshed)
        guard let endpoint = Self.refreshedRetainedUSBEndpoint(
            retained: retained,
            available: refreshed
        ) else {
            return false
        }
        retainedUSBEndpoint = endpoint
        selected = .usb(endpoint)
        selectedDeviceSlot.replace(with: endpoint.device)
        updateState(.disconnected)
        return true
    }

    /// Return whether one exact, verified TH-D75 USB endpoint is available for
    /// a consented handoff from a Bluetooth link that answered MMDVM.
    func hasSoleVerifiedUSBEndpoint() async throws -> Bool {
        let refreshed = usbFactory.availableEndpoints()
        try Self.validateUSBEndpoints(refreshed)
        return refreshed.count == 1
    }

    func retainSoleIFDSPUSBEndpoint() async throws -> Bool {
        guard cleanupSlot == nil else {
            throw AzimuthRadioSelectionError.transportIsOpen
        }
        let bluetoothEndpoint: AzimuthBluetoothEndpoint
        switch selected {
        case .bluetooth(let endpoint), .qualifiedBluetooth(let endpoint, _):
            bluetoothEndpoint = endpoint
        case .usb, .bluetoothMatchingUSBSerial, nil:
            retainedIFDSPUSBHandoff = nil
            throw AzimuthRadioSelectionError.ifDSPOriginalBluetoothUnavailable
        }
        let refreshed = usbFactory.availableEndpoints()
        try Self.validateUSBEndpoints(refreshed)
        guard refreshed.count == 1, let endpoint = refreshed.first else {
            retainedIFDSPUSBHandoff = nil
            return false
        }
        retainedIFDSPUSBHandoff = IFDSPUSBHandoffContext(
            bluetoothEndpoint: bluetoothEndpoint,
            usbEndpoint: endpoint
        )
        return true
    }

    func selectRetainedIFDSPUSBEndpoint() async throws -> Bool {
        guard activeTransport == nil, cleanupSlot == nil else {
            throw AzimuthRadioSelectionError.transportIsOpen
        }
        guard let context = retainedIFDSPUSBHandoff else {
            throw AzimuthRadioSelectionError.ifDSPUSBHandoffContextUnavailable
        }
        let refreshed = usbFactory.availableEndpoints()
        try Self.validateUSBEndpoints(refreshed)
        guard let endpoint = Self.refreshedRetainedUSBEndpoint(
            retained: context.usbEndpoint,
            available: refreshed
        ) else {
            return false
        }
        retainedUSBEndpoint = endpoint
        selected = .usb(endpoint)
        selectedDeviceSlot.replace(with: endpoint.device)
        updateState(.disconnected)
        return true
    }

    func restoreRetainedIFDSPBluetoothEndpoint(
        expectedSerialNumber: String
    ) async throws {
        guard activeTransport == nil, cleanupSlot == nil else {
            throw AzimuthRadioSelectionError.transportIsOpen
        }
        guard let context = retainedIFDSPUSBHandoff else {
            throw AzimuthRadioSelectionError.ifDSPUSBHandoffContextUnavailable
        }
        let qualified = retainQualifiedBluetoothEndpoint(
            AzimuthBluetoothEndpoint(
                address: context.bluetoothEndpoint.address,
                displayName: context.bluetoothEndpoint.displayName,
                verifiedCATSerialNumber: expectedSerialNumber
            )
        )
        selected = .qualifiedBluetooth(
            qualified,
            expectedUSBSerial: expectedSerialNumber
        )
        selectedDeviceSlot.replace(with: qualified.device)
        retainedIFDSPUSBHandoff = nil
        updateState(.disconnected)
    }

    func finishRetainedIFDSPUSBHandoff() async {
        retainedIFDSPUSBHandoff = nil
    }

    /// Select the sole exact USB endpoint without opening or changing the
    /// radio. The exact Bluetooth endpoint currently selected is retained for
    /// the separately consented Menu 985 / Menu 650 routing lifecycle. The next
    /// USB operation proves CAT model and obtains the authoritative radio
    /// serial with `AE`; no USB descriptor serial is required.
    func selectSoleUSBForBluetoothMMDVM() async throws {
        guard activeTransport == nil, cleanupSlot == nil else {
            throw AzimuthRadioSelectionError.transportIsOpen
        }
        let originalBluetoothEndpoint: AzimuthBluetoothEndpoint
        switch selected {
        case .bluetooth(let endpoint), .qualifiedBluetooth(let endpoint, _):
            originalBluetoothEndpoint = endpoint
        case .usb, .bluetoothMatchingUSBSerial, nil:
            throw AzimuthRadioSelectionError.bluetoothMmdvmOriginalBluetoothUnavailable
        }
        let refreshed = usbFactory.availableEndpoints()
        try Self.validateUSBEndpoints(refreshed)
        guard refreshed.count == 1, let endpoint = refreshed.first else {
            throw AzimuthRadioSelectionError.bluetoothMmdvmUSBFallbackUnavailable(
                attachedUSBCount: refreshed.count
            )
        }
        retainedIFDSPUSBHandoff = nil
        retainedBluetoothMmdvmRoutingEndpoint = originalBluetoothEndpoint
        retainedUSBEndpoint = endpoint
        selected = .usb(endpoint)
        selectedDeviceSlot.replace(with: endpoint.device)
        updateState(.disconnected)
    }

    /// Select the exact Bluetooth address retained before USB routing and make
    /// the next open prove its CAT serial equals the serial obtained from CAT
    /// on the retained exact USB endpoint before routing.
    ///
    /// No paired-device scan or display-name matching is permitted here. A
    /// different physical radio at the address fails inside the qualified exact
    /// Bluetooth link before its byte stream reaches the controller.
    func selectOriginalBluetoothAfterUSBRouting(
        expectedSerialNumber: String
    ) async throws {
        guard activeTransport == nil, cleanupSlot == nil else {
            throw AzimuthRadioSelectionError.transportIsOpen
        }
        guard let original = retainedBluetoothMmdvmRoutingEndpoint else {
            throw AzimuthRadioSelectionError.bluetoothMmdvmUSBRoutingContextUnavailable
        }
        guard case .usb(let selectedUSB) = selected,
              selectedUSB == retainedUSBEndpoint else {
            throw AzimuthRadioSelectionError.bluetoothMmdvmUSBRoutingContextUnavailable
        }

        retainedIFDSPUSBHandoff = nil
        let qualified = retainQualifiedBluetoothEndpoint(
            AzimuthBluetoothEndpoint(
                address: original.address,
                displayName: original.displayName,
                verifiedCATSerialNumber: expectedSerialNumber
            )
        )
        selected = .qualifiedBluetooth(
            qualified,
            expectedUSBSerial: expectedSerialNumber
        )
        selectedDeviceSlot.replace(with: qualified.device)
        retainedBluetoothMmdvmRoutingEndpoint = nil
        updateState(.disconnected)
    }

    /// Restore the exact raw Bluetooth endpoint after a failure which is known
    /// to precede the USB mutation gate.
    ///
    /// The MMDVM response did not contain a unit serial, so this deliberately
    /// does not attach the USB descriptor serial as if it had been proved.
    func restoreOriginalBluetoothAfterUSBRoutingFailure() async throws {
        guard activeTransport == nil, cleanupSlot == nil else {
            throw AzimuthRadioSelectionError.transportIsOpen
        }
        guard let original = retainedBluetoothMmdvmRoutingEndpoint else {
            throw AzimuthRadioSelectionError.bluetoothMmdvmUSBRoutingContextUnavailable
        }
        retainedIFDSPUSBHandoff = nil
        selected = .bluetooth(original)
        selectedDeviceSlot.replace(with: original.device)
        retainedBluetoothMmdvmRoutingEndpoint = nil
        updateState(.disconnected)
    }

    public func open() async throws {
        guard cleanupSlot == nil else {
            throw AzimuthRadioSelectionError.transportIsOpen
        }
        guard activeTransport == nil else {
            if currentState == .connected { return }
            throw AzimuthRadioSelectionError.transportIsOpen
        }

        generation &+= 1
        let attempt = generation
        guard let selection = selected else {
            throw RadioEndpointSelectionError.noSelection
        }
        var attemptSelection = selection
        let transport: any AzimuthRadioTransport
        switch selection {
        case .usb(let endpoint):
            transport = try usbFactory.makeTransport(endpoint: endpoint)
        case .bluetooth(let endpoint):
            transport = AzimuthBluetoothRadioTransport(
                endpoint: endpoint,
                factory: bluetoothFactory
            )
        case .qualifiedBluetooth(let endpoint, let serialNumber):
            transport = AzimuthBluetoothRadioTransport(
                endpoint: endpoint,
                expectedUSBSerialNumber: serialNumber,
                factory: bluetoothFactory
            )
        case .bluetoothMatchingUSBSerial(let serialNumber):
            transport = AzimuthBluetoothRadioTransport(
                expectedUSBSerialNumber: serialNumber,
                factory: bluetoothFactory
            )
        }

        activeTransport = transport
        activeSelection = selection
        activeSlot.replace(with: transport)
        observeState(of: transport, generation: attempt)
        updateState(.connecting)

        do {
            try await transport.open()
            try ensureCurrent(attempt, selection: selection)
            if case .bluetoothMatchingUSBSerial(let expectedSerialNumber) = selection {
                guard let bluetooth = transport as? AzimuthBluetoothRadioTransport,
                      let resolved = await bluetooth.resolvedEndpoint else {
                    throw AzimuthRadioSelectionError.resolvedBluetoothAddressUnavailable
                }
                try ensureCurrent(attempt, selection: attemptSelection)
                let retainedResolved = retainQualifiedBluetoothEndpoint(resolved)
                let resolvedSelection = AzimuthTransportSelection.qualifiedBluetooth(
                    retainedResolved,
                    expectedUSBSerial: expectedSerialNumber
                )
                selected = resolvedSelection
                activeSelection = resolvedSelection
                attemptSelection = resolvedSelection
                selectedDeviceSlot.replace(with: retainedResolved.device)
            }
            let hardwareSerialNumber = await transport.hardwareSerialNumber
            try ensureCurrent(attempt, selection: attemptSelection)
            let usbDeviceRegistryEntryID = await transport
                .macOSUSBDeviceRegistryEntryID
            try ensureCurrent(attempt, selection: attemptSelection)
            let childState = await transport.state
            try ensureCurrent(attempt, selection: attemptSelection)
            currentHardwareSerialNumber = hardwareSerialNumber
            currentMacOSUSBDeviceRegistryEntryID = usbDeviceRegistryEntryID
                .flatMap { $0 == 0 ? nil : $0 }
            updateState(childState)
        } catch is CancellationError {
            if attempt == generation {
                await releaseActiveTransport(transport, finalState: .disconnected)
            }
            throw CancellationError()
        } catch {
            guard attempt == generation else { throw CancellationError() }
            let reason = Self.describe(error)
            await releaseActiveTransport(
                transport,
                finalState: .failed(message: reason)
            )
            throw error
        }
    }

    public func close() async {
        if let cleanupSlot {
            await cleanupSlot.task.value
            completeCleanup(cleanupSlot)
            updateState(.disconnected)
            return
        }
        generation &+= 1
        stateObserver?.cancel()
        stateObserver = nil
        let transport = activeTransport
        let closingSelection = activeSelection
        activeTransport = nil
        activeSelection = nil
        currentHardwareSerialNumber = nil
        currentMacOSUSBDeviceRegistryEntryID = nil
        activeSlot.replace(with: nil)
        guard let transport else {
            updateState(.disconnected)
            return
        }
        retainBluetoothReconnectEndpoint(from: closingSelection)
        let cleanup = startCleanup(
            transport,
            finalState: .disconnected
        )
        await cleanup.task.value
        completeCleanup(cleanup)
    }

    public nonisolated func setBaudRate(baud: UInt32) throws {
        try activeSlot.setBaudRate(baud)
    }

    public func write(_ bytes: [UInt8]) async throws {
        guard let transport = activeTransport else {
            throw AzimuthRadioTransportError.notConnected
        }
        let attempt = generation
        let selection = activeSelection
        do {
            try await transport.write(bytes)
            try ensureCurrent(attempt, selection: selection)
        } catch is CancellationError {
            throw CancellationError()
        } catch {
            if attempt == generation {
                currentHardwareSerialNumber = nil
                currentMacOSUSBDeviceRegistryEntryID = nil
            }
            throw error
        }
    }

    public func read(maxBytes: Int) async throws -> [UInt8] {
        guard let transport = activeTransport else { return [] }
        let attempt = generation
        let selection = activeSelection
        do {
            let bytes = try await transport.read(maxBytes: maxBytes)
            try ensureCurrent(attempt, selection: selection)
            return bytes
        } catch is CancellationError {
            throw CancellationError()
        } catch {
            if attempt == generation {
                currentHardwareSerialNumber = nil
                currentMacOSUSBDeviceRegistryEntryID = nil
            }
            throw error
        }
    }

    private func observeState(
        of transport: any AzimuthRadioTransport,
        generation attempt: UInt64
    ) {
        stateObserver?.cancel()
        let stream = transport.stateStream
        stateObserver = Task { [weak self] in
            for await next in stream {
                guard !Task.isCancelled else { return }
                await self?.forwardState(next, generation: attempt)
            }
        }
    }

    private func forwardState(
        _ next: AzimuthRadioTransportState,
        generation attempt: UInt64
    ) async {
        guard attempt == generation, activeTransport != nil else { return }
        switch next {
        case .connected:
            guard let transport = activeTransport else { return }
            let serialNumber = await transport.hardwareSerialNumber
            let usbDeviceRegistryEntryID = await transport
                .macOSUSBDeviceRegistryEntryID
            guard attempt == generation, activeTransport != nil else { return }
            currentHardwareSerialNumber = serialNumber
            currentMacOSUSBDeviceRegistryEntryID = usbDeviceRegistryEntryID
                .flatMap { $0 == 0 ? nil : $0 }
        case .disconnected, .failed:
            currentHardwareSerialNumber = nil
            currentMacOSUSBDeviceRegistryEntryID = nil
        case .connecting:
            break
        }
        updateState(next)
    }

    private func ensureCurrent(
        _ attempt: UInt64,
        selection: AzimuthTransportSelection?
    ) throws {
        guard attempt == generation, selection == activeSelection else {
            throw CancellationError()
        }
        try Task.checkCancellation()
    }

    private func releaseActiveTransport(
        _ transport: any AzimuthRadioTransport,
        finalState: AzimuthRadioTransportState
    ) async {
        stateObserver?.cancel()
        stateObserver = nil
        activeTransport = nil
        activeSelection = nil
        currentHardwareSerialNumber = nil
        currentMacOSUSBDeviceRegistryEntryID = nil
        activeSlot.replace(with: nil)
        let cleanup = startCleanup(transport, finalState: finalState)
        await cleanup.task.value
        completeCleanup(cleanup)
    }

    private func startCleanup(
        _ transport: any AzimuthRadioTransport,
        finalState: AzimuthRadioTransportState
    ) -> CleanupSlot {
        let id = nextCleanupID
        nextCleanupID &+= 1
        let cleanup = CleanupSlot(
            id: id,
            task: Task { await transport.close() },
            finalState: finalState
        )
        cleanupSlot = cleanup
        return cleanup
    }

    private func completeCleanup(_ completed: CleanupSlot) {
        guard cleanupSlot?.id == completed.id else { return }
        cleanupSlot = nil
        updateState(completed.finalState)
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

    private static func radioEndpoint(for endpoint: AzimuthUSBEndpoint) -> RadioEndpoint {
        RadioEndpoint(
            id: endpoint.id,
            name: endpoint.displayName,
            transport: .usb,
            detail: endpoint.devicePath
        )
    }

    private static func radioEndpoint(
        for endpoint: AzimuthBluetoothEndpoint
    ) -> RadioEndpoint {
        RadioEndpoint(
            id: endpoint.id,
            name: endpoint.displayName,
            transport: .bluetooth,
            detail: endpoint.address
        )
    }

    private func retainQualifiedBluetoothEndpoint(
        _ endpoint: AzimuthBluetoothEndpoint
    ) -> AzimuthBluetoothEndpoint {
        guard let index = lastBluetoothEndpoints.firstIndex(
            where: { $0.id == endpoint.id }
        ) else {
            lastBluetoothEndpoints.append(endpoint)
            return endpoint
        }
        let existing = lastBluetoothEndpoints[index]
        let retained = AzimuthBluetoothEndpoint(
            address: endpoint.address,
            displayName: existing.displayName,
            verifiedCATSerialNumber: endpoint.verifiedCATSerialNumber
                ?? existing.verifiedCATSerialNumber
        )
        lastBluetoothEndpoints[index] = retained
        return retained
    }

    private func retainBluetoothReconnectEndpoint(
        from selection: AzimuthTransportSelection?
    ) {
        switch selection {
        case .bluetooth(let endpoint), .qualifiedBluetooth(let endpoint, _):
            retainedBluetoothReconnectEndpoint = endpoint
        case .usb, .bluetoothMatchingUSBSerial, nil:
            retainedBluetoothReconnectEndpoint = nil
        }
    }

    /// Follow the endpoint retained before consent without treating any USB
    /// descriptor field as radio identity. Registry identity is strongest while
    /// the original enumeration remains alive; serial and path are continuity
    /// hints across re-enumeration. If every hint changed, only a sole qualified
    /// endpoint is safe to probe, and CAT `AE` remains the final authority.
    private static func refreshedRetainedUSBEndpoint(
        retained: AzimuthUSBEndpoint,
        available: [AzimuthUSBEndpoint]
    ) -> AzimuthUSBEndpoint? {
        let registryMatches = available.filter {
            $0.usbDeviceRegistryEntryID == retained.usbDeviceRegistryEntryID
        }
        if registryMatches.count == 1 {
            return registryMatches[0]
        }
        if registryMatches.count > 1 {
            return nil
        }
        if let serialNumber = retained.usbSerialNumber {
            let serialMatches = available.filter {
                $0.usbSerialNumber == serialNumber
            }
            if serialMatches.count == 1 {
                return serialMatches[0]
            }
        }
        let pathMatches = available.filter {
            $0.devicePath == retained.devicePath
        }
        if pathMatches.count == 1 {
            return pathMatches[0]
        }
        guard available.count == 1 else { return nil }
        return available[0]
    }

    private static func mergeBluetoothEndpoints(
        _ current: [AzimuthBluetoothEndpoint],
        retainingQualifiedFrom previous: [AzimuthBluetoothEndpoint]
    ) -> [AzimuthBluetoothEndpoint] {
        var qualifiedByID: [
            String: (endpoint: AzimuthBluetoothEndpoint, serialNumber: String)
        ] = [:]
        for endpoint in previous {
            guard let serialNumber = endpoint.verifiedCATSerialNumber else {
                continue
            }
            qualifiedByID[endpoint.id] = (endpoint, serialNumber)
        }
        return current.map { endpoint in
            guard let qualified = qualifiedByID[endpoint.id] else {
                return endpoint
            }
            return AzimuthBluetoothEndpoint(
                address: endpoint.address,
                displayName: endpoint.displayName,
                verifiedCATSerialNumber: qualified.serialNumber
            )
        }
    }

    private func pairedBluetoothDiscovery() async throws -> AzimuthBluetoothDiscoverySnapshot {
        let discovery = try await bluetoothFactory.pairedDeviceDiscovery()
        try Self.validateBluetoothDiscovery(discovery)
        return discovery
    }

    private func ensureCurrentEndpointDiscovery(_ expected: UInt64) throws {
        guard endpointDiscoveryGeneration == expected else {
            throw CancellationError()
        }
        try Task.checkCancellation()
    }

    private static func validateBluetoothDiscovery(
        _ discovery: AzimuthBluetoothDiscoverySnapshot
    ) throws {
        let endpoints = discovery.pairedEndpoints
        try Self.validateBluetoothEndpoints(endpoints)
    }

    private static func validateBluetoothEndpoints(
        _ endpoints: [AzimuthBluetoothEndpoint]
    ) throws {
        var identifiers: Set<String> = []
        for endpoint in endpoints {
            guard isCanonicalBluetoothAddress(endpoint.address),
                  !endpoint.displayName.isEmpty,
                  endpoint.verifiedCATSerialNumber?.isEmpty != true else {
                throw RadioEndpointSelectionError.malformedEndpoint
            }
            guard identifiers.insert(endpoint.id).inserted else {
                throw RadioEndpointSelectionError.duplicateEndpoint(
                    id: endpoint.id
                )
            }
        }
    }

    private static func isCanonicalBluetoothAddress(_ address: String) -> Bool {
        let bytes = Array(address.utf8)
        guard bytes.count == 17 else { return false }
        for (index, byte) in bytes.enumerated() {
            if index % 3 == 2 {
                guard byte == 0x2D else { return false }
            } else {
                let decimal: ClosedRange<UInt8> = 0x30 ... 0x39
                let uppercaseHex: ClosedRange<UInt8> = 0x41 ... 0x46
                guard decimal.contains(byte) || uppercaseHex.contains(byte) else {
                    return false
                }
            }
        }
        return true
    }

    private static func validateUSBEndpoints(
        _ endpoints: [AzimuthUSBEndpoint]
    ) throws {
        var identifiers: Set<String> = []
        for endpoint in endpoints {
            guard !endpoint.id.isEmpty,
                  !endpoint.displayName.isEmpty,
                  !endpoint.devicePath.isEmpty,
                  endpoint.usbDeviceRegistryEntryID != 0,
                  endpoint.usbSerialNumber?.isEmpty != true,
                  endpoint.id == AzimuthUSBEndpoint.stableID(
                      devicePath: endpoint.devicePath,
                      usbSerialNumber: endpoint.usbSerialNumber
                  ) else {
                throw RadioEndpointSelectionError.malformedEndpoint
            }
            guard identifiers.insert(endpoint.id).inserted else {
                throw RadioEndpointSelectionError.duplicateEndpoint(id: endpoint.id)
            }
        }
    }

    private static func validateCombinedEndpointIDs(
        usb: [AzimuthUSBEndpoint],
        bluetooth: [AzimuthBluetoothEndpoint]
    ) throws {
        var identifiers = Set(usb.map(\.id))
        for endpoint in bluetooth where !identifiers.insert(endpoint.id).inserted {
            throw RadioEndpointSelectionError.duplicateEndpoint(id: endpoint.id)
        }
    }
}

/// Main-actor endpoint-selection facade consumed by `AzimuthSceneModel`.
@MainActor
final class AzimuthSelectableRadioEndpointSelector: RadioEndpointSelecting {
    let initialEndpoints: [RadioEndpoint]
    private let router: AzimuthSelectableRadioTransport

    init(router: AzimuthSelectableRadioTransport) {
        self.router = router
        initialEndpoints = router.initialEndpoints
    }

    func refreshEndpoints() async throws -> RadioEndpointDiscoverySnapshot {
        try await router.availableEndpointSnapshot()
    }

    func selectEndpoint(id: String) async throws {
        try await router.selectEndpoint(id: id)
    }

    func selectedEndpoint() async -> RadioEndpoint? {
        await router.selectedRadioEndpoint()
    }
}
