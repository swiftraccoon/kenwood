// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import XCTest
@testable import Azimuth

private final class AzimuthRouterMockTransport: AzimuthRadioTransport,
    @unchecked Sendable
{
    let device: AzimuthRadioDevice
    let stateStream: AsyncStream<AzimuthRadioTransportState>

    private let lock = NSLock()
    private let continuation: AsyncStream<AzimuthRadioTransportState>.Continuation
    private var currentState: AzimuthRadioTransportState = .disconnected
    private var serialNumber: String?
    private var opens = 0
    private var closes = 0
    private var recordedWrites: [[UInt8]] = []
    private var recordedBaudRates: [UInt32] = []
    private var reads: [[UInt8]] = []

    init(device: AzimuthRadioDevice, serialNumber: String? = nil) {
        self.device = device
        self.serialNumber = serialNumber
        var continuation: AsyncStream<AzimuthRadioTransportState>.Continuation!
        stateStream = AsyncStream { continuation = $0 }
        self.continuation = continuation
    }

    var state: AzimuthRadioTransportState {
        get async { lock.withLock { currentState } }
    }

    var hardwareSerialNumber: String? {
        get async { lock.withLock { serialNumber } }
    }

    var openCount: Int { lock.withLock { opens } }
    var closeCount: Int { lock.withLock { closes } }
    var writes: [[UInt8]] { lock.withLock { recordedWrites } }
    var baudRates: [UInt32] { lock.withLock { recordedBaudRates } }

    func enqueueRead(_ bytes: [UInt8]) {
        lock.withLock { reads.append(bytes) }
    }

    func emit(_ next: AzimuthRadioTransportState) {
        lock.withLock { currentState = next }
        continuation.yield(next)
    }

    func open() async throws {
        lock.withLock { opens += 1 }
        emit(.connected)
    }

    func close() async {
        lock.withLock { closes += 1 }
        emit(.disconnected)
    }

    func setBaudRate(baud: UInt32) throws {
        lock.withLock { recordedBaudRates.append(baud) }
    }

    func write(_ bytes: [UInt8]) async throws {
        lock.withLock { recordedWrites.append(bytes) }
    }

    func read(maxBytes: Int) async throws -> [UInt8] {
        lock.withLock {
            guard !reads.isEmpty else { return [] }
            let bytes = reads.removeFirst()
            return Array(bytes.prefix(maxBytes))
        }
    }
}

private final class AzimuthRouterMockBluetoothLink: AzimuthBluetoothByteLink,
    @unchecked Sendable
{
    let identityReadStarted: AsyncStream<Void>
    private let lock = NSLock()
    private let identityReadStartedContinuation: AsyncStream<Void>.Continuation
    private let serialNumber: String?
    private let resolvedAddress: String?
    private var opens = 0
    private var closes = 0
    private var recordedWrites: [[UInt8]] = []
    private var reads: [[UInt8]] = []
    private var blockIdentityRead: Bool
    private var identityContinuation: CheckedContinuation<String?, Never>?

    init(
        serialNumber: String?,
        matchedAddress: String? = nil,
        blockIdentityRead: Bool = false
    ) {
        self.serialNumber = serialNumber
        resolvedAddress = matchedAddress
        self.blockIdentityRead = blockIdentityRead
        var continuation: AsyncStream<Void>.Continuation!
        identityReadStarted = AsyncStream { continuation = $0 }
        identityReadStartedContinuation = continuation
    }

    var hardwareSerialNumber: String? {
        get async {
            let shouldBlock = lock.withLock { blockIdentityRead }
            guard shouldBlock else { return serialNumber }
            return await withCheckedContinuation { continuation in
                lock.withLock { identityContinuation = continuation }
                identityReadStartedContinuation.yield(())
            }
        }
    }

    var matchedAddress: String? {
        get async { resolvedAddress }
    }

    var openCount: Int { lock.withLock { opens } }
    var closeCount: Int { lock.withLock { closes } }
    var writes: [[UInt8]] { lock.withLock { recordedWrites } }

    func releaseIdentityRead() {
        let continuation = lock.withLock {
            blockIdentityRead = false
            let continuation = identityContinuation
            identityContinuation = nil
            return continuation
        }
        continuation?.resume(returning: serialNumber)
    }

    func enqueueRead(_ bytes: [UInt8]) {
        lock.withLock { reads.append(bytes) }
    }

    func open() async throws {
        lock.withLock { opens += 1 }
    }

    func close() async {
        lock.withLock { closes += 1 }
    }

    func write(_ bytes: [UInt8]) async throws {
        lock.withLock { recordedWrites.append(bytes) }
    }

    func read(maxBytes: Int) async throws -> [UInt8] {
        lock.withLock {
            guard !reads.isEmpty else { return [] }
            return Array(reads.removeFirst().prefix(maxBytes))
        }
    }
}

