// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

#if os(macOS)

import Darwin
import Foundation
import XCTest
@testable import Azimuth

final class AzimuthPOSIXUSBDiscoveryTests: XCTestCase {
    func testDiscoveryIncludesOnlySortedVerifiedTHD75CalloutDevices() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: false
        )
        defer { try? FileManager.default.removeItem(at: directory) }
        for name in [
            "cu.usbmodemC",
            "cu.usbmodemB",
            "tty.usbmodemA",
            "cu.Bluetooth",
            "cu.usbmodemA",
        ] {
            XCTAssertTrue(FileManager.default.createFile(
                atPath: directory.appendingPathComponent(name).path,
                contents: Data()
            ))
        }

        let radioA = directory.appendingPathComponent("cu.usbmodemA").path
        let unrelatedB = directory.appendingPathComponent("cu.usbmodemB").path
        let radioC = directory.appendingPathComponent("cu.usbmodemC").path
        let verified = POSIXAzimuthUSBSerialLink.availableDevicePaths(
            directory: directory.path,
            identityProvider: { path in
                if path == radioA || path == radioC {
                    return .init(
                        vendorID: POSIXAzimuthUSBSerialLink.thD75VendorID,
                        productID: POSIXAzimuthUSBSerialLink.thD75ProductID
                    )
                }
                if path == unrelatedB {
                    return .init(vendorID: 0x2341, productID: 0x0043)
                }
                return nil
            }
        )

        XCTAssertEqual(verified, [radioA, radioC])
    }

    func testImplicitSelectionRejectsMultipleVerifiedRadios() throws {
        let paths = ["/dev/cu.usbmodemA", "/dev/cu.usbmodemB"]

        XCTAssertThrowsError(
            try POSIXAzimuthUSBSerialLink.selectDevicePath(
                requestedPath: nil,
                verifiedPaths: paths
            )
        ) { error in
            guard case AzimuthUSBLinkError.ambiguousDevices(let found) = error else {
                return XCTFail("unexpected error: \(error)")
            }
            XCTAssertEqual(found, paths)
        }
        XCTAssertEqual(
            try POSIXAzimuthUSBSerialLink.selectDevicePath(
                requestedPath: paths[1],
                verifiedPaths: paths
            ),
            paths[1]
        )
        XCTAssertThrowsError(
            try POSIXAzimuthUSBSerialLink.selectDevicePath(
                requestedPath: "/dev/cu.usbmodem-unverified",
                verifiedPaths: paths
            )
        ) { error in
            XCTAssertEqual(error as? AzimuthUSBLinkError, .serviceNotFound)
        }
        XCTAssertThrowsError(
            try POSIXAzimuthUSBSerialLink.selectDevicePath(
                requestedPath: nil,
                verifiedPaths: []
            )
        ) { error in
            XCTAssertEqual(error as? AzimuthUSBLinkError, .serviceNotFound)
        }
    }

    func testOpenedDeviceBindingIncludesNodeIdentityAndCharacterType() {
        let character = POSIXAzimuthUSBSerialLink.openedDeviceNode(
            device: 7,
            inode: 11,
            rawDevice: 13,
            mode: UInt16(S_IFCHR) | 0o600
        )
        let replacement = POSIXAzimuthUSBSerialLink.openedDeviceNode(
            device: 7,
            inode: 12,
            rawDevice: 14,
            mode: UInt16(S_IFCHR) | 0o600
        )

        XCTAssertTrue(character.isCharacterDevice)
        XCTAssertNotEqual(character, replacement)
        XCTAssertFalse(
            POSIXAzimuthUSBSerialLink.openedDeviceNode(
                device: 7,
                inode: 11,
                rawDevice: 13,
                mode: UInt16(S_IFREG) | 0o600
            ).isCharacterDevice
        )
    }

    func testOpenedDeviceBindingAcceptsHighBitDarwinDeviceIdentifier() {
        var status = stat()
        status.st_dev = Int32(bitPattern: 0xE1A0_B96C)
        status.st_ino = 1_469
        status.st_rdev = 150_994_951
        status.st_mode = UInt16(S_IFCHR) | 0o600

        let node = POSIXAzimuthUSBSerialLink.openedDeviceNode(status)

        XCTAssertEqual(node.device, status.st_dev)
        XCTAssertEqual(node.inode, status.st_ino)
        XCTAssertEqual(node.rawDevice, status.st_rdev)
        XCTAssertTrue(node.isCharacterDevice)
    }

    func testTHD75QualificationUsesBothUSBIdentifiers() {
        let exact = POSIXAzimuthUSBSerialLink.USBIdentity(
            vendorID: POSIXAzimuthUSBSerialLink.thD75VendorID,
            productID: POSIXAzimuthUSBSerialLink.thD75ProductID,
            serialNumber: "C0000001"
        )
        XCTAssertTrue(POSIXAzimuthUSBSerialLink.isTHD75(exact))
        XCTAssertFalse(POSIXAzimuthUSBSerialLink.isTHD75(.init(
            vendorID: exact.vendorID,
            productID: 0x0001,
            serialNumber: exact.serialNumber
        )))
        XCTAssertFalse(POSIXAzimuthUSBSerialLink.isTHD75(.init(
            vendorID: 0x0001,
            productID: exact.productID,
            serialNumber: exact.serialNumber
        )))
    }

    func testUSBEndpointIdentityUsesDescriptorSerialWithPathFallback() {
        XCTAssertEqual(
            AzimuthUSBEndpoint.stableID(
                devicePath: "/dev/cu.usbmodem101",
                usbSerialNumber: "C0000001"
            ),
            "usb:serial:C0000001"
        )
        XCTAssertEqual(
            AzimuthUSBEndpoint.stableID(
                devicePath: "/dev/cu.usbmodem101",
                usbSerialNumber: nil
            ),
            "tty:/dev/cu.usbmodem101"
        )
    }

    func testOpenBindingPublishesOnlyStableRegistryAndNodeIdentity() throws {
        let fixture = POSIXOpenFixture()

        let opened = try POSIXAzimuthUSBSerialLink.openAndBindDevice(
            requestedPath: nil,
            access: fixture.access()
        )

        XCTAssertEqual(opened.fileDescriptor, fixture.fileDescriptor)
        XCTAssertEqual(opened.path, fixture.path)
        XCTAssertEqual(opened.identity.serialNumber, "C0000001")
        XCTAssertEqual(opened.usbDeviceRegistryEntryID, 0xCAFE)
        XCTAssertEqual(fixture.configureCallCount, 1)
        XCTAssertTrue(fixture.closedFileDescriptors.isEmpty)
    }

    func testOpenBindingAcceptsTHD75WithoutDescriptorSerial() throws {
        let fixture = POSIXOpenFixture()
        let anonymous = POSIXAzimuthUSBSerialLink.RegisteredUSBDevice(
            identity: .init(
                vendorID: POSIXAzimuthUSBSerialLink.thD75VendorID,
                productID: POSIXAzimuthUSBSerialLink.thD75ProductID,
                serialNumber: nil
            ),
            usbDeviceRegistryEntryID: 0xCAFE
        )
        fixture.registeredDevices = [anonymous, anonymous]

        let opened = try POSIXAzimuthUSBSerialLink.openAndBindDevice(
            requestedPath: fixture.path,
            access: fixture.access()
        )

        XCTAssertNil(opened.identity.serialNumber)
        XCTAssertEqual(opened.usbDeviceRegistryEntryID, 0xCAFE)
        XCTAssertEqual(fixture.configureCallCount, 1)
        XCTAssertTrue(fixture.closedFileDescriptors.isEmpty)
    }

    func testExactEndpointRejectsDifferentRadioAtReusedPathBeforeOpen() {
        let fixture = POSIXOpenFixture()
        fixture.registeredDevices = [
            .init(
                identity: .init(
                    vendorID: POSIXAzimuthUSBSerialLink.thD75VendorID,
                    productID: POSIXAzimuthUSBSerialLink.thD75ProductID,
                    serialNumber: "C0000002"
                ),
                usbDeviceRegistryEntryID: 0xBEEF
            ),
        ]
        let link = POSIXAzimuthUSBSerialLink(
            devicePath: fixture.path,
            expectedSerialNumber: "C0000001",
            openSystemAccess: fixture.access()
        )

        XCTAssertThrowsError(try link.open()) { error in
            XCTAssertEqual(
                error as? AzimuthUSBLinkError,
                .openedDeviceSerialMismatch(
                    path: fixture.path,
                    expected: "C0000001",
                    actual: "C0000002"
                )
            )
        }
        XCTAssertEqual(fixture.openFileDescriptorCallCount, 0)
        XCTAssertEqual(fixture.configureCallCount, 0)
        XCTAssertTrue(fixture.closedFileDescriptors.isEmpty)
    }

    func testOpenBindingRejectsRegistryEntrySwapAndClosesDescriptor() {
        let fixture = POSIXOpenFixture()
        fixture.registeredDevices[1] = .init(
            identity: fixture.identity,
            usbDeviceRegistryEntryID: 0xBEEF
        )

        assertUnstableIdentity(fixture)
    }

    func testOpenBindingRejectsDeviceNodeSwapAndClosesDescriptor() {
        let fixture = POSIXOpenFixture()
        fixture.pathNodes[1] = POSIXAzimuthUSBSerialLink.openedDeviceNode(
            device: 7,
            inode: 99,
            rawDevice: 101,
            mode: UInt16(S_IFCHR) | 0o600
        )

        assertUnstableIdentity(fixture)
    }

    func testOpenBindingRejectsNonCharacterDescriptorAndClosesIt() {
        let fixture = POSIXOpenFixture()
        let regular = POSIXAzimuthUSBSerialLink.openedDeviceNode(
            device: 7,
            inode: 11,
            rawDevice: 13,
            mode: UInt16(S_IFREG) | 0o600
        )
        fixture.fileDescriptorNode = regular
        fixture.pathNodes = [regular, regular]

        assertUnstableIdentity(fixture)
    }

    func testOpenBindingClosesDescriptorWhenConfigurationFails() {
        let fixture = POSIXOpenFixture()
        fixture.configurationError = .systemCall(
            operation: "test configure",
            code: EIO
        )

        XCTAssertThrowsError(
            try POSIXAzimuthUSBSerialLink.openAndBindDevice(
                requestedPath: nil,
                access: fixture.access()
            )
        ) { error in
            XCTAssertEqual(
                error as? AzimuthUSBLinkError,
                fixture.configurationError
            )
        }
        XCTAssertEqual(fixture.closedFileDescriptors, [fixture.fileDescriptor])
    }

    func testOpenedSerialIsCachedUntilCloseAndThenCleared() throws {
        var descriptors = [Int32](repeating: -1, count: 2)
        XCTAssertEqual(Darwin.pipe(&descriptors), 0)
        let fixture = POSIXOpenFixture(fileDescriptor: descriptors[0])
        fixture.closeRealFileDescriptor = true
        let link = POSIXAzimuthUSBSerialLink(openSystemAccess: fixture.access())
        defer {
            link.close()
            _ = Darwin.close(descriptors[1])
        }

        XCTAssertNil(link.hardwareSerialNumber)
        XCTAssertNil(link.macOSUSBDeviceRegistryEntryID)
        try link.open()
        XCTAssertEqual(link.hardwareSerialNumber, "C0000001")
        XCTAssertEqual(link.macOSUSBDeviceRegistryEntryID, 0xCAFE)

        // A pathname lookup after open must never replace the cached identity.
        fixture.registeredDevices = [.init(
            identity: .init(
                vendorID: POSIXAzimuthUSBSerialLink.thD75VendorID,
                productID: POSIXAzimuthUSBSerialLink.thD75ProductID,
                serialNumber: "C0000002"
            ),
            usbDeviceRegistryEntryID: 0xBEEF
        )]
        XCTAssertEqual(link.hardwareSerialNumber, "C0000001")
        XCTAssertEqual(link.macOSUSBDeviceRegistryEntryID, 0xCAFE)

        link.close()
        XCTAssertEqual(fixture.descriptorClosed.wait(timeout: .now() + 1), .success)
        XCTAssertNil(link.hardwareSerialNumber)
        XCTAssertNil(link.macOSUSBDeviceRegistryEntryID)
        XCTAssertEqual(fixture.closedFileDescriptors, [descriptors[0]])
    }

    func testOpenBindingRejectsZeroUSBDeviceRegistryIdentity() {
        let fixture = POSIXOpenFixture()
        fixture.registeredDevices[0] = .init(
            identity: fixture.identity,
            usbDeviceRegistryEntryID: 0
        )

        XCTAssertThrowsError(
            try POSIXAzimuthUSBSerialLink.openAndBindDevice(
                requestedPath: nil,
                access: fixture.access()
            )
        ) { error in
            XCTAssertEqual(
                error as? AzimuthUSBLinkError,
                .openedDeviceIdentityUnstable(fixture.path)
            )
        }
        XCTAssertEqual(fixture.openFileDescriptorCallCount, 0)
    }

    func testReadabilityEventRejectsReusedDescriptorFromOlderGeneration() {
        XCTAssertTrue(POSIXAzimuthUSBSerialLink.eventMatchesCurrentSession(
            eventFileDescriptor: 42,
            eventGeneration: 7,
            currentFileDescriptor: 42,
            currentGeneration: 7
        ))
        XCTAssertFalse(POSIXAzimuthUSBSerialLink.eventMatchesCurrentSession(
            eventFileDescriptor: 42,
            eventGeneration: 7,
            currentFileDescriptor: 42,
            currentGeneration: 8
        ))
        XCTAssertFalse(POSIXAzimuthUSBSerialLink.eventMatchesCurrentSession(
            eventFileDescriptor: 41,
            eventGeneration: 7,
            currentFileDescriptor: 42,
            currentGeneration: 7
        ))
    }

    func testCloseQuiescesOldReadSourceBeforeReopenTouchesTTY() throws {
        let fixture = try POSIXLifecycleFixture()
        let eventQueue = DispatchQueue(label: "org.swiftraccoon.azimuth.posix-usb.race-test")
        eventQueue.suspend()
        var eventQueueIsSuspended = true
        let link = POSIXAzimuthUSBSerialLink(
            openSystemAccess: fixture.access(),
            eventQueue: eventQueue
        )
        defer {
            if eventQueueIsSuspended { eventQueue.resume() }
            link.close()
            fixture.closeWriteDescriptors()
        }

        try link.open()
        let oldDoorbell = POSIXDoorbellRecorder()
        try link.armDoorbell { hasData in
            oldDoorbell.record(hasData)
        }
        try fixture.writeToSession(0, byte: 0x11)

        link.close()
        XCTAssertEqual(oldDoorbell.fired.wait(timeout: .now() + 1), .success)
        XCTAssertEqual(oldDoorbell.values, [false])
        XCTAssertEqual(fixture.closedFileDescriptors, [])

        let reopenStarted = DispatchSemaphore(value: 0)
        let reopenFinished = DispatchSemaphore(value: 0)
        let reopenResult = POSIXReopenResult()
        DispatchQueue.global().async {
            reopenStarted.signal()
            do {
                try link.open()
            } catch {
                reopenResult.record(error)
            }
            reopenFinished.signal()
        }
        XCTAssertEqual(reopenStarted.wait(timeout: .now() + 1), .success)
        XCTAssertEqual(reopenFinished.wait(timeout: .now() + 0.1), .timedOut)
        XCTAssertEqual(fixture.openedFileDescriptors.count, 1)
        XCTAssertEqual(fixture.closedFileDescriptors, [])

        eventQueue.resume()
        eventQueueIsSuspended = false
        XCTAssertEqual(reopenFinished.wait(timeout: .now() + 1), .success)
        XCTAssertNil(reopenResult.error)
        XCTAssertEqual(
            fixture.lifecycleEvents,
            [
                .opened(fixture.readFileDescriptors[0]),
                .closed(fixture.readFileDescriptors[0]),
                .opened(fixture.readFileDescriptors[1]),
            ]
        )

        let currentDoorbell = POSIXDoorbellRecorder()
        try link.armDoorbell { hasData in
            currentDoorbell.record(hasData)
        }
        try fixture.writeToSession(1, byte: 0x22)
        XCTAssertEqual(currentDoorbell.fired.wait(timeout: .now() + 1), .success)
        XCTAssertEqual(currentDoorbell.values, [true])

        link.close()
        XCTAssertEqual(fixture.descriptorClosed.wait(timeout: .now() + 1), .success)
        XCTAssertEqual(fixture.descriptorClosed.wait(timeout: .now() + 1), .success)
        XCTAssertEqual(
            fixture.closedFileDescriptors,
            fixture.readFileDescriptors
        )
    }

    private func assertUnstableIdentity(
        _ fixture: POSIXOpenFixture,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        XCTAssertThrowsError(
            try POSIXAzimuthUSBSerialLink.openAndBindDevice(
                requestedPath: nil,
                access: fixture.access()
            ),
            file: file,
            line: line
        ) { error in
            XCTAssertEqual(
                error as? AzimuthUSBLinkError,
                .openedDeviceIdentityUnstable(fixture.path),
                file: file,
                line: line
            )
        }
        XCTAssertEqual(
            fixture.closedFileDescriptors,
            [fixture.fileDescriptor],
            file: file,
            line: line
        )
        XCTAssertEqual(fixture.configureCallCount, 0, file: file, line: line)
    }
}

