// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

#if os(macOS)

import Darwin
import Dispatch
import Foundation
import IOKit

/// Native macOS transport through Apple's public CDC ACM tty service.
/// No dext or private framework is needed on macOS: the TH-D75 appears as a
/// `/dev/cu.usbmodem*` device owned by the system USB serial driver.
public final class POSIXAzimuthUSBSerialLink: AzimuthUSBSerialLink, @unchecked Sendable {
    public static let deviceDirectory = "/dev"
    public static let calloutPrefix = "cu.usbmodem"
    static let thD75VendorID: UInt16 = 0x2166
    static let thD75ProductID: UInt16 = 0x9023

    struct USBIdentity: Equatable {
        let vendorID: UInt16
        let productID: UInt16
        let serialNumber: String?

        init(vendorID: UInt16, productID: UInt16, serialNumber: String? = nil) {
            self.vendorID = vendorID
            self.productID = productID
            self.serialNumber = serialNumber
        }
    }

    struct RegisteredUSBDevice: Equatable {
        let identity: USBIdentity
        let usbDeviceRegistryEntryID: UInt64
    }

    struct AvailableDeviceDescriptor: Equatable, Sendable {
        let path: String
        let serialNumber: String?
        let usbDeviceRegistryEntryID: UInt64
    }

    struct OpenedDeviceNode: Equatable {
        let device: dev_t
        let inode: ino_t
        let rawDevice: dev_t
        let mode: mode_t

        var isCharacterDevice: Bool {
            mode & UInt16(S_IFMT) == UInt16(S_IFCHR)
        }
    }

    struct BoundUSBDevice: Equatable {
        let fileDescriptor: Int32
        let path: String
        let identity: USBIdentity
        let usbDeviceRegistryEntryID: UInt64
    }

    struct OpenSystemAccess {
        let availableDevicePaths: () -> [String]
        let registeredUSBDevice: (String) -> RegisteredUSBDevice?
        let openFileDescriptor: (String) throws -> Int32
        let nodeForFileDescriptor: (Int32) throws -> OpenedDeviceNode
        let nodeForPath: (String) throws -> OpenedDeviceNode
        let configure: (Int32) throws -> Void
        let closeFileDescriptor: (Int32) -> Void
    }

    /// Owns one read source's exact descriptor until Dispatch has finished all
    /// event delivery for that source. Keeping this separate from the link's
    /// current descriptor prevents an old source from closing a reused fd.
    private final class ReadabilitySession: @unchecked Sendable {
        let fileDescriptor: Int32
        let generation: UInt64

        private let closeFileDescriptor: (Int32) -> Void
        private let cancellationCompletion = DispatchGroup()

        init(
            fileDescriptor: Int32,
            generation: UInt64,
            closeFileDescriptor: @escaping (Int32) -> Void
        ) {
            self.fileDescriptor = fileDescriptor
            self.generation = generation
            self.closeFileDescriptor = closeFileDescriptor
            cancellationCompletion.enter()
        }

        var cancellationHasCompleted: Bool {
            cancellationCompletion.wait(timeout: .now()) == .success
        }

        func finishCancellation() {
            closeFileDescriptor(fileDescriptor)
            cancellationCompletion.leave()
        }

        func waitForCancellation() {
            cancellationCompletion.wait()
        }
    }

    private struct ReadabilitySourceSession {
        let source: DispatchSourceRead
        let session: ReadabilitySession
    }

    private struct ArmedDoorbell {
        let fileDescriptor: Int32
        let generation: UInt64
        let callback: @Sendable (Bool) -> Void
    }