private final class AzimuthRouterMockBluetoothFactory: AzimuthBluetoothLinkFactory,
    @unchecked Sendable
{
    let discoveryStarted: AsyncStream<Void>
    let searchStarted: AsyncStream<Void>
    private let lock = NSLock()
    private let discoveryStartedContinuation: AsyncStream<Void>.Continuation
    private let searchStartedContinuation: AsyncStream<Void>.Continuation
    private var candidates: [AzimuthBluetoothEndpoint]
    private var totalPairedDeviceCount: UInt32
    private var pairedDeviceAddresses: [String]
    private var discoveryError: Error?
    private var exactLinks: [String: AzimuthRouterMockBluetoothLink]
    private let matchingLink: AzimuthRouterMockBluetoothLink
    private var requestedAddresses: [String] = []
    private var requestedQualifiedTargets: [String] = []
    private var requestedSerialNumbers: [String] = []
    private var discoveries = 0
    private var searchResults: [AzimuthBluetoothRadioSearchSnapshot] = []
    private var searchExclusions: [[String]] = []
    private var shouldBlockDiscovery = false
    private var discoveryContinuation: CheckedContinuation<Void, Never>?
    private var shouldBlockSearch = false
    private var searchContinuation: CheckedContinuation<Void, Never>?

    init(
        candidates: [AzimuthBluetoothEndpoint],
        exactLinks: [String: AzimuthRouterMockBluetoothLink],
        matchingLink: AzimuthRouterMockBluetoothLink
    ) {
        self.candidates = candidates
        totalPairedDeviceCount = UInt32(candidates.count)
        pairedDeviceAddresses = candidates.map(\.address)
        self.exactLinks = exactLinks
        self.matchingLink = matchingLink
        var continuation: AsyncStream<Void>.Continuation!
        discoveryStarted = AsyncStream { continuation = $0 }
        discoveryStartedContinuation = continuation
        var searchStreamContinuation: AsyncStream<Void>.Continuation!
        searchStarted = AsyncStream { searchStreamContinuation = $0 }
        searchStartedContinuation = searchStreamContinuation
    }

    var exactRequests: [String] { lock.withLock { requestedAddresses } }
    var qualifiedRequests: [String] { lock.withLock { requestedQualifiedTargets } }
    var serialRequests: [String] { lock.withLock { requestedSerialNumbers } }
    var discoveryCount: Int { lock.withLock { discoveries } }
    var recordedSearchExclusions: [[String]] { lock.withLock { searchExclusions } }

    func replaceCandidates(_ endpoints: [AzimuthBluetoothEndpoint]) {
        lock.withLock {
            candidates = endpoints
            totalPairedDeviceCount = UInt32(endpoints.count)
            pairedDeviceAddresses = endpoints.map(\.address)
        }
    }

    func replaceDiscovery(
        likelyCandidates: [AzimuthBluetoothEndpoint],
        totalPairedDeviceCount: UInt32,
        pairedDeviceAddresses: [String]? = nil
    ) {
        lock.withLock {
            candidates = likelyCandidates
            self.totalPairedDeviceCount = totalPairedDeviceCount
            self.pairedDeviceAddresses = pairedDeviceAddresses
                ?? Self.syntheticPairedAddresses(
                    candidates: likelyCandidates,
                    total: totalPairedDeviceCount
                )
        }
    }

    func failDiscovery(with error: Error) {
        lock.withLock { discoveryError = error }
    }

    func blockNextDiscovery() {
        lock.withLock { shouldBlockDiscovery = true }
    }

    func releaseDiscovery() {
        let continuation = lock.withLock {
            let continuation = discoveryContinuation
            discoveryContinuation = nil
            return continuation
        }
        continuation?.resume()
    }

    func blockNextSearch() {
        lock.withLock { shouldBlockSearch = true }
    }

    func releaseSearch() {
        let continuation = lock.withLock {
            let continuation = searchContinuation
            searchContinuation = nil
            return continuation
        }
        continuation?.resume()
    }

    func enqueueSearchResult(_ result: AzimuthBluetoothRadioSearchSnapshot) {
        lock.withLock { searchResults.append(result) }
    }

    func pairedDeviceDiscovery() async throws -> AzimuthBluetoothDiscoverySnapshot {
        let shouldBlock = lock.withLock {
            let block = shouldBlockDiscovery
            shouldBlockDiscovery = false
            return block
        }
        if shouldBlock {
            await withCheckedContinuation { continuation in
                lock.withLock { discoveryContinuation = continuation }
                discoveryStartedContinuation.yield(())
            }
        }
        return try lock.withLock {
            discoveries += 1
            if let discoveryError { throw discoveryError }
            return AzimuthBluetoothDiscoverySnapshot(
                likelyRadioEndpoints: candidates,
                totalPairedDeviceCount: totalPairedDeviceCount,
                pairedDeviceAddresses: pairedDeviceAddresses
            )
        }
    }

    private static func syntheticPairedAddresses(
        candidates: [AzimuthBluetoothEndpoint],
        total: UInt32
    ) -> [String] {
        var addresses = candidates.map(\.address)
        while addresses.count < Int(total) {
            addresses.append(
                String(format: "FE:ED:00:00:00:%02X", addresses.count)
            )
        }
        return addresses
    }

    func findCustomNamedRadios(
        previouslyProbedAddresses: [String]
    ) async throws -> AzimuthBluetoothRadioSearchSnapshot {
        let shouldBlock = lock.withLock {
            let block = shouldBlockSearch
            shouldBlockSearch = false
            return block
        }
        if shouldBlock {
            await withCheckedContinuation { continuation in
                lock.withLock { searchContinuation = continuation }
                searchStartedContinuation.yield(())
            }
        }
        return try lock.withLock {
            searchExclusions.append(previouslyProbedAddresses)
            guard !searchResults.isEmpty else {
                throw RadioEndpointSelectionError.customBluetoothSearchUnavailable
            }
            return searchResults.removeFirst()
        }
    }

    func makeLink(exactAddress: String) async throws -> any AzimuthBluetoothByteLink {
        try lock.withLock {
            requestedAddresses.append(exactAddress)
            guard let link = exactLinks[exactAddress] else {
                throw RadioEndpointSelectionError.invalidEndpoint(id: exactAddress)
            }
            return link
        }
    }

    func makeLink(
        exactAddress: String,
        matchingExpectedUSBSerialNumber serialNumber: String
    ) async throws -> any AzimuthBluetoothByteLink {
        lock.withLock {
            requestedQualifiedTargets.append("\(exactAddress)|\(serialNumber)")
        }
        return matchingLink
    }

    func makeLink(
        matchingExpectedUSBSerialNumber serialNumber: String
    ) async throws -> any AzimuthBluetoothByteLink {
        lock.withLock { requestedSerialNumbers.append(serialNumber) }
        return matchingLink
    }
}

private final class AzimuthRouterMockUSBFactory: AzimuthUSBTransportFactory,
    @unchecked Sendable
{
    private let lock = NSLock()
    private var endpoints: [AzimuthUSBEndpoint]
    private var transports: [String: AzimuthRouterMockTransport]
    private var requestedEndpointIDs: [String] = []
    private var requestedDevicePaths: [String] = []

    init(
        endpoints: [AzimuthUSBEndpoint],
        transports: [String: AzimuthRouterMockTransport]
    ) {
        self.endpoints = endpoints
        self.transports = transports
    }

    var requests: [String] { lock.withLock { requestedEndpointIDs } }
    var requestedPaths: [String] { lock.withLock { requestedDevicePaths } }

    func replaceEndpoints(_ next: [AzimuthUSBEndpoint]) {
        lock.withLock { endpoints = next }
    }

    func availableEndpoints() -> [AzimuthUSBEndpoint] {
        lock.withLock { endpoints }
    }

    func makeTransport(
        endpoint: AzimuthUSBEndpoint
    ) throws -> any AzimuthRadioTransport {
        try lock.withLock {
            requestedEndpointIDs.append(endpoint.id)
            requestedDevicePaths.append(endpoint.devicePath)
            guard let transport = transports[endpoint.id] else {
                throw RadioEndpointSelectionError.invalidEndpoint(id: endpoint.id)
            }
            return transport
        }
    }
}

private final class AzimuthOutOfOrderBluetoothFactory:
    AzimuthBluetoothLinkFactory,
    @unchecked Sendable
{
    let discoveryStarted: AsyncStream<Int>

    private let lock = NSLock()
    private let startedContinuation: AsyncStream<Int>.Continuation
    private var nextCall = 0
    private var pending: [
        Int: CheckedContinuation<AzimuthBluetoothDiscoverySnapshot, Never>
    ] = [:]

    init() {
        var continuation: AsyncStream<Int>.Continuation!
        discoveryStarted = AsyncStream { continuation = $0 }
        startedContinuation = continuation
    }

    func complete(
        call: Int,
        with snapshot: AzimuthBluetoothDiscoverySnapshot
    ) {
        let continuation = lock.withLock { pending.removeValue(forKey: call) }
        continuation?.resume(returning: snapshot)
    }

    func pairedDeviceDiscovery() async throws -> AzimuthBluetoothDiscoverySnapshot {
        let call = lock.withLock { () -> Int in
            defer { nextCall += 1 }
            return nextCall
        }
        return await withCheckedContinuation { continuation in
            lock.withLock { pending[call] = continuation }
            startedContinuation.yield(call)
        }
    }

    func findCustomNamedRadios(
        previouslyProbedAddresses: [String]
    ) async throws -> AzimuthBluetoothRadioSearchSnapshot {
        _ = previouslyProbedAddresses
        throw RadioEndpointSelectionError.customBluetoothSearchUnavailable
    }

    func makeLink(
        exactAddress: String
    ) async throws -> any AzimuthBluetoothByteLink {
        throw RadioEndpointSelectionError.invalidEndpoint(id: exactAddress)
    }

    func makeLink(
        exactAddress: String,
        matchingExpectedUSBSerialNumber serialNumber: String
    ) async throws -> any AzimuthBluetoothByteLink {
        _ = serialNumber
        throw RadioEndpointSelectionError.invalidEndpoint(id: exactAddress)
    }

    func makeLink(
        matchingExpectedUSBSerialNumber serialNumber: String
    ) async throws -> any AzimuthBluetoothByteLink {
        throw RadioEndpointSelectionError.invalidEndpoint(id: serialNumber)
    }
}

final class AzimuthSelectableRadioTransportTests: XCTestCase {
    private static let firstUSB = AzimuthUSBEndpoint(
        id: "usb:serial:B3B00001",
        displayName: "Kenwood TH-D75",
        devicePath: "/dev/cu.usbmodem101",
        usbSerialNumber: "B3B00001"
    )
    private static let secondUSB = AzimuthUSBEndpoint(
        id: "usb:serial:B3B00002",
        displayName: "Kenwood TH-D75",
        devicePath: "/dev/cu.usbmodem201",
        usbSerialNumber: "B3B00002"
    )
    private static let firstBluetooth = AzimuthBluetoothEndpoint(
        address: "00-11-22-33-44-55",
        displayName: "TH-D75"
    )
    private static let secondBluetooth = AzimuthBluetoothEndpoint(
        address: "AA-BB-CC-DD-EE-FF",
        displayName: "TH-D75"
    )