private final class POSIXOpenFixture {
    let path = "/dev/cu.usbmodem-test"
    let identity = POSIXAzimuthUSBSerialLink.USBIdentity(
        vendorID: POSIXAzimuthUSBSerialLink.thD75VendorID,
        productID: POSIXAzimuthUSBSerialLink.thD75ProductID,
        serialNumber: "C0000001"
    )
    let fileDescriptor: Int32
    var registeredDevices: [POSIXAzimuthUSBSerialLink.RegisteredUSBDevice?]
    var fileDescriptorNode: POSIXAzimuthUSBSerialLink.OpenedDeviceNode
    var pathNodes: [POSIXAzimuthUSBSerialLink.OpenedDeviceNode]
    var configurationError: AzimuthUSBLinkError?
    var openFileDescriptorCallCount = 0
    var configureCallCount = 0
    var closedFileDescriptors: [Int32] = []
    var closeRealFileDescriptor = false
    let descriptorClosed = DispatchSemaphore(value: 0)

    init(fileDescriptor: Int32 = 42) {
        self.fileDescriptor = fileDescriptor
        let node = POSIXAzimuthUSBSerialLink.openedDeviceNode(
            device: 7,
            inode: 11,
            rawDevice: 13,
            mode: UInt16(S_IFCHR) | 0o600
        )
        fileDescriptorNode = node
        pathNodes = [node, node]
        let registered = POSIXAzimuthUSBSerialLink.RegisteredUSBDevice(
            identity: identity,
            usbDeviceRegistryEntryID: 0xCAFE
        )
        registeredDevices = [registered, registered]
    }

