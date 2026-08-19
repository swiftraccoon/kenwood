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
    case bluetoothSearchInProgress
    case ambiguousQualifiedBluetoothSerial(serialNumber: String)
    case expectedUSBRadioUnavailable(serialNumber: String)
    case differentUSBRadioAtRetainedPath(expected: String, actual: String?)
    case resolvedBluetoothAddressUnavailable

    public var errorDescription: String? {
        switch self {
        case .transportIsOpen:
            "Disconnect the current radio before changing connection methods."
        case .bluetoothSearchInProgress:
            "Wait for the custom-named Bluetooth radio search to finish or stop it before connecting."
        case .ambiguousQualifiedBluetoothSerial(let serialNumber):
            "More than one retained Bluetooth address claims CAT serial \(serialNumber). Refresh paired devices before continuing."
        case .expectedUSBRadioUnavailable(let serialNumber):
            "USB radio \(serialNumber) has not re-enumerated yet."
        case .differentUSBRadioAtRetainedPath(let expected, let actual):
            "A different USB radio appeared at the recovery path. Expected \(expected), found \(actual ?? "no stable serial number")."
        case .resolvedBluetoothAddressUnavailable:
            "The same-radio Bluetooth link opened without reporting its exact paired address. Azimuth closed it rather than retaining an ambiguous connection."
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

/// Routes one Azimuth controller to either USB or one exact Bluetooth radio.
///
/// Only one child transport is active at a time. Endpoint changes are accepted
/// only while disconnected, stale child state is generation-fenced, and the
/// same-radio fallback delegates exact serial qualification to the core.
public actor AzimuthSelectableRadioTransport: AzimuthRadioTransport,
    AzimuthSameRadioBluetoothSelecting
{
    private struct CleanupSlot: Sendable {
        let id: UInt64
        let task: Task<Void, Never>
        let finalState: AzimuthRadioTransportState
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
    private var lastBluetoothEndpoints: [AzimuthBluetoothEndpoint] = []
    private var lastTotalPairedBluetoothDeviceCount: UInt32?
    private var lastLikelyBluetoothRadioCount: UInt32?
    private var customSearchProbedAddresses: Set<String> = []
    private var lastCustomSearchTotalUnhintedCandidateCount: UInt32?
    private var bluetoothSearchInProgress = false
    private var endpointDiscoveryGeneration: UInt64 = 0
    private var activeTransport: (any AzimuthRadioTransport)?
    private var activeSelection: AzimuthTransportSelection?
    private var stateObserver: Task<Void, Never>?
    private var currentState: AzimuthRadioTransportState = .disconnected
    private var currentHardwareSerialNumber: String?
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

    /// Returns USB followed by likely-radio entries from one paired-device
    /// inventory. The inventory's total paired count remains independent so a
    /// custom-named TH-D75 can still authorize a serial-qualified handoff try.
    func availableEndpointSnapshot() async throws -> RadioEndpointDiscoverySnapshot {
        guard !bluetoothSearchInProgress else {
            throw AzimuthRadioSelectionError.bluetoothSearchInProgress
        }
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
            lastTotalPairedBluetoothDeviceCount = nil
            lastLikelyBluetoothRadioCount = nil
            return RadioEndpointDiscoverySnapshot(
                endpoints: usb.map(Self.radioEndpoint(for:)) + retainedQualified.map {
                    Self.radioEndpoint(for: $0)
                },
                warning: "Bluetooth connections unavailable: \(Self.describe(error))",
                pairedBluetoothDeviceCount: nil,
                likelyBluetoothRadioCount: nil
            )
        }
        let usb = usbFactory.availableEndpoints()
        try Self.validateUSBEndpoints(usb)
        try ensureCurrentEndpointDiscovery(discoveryGeneration)
        let likelyBluetooth = bluetoothDiscovery.likelyRadioEndpoints
        let bluetooth = Self.mergeLikelyBluetoothEndpoints(
            likelyBluetooth,
            retainingQualifiedFrom: lastBluetoothEndpoints,
            pairedDeviceAddresses: bluetoothDiscovery.pairedDeviceAddresses
        )
        try Self.validateCombinedEndpointIDs(usb: usb, bluetooth: bluetooth)
        lastBluetoothEndpoints = bluetooth
        lastTotalPairedBluetoothDeviceCount =
            bluetoothDiscovery.totalPairedDeviceCount
        lastLikelyBluetoothRadioCount = UInt32(likelyBluetooth.count)
        customSearchProbedAddresses = []
        lastCustomSearchTotalUnhintedCandidateCount = nil
        return RadioEndpointDiscoverySnapshot(
            endpoints: usb.map(Self.radioEndpoint(for:)) + bluetooth.map {
                Self.radioEndpoint(for: $0)
            },
            pairedBluetoothDeviceCount: bluetoothDiscovery.totalPairedDeviceCount,
            likelyBluetoothRadioCount: UInt32(likelyBluetooth.count)
        )
    }

    /// Explicitly CAT-probe paired devices omitted from the ordinary picker.
    /// Only exact endpoints proved as TH-D75 radios are merged into the
    /// retained snapshot. A partial bounded result keeps its proven rows while
    /// reporting that the search was incomplete.
    func findCustomNamedBluetoothRadios() async throws
        -> RadioEndpointBluetoothSearchResult {
        guard activeTransport == nil, cleanupSlot == nil else {
            throw AzimuthRadioSelectionError.transportIsOpen
        }
        guard !bluetoothSearchInProgress else {
            throw AzimuthRadioSelectionError.bluetoothSearchInProgress
        }
        // An explicit radio search supersedes a still-running optional picker
        // refresh. The generated discovery call is not Swift-cancellable, so
        // invalidate its eventual result before starting this newer inventory.
        endpointDiscoveryGeneration &+= 1
        bluetoothSearchInProgress = true
        defer { bluetoothSearchInProgress = false }

        let previousProbedAddresses = customSearchProbedAddresses
        let search = try await bluetoothFactory.findCustomNamedRadios(
            previouslyProbedAddresses: previousProbedAddresses.sorted()
        )
        let usb = usbFactory.availableEndpoints()
        try Self.validateUSBEndpoints(usb)
        try Self.validateBluetoothRadioSearch(
            search,
            previouslyProbedAddresses: previousProbedAddresses
        )
        if search.hasInventorySnapshot {
            let discovery = AzimuthBluetoothDiscoverySnapshot(
                likelyRadioEndpoints: search.likelyRadioEndpoints,
                totalPairedDeviceCount: search.totalPairedDeviceCount,
                pairedDeviceAddresses: search.pairedDeviceAddresses
            )
            try Self.validateBluetoothDiscovery(discovery)
            customSearchProbedAddresses = Set(search.currentProbedAddresses)
            lastCustomSearchTotalUnhintedCandidateCount =
                search.totalUnhintedCandidateCount
            lastTotalPairedBluetoothDeviceCount =
                search.totalPairedDeviceCount
            lastLikelyBluetoothRadioCount =
                UInt32(search.likelyRadioEndpoints.count)
        }
        let effectiveTotal = lastCustomSearchTotalUnhintedCandidateCount
            ?? search.totalUnhintedCandidateCount

        var merged = if search.hasInventorySnapshot {
            Self.mergeLikelyBluetoothEndpoints(
                search.likelyRadioEndpoints,
                retainingQualifiedFrom: lastBluetoothEndpoints,
                pairedDeviceAddresses: search.pairedDeviceAddresses
            )
        } else {
            lastBluetoothEndpoints
        }
        for proven in search.provenRadioEndpoints {
            if let index = merged.firstIndex(where: { $0.id == proven.id }) {
                // Prefer the newly serial-qualified record over a presentation-
                // only discovery hint for the same exact address.
                merged[index] = proven
            } else {
                merged.append(proven)
            }
        }
        try Self.validateCombinedEndpointIDs(usb: usb, bluetooth: merged)
        lastBluetoothEndpoints = merged

        let warning: String?
        if search.isComplete {
            warning = nil
        } else {
            warning = "Bluetooth radio search checked \(customSearchProbedAddresses.count) of \(effectiveTotal) unhinted paired devices before its safety bound. Any proved TH-D75 connections were retained."
        }
        let snapshot = RadioEndpointDiscoverySnapshot(
            endpoints: usb.map(Self.radioEndpoint(for:)) + merged.map {
                Self.radioEndpoint(for: $0)
            },
            warning: warning,
            pairedBluetoothDeviceCount: lastTotalPairedBluetoothDeviceCount,
            likelyBluetoothRadioCount: lastLikelyBluetoothRadioCount
        )
        return RadioEndpointBluetoothSearchResult(
            snapshot: snapshot,
            probedCandidateCount: UInt32(customSearchProbedAddresses.count),
            totalUnhintedCandidateCount: effectiveTotal,
            isComplete: search.isComplete,
            wasCancelled: search.wasCancelled
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
        guard !bluetoothSearchInProgress else {
            throw AzimuthRadioSelectionError.bluetoothSearchInProgress
        }
        guard activeTransport == nil, cleanupSlot == nil else {
            throw AzimuthRadioSelectionError.transportIsOpen
        }
        let usb = usbFactory.availableEndpoints()
        try Self.validateUSBEndpoints(usb)
        if let endpoint = usb.first(where: { $0.id == id }) {
            selected = .usb(endpoint)
            retainedUSBEndpoint = endpoint
            selectedDeviceSlot.replace(with: endpoint.device)
            updateState(.disconnected)
            return
        }
        guard let candidate = lastBluetoothEndpoints.first(where: { $0.id == id }) else {
            throw RadioEndpointSelectionError.invalidEndpoint(id: id)
        }
        if let serialNumber = candidate.verifiedCATSerialNumber {
            selected = .qualifiedBluetooth(
                candidate,
                expectedUSBSerial: serialNumber
            )
        } else {
            selected = .bluetooth(candidate)
        }
        selectedDeviceSlot.replace(with: candidate.device)
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
        retainedUSBEndpoint = current
        selected = .usb(current)
        selectedDeviceSlot.replace(with: current.device)
        updateState(.disconnected)
    }

    public func open() async throws {
        guard !bluetoothSearchInProgress else {
            throw AzimuthRadioSelectionError.bluetoothSearchInProgress
        }
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
        case .bluetooth(let candidate):
            transport = AzimuthBluetoothRadioTransport(
                endpoint: candidate,
                factory: bluetoothFactory
            )
        case .qualifiedBluetooth(let candidate, let serialNumber):
            transport = AzimuthBluetoothRadioTransport(
                endpoint: candidate,
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
            let childState = await transport.state
            try ensureCurrent(attempt, selection: attemptSelection)
            currentHardwareSerialNumber = hardwareSerialNumber
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
        activeTransport = nil
        activeSelection = nil
        currentHardwareSerialNumber = nil
        activeSlot.replace(with: nil)
        guard let transport else {
            updateState(.disconnected)
            return
        }
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
            if attempt == generation { currentHardwareSerialNumber = nil }
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
            if attempt == generation { currentHardwareSerialNumber = nil }
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
            guard attempt == generation, activeTransport != nil else { return }
            currentHardwareSerialNumber = serialNumber
        case .disconnected, .failed:
            currentHardwareSerialNumber = nil
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

    private static func mergeLikelyBluetoothEndpoints(
        _ likely: [AzimuthBluetoothEndpoint],
        retainingQualifiedFrom previous: [AzimuthBluetoothEndpoint],
        pairedDeviceAddresses: [String]
    ) -> [AzimuthBluetoothEndpoint] {
        let pairedIDs = Set(
            pairedDeviceAddresses.map(AzimuthBluetoothEndpoint.stableID(for:))
        )
        var qualifiedByID: [
            String: (endpoint: AzimuthBluetoothEndpoint, serialNumber: String)
        ] = [:]
        for endpoint in previous {
            guard pairedIDs.contains(endpoint.id),
                  let serialNumber = endpoint.verifiedCATSerialNumber else {
                continue
            }
            qualifiedByID[endpoint.id] = (endpoint, serialNumber)
        }
        var merged = likely.map { endpoint in
            guard let qualified = qualifiedByID[endpoint.id] else {
                return endpoint
            }
            return AzimuthBluetoothEndpoint(
                address: endpoint.address,
                displayName: endpoint.displayName,
                verifiedCATSerialNumber: qualified.serialNumber
            )
        }
        let existingIDs = Set(merged.map(\.id))
        merged.append(contentsOf: qualifiedByID.values.compactMap { qualified in
            existingIDs.contains(qualified.endpoint.id) ? nil : qualified.endpoint
        })
        return merged
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
        let endpoints = discovery.likelyRadioEndpoints
        try Self.validateBluetoothEndpoints(endpoints)
        let pairedIDs = Set(
            discovery.pairedDeviceAddresses.map(AzimuthBluetoothEndpoint.stableID(for:))
        )
        guard let pairedAddressCount = UInt32(
            exactly: discovery.pairedDeviceAddresses.count
        ),
              pairedAddressCount == discovery.totalPairedDeviceCount,
              pairedIDs.count == discovery.pairedDeviceAddresses.count,
              discovery.pairedDeviceAddresses.allSatisfy({ !$0.isEmpty }),
              endpoints.allSatisfy({ pairedIDs.contains($0.id) }) else {
            throw RadioEndpointSelectionError.malformedEndpoint
        }
        var identifiers: Set<String> = []
        for endpoint in endpoints where !identifiers.insert(endpoint.id).inserted {
            throw RadioEndpointSelectionError.duplicateEndpoint(id: endpoint.id)
        }
        guard UInt32(endpoints.count) <= discovery.totalPairedDeviceCount else {
            throw RadioEndpointSelectionError.malformedEndpoint
        }
    }

    private static func validateBluetoothEndpoints(
        _ endpoints: [AzimuthBluetoothEndpoint]
    ) throws {
        var identifiers: Set<String> = []
        for endpoint in endpoints {
            guard !endpoint.address.isEmpty,
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

    private static func validateBluetoothRadioSearch(
        _ search: AzimuthBluetoothRadioSearchSnapshot,
        previouslyProbedAddresses: Set<String>
    ) throws {
        try validateBluetoothEndpoints(search.provenRadioEndpoints)
        let probedAddresses = Set(search.probedAddresses)
        let currentProbedAddresses = Set(search.currentProbedAddresses)
        let pairedIDs = Set(
            search.pairedDeviceAddresses.map(AzimuthBluetoothEndpoint.stableID(for:))
        )
        let likelyIDs = Set(search.likelyRadioEndpoints.map(\.id))
        let probedIDs = Set(
            search.probedAddresses.map(AzimuthBluetoothEndpoint.stableID(for:))
        )
        let currentProbedIDs = Set(
            search.currentProbedAddresses.map(AzimuthBluetoothEndpoint.stableID(for:))
        )
        guard let provenCount = UInt32(exactly: search.provenRadioEndpoints.count),
              let returnedProbedCount = UInt32(exactly: probedAddresses.count),
              let likelyCount = UInt32(exactly: search.likelyRadioEndpoints.count),
              returnedProbedCount == search.probedCandidateCount,
              probedAddresses.count == search.probedAddresses.count,
              currentProbedAddresses.count == search.currentProbedAddresses.count,
              probedAddresses.allSatisfy({ !$0.isEmpty }),
              currentProbedAddresses.allSatisfy({ !$0.isEmpty }),
              probedAddresses.isDisjoint(with: previouslyProbedAddresses),
              search.provenRadioEndpoints.allSatisfy({
                  $0.verifiedCATSerialNumber != nil
                      && probedAddresses.contains($0.address)
              }),
              search.probedCandidateCount <= search.totalUnhintedCandidateCount,
              provenCount <= search.probedCandidateCount,
              search.hasInventorySnapshot
                || (probedAddresses.isEmpty
                    && currentProbedAddresses == previouslyProbedAddresses
                    && search.provenRadioEndpoints.isEmpty
                    && search.likelyRadioEndpoints.isEmpty
                    && search.pairedDeviceAddresses.isEmpty
                    && search.totalPairedDeviceCount == 0
                    && !search.isComplete
                    && search.wasCancelled),
              !search.hasInventorySnapshot
                || currentProbedAddresses.isSuperset(of: probedAddresses),
              !search.hasInventorySnapshot
                || currentProbedAddresses.isSubset(
                    of: previouslyProbedAddresses.union(probedAddresses)
                ),
              !search.hasInventorySnapshot
                || currentProbedAddresses.count <= search.totalUnhintedCandidateCount,
              !search.hasInventorySnapshot
                || probedIDs.isSubset(of: pairedIDs),
              !search.hasInventorySnapshot
                || currentProbedIDs.isSubset(of: pairedIDs),
              !search.hasInventorySnapshot
                || probedIDs.isDisjoint(with: likelyIDs),
              !search.hasInventorySnapshot
                || currentProbedIDs.isDisjoint(with: likelyIDs),
              !search.hasInventorySnapshot
                || search.provenRadioEndpoints.allSatisfy({
                    pairedIDs.contains($0.id)
                        && !likelyIDs.contains($0.id)
                }),
              !search.hasInventorySnapshot
                || likelyCount <= search.totalPairedDeviceCount,
              !search.hasInventorySnapshot
                || search.totalUnhintedCandidateCount
                    == search.totalPairedDeviceCount
                        - likelyCount,
              !search.isComplete
                || (search.hasInventorySnapshot
                    && currentProbedAddresses.count
                        == search.totalUnhintedCandidateCount)
        else {
            throw RadioEndpointSelectionError.malformedEndpoint
        }
        var identifiers: Set<String> = []
        for endpoint in search.provenRadioEndpoints
        where !identifiers.insert(endpoint.id).inserted {
            throw RadioEndpointSelectionError.duplicateEndpoint(id: endpoint.id)
        }
    }

    private static func validateUSBEndpoints(
        _ endpoints: [AzimuthUSBEndpoint]
    ) throws {
        var identifiers: Set<String> = []
        for endpoint in endpoints {
            guard !endpoint.id.isEmpty,
                  !endpoint.displayName.isEmpty,
                  !endpoint.devicePath.isEmpty,
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
    let supportsCustomNamedBluetoothSearch = true
    private let router: AzimuthSelectableRadioTransport

    init(router: AzimuthSelectableRadioTransport) {
        self.router = router
        initialEndpoints = router.initialEndpoints
    }

    func refreshEndpoints() async throws -> RadioEndpointDiscoverySnapshot {
        try await router.availableEndpointSnapshot()
    }

    func findCustomNamedBluetoothRadios() async throws
        -> RadioEndpointBluetoothSearchResult {
        try await router.findCustomNamedBluetoothRadios()
    }

    func selectEndpoint(id: String) async throws {
        try await router.selectEndpoint(id: id)
    }

    func selectedEndpoint() async -> RadioEndpoint? {
        await router.selectedRadioEndpoint()
    }
}