    private let requestedPath: String?
    /// Stable USB descriptor serial captured by endpoint discovery. When set,
    /// opening the reusable tty pathname is permitted only while it still
    /// resolves to that same physical radio.
    private let expectedSerialNumber: String?
    private let openSystemAccess: OpenSystemAccess
    private let operationLock = NSLock()
    private let lock = NSLock()
    private let eventQueue: DispatchQueue
    private let eventQueueKey = DispatchSpecificKey<Bool>()
    private var fileDescriptor: Int32 = -1
    private var connectionGeneration: UInt64 = 0
    private var openedPath: String?
    /// Identity bound during `open()` to the exact device node held by
    /// `fileDescriptor`. Never re-resolve this from a reusable tty pathname.
    private var openedIdentity: USBIdentity?
    /// Current IORegistry identity of the physical USB-device ancestor shared
    /// by this CDC interface and the radio's CoreAudio interface.
    ///
    /// Registry entry IDs are valid only for the current enumeration, so this
    /// value is published only while the exact bound descriptor remains open.
    private var openedUSBDeviceRegistryEntryID: UInt64?
    private var readabilitySourceSession: ReadabilitySourceSession?
    /// A cancelled source remains retained until its handler has closed the
    /// old descriptor. The next `open()` waits on this barrier before touching
    /// the tty path or asserting DTR/RTS for a new session.
    private var retiringReadabilitySourceSession: ReadabilitySourceSession?
    private var doorbell: ArmedDoorbell?

    /// Pass an exact verified TH-D75 callout path when more than one radio is
    /// attached. Nil is accepted only when discovery finds exactly one radio.
    public init(
        devicePath: String? = nil,
        expectedSerialNumber: String? = nil
    ) {
        requestedPath = devicePath
        self.expectedSerialNumber = expectedSerialNumber
        openSystemAccess = Self.productionOpenSystemAccess
        eventQueue = DispatchQueue(label: "org.swiftraccoon.azimuth.posix-usb")
        eventQueue.setSpecific(key: eventQueueKey, value: true)
    }

    init(
        devicePath: String? = nil,
        expectedSerialNumber: String? = nil,
        openSystemAccess: OpenSystemAccess,
        eventQueue: DispatchQueue = DispatchQueue(
            label: "org.swiftraccoon.azimuth.posix-usb.test"
        )
    ) {
        requestedPath = devicePath
        self.expectedSerialNumber = expectedSerialNumber
        self.openSystemAccess = openSystemAccess
        self.eventQueue = eventQueue
        eventQueue.setSpecific(key: eventQueueKey, value: true)
    }

    public var connectionDescription: String {
        lock.lock()
        defer { lock.unlock() }
        return openedPath ?? requestedPath ?? "macOS USB CDC"
    }

    public var hardwareSerialNumber: String? {
        lock.lock()
        defer { lock.unlock() }
        return openedIdentity?.serialNumber
    }

    public var macOSUSBDeviceRegistryEntryID: UInt64? {
        lock.lock()
        defer { lock.unlock() }
        return openedUSBDeviceRegistryEntryID
    }

    public func servicePresent() -> Bool {
        let verifiedPaths = openSystemAccess.availableDevicePaths()
        if let requestedPath {
            return verifiedPaths.contains(requestedPath)
        }
        return !verifiedPaths.isEmpty
    }

    public func open() throws {
        operationLock.lock()
        defer { operationLock.unlock() }
        try waitForRetiringReadabilitySource()
        lock.lock()
        defer { lock.unlock() }
        guard fileDescriptor < 0 else { return }

        let opened = try Self.openAndBindDevice(
            requestedPath: requestedPath,
            expectedSerialNumber: expectedSerialNumber,
            access: openSystemAccess
        )

        connectionGeneration &+= 1
        let session = ReadabilitySession(
            fileDescriptor: opened.fileDescriptor,
            generation: connectionGeneration,
            closeFileDescriptor: openSystemAccess.closeFileDescriptor
        )
        let source = DispatchSource.makeReadSource(
            fileDescriptor: opened.fileDescriptor,
            queue: eventQueue
        )
        source.setEventHandler { [weak self, session] in
            self?.readabilityChanged(
                fileDescriptor: session.fileDescriptor,
                generation: session.generation
            )
        }
        source.setCancelHandler { [session] in
            session.finishCancellation()
        }
        fileDescriptor = opened.fileDescriptor
        openedPath = opened.path
        openedIdentity = opened.identity
        openedUSBDeviceRegistryEntryID = opened.usbDeviceRegistryEntryID
        readabilitySourceSession = ReadabilitySourceSession(
            source: source,
            session: session
        )
        source.resume()
    }