    func testCancelledOlderDiscoveryCannotOverwriteNewerInventory() async throws {
        let bluetoothFactory = AzimuthOutOfOrderBluetoothFactory()
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: bluetoothFactory
        )
        var started = bluetoothFactory.discoveryStarted.makeAsyncIterator()
        let older = Task { try await router.availableEndpointSnapshot() }
        let olderCall = await started.next()
        XCTAssertEqual(olderCall, 0)
        older.cancel()

        let newer = Task { try await router.availableEndpointSnapshot() }
        let newerCall = await started.next()
        XCTAssertEqual(newerCall, 1)
        bluetoothFactory.complete(
            call: 1,
            with: AzimuthBluetoothDiscoverySnapshot(
                likelyRadioEndpoints: [Self.secondBluetooth],
                totalPairedDeviceCount: 1,
                pairedDeviceAddresses: [Self.secondBluetooth.address]
            )
        )
        let current = try await newer.value
        XCTAssertTrue(current.endpoints.contains { $0.id == Self.secondBluetooth.id })

        bluetoothFactory.complete(
            call: 0,
            with: AzimuthBluetoothDiscoverySnapshot(
                likelyRadioEndpoints: [Self.firstBluetooth],
                totalPairedDeviceCount: 1,
                pairedDeviceAddresses: [Self.firstBluetooth.address]
            )
        )
        do {
            _ = try await older.value
            XCTFail("The stale discovery must not publish after a newer pass")
        } catch is CancellationError {
            // Expected.
        }