    func access() -> POSIXAzimuthUSBSerialLink.OpenSystemAccess {
        .init(
            availableDevicePaths: { [self] in [path] },
            registeredUSBDevice: { _ in
                guard !self.registeredDevices.isEmpty else { return nil }
                return self.registeredDevices.removeFirst()
            },
            openFileDescriptor: { _ in
                self.openFileDescriptorCallCount += 1
                return self.fileDescriptor
            },
            nodeForFileDescriptor: { _ in self.fileDescriptorNode },
            nodeForPath: { _ in
                guard !self.pathNodes.isEmpty else {
                    throw AzimuthUSBLinkError.openedDeviceIdentityUnstable(self.path)
                }
                return self.pathNodes.removeFirst()
            },
            configure: { _ in
                self.configureCallCount += 1
                if let configurationError = self.configurationError {
                    throw configurationError
                }
            },
            closeFileDescriptor: { descriptor in
                self.closedFileDescriptors.append(descriptor)
                if self.closeRealFileDescriptor { _ = Darwin.close(descriptor) }
                self.descriptorClosed.signal()
            }
        )
    }
}

private final class POSIXReopenResult: @unchecked Sendable {
    private let lock = NSLock()
    private var recordedError: Error?

    var error: Error? {
        lock.withLock { recordedError }
    }