    public func close() {
        operationLock.lock()
        defer { operationLock.unlock() }
        lock.lock()
        let sourceSession = readabilitySourceSession
        let pendingDoorbell = doorbell?.callback
        connectionGeneration &+= 1
        fileDescriptor = -1
        openedPath = nil
        openedIdentity = nil
        openedUSBDeviceRegistryEntryID = nil
        readabilitySourceSession = nil
        if let sourceSession {
            precondition(retiringReadabilitySourceSession == nil)
            retiringReadabilitySourceSession = sourceSession
        }
        doorbell = nil
        lock.unlock()

        // The cancel handler, and only the cancel handler, owns and closes the
        // exact descriptor captured by this source. `open()` waits for it.
        sourceSession?.source.cancel()
        pendingDoorbell?(false)
    }

    public func write(_ bytes: [UInt8]) throws {
        operationLock.lock()
        defer { operationLock.unlock() }
        guard (1...AzimuthUSBABIV1.maximumTransferBytes).contains(bytes.count) else {
            throw AzimuthUSBLinkError.invalidTransferLength(bytes.count)
        }
        let opened = try currentFileDescriptor()
        var offset = 0
        let deadline = DispatchTime.now().uptimeNanoseconds + 200_000_000

        try bytes.withUnsafeBytes { rawBuffer in
            guard let base = rawBuffer.baseAddress else {
                throw AzimuthUSBLinkError.invalidTransferLength(0)
            }
            while offset < rawBuffer.count {
                let result = Darwin.write(
                    opened,
                    base.advanced(by: offset),
                    rawBuffer.count - offset
                )
                if result > 0 {
                    offset += result
                    continue
                }
                if result < 0, errno == EINTR { continue }
                if result < 0, errno == EAGAIN || errno == EWOULDBLOCK {
                    let now = DispatchTime.now().uptimeNanoseconds
                    guard now < deadline else {
                        if offset == 0 { throw AzimuthUSBLinkError.backpressure }
                        throw AzimuthUSBLinkError.systemCall(
                            operation: "partial USB serial write timeout",
                            code: ETIMEDOUT
                        )
                    }
                    let remainingMilliseconds = Int32(
                        min((deadline - now) / 1_000_000, UInt64(Int32.max))
                    )
                    var descriptor = pollfd(
                        fd: opened,
                        events: Int16(POLLOUT),
                        revents: 0
                    )
                    let pollResult = Darwin.poll(&descriptor, 1, remainingMilliseconds)
                    if pollResult > 0 { continue }
                    if pollResult < 0, errno == EINTR { continue }
                    if pollResult == 0, offset == 0 {
                        throw AzimuthUSBLinkError.backpressure
                    }
                    throw AzimuthUSBLinkError.systemCall(
                        operation: "poll USB serial write",
                        code: pollResult == 0 ? ETIMEDOUT : errno
                    )
                }
                throw AzimuthUSBLinkError.systemCall(
                    operation: "write USB serial",
                    code: errno
                )
            }
        }
    }

    public func setBaudRate(baud: UInt32) throws {
        operationLock.lock()
        defer { operationLock.unlock() }
        guard AzimuthUSBABIV2.supportedBaudRates.contains(baud) else {
            throw AzimuthUSBLinkError.unsupportedBaudRate(baud)
        }
        let opened = try currentFileDescriptor()
        var settings = termios()
        guard tcgetattr(opened, &settings) == 0 else {
            throw AzimuthUSBLinkError.systemCall(operation: "tcgetattr", code: errno)
        }
        let speed: speed_t = baud == 9_600 ? speed_t(B9600) : speed_t(B115200)
        guard cfsetspeed(&settings, speed) == 0 else {
            throw AzimuthUSBLinkError.systemCall(operation: "cfsetspeed", code: errno)
        }
        guard tcsetattr(opened, TCSANOW, &settings) == 0 else {
            throw AzimuthUSBLinkError.systemCall(operation: "tcsetattr", code: errno)
        }
    }

    public func drain(maxBytes: Int) throws -> [UInt8] {
        operationLock.lock()
        defer { operationLock.unlock() }
        guard maxBytes > 0 else { throw AzimuthUSBLinkError.invalidTransferLength(maxBytes) }
        let opened = try currentFileDescriptor()
        var bytes = [UInt8](
            repeating: 0,
            count: min(maxBytes, AzimuthUSBABIV1.maximumTransferBytes)
        )
        let result = bytes.withUnsafeMutableBytes { rawBuffer in
            Darwin.read(opened, rawBuffer.baseAddress, rawBuffer.count)
        }
        if result > 0 {
            bytes.removeLast(bytes.count - result)
            return bytes
        }
        if result < 0, errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK {
            return []
        }
        // A zero-length tty read means EOF/hangup, not an empty nonblocking
        // queue (that case is EAGAIN). Surface it so parked reads are woken.
        throw AzimuthUSBLinkError.systemCall(
            operation: "read USB serial",
            code: result == 0 ? ENXIO : errno
        )
    }