        try await router.selectEndpoint(id: Self.secondBluetooth.id)
        let selected = await router.selectedRadioEndpoint()
        XCTAssertEqual(selected?.id, Self.secondBluetooth.id)
        let staleError = try await captureError {
            try await router.selectEndpoint(id: Self.firstBluetooth.id)
        }
        XCTAssertEqual(
            staleError as? RadioEndpointSelectionError,
            .invalidEndpoint(id: Self.firstBluetooth.id)
        )
    }

    func testBluetoothTransportUsesExactAddressAndForwardsBytesAndIdentity() async throws {
        let link = AzimuthRouterMockBluetoothLink(serialNumber: "B3B00001")
        link.enqueueRead([4, 5, 6])
        let factory = makeFactory(firstLink: link)
        let transport = AzimuthBluetoothRadioTransport(
            endpoint: Self.firstBluetooth,
            factory: factory
        )

        try await transport.open()
        let openedSerialNumber = await transport.hardwareSerialNumber
        let openedState = await transport.state
        XCTAssertEqual(factory.exactRequests, [Self.firstBluetooth.address])
        XCTAssertEqual(openedSerialNumber, "B3B00001")
        XCTAssertEqual(openedState, .connected)
        XCTAssertEqual(transport.device.id, Self.firstBluetooth.id)

        try transport.setBaudRate(baud: 9_600)
        try await transport.write([1, 2, 3])
        let readBytes = try await transport.read(maxBytes: 2)
        XCTAssertEqual(readBytes, [4, 5])
        XCTAssertEqual(link.writes, [[1, 2, 3]])

        await transport.close()
        let closedSerialNumber = await transport.hardwareSerialNumber
        let closedState = await transport.state
        XCTAssertNil(closedSerialNumber)
        XCTAssertEqual(closedState, .disconnected)
        XCTAssertEqual(link.closeCount, 1)
    }

    func testBluetoothCloseFencesSuspendedOpenIdentityPublication() async throws {
        let link = AzimuthRouterMockBluetoothLink(
            serialNumber: "B3B00001",
            blockIdentityRead: true
        )
        let transport = AzimuthBluetoothRadioTransport(
            endpoint: Self.firstBluetooth,
            factory: makeFactory(firstLink: link)
        )
        var identityStarted = link.identityReadStarted.makeAsyncIterator()
        let open = Task { try await transport.open() }
        let childIdentityStarted: Void? = await identityStarted.next()
        XCTAssertNotNil(childIdentityStarted)

        await transport.close()
        link.releaseIdentityRead()

        do {
            try await open.value
            XCTFail("A close during identity publication must cancel the stale open")
        } catch is CancellationError {
            // Expected.
        } catch {
            XCTFail("Expected CancellationError, received \(error)")
        }
        let finalChildState = await transport.state
        let finalChildSerial = await transport.hardwareSerialNumber
        XCTAssertEqual(finalChildState, .disconnected)
        XCTAssertNil(finalChildSerial)
        XCTAssertEqual(link.closeCount, 1)
    }

    func testRouterCloseFencesSuspendedChildIdentityPublication() async throws {
        let link = AzimuthRouterMockBluetoothLink(
            serialNumber: "B3B00001",
            blockIdentityRead: true
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: makeFactory(firstLink: link)
        )
        _ = try await router.availableEndpointSnapshot()
        try await router.selectEndpoint(id: Self.firstBluetooth.id)
        var identityStarted = link.identityReadStarted.makeAsyncIterator()
        let open = Task { try await router.open() }
        let routerIdentityStarted: Void? = await identityStarted.next()
        XCTAssertNotNil(routerIdentityStarted)

        await router.close()
        link.releaseIdentityRead()

        do {
            try await open.value
            XCTFail("Router close must cancel stale child-open publication")
        } catch is CancellationError {
            // Expected.
        } catch {
            XCTFail("Expected CancellationError, received \(error)")
        }
        let finalRouterState = await router.state
        let finalRouterSerial = await router.hardwareSerialNumber
        XCTAssertEqual(finalRouterState, .disconnected)
        XCTAssertNil(finalRouterSerial)
        XCTAssertEqual(link.closeCount, 1)
    }

    func testRouterDiscoversBothPathsAndSelectsDuplicateNamedRadioByAddress() async throws {
        let firstLink = AzimuthRouterMockBluetoothLink(serialNumber: "B3B00001")
        let secondLink = AzimuthRouterMockBluetoothLink(serialNumber: "B3B00002")
        secondLink.enqueueRead([9, 8])
        let factory = makeFactory(firstLink: firstLink, secondLink: secondLink)
        let usb = AzimuthRouterMockTransport(
            device: .thD75USBC,
            serialNumber: "B3B00001"
        )
        let usbFactory = makeUSBFactory(transport: usb)
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: usbFactory,
            bluetoothFactory: factory
        )
        let selector = await MainActor.run {
            AzimuthSelectableRadioEndpointSelector(router: router)
        }

        let endpoints = try await selector.refreshEndpoints().endpoints
        XCTAssertEqual(endpoints.map(\.transport), [.usb, .bluetooth, .bluetooth])
        XCTAssertEqual(endpoints.dropFirst().map(\.name), ["TH-D75", "TH-D75"])

        try await selector.selectEndpoint(id: Self.secondBluetooth.id)
        XCTAssertEqual(router.device.id, Self.secondBluetooth.id)
        XCTAssertEqual(router.device.connection, "Bluetooth")
        try await router.open()

        let routedSerialNumber = await router.hardwareSerialNumber
        let routedBytes = try await router.read(maxBytes: 8)
        XCTAssertEqual(factory.exactRequests, [Self.secondBluetooth.address])
        XCTAssertEqual(usb.openCount, 0)
        XCTAssertEqual(routedSerialNumber, "B3B00002")
        XCTAssertEqual(routedBytes, [9, 8])

        let selectionWhileOpen = try await captureError {
            try await router.selectEndpoint(id: Self.firstUSB.id)
        }
        XCTAssertEqual(
            selectionWhileOpen as? AzimuthRadioSelectionError,
            .transportIsOpen
        )
        await router.close()
    }

    func testRouterForwardsUSBStateIdentityBaudAndWritesByDefault() async throws {
        let usb = AzimuthRouterMockTransport(
            device: .thD75USBC,
            serialNumber: "B3B00001"
        )
        let factory = makeFactory()
        let usbFactory = makeUSBFactory(transport: usb)
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: usbFactory,
            bluetoothFactory: factory
        )

        try await router.open()
        try router.setBaudRate(baud: 115_200)
        try await router.write([7, 6, 5])

        let openedState = await router.state
        let openedSerialNumber = await router.hardwareSerialNumber
        XCTAssertEqual(openedState, .connected)
        XCTAssertEqual(openedSerialNumber, "B3B00001")
        XCTAssertEqual(usbFactory.requests, [Self.firstUSB.id])
        XCTAssertEqual(usb.baudRates, [115_200])
        XCTAssertEqual(usb.writes, [[7, 6, 5]])

        await router.close()
        let closedSerialNumber = await router.hardwareSerialNumber
        let closedState = await router.state
        XCTAssertNil(closedSerialNumber)
        XCTAssertEqual(closedState, .disconnected)
    }

    func testSameRadioFallbackUsesExpectedUSBSerialCoreTarget() async throws {
        let matching = AzimuthRouterMockBluetoothLink(
            serialNumber: "B3B00001",
            matchedAddress: Self.firstBluetooth.address
        )
        let factory = makeFactory(matchingLink: matching)
        let usb = AzimuthRouterMockTransport(device: Self.firstUSB.device)
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(transport: usb),
            bluetoothFactory: factory
        )

        let endpoint = try await router.selectBluetooth(
            matchingSerialNumber: "B3B00001"
        )
        XCTAssertEqual(endpoint.transport, .bluetooth)
        XCTAssertEqual(router.device.connection, "Bluetooth")

        try await router.open()
        let matchedSerialNumber = await router.hardwareSerialNumber
        XCTAssertEqual(factory.serialRequests, ["B3B00001"])
        XCTAssertEqual(matchedSerialNumber, "B3B00001")
        XCTAssertEqual(matching.openCount, 1)
        let resolvedEndpointID = await router.selectedEndpointID
        XCTAssertEqual(resolvedEndpointID, Self.firstBluetooth.id)
        XCTAssertEqual(router.device.id, Self.firstBluetooth.id)
        await router.close()

        try await router.selectEndpoint(id: Self.firstBluetooth.id)
        try await router.open()
        XCTAssertEqual(factory.serialRequests, ["B3B00001"])
        XCTAssertEqual(
            factory.qualifiedRequests,
            ["\(Self.firstBluetooth.address)|B3B00001"]
        )
        XCTAssertEqual(matching.openCount, 2)
        await router.close()
    }

    func testSameRadioSelectionPrefersUniqueRetainedQualifiedAddress() async throws {
        let qualified = AzimuthBluetoothEndpoint(
            address: Self.firstBluetooth.address,
            displayName: "Field TH-D75",
            verifiedCATSerialNumber: "B3B00001"
        )
        let matching = AzimuthRouterMockBluetoothLink(
            serialNumber: "B3B00001",
            matchedAddress: qualified.address
        )
        let factory = makeFactory(matchingLink: matching)
        factory.replaceDiscovery(
            likelyCandidates: [qualified],
            totalPairedDeviceCount: 1,
            pairedDeviceAddresses: [qualified.address]
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: factory
        )
        _ = try await router.availableEndpointSnapshot()

        let endpoint = try await router.selectBluetooth(
            matchingSerialNumber: "B3B00001"
        )
        try await router.open()

        XCTAssertEqual(endpoint.id, qualified.id)
        XCTAssertTrue(factory.serialRequests.isEmpty)
        XCTAssertEqual(
            factory.qualifiedRequests,
            ["\(qualified.address)|B3B00001"]
        )
        let knownAddress = try await router.knownQualifiedBluetoothAddress(
            expectedSerialNumber: "B3B00001"
        )
        XCTAssertEqual(knownAddress, qualified.address)
        await router.close()
    }

    func testSameRadioSelectionFailsClosedOnDuplicateQualifiedSerial() async throws {
        let first = AzimuthBluetoothEndpoint(
            address: Self.firstBluetooth.address,
            displayName: "First D75",
            verifiedCATSerialNumber: "B3B00001"
        )
        let second = AzimuthBluetoothEndpoint(
            address: Self.secondBluetooth.address,
            displayName: "Second D75",
            verifiedCATSerialNumber: "B3B00001"
        )
        let factory = makeFactory()
        factory.replaceDiscovery(
            likelyCandidates: [first, second],
            totalPairedDeviceCount: 2,
            pairedDeviceAddresses: [first.address, second.address]
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: factory
        )
        _ = try await router.availableEndpointSnapshot()

        let error = try await captureError {
            _ = try await router.selectBluetooth(
                matchingSerialNumber: "B3B00001"
            )
        }

        XCTAssertEqual(
            error as? AzimuthRadioSelectionError,
            .ambiguousQualifiedBluetoothSerial(serialNumber: "B3B00001")
        )
        XCTAssertTrue(factory.serialRequests.isEmpty)
        XCTAssertTrue(factory.qualifiedRequests.isEmpty)
    }

    func testUnhintedFallbackCanBeReselectedAndRemainsSerialQualified() async throws {
        let customNamed = AzimuthBluetoothEndpoint(
            address: "10-20-30-40-50-60",
            displayName: "Field Control"
        )
        let matching = AzimuthRouterMockBluetoothLink(
            serialNumber: "B3B00001",
            matchedAddress: customNamed.address
        )
        let factory = makeFactory(matchingLink: matching)
        factory.replaceDiscovery(
            likelyCandidates: [],
            totalPairedDeviceCount: 1
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: factory
        )
        _ = try await router.availableEndpointSnapshot()

        _ = try await router.selectBluetooth(matchingSerialNumber: "B3B00001")
        try await router.open()
        let selectedAfterFallback = await router.selectedRadioEndpoint()
        let resolved = try XCTUnwrap(selectedAfterFallback)
        await router.close()

        try await router.selectEndpoint(id: resolved.id)
        try await router.open()

        XCTAssertEqual(factory.serialRequests, ["B3B00001"])
        XCTAssertEqual(
            factory.qualifiedRequests,
            ["\(customNamed.address)|B3B00001"]
        )
        let reselectedEndpointID = await router.selectedEndpointID
        XCTAssertEqual(reselectedEndpointID, customNamed.id)
        await router.close()
    }

    func testQualifiedFallbackSurvivesOrdinaryDiscoveryRefresh() async throws {
        let matching = AzimuthRouterMockBluetoothLink(
            serialNumber: "B3B00001",
            matchedAddress: Self.firstBluetooth.address
        )
        let factory = makeFactory(matchingLink: matching)
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: factory
        )
        _ = try await router.availableEndpointSnapshot()
        _ = try await router.selectBluetooth(matchingSerialNumber: "B3B00001")
        try await router.open()
        await router.close()

        _ = try await router.availableEndpointSnapshot()
        try await router.selectEndpoint(id: Self.firstBluetooth.id)
        try await router.open()

        XCTAssertEqual(
            factory.qualifiedRequests,
            ["\(Self.firstBluetooth.address)|B3B00001"]
        )
        await router.close()
    }

    func testFreshDiscoveryRetainsStillPairedUnhintedQualifiedEndpoint() async throws {
        let custom = AzimuthBluetoothEndpoint(
            address: "10:20:30:40:50:60",
            displayName: "Kenwood TH-D75"
        )
        let matching = AzimuthRouterMockBluetoothLink(
            serialNumber: "B3B00001",
            matchedAddress: custom.address
        )
        let factory = makeFactory(matchingLink: matching)
        factory.replaceDiscovery(
            likelyCandidates: [],
            totalPairedDeviceCount: 1,
            pairedDeviceAddresses: [custom.address]
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: factory
        )
        _ = try await router.availableEndpointSnapshot()
        _ = try await router.selectBluetooth(matchingSerialNumber: "B3B00001")
        try await router.open()
        await router.close()

        let refreshed = try await router.availableEndpointSnapshot()
        XCTAssertTrue(refreshed.endpoints.contains { $0.id == custom.id })

        try await router.selectEndpoint(id: custom.id)
        try await router.open()
        XCTAssertEqual(
            factory.qualifiedRequests,
            ["\(custom.address)|B3B00001"]
        )
        await router.close()
    }

    func testFreshDiscoveryRemovesUnpairedQualifiedEndpoint() async throws {
        let custom = AzimuthBluetoothEndpoint(
            address: "10:20:30:40:50:60",
            displayName: "Kenwood TH-D75"
        )
        let matching = AzimuthRouterMockBluetoothLink(
            serialNumber: "B3B00001",
            matchedAddress: custom.address
        )
        let factory = makeFactory(matchingLink: matching)
        factory.replaceDiscovery(
            likelyCandidates: [],
            totalPairedDeviceCount: 1,
            pairedDeviceAddresses: [custom.address]
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: factory
        )
        _ = try await router.availableEndpointSnapshot()
        _ = try await router.selectBluetooth(matchingSerialNumber: "B3B00001")
        try await router.open()
        await router.close()

        factory.replaceDiscovery(
            likelyCandidates: [],
            totalPairedDeviceCount: 0,
            pairedDeviceAddresses: []
        )
        let refreshed = try await router.availableEndpointSnapshot()

        XCTAssertFalse(refreshed.endpoints.contains { $0.id == custom.id })
        let error = try await captureError {
            try await router.selectEndpoint(id: custom.id)
        }
        XCTAssertEqual(
            error as? RadioEndpointSelectionError,
            .invalidEndpoint(id: custom.id)
        )
    }

    func testRecoveryCanSelectUSBAgainBeforeOpen() async throws {
        let usb = AzimuthRouterMockTransport(device: Self.firstUSB.device)
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(transport: usb),
            bluetoothFactory: makeFactory()
        )

        _ = try await router.selectBluetooth(
            matchingSerialNumber: "B3B00001"
        )
        XCTAssertEqual(router.device.connection, "Bluetooth")

        try await router.selectUSBForRecovery(
            expectedSerialNumber: "B3B00001"
        )
        XCTAssertEqual(router.device, usb.device)
        try await router.open()
        XCTAssertEqual(usb.openCount, 1)
        await router.close()
    }

    func testRefreshRejectsDuplicateStableIdentifiers() async throws {
        let duplicate = AzimuthBluetoothEndpoint(
            address: Self.firstBluetooth.address,
            displayName: "Renamed radio"
        )
        let factory = makeFactory()
        factory.replaceCandidates([Self.firstBluetooth, duplicate])
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: factory
        )

        let error = try await captureError {
            _ = try await router.availableEndpointSnapshot()
        }
        XCTAssertEqual(
            error as? RadioEndpointSelectionError,
            .duplicateEndpoint(id: Self.firstBluetooth.id)
        )
    }

    func testDiscoveryOmitsUSBWhenNoVerifiedDeviceIsAttached() async throws {
        let usbFactory = AzimuthRouterMockUSBFactory(
            endpoints: [],
            transports: [:]
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: usbFactory,
            bluetoothFactory: makeFactory()
        )
        let selector = await MainActor.run {
            AzimuthSelectableRadioEndpointSelector(router: router)
        }

        let initialEndpoints = await MainActor.run { selector.initialEndpoints }
        XCTAssertTrue(initialEndpoints.isEmpty)
        let endpoints = try await selector.refreshEndpoints().endpoints
        XCTAssertEqual(endpoints.map(\.transport), [.bluetooth, .bluetooth])
        XCTAssertFalse(endpoints.contains { $0.transport == .usb })
    }

    func testSecondUSBSelectionBuildsOnlyItsExactPathTransport() async throws {
        let first = AzimuthRouterMockTransport(device: Self.firstUSB.device)
        let second = AzimuthRouterMockTransport(
            device: Self.secondUSB.device,
            serialNumber: "B3B00002"
        )
        let usbFactory = AzimuthRouterMockUSBFactory(
            endpoints: [Self.firstUSB, Self.secondUSB],
            transports: [
                Self.firstUSB.id: first,
                Self.secondUSB.id: second,
            ]
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: usbFactory,
            bluetoothFactory: makeFactory()
        )

        let endpoints = try await router.availableEndpointSnapshot().endpoints
        XCTAssertEqual(
            endpoints.filter { $0.transport == .usb }.map(\.detail),
            [Self.firstUSB.devicePath, Self.secondUSB.devicePath]
        )
        try await router.selectEndpoint(id: Self.secondUSB.id)
        try await router.open()

        XCTAssertEqual(router.device, Self.secondUSB.device)
        XCTAssertEqual(usbFactory.requests, [Self.secondUSB.id])
        XCTAssertEqual(first.openCount, 0)
        XCTAssertEqual(second.openCount, 1)
        await router.close()
    }

    func testRecoveryWaitsWhileExpectedUSBSerialIsAbsent() async throws {
        let usb = AzimuthRouterMockTransport(device: Self.secondUSB.device)
        let usbFactory = makeUSBFactory(
            endpoint: Self.secondUSB,
            transport: usb
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: usbFactory,
            bluetoothFactory: makeFactory()
        )

        _ = try await router.selectBluetooth(
            matchingSerialNumber: "B3B00002"
        )
        usbFactory.replaceEndpoints([])
        let error = try await captureError {
            try await router.selectUSBForRecovery(
                expectedSerialNumber: "B3B00002"
            )
        }

        XCTAssertEqual(
            error as? AzimuthRadioSelectionError,
            .expectedUSBRadioUnavailable(serialNumber: "B3B00002")
        )
        XCTAssertTrue(usbFactory.requests.isEmpty)
        XCTAssertEqual(usb.openCount, 0)
    }

    func testRecoveryFollowsSameUSBSerialToNewTTYPath() async throws {
        let oldEndpoint = AzimuthUSBEndpoint(
            id: "usb:serial:B3B00001",
            displayName: "Kenwood TH-D75",
            devicePath: "/dev/cu.usbmodem101",
            usbSerialNumber: "B3B00001"
        )
        let newEndpoint = AzimuthUSBEndpoint(
            id: oldEndpoint.id,
            displayName: oldEndpoint.displayName,
            devicePath: "/dev/cu.usbmodem301",
            usbSerialNumber: "B3B00001"
        )
        let usb = AzimuthRouterMockTransport(
            device: newEndpoint.device,
            serialNumber: "B3B00001"
        )
        let usbFactory = AzimuthRouterMockUSBFactory(
            endpoints: [oldEndpoint],
            transports: [oldEndpoint.id: usb]
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: usbFactory,
            bluetoothFactory: makeFactory()
        )
        usbFactory.replaceEndpoints([newEndpoint])

        try await router.selectUSBForRecovery(
            expectedSerialNumber: "B3B00001"
        )
        try await router.open()

        XCTAssertEqual(usbFactory.requestedPaths, [newEndpoint.devicePath])
        let recoveredSerialNumber = await router.hardwareSerialNumber
        XCTAssertEqual(recoveredSerialNumber, "B3B00001")
        await router.close()
    }

    func testRecoveryRejectsDifferentSerialReusingRetainedTTYPath() async throws {
        let oldEndpoint = AzimuthUSBEndpoint(
            id: "usb:serial:B3B00001",
            displayName: "Kenwood TH-D75",
            devicePath: "/dev/cu.usbmodem101",
            usbSerialNumber: "B3B00001"
        )
        let replacement = AzimuthUSBEndpoint(
            id: "usb:serial:B3B99999",
            displayName: "Kenwood TH-D75",
            devicePath: oldEndpoint.devicePath,
            usbSerialNumber: "B3B99999"
        )
        let usbFactory = AzimuthRouterMockUSBFactory(
            endpoints: [oldEndpoint],
            transports: [:]
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: usbFactory,
            bluetoothFactory: makeFactory()
        )
        usbFactory.replaceEndpoints([replacement])

        let error = try await captureError {
            try await router.selectUSBForRecovery(
                expectedSerialNumber: "B3B00001"
            )
        }

        XCTAssertEqual(
            error as? AzimuthRadioSelectionError,
            .differentUSBRadioAtRetainedPath(
                expected: "B3B00001",
                actual: "B3B99999"
            )
        )
    }

    func testBluetoothDiscoveryFailureStillPublishesFreshUSBEndpoint() async throws {
        let bluetoothFactory = makeFactory()
        bluetoothFactory.failDiscovery(with: RouterDiscoveryError.bluetoothDenied)
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: bluetoothFactory
        )

        let snapshot = try await router.availableEndpointSnapshot()

        XCTAssertEqual(snapshot.endpoints.map(\.id), [Self.firstUSB.id])
        XCTAssertNil(snapshot.pairedBluetoothDeviceCount)
        XCTAssertEqual(snapshot.endpoints.first?.detail, Self.firstUSB.devicePath)
        XCTAssertTrue(
            snapshot.warning?.contains("Bluetooth connections unavailable") == true
        )
        XCTAssertTrue(snapshot.warning?.contains("permission was denied") == true)
    }

    func testEndpointRefreshUsesUSBSnapshotTakenAfterBlockedBluetoothDiscovery() async throws {
        let bluetoothFactory = makeFactory()
        bluetoothFactory.blockNextDiscovery()
        let usbFactory = makeUSBFactory()
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: usbFactory,
            bluetoothFactory: bluetoothFactory
        )
        var discoveryStarted = bluetoothFactory.discoveryStarted.makeAsyncIterator()
        let refresh = Task { try await router.availableEndpointSnapshot() }
        let didStart: Void? = await discoveryStarted.next()
        XCTAssertNotNil(didStart)

        usbFactory.replaceEndpoints([Self.secondUSB])
        bluetoothFactory.releaseDiscovery()
        let snapshot = try await refresh.value

        XCTAssertEqual(
            snapshot.endpoints.filter { $0.transport == .usb }.map(\.detail),
            [Self.secondUSB.devicePath]
        )
    }

    func testCustomSearchUsesUSBSnapshotTakenAfterBlockedProbe() async throws {
        let bluetoothFactory = makeFactory()
        bluetoothFactory.replaceDiscovery(
            likelyCandidates: [],
            totalPairedDeviceCount: 1
        )
        bluetoothFactory.enqueueSearchResult(
            AzimuthBluetoothRadioSearchSnapshot(
                provenRadioEndpoints: [],
                likelyRadioEndpoints: [],
                pairedDeviceAddresses: ["FE:ED:00:00:00:00"],
                totalPairedDeviceCount: 1,
                probedAddresses: ["FE:ED:00:00:00:00"],
                currentProbedAddresses: ["FE:ED:00:00:00:00"],
                hasInventorySnapshot: true,
                probedCandidateCount: 1,
                totalUnhintedCandidateCount: 1,
                isComplete: true,
                wasCancelled: false
            )
        )
        let usbFactory = makeUSBFactory()
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: usbFactory,
            bluetoothFactory: bluetoothFactory
        )
        _ = try await router.availableEndpointSnapshot()
        bluetoothFactory.blockNextSearch()
        var searchStarted = bluetoothFactory.searchStarted.makeAsyncIterator()
        let search = Task { try await router.findCustomNamedBluetoothRadios() }
        let didStart: Void? = await searchStarted.next()
        XCTAssertNotNil(didStart)

        usbFactory.replaceEndpoints([Self.secondUSB])
        bluetoothFactory.releaseSearch()
        let result = try await search.value

        XCTAssertEqual(
            result.snapshot.endpoints
                .filter { $0.transport == .usb }
                .map(\.detail),
            [Self.secondUSB.devicePath]
        )
    }

    func testBluetoothSelectionUsesRetainedSnapshotWithoutSecondHelperLaunch() async throws {
        let bluetoothFactory = makeFactory()
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: bluetoothFactory
        )
        _ = try await router.availableEndpointSnapshot()
        bluetoothFactory.failDiscovery(with: RouterDiscoveryError.bluetoothDenied)

        try await router.selectEndpoint(id: Self.secondBluetooth.id)
        try await router.open()

        XCTAssertEqual(bluetoothFactory.discoveryCount, 1)
        XCTAssertEqual(
            bluetoothFactory.exactRequests,
            [Self.secondBluetooth.address]
        )
        await router.close()
    }

    func testCustomNamedPairedDeviceEnablesSerialScanWithoutPickerRow() async throws {
        let customNamed = AzimuthBluetoothEndpoint(
            address: "10-20-30-40-50-60",
            displayName: "Field Control"
        )
        let matching = AzimuthRouterMockBluetoothLink(
            serialNumber: "B3B00001",
            matchedAddress: customNamed.address
        )
        let bluetoothFactory = makeFactory(matchingLink: matching)
        bluetoothFactory.replaceDiscovery(
            likelyCandidates: [],
            totalPairedDeviceCount: 1
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: bluetoothFactory
        )

        let snapshot = try await router.availableEndpointSnapshot()
        XCTAssertEqual(snapshot.endpoints.map(\.transport), [.usb])
        XCTAssertEqual(snapshot.pairedBluetoothDeviceCount, 1)

        _ = try await router.selectBluetooth(
            matchingSerialNumber: "B3B00001"
        )
        try await router.open()

        XCTAssertEqual(bluetoothFactory.serialRequests, ["B3B00001"])
        let resolvedCustomEndpointID = await router.selectedEndpointID
        XCTAssertEqual(resolvedCustomEndpointID, customNamed.id)
        let resolvedDetail = await router.selectedRadioEndpoint()?.detail
        XCTAssertEqual(resolvedDetail, customNamed.address)
        await router.close()
    }

    func testZeroPairedDevicesReportsKnownZeroWithoutPickerRows() async throws {
        let bluetoothFactory = makeFactory()
        bluetoothFactory.replaceDiscovery(
            likelyCandidates: [],
            totalPairedDeviceCount: 0
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: bluetoothFactory
        )

        let snapshot = try await router.availableEndpointSnapshot()

        XCTAssertEqual(snapshot.endpoints.map(\.transport), [.usb])
        XCTAssertEqual(snapshot.pairedBluetoothDeviceCount, 0)
        XCTAssertNil(snapshot.warning)
    }

    func testCustomNamedSearchAdvancesPastEightWithExactExclusions() async throws {
        let bluetoothFactory = makeFactory()
        bluetoothFactory.replaceDiscovery(
            likelyCandidates: [Self.firstBluetooth, Self.secondBluetooth],
            totalPairedDeviceCount: 11
        )
        let addresses = (1...9).map {
            String(format: "10:20:30:40:50:%02X", $0)
        }
        let firstProven = AzimuthBluetoothEndpoint(
            address: addresses[7],
            displayName: "Field Seven",
            verifiedCATSerialNumber: "B3B00007"
        )
        let secondProven = AzimuthBluetoothEndpoint(
            address: addresses[8],
            displayName: "Field Eight",
            verifiedCATSerialNumber: "B3B00008"
        )
        bluetoothFactory.enqueueSearchResult(
            AzimuthBluetoothRadioSearchSnapshot(
                provenRadioEndpoints: [firstProven],
                likelyRadioEndpoints: [Self.firstBluetooth, Self.secondBluetooth],
                pairedDeviceAddresses: [
                    Self.firstBluetooth.address,
                    Self.secondBluetooth.address,
                ] + addresses,
                totalPairedDeviceCount: 11,
                probedAddresses: Array(addresses.prefix(8)),
                currentProbedAddresses: Array(addresses.prefix(8)),
                hasInventorySnapshot: true,
                probedCandidateCount: 8,
                totalUnhintedCandidateCount: 9,
                isComplete: false,
                wasCancelled: false
            )
        )
        bluetoothFactory.enqueueSearchResult(
            AzimuthBluetoothRadioSearchSnapshot(
                provenRadioEndpoints: [secondProven],
                likelyRadioEndpoints: [Self.firstBluetooth, Self.secondBluetooth],
                pairedDeviceAddresses: [
                    Self.firstBluetooth.address,
                    Self.secondBluetooth.address,
                ] + addresses,
                totalPairedDeviceCount: 11,
                probedAddresses: [addresses[8]],
                currentProbedAddresses: addresses,
                hasInventorySnapshot: true,
                probedCandidateCount: 1,
                totalUnhintedCandidateCount: 9,
                isComplete: true,
                wasCancelled: false
            )
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: bluetoothFactory
        )
        _ = try await router.availableEndpointSnapshot()

        let firstPass = try await router.findCustomNamedBluetoothRadios()
        let secondPass = try await router.findCustomNamedBluetoothRadios()

        XCTAssertFalse(firstPass.isComplete)
        XCTAssertEqual(firstPass.probedCandidateCount, 8)
        XCTAssertTrue(secondPass.isComplete)
        XCTAssertEqual(secondPass.probedCandidateCount, 9)
        XCTAssertEqual(
            bluetoothFactory.recordedSearchExclusions,
            [[], Array(addresses.prefix(8)).sorted()]
        )
        XCTAssertTrue(secondPass.snapshot.endpoints.contains { $0.id == firstProven.id })
        XCTAssertTrue(secondPass.snapshot.endpoints.contains { $0.id == secondProven.id })
        let knownAddress = try await router.knownQualifiedBluetoothAddress(
            expectedSerialNumber: "B3B00007"
        )
        XCTAssertEqual(
            knownAddress,
            firstProven.address,
            "Recovery must reuse the proved exact address instead of rescanning a bounded inventory."
        )
    }

    func testStoppedCustomNamedSearchRetainsCompletedProofs() async throws {
        let bluetoothFactory = makeFactory()
        bluetoothFactory.replaceDiscovery(
            likelyCandidates: [],
            totalPairedDeviceCount: 2
        )
        let proven = AzimuthBluetoothEndpoint(
            address: "10:20:30:40:50:60",
            displayName: "Portable D75",
            verifiedCATSerialNumber: "B3B00009"
        )
        bluetoothFactory.enqueueSearchResult(
            AzimuthBluetoothRadioSearchSnapshot(
                provenRadioEndpoints: [proven],
                likelyRadioEndpoints: [],
                pairedDeviceAddresses: [
                    proven.address,
                    "FE:ED:00:00:00:01",
                ],
                totalPairedDeviceCount: 2,
                probedAddresses: [proven.address],
                currentProbedAddresses: [proven.address],
                hasInventorySnapshot: true,
                probedCandidateCount: 1,
                totalUnhintedCandidateCount: 2,
                isComplete: false,
                wasCancelled: true
            )
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: bluetoothFactory
        )
        _ = try await router.availableEndpointSnapshot()

        let stopped = try await router.findCustomNamedBluetoothRadios()

        XCTAssertTrue(stopped.wasCancelled)
        XCTAssertFalse(stopped.isComplete)
        XCTAssertEqual(stopped.probedCandidateCount, 1)
        XCTAssertTrue(stopped.snapshot.endpoints.contains { $0.id == proven.id })
    }

    func testCancelledSecondPassBeforeInventoryRetainsPriorProgress() async throws {
        let bluetoothFactory = makeFactory()
        bluetoothFactory.replaceDiscovery(
            likelyCandidates: [],
            totalPairedDeviceCount: 2
        )
        let firstAddress = "FE:ED:00:00:00:00"
        bluetoothFactory.enqueueSearchResult(
            AzimuthBluetoothRadioSearchSnapshot(
                provenRadioEndpoints: [],
                likelyRadioEndpoints: [],
                pairedDeviceAddresses: [
                    firstAddress,
                    "FE:ED:00:00:00:01",
                ],
                totalPairedDeviceCount: 2,
                probedAddresses: [firstAddress],
                currentProbedAddresses: [firstAddress],
                hasInventorySnapshot: true,
                probedCandidateCount: 1,
                totalUnhintedCandidateCount: 2,
                isComplete: false,
                wasCancelled: false
            )
        )
        bluetoothFactory.enqueueSearchResult(
            AzimuthBluetoothRadioSearchSnapshot(
                provenRadioEndpoints: [],
                likelyRadioEndpoints: [],
                pairedDeviceAddresses: [],
                totalPairedDeviceCount: 0,
                probedAddresses: [],
                currentProbedAddresses: [firstAddress],
                hasInventorySnapshot: false,
                probedCandidateCount: 0,
                totalUnhintedCandidateCount: 0,
                isComplete: false,
                wasCancelled: true
            )
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: bluetoothFactory
        )
        _ = try await router.availableEndpointSnapshot()
        _ = try await router.findCustomNamedBluetoothRadios()
        bluetoothFactory.blockNextSearch()
        var started = bluetoothFactory.searchStarted.makeAsyncIterator()
        let second = Task { try await router.findCustomNamedBluetoothRadios() }
        let didStart: Void? = await started.next()
        XCTAssertNotNil(didStart)
        second.cancel()
        bluetoothFactory.releaseSearch()

        let result = try await second.value

        XCTAssertTrue(result.wasCancelled)
        XCTAssertEqual(result.probedCandidateCount, 1)
        XCTAssertEqual(result.totalUnhintedCandidateCount, 2)
        XCTAssertEqual(
            bluetoothFactory.recordedSearchExclusions,
            [[], [firstAddress]]
        )
    }

    func testCustomSearchRejectsUnaccountedCompletedAddress() async throws {
        let bluetoothFactory = makeFactory()
        bluetoothFactory.replaceDiscovery(
            likelyCandidates: [],
            totalPairedDeviceCount: 2
        )
        bluetoothFactory.enqueueSearchResult(
            AzimuthBluetoothRadioSearchSnapshot(
                provenRadioEndpoints: [],
                likelyRadioEndpoints: [],
                pairedDeviceAddresses: [
                    "FE:ED:00:00:00:00",
                    "FE:ED:00:00:00:01",
                ],
                totalPairedDeviceCount: 2,
                probedAddresses: ["FE:ED:00:00:00:00"],
                currentProbedAddresses: [
                    "FE:ED:00:00:00:00",
                    "FE:ED:00:00:00:01",
                ],
                hasInventorySnapshot: true,
                probedCandidateCount: 1,
                totalUnhintedCandidateCount: 2,
                isComplete: true,
                wasCancelled: false
            )
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: bluetoothFactory
        )
        _ = try await router.availableEndpointSnapshot()

        let error = try await captureError {
            _ = try await router.findCustomNamedBluetoothRadios()
        }

        XCTAssertEqual(
            error as? RadioEndpointSelectionError,
            .malformedEndpoint
        )
    }

    func testCustomSearchRejectsEndpointWithoutCATSerialProof() async throws {
        let bluetoothFactory = makeFactory()
        bluetoothFactory.replaceDiscovery(
            likelyCandidates: [],
            totalPairedDeviceCount: 1
        )
        let unproved = AzimuthBluetoothEndpoint(
            address: "FE:ED:00:00:00:00",
            displayName: "Unproved device"
        )
        bluetoothFactory.enqueueSearchResult(
            AzimuthBluetoothRadioSearchSnapshot(
                provenRadioEndpoints: [unproved],
                likelyRadioEndpoints: [],
                pairedDeviceAddresses: [unproved.address],
                totalPairedDeviceCount: 1,
                probedAddresses: [unproved.address],
                currentProbedAddresses: [unproved.address],
                hasInventorySnapshot: true,
                probedCandidateCount: 1,
                totalUnhintedCandidateCount: 1,
                isComplete: true,
                wasCancelled: false
            )
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: bluetoothFactory
        )
        _ = try await router.availableEndpointSnapshot()

        let error = try await captureError {
            _ = try await router.findCustomNamedBluetoothRadios()
        }

        XCTAssertEqual(
            error as? RadioEndpointSelectionError,
            .malformedEndpoint
        )
    }

    func testSearchProgressPrunesAddressesMissingFromCurrentInventory() async throws {
        let bluetoothFactory = makeFactory()
        bluetoothFactory.replaceDiscovery(
            likelyCandidates: [],
            totalPairedDeviceCount: 2
        )
        let removed = "FE:ED:00:00:00:00"
        let retained = "FE:ED:00:00:00:01"
        bluetoothFactory.enqueueSearchResult(
            AzimuthBluetoothRadioSearchSnapshot(
                provenRadioEndpoints: [],
                likelyRadioEndpoints: [],
                pairedDeviceAddresses: [removed, retained],
                totalPairedDeviceCount: 2,
                probedAddresses: [removed],
                currentProbedAddresses: [removed],
                hasInventorySnapshot: true,
                probedCandidateCount: 1,
                totalUnhintedCandidateCount: 2,
                isComplete: false,
                wasCancelled: false
            )
        )
        bluetoothFactory.enqueueSearchResult(
            AzimuthBluetoothRadioSearchSnapshot(
                provenRadioEndpoints: [],
                likelyRadioEndpoints: [],
                pairedDeviceAddresses: [retained],
                totalPairedDeviceCount: 1,
                probedAddresses: [retained],
                currentProbedAddresses: [retained],
                hasInventorySnapshot: true,
                probedCandidateCount: 1,
                totalUnhintedCandidateCount: 1,
                isComplete: true,
                wasCancelled: false
            )
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: bluetoothFactory
        )
        _ = try await router.availableEndpointSnapshot()

        _ = try await router.findCustomNamedBluetoothRadios()
        let result = try await router.findCustomNamedBluetoothRadios()

        XCTAssertTrue(result.isComplete)
        XCTAssertEqual(result.probedCandidateCount, 1)
        XCTAssertEqual(result.totalUnhintedCandidateCount, 1)
        XCTAssertEqual(
            bluetoothFactory.recordedSearchExclusions,
            [[], [removed]]
        )
    }

    func testCustomSearchPrunesQualifiedEndpointUnpairedBetweenPasses() async throws {
        let bluetoothFactory = makeFactory()
        let removed = AzimuthBluetoothEndpoint(
            address: "10:20:30:40:50:60",
            displayName: "Field D75",
            verifiedCATSerialNumber: "B3B00009"
        )
        let retainedAddress = "10:20:30:40:50:61"
        bluetoothFactory.replaceDiscovery(
            likelyCandidates: [],
            totalPairedDeviceCount: 2,
            pairedDeviceAddresses: [removed.address, retainedAddress]
        )
        bluetoothFactory.enqueueSearchResult(
            AzimuthBluetoothRadioSearchSnapshot(
                provenRadioEndpoints: [removed],
                likelyRadioEndpoints: [],
                pairedDeviceAddresses: [removed.address, retainedAddress],
                totalPairedDeviceCount: 2,
                probedAddresses: [removed.address],
                currentProbedAddresses: [removed.address],
                hasInventorySnapshot: true,
                probedCandidateCount: 1,
                totalUnhintedCandidateCount: 2,
                isComplete: false,
                wasCancelled: false
            )
        )
        bluetoothFactory.enqueueSearchResult(
            AzimuthBluetoothRadioSearchSnapshot(
                provenRadioEndpoints: [],
                likelyRadioEndpoints: [],
                pairedDeviceAddresses: [retainedAddress],
                totalPairedDeviceCount: 1,
                probedAddresses: [retainedAddress],
                currentProbedAddresses: [retainedAddress],
                hasInventorySnapshot: true,
                probedCandidateCount: 1,
                totalUnhintedCandidateCount: 1,
                isComplete: true,
                wasCancelled: false
            )
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: bluetoothFactory
        )
        _ = try await router.availableEndpointSnapshot()

        let first = try await router.findCustomNamedBluetoothRadios()
        let second = try await router.findCustomNamedBluetoothRadios()

        XCTAssertTrue(first.snapshot.endpoints.contains { $0.id == removed.id })
        XCTAssertFalse(second.snapshot.endpoints.contains { $0.id == removed.id })
        XCTAssertEqual(second.snapshot.pairedBluetoothDeviceCount, 1)
        XCTAssertEqual(second.snapshot.likelyBluetoothRadioCount, 0)
    }

    func testCustomSearchPromotesNowLikelyEndpointAndRetainsSerialProof() async throws {
        let bluetoothFactory = makeFactory()
        let proved = AzimuthBluetoothEndpoint(
            address: Self.firstBluetooth.address,
            displayName: "Custom Field Name",
            verifiedCATSerialNumber: "B3B00001"
        )
        let remainingAddress = "10:20:30:40:50:61"
        let refreshedLikely = AzimuthBluetoothEndpoint(
            address: proved.address,
            displayName: "TH-D75 Living Room"
        )
        bluetoothFactory.replaceDiscovery(
            likelyCandidates: [],
            totalPairedDeviceCount: 2,
            pairedDeviceAddresses: [proved.address, remainingAddress]
        )
        bluetoothFactory.enqueueSearchResult(
            AzimuthBluetoothRadioSearchSnapshot(
                provenRadioEndpoints: [proved],
                likelyRadioEndpoints: [],
                pairedDeviceAddresses: [proved.address, remainingAddress],
                totalPairedDeviceCount: 2,
                probedAddresses: [proved.address],
                currentProbedAddresses: [proved.address],
                hasInventorySnapshot: true,
                probedCandidateCount: 1,
                totalUnhintedCandidateCount: 2,
                isComplete: false,
                wasCancelled: false
            )
        )
        bluetoothFactory.enqueueSearchResult(
            AzimuthBluetoothRadioSearchSnapshot(
                provenRadioEndpoints: [],
                likelyRadioEndpoints: [refreshedLikely],
                pairedDeviceAddresses: [proved.address, remainingAddress],
                totalPairedDeviceCount: 2,
                probedAddresses: [remainingAddress],
                currentProbedAddresses: [remainingAddress],
                hasInventorySnapshot: true,
                probedCandidateCount: 1,
                totalUnhintedCandidateCount: 1,
                isComplete: true,
                wasCancelled: false
            )
        )
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: bluetoothFactory
        )
        _ = try await router.availableEndpointSnapshot()
        _ = try await router.findCustomNamedBluetoothRadios()

        let second = try await router.findCustomNamedBluetoothRadios()
        let refreshed = try XCTUnwrap(
            second.snapshot.endpoints.first { $0.id == proved.id }
        )
        XCTAssertEqual(refreshed.name, refreshedLikely.displayName)
        XCTAssertEqual(second.snapshot.likelyBluetoothRadioCount, 1)

        try await router.selectEndpoint(id: proved.id)
        try await router.open()

        XCTAssertEqual(
            bluetoothFactory.qualifiedRequests,
            ["\(proved.address)|B3B00001"]
        )
        await router.close()
    }

    func testBluetoothSelectionRejectsIdentifierAbsentFromRetainedSnapshot() async throws {
        let bluetoothFactory = makeFactory()
        bluetoothFactory.replaceCandidates([Self.firstBluetooth])
        let router = try AzimuthSelectableRadioTransport(
            usbFactory: makeUSBFactory(),
            bluetoothFactory: bluetoothFactory
        )
        _ = try await router.availableEndpointSnapshot()

        let error = try await captureError {
            try await router.selectEndpoint(id: Self.secondBluetooth.id)
        }

        XCTAssertEqual(
            error as? RadioEndpointSelectionError,
            .invalidEndpoint(id: Self.secondBluetooth.id)
        )
        XCTAssertEqual(bluetoothFactory.discoveryCount, 1)
        XCTAssertTrue(bluetoothFactory.exactRequests.isEmpty)
    }

    private func makeFactory(
        firstLink: AzimuthRouterMockBluetoothLink = .init(serialNumber: "B3B00001"),
        secondLink: AzimuthRouterMockBluetoothLink = .init(serialNumber: "B3B00002"),
        matchingLink: AzimuthRouterMockBluetoothLink = .init(
            serialNumber: "B3B00001",
            matchedAddress: firstBluetooth.address
        )
    ) -> AzimuthRouterMockBluetoothFactory {
        AzimuthRouterMockBluetoothFactory(
            candidates: [Self.firstBluetooth, Self.secondBluetooth],
            exactLinks: [
                Self.firstBluetooth.address: firstLink,
                Self.secondBluetooth.address: secondLink,
            ],
            matchingLink: matchingLink
        )
    }

    private func makeUSBFactory(
        endpoint: AzimuthUSBEndpoint = firstUSB,
        transport: AzimuthRouterMockTransport? = nil
    ) -> AzimuthRouterMockUSBFactory {
        let resolvedTransport = transport ?? AzimuthRouterMockTransport(
            device: endpoint.device,
            serialNumber: "B3B00001"
        )
        return AzimuthRouterMockUSBFactory(
            endpoints: [endpoint],
            transports: [endpoint.id: resolvedTransport]
        )
    }

    private func captureError(
        _ operation: () async throws -> Void
    ) async throws -> Error {
        do {
            try await operation()
            throw TestFailure.expectedError
        } catch TestFailure.expectedError {
            throw TestFailure.expectedError
        } catch {
            return error
        }
    }
}

private enum TestFailure: Error {
    case expectedError
}

private enum RouterDiscoveryError: LocalizedError {
    case bluetoothDenied

    var errorDescription: String? {
        "Bluetooth permission was denied."
    }
}