    func record(_ error: Error) {
        lock.withLock { recordedError = error }
    }
}

private final class POSIXDoorbellRecorder: @unchecked Sendable {
    let fired = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var recordedValues: [Bool] = []

    var values: [Bool] {
        lock.withLock { recordedValues }
    }

    func record(_ value: Bool) {
        lock.withLock { recordedValues.append(value) }
        fired.signal()
    }
}

private final class POSIXLifecycleFixture: @unchecked Sendable {
    enum Event: Equatable {
        case opened(Int32)
        case closed(Int32)
    }

    let path = "/dev/cu.usbmodem-race-test"
    let identity = POSIXAzimuthUSBSerialLink.USBIdentity(
        vendorID: POSIXAzimuthUSBSerialLink.thD75VendorID,
        productID: POSIXAzimuthUSBSerialLink.thD75ProductID,
        serialNumber: "C0000001"
    )
    let readFileDescriptors: [Int32]
    let writeFileDescriptors: [Int32]
    let descriptorClosed = DispatchSemaphore(value: 0)

    private let lock = NSLock()
    private var remainingReadFileDescriptors: [Int32]
    private var recordedOpenedFileDescriptors: [Int32] = []
    private var recordedClosedFileDescriptors: [Int32] = []
    private var recordedLifecycleEvents: [Event] = []
    private var writeDescriptorsAreClosed = false