    public func armDoorbell(onFire: @escaping @Sendable (Bool) -> Void) throws {
        operationLock.lock()
        defer { operationLock.unlock() }
        lock.lock()
        guard fileDescriptor >= 0 else {
            lock.unlock()
            throw AzimuthUSBLinkError.notOpen
        }
        guard doorbell == nil else {
            lock.unlock()
            throw AzimuthUSBLinkError.systemCall(
                operation: "arm duplicate USB serial doorbell",
                code: EBUSY
            )
        }
        let opened = fileDescriptor
        let generation = connectionGeneration
        doorbell = ArmedDoorbell(
            fileDescriptor: opened,
            generation: generation,
            callback: onFire
        )
        lock.unlock()

        // Dispatch sources are level-triggered, but an explicit readiness
        // probe makes the "arm while data is pending fires now" contract
        // unambiguous and identical to the dext implementation.
        var descriptor = pollfd(
            fd: opened,
            events: Int16(POLLIN | POLLERR | POLLHUP),
            revents: 0
        )
        if Darwin.poll(&descriptor, 1, 0) > 0 {
            consumeDoorbell(
                dataAvailable: true,
                fileDescriptor: opened,
                generation: generation
            )
        }
    }

    public static func availableDevicePaths(
        directory: String = deviceDirectory
    ) -> [String] {
        availableDeviceDescriptors(directory: directory).map(\.path)
    }

    static func availableDeviceDescriptors(
        directory: String = deviceDirectory
    ) -> [AvailableDeviceDescriptor] {
        let names = (try? FileManager.default.contentsOfDirectory(
            atPath: directory
        )) ?? []
        return names
            .filter { $0.hasPrefix(calloutPrefix) }
            .sorted()
            .map { (directory as NSString).appendingPathComponent($0) }
            .compactMap { path in
                guard let registered = registeredUSBDevice(for: path),
                      registered.usbDeviceRegistryEntryID != 0,
                      isTHD75(registered.identity) else {
                    return nil
                }
                return AvailableDeviceDescriptor(
                    path: path,
                    serialNumber: registered.identity.serialNumber,
                    usbDeviceRegistryEntryID: registered.usbDeviceRegistryEntryID
                )
            }
    }