    init() throws {
        var first = [Int32](repeating: -1, count: 2)
        guard Darwin.pipe(&first) == 0 else {
            throw AzimuthUSBLinkError.systemCall(operation: "create first test pipe", code: errno)
        }
        var second = [Int32](repeating: -1, count: 2)
        guard Darwin.pipe(&second) == 0 else {
            _ = Darwin.close(first[0])
            _ = Darwin.close(first[1])
            throw AzimuthUSBLinkError.systemCall(operation: "create second test pipe", code: errno)
        }
        readFileDescriptors = [first[0], second[0]]
        writeFileDescriptors = [first[1], second[1]]
        remainingReadFileDescriptors = readFileDescriptors
    }

    var openedFileDescriptors: [Int32] {
        lock.withLock { recordedOpenedFileDescriptors }
    }

    var closedFileDescriptors: [Int32] {
        lock.withLock { recordedClosedFileDescriptors }
    }

    var lifecycleEvents: [Event] {
        lock.withLock { recordedLifecycleEvents }
    }

    func access() -> POSIXAzimuthUSBSerialLink.OpenSystemAccess {
        let node = POSIXAzimuthUSBSerialLink.openedDeviceNode(
            device: 7,
            inode: 11,
            rawDevice: 13,
            mode: UInt16(S_IFCHR) | 0o600
        )
        let registered = POSIXAzimuthUSBSerialLink.RegisteredUSBDevice(
            identity: identity,
            usbDeviceRegistryEntryID: 0xCAFE
        )
        return .init(
            availableDevicePaths: { [path] in [path] },
            registeredUSBDevice: { _ in registered },
            openFileDescriptor: { [self] _ in
                try lock.withLock {
                    guard !remainingReadFileDescriptors.isEmpty else {
                        throw AzimuthUSBLinkError.systemCall(
                            operation: "open exhausted test descriptor",
                            code: EMFILE
                        )
                    }
                    let descriptor = remainingReadFileDescriptors.removeFirst()
                    recordedOpenedFileDescriptors.append(descriptor)
                    recordedLifecycleEvents.append(.opened(descriptor))
                    return descriptor
                }
            },
            nodeForFileDescriptor: { _ in node },
            nodeForPath: { _ in node },
            configure: { _ in },
            closeFileDescriptor: { [self] descriptor in
                _ = Darwin.close(descriptor)
                lock.withLock {
                    recordedClosedFileDescriptors.append(descriptor)
                    recordedLifecycleEvents.append(.closed(descriptor))
                }
                descriptorClosed.signal()
            }
        )
    }

    func writeToSession(_ index: Int, byte: UInt8) throws {
        var byte = byte
        let result = withUnsafePointer(to: &byte) {
            Darwin.write(writeFileDescriptors[index], $0, 1)
        }
        guard result == 1 else {
            throw AzimuthUSBLinkError.systemCall(operation: "write test pipe", code: errno)
        }
    }

    func closeWriteDescriptors() {
        let descriptors: [Int32] = lock.withLock {
            guard !writeDescriptorsAreClosed else { return [] }
            writeDescriptorsAreClosed = true
            return writeFileDescriptors
        }
        for descriptor in descriptors { _ = Darwin.close(descriptor) }
    }
}

#endif