    static func availableDevicePaths(
        directory: String,
        identityProvider: (String) -> USBIdentity?
    ) -> [String] {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: directory)) ?? []
        return names
            .filter { $0.hasPrefix(calloutPrefix) }
            .sorted()
            .map { (directory as NSString).appendingPathComponent($0) }
            .filter { path in
                guard let identity = identityProvider(path) else { return false }
                return isTHD75(identity)
            }
    }

    static func selectDevicePath(
        requestedPath: String?,
        verifiedPaths: [String]
    ) throws -> String {
        if let requestedPath {
            guard verifiedPaths.contains(requestedPath) else {
                throw AzimuthUSBLinkError.serviceNotFound
            }
            return requestedPath
        }
        guard let onlyPath = verifiedPaths.first else {
            throw AzimuthUSBLinkError.serviceNotFound
        }
        guard verifiedPaths.count == 1 else {
            throw AzimuthUSBLinkError.ambiguousDevices(verifiedPaths)
        }
        return onlyPath
    }

    static func openAndBindDevice(
        requestedPath: String?,
        expectedSerialNumber: String? = nil,
        access: OpenSystemAccess
    ) throws -> BoundUSBDevice {
        let path = try selectDevicePath(
            requestedPath: requestedPath,
            verifiedPaths: access.availableDevicePaths()
        )
        guard let identityBeforeOpen = access.registeredUSBDevice(path) else {
            throw AzimuthUSBLinkError.openedDeviceIdentityUnstable(path)
        }
        guard identityBeforeOpen.usbDeviceRegistryEntryID != 0 else {
            throw AzimuthUSBLinkError.openedDeviceIdentityUnstable(path)
        }
        if let expectedSerialNumber,
           identityBeforeOpen.identity.serialNumber != expectedSerialNumber {
            throw AzimuthUSBLinkError.openedDeviceSerialMismatch(
                path: path,
                expected: expectedSerialNumber,
                actual: identityBeforeOpen.identity.serialNumber
            )
        }

        let fileDescriptor = try access.openFileDescriptor(path)
        var mustClose = true
        defer {
            if mustClose { access.closeFileDescriptor(fileDescriptor) }
        }

        let openedNode = try access.nodeForFileDescriptor(fileDescriptor)
        let pathNodeBeforeIdentity = try access.nodeForPath(path)
        guard openedNode.isCharacterDevice,
              openedNode == pathNodeBeforeIdentity,
              let identityAfterOpen = access.registeredUSBDevice(path),
              identityAfterOpen.usbDeviceRegistryEntryID != 0,
              identityAfterOpen == identityBeforeOpen else {
            throw AzimuthUSBLinkError.openedDeviceIdentityUnstable(path)
        }
        if let expectedSerialNumber,
           identityAfterOpen.identity.serialNumber != expectedSerialNumber {
            throw AzimuthUSBLinkError.openedDeviceSerialMismatch(
                path: path,
                expected: expectedSerialNumber,
                actual: identityAfterOpen.identity.serialNumber
            )
        }
        let pathNodeAfterIdentity = try access.nodeForPath(path)
        guard openedNode == pathNodeAfterIdentity,
              isTHD75(identityAfterOpen.identity) else {
            throw AzimuthUSBLinkError.openedDeviceIdentityUnstable(path)
        }

        try access.configure(fileDescriptor)
        mustClose = false
        return BoundUSBDevice(
            fileDescriptor: fileDescriptor,
            path: path,
            identity: identityAfterOpen.identity,
            usbDeviceRegistryEntryID: identityAfterOpen.usbDeviceRegistryEntryID
        )
    }

    /// Resolve the tty's IORegistry ancestry before treating it as a radio.
    /// A `/dev/cu.usbmodem*` basename alone also describes development boards,
    /// phones, and unrelated CDC devices and is not safe identification.
    private static func registeredUSBIdentity(for path: String) -> USBIdentity? {
        registeredUSBDevice(for: path)?.identity
    }

    private static func registeredUSBDevice(for path: String) -> RegisteredUSBDevice? {
        guard let matching = IOServiceMatching("IOSerialBSDClient") else {
            return nil
        }
        var iterator: io_iterator_t = 0
        guard IOServiceGetMatchingServices(
            kIOMainPortDefault,
            matching,
            &iterator
        ) == KERN_SUCCESS else {
            return nil
        }
        defer { IOObjectRelease(iterator) }

        while true {
            let service = IOIteratorNext(iterator)
            guard service != IO_OBJECT_NULL else { return nil }

            let calloutPath = IORegistryEntryCreateCFProperty(
                service,
                "IOCalloutDevice" as CFString,
                kCFAllocatorDefault,
                0
            )?.takeRetainedValue() as? String
            if calloutPath == path {
                let registered = thD75USBDeviceAncestor(startingAt: service)
                IOObjectRelease(service)
                return registered
            }
            IOObjectRelease(service)
        }
    }

    /// Resolve the first physical USB-device ancestor, never a USB interface
    /// or the child IOSerialBSDClient. Its registry ID is also observed from
    /// the sibling CoreAudio engine and therefore forms the exact
    /// in-enumeration join between CAT and IF audio.
    static func thD75USBDeviceAncestor(
        startingAt entry: io_registry_entry_t
    ) -> RegisteredUSBDevice? {
        var current = entry
        IOObjectRetain(current)
        defer { IOObjectRelease(current) }

        while current != IO_OBJECT_NULL {
            if isPhysicalUSBDevice(current) {
                guard let vendor = localNumberProperty(
                    entry: current,
                    key: "idVendor"
                ),
                let product = localNumberProperty(
                    entry: current,
                    key: "idProduct"
                ) else {
                    return nil
                }
                let identity = USBIdentity(
                    vendorID: vendor.uint16Value,
                    productID: product.uint16Value,
                    serialNumber: ["USB Serial Number", "kUSBSerialNumberString"]
                        .lazy
                        .compactMap { localStringProperty(entry: current, key: $0) }
                        .first { !$0.isEmpty }
                )
                guard isTHD75(identity) else {
                    return nil
                }
                var registryEntryID: UInt64 = 0
                guard IORegistryEntryGetRegistryEntryID(
                    current,
                    &registryEntryID
                ) == KERN_SUCCESS,
                registryEntryID != 0 else {
                    return nil
                }
                return RegisteredUSBDevice(
                    identity: identity,
                    usbDeviceRegistryEntryID: registryEntryID
                )
            }

            var parent: io_registry_entry_t = IO_OBJECT_NULL
            guard IORegistryEntryGetParentEntry(
                current,
                kIOServicePlane,
                &parent
            ) == KERN_SUCCESS else {
                break
            }
            IOObjectRelease(current)
            current = parent
        }
        return nil
    }

    private static func isPhysicalUSBDevice(
        _ entry: io_registry_entry_t
    ) -> Bool {
        IOObjectConformsTo(entry, "IOUSBHostDevice") != 0
            || IOObjectConformsTo(entry, "IOUSBDevice") != 0
    }

    private static func localStringProperty(
        entry: io_registry_entry_t,
        key: String
    ) -> String? {
        IORegistryEntryCreateCFProperty(
            entry,
            key as CFString,
            kCFAllocatorDefault,
            0
        )?.takeRetainedValue() as? String
    }

    private static func localNumberProperty(
        entry: io_registry_entry_t,
        key: String
    ) -> NSNumber? {
        IORegistryEntryCreateCFProperty(
            entry,
            key as CFString,
            kCFAllocatorDefault,
            0
        )?.takeRetainedValue() as? NSNumber
    }

    static func isTHD75(_ identity: USBIdentity) -> Bool {
        identity.vendorID == thD75VendorID
            && identity.productID == thD75ProductID
    }

    private static var productionOpenSystemAccess: OpenSystemAccess {
        OpenSystemAccess(
            availableDevicePaths: { availableDevicePaths() },
            registeredUSBDevice: { registeredUSBDevice(for: $0) },
            openFileDescriptor: { path in
                let opened = Darwin.open(
                    path,
                    O_RDWR | O_NOCTTY | O_NONBLOCK | O_CLOEXEC | O_NOFOLLOW
                )
                guard opened >= 0 else {
                    throw AzimuthUSBLinkError.systemCall(
                        operation: "open \(path)",
                        code: errno
                    )
                }
                return opened
            },
            nodeForFileDescriptor: { try openedDeviceNode(fileDescriptor: $0) },
            nodeForPath: { try openedDeviceNode(path: $0) },
            configure: { try configure(fd: $0) },
            closeFileDescriptor: { _ = Darwin.close($0) }
        )
    }

    static func openedDeviceNode(
        device: dev_t,
        inode: ino_t,
        rawDevice: dev_t,
        mode: mode_t
    ) -> OpenedDeviceNode {
        OpenedDeviceNode(
            device: device,
            inode: inode,
            rawDevice: rawDevice,
            mode: mode
        )
    }

    private static func openedDeviceNode(fileDescriptor: Int32) throws -> OpenedDeviceNode {
        var status = stat()
        guard Darwin.fstat(fileDescriptor, &status) == 0 else {
            throw AzimuthUSBLinkError.systemCall(
                operation: "fstat opened USB serial device",
                code: errno
            )
        }
        return openedDeviceNode(status)
    }

    private static func openedDeviceNode(path: String) throws -> OpenedDeviceNode {
        var status = stat()
        let result = path.withCString {
            Darwin.fstatat(AT_FDCWD, $0, &status, 0)
        }
        guard result == 0 else {
            throw AzimuthUSBLinkError.systemCall(
                operation: "stat \(path)",
                code: errno
            )
        }
        return openedDeviceNode(status)
    }

    static func openedDeviceNode(_ status: stat) -> OpenedDeviceNode {
        openedDeviceNode(
            device: status.st_dev,
            inode: status.st_ino,
            rawDevice: status.st_rdev,
            mode: status.st_mode
        )
    }

    private func readabilityChanged(fileDescriptor: Int32, generation: UInt64) {
        consumeDoorbell(
            dataAvailable: true,
            fileDescriptor: fileDescriptor,
            generation: generation
        )
    }

    private func consumeDoorbell(
        dataAvailable: Bool,
        fileDescriptor eventFileDescriptor: Int32,
        generation eventGeneration: UInt64
    ) {
        lock.lock()
        guard Self.eventMatchesCurrentSession(
            eventFileDescriptor: eventFileDescriptor,
            eventGeneration: eventGeneration,
            currentFileDescriptor: fileDescriptor,
            currentGeneration: connectionGeneration
        ),
        let armed = doorbell,
        armed.fileDescriptor == eventFileDescriptor,
        armed.generation == eventGeneration else {
            lock.unlock()
            return
        }
        doorbell = nil
        lock.unlock()
        armed.callback(dataAvailable)
    }

    static func eventMatchesCurrentSession(
        eventFileDescriptor: Int32,
        eventGeneration: UInt64,
        currentFileDescriptor: Int32,
        currentGeneration: UInt64
    ) -> Bool {
        eventFileDescriptor >= 0
            && eventFileDescriptor == currentFileDescriptor
            && eventGeneration == currentGeneration
    }

    private func waitForRetiringReadabilitySource() throws {
        lock.lock()
        let retiring = retiringReadabilitySourceSession
        lock.unlock()
        guard let retiring else { return }

        if DispatchQueue.getSpecific(key: eventQueueKey) == true,
           !retiring.session.cancellationHasCompleted {
            throw AzimuthUSBLinkError.systemCall(
                operation: "reopen USB serial from its read event callback",
                code: EDEADLK
            )
        }
        retiring.session.waitForCancellation()

        lock.lock()
        if retiringReadabilitySourceSession?.session === retiring.session {
            retiringReadabilitySourceSession = nil
        }
        lock.unlock()
    }

    private func currentFileDescriptor() throws -> Int32 {
        lock.lock()
        defer { lock.unlock() }
        guard fileDescriptor >= 0 else { throw AzimuthUSBLinkError.notOpen }
        return fileDescriptor
    }

    private static func configure(fd: Int32) throws {
        var settings = termios()
        guard tcgetattr(fd, &settings) == 0 else {
            throw AzimuthUSBLinkError.systemCall(operation: "tcgetattr", code: errno)
        }
        cfmakeraw(&settings)
        settings.c_cflag &= ~tcflag_t(PARENB | CSTOPB | CSIZE)
        settings.c_cflag |= tcflag_t(CLOCAL | CREAD | CS8)
        guard cfsetspeed(&settings, speed_t(B115200)) == 0 else {
            throw AzimuthUSBLinkError.systemCall(operation: "cfsetspeed", code: errno)
        }
        guard tcsetattr(fd, TCSANOW, &settings) == 0 else {
            throw AzimuthUSBLinkError.systemCall(operation: "tcsetattr", code: errno)
        }
        tcflush(fd, TCIOFLUSH)

        // Match a normal serial-port open. The TH-D75's CDC firmware expects
        // host control-line state before it begins returning CAT responses.
        var modemLines: Int32 = TIOCM_DTR | TIOCM_RTS
        guard ioctl(fd, TIOCMBIS, &modemLines) == 0 else {
            throw AzimuthUSBLinkError.systemCall(operation: "set DTR/RTS", code: errno)
        }
    }
}

public extension AzimuthUSBSerialTransport {
    nonisolated static func platformDefault(
        devicePath: String? = nil,
        expectedSerialNumber: String? = nil
    ) -> AzimuthUSBSerialTransport {
        AzimuthUSBSerialTransport(
            link: POSIXAzimuthUSBSerialLink(
                devicePath: devicePath,
                expectedSerialNumber: expectedSerialNumber
            )
        )
    }

    nonisolated static func availableDevices() -> [AzimuthRadioDevice] {
        POSIXAzimuthUSBSerialLink.availableDeviceDescriptors().map { descriptor in
            AzimuthRadioDevice(
                id: AzimuthUSBEndpoint.stableID(
                    devicePath: descriptor.path,
                    usbSerialNumber: descriptor.serialNumber
                ),
                name: "Kenwood TH-D75",
                connectionKind: .usb,
                connection: "USB-C"
            )
        }
    }
}

#endif
