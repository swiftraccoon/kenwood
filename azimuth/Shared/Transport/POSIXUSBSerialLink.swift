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
        let registryEntryID: UInt64
    }

    struct OpenedDeviceNode: Equatable {
        let device: UInt64
        let inode: UInt64
        let rawDevice: UInt64
        let mode: UInt16

        var isCharacterDevice: Bool {
            mode & UInt16(S_IFMT) == UInt16(S_IFCHR)
        }
    }

    struct BoundUSBDevice: Equatable {
        let fileDescriptor: Int32
        let path: String
        let identity: USBIdentity
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
    private var readabilitySourceSession: ReadabilitySourceSession?
    /// A cancelled source remains retained until its handler has closed the
    /// old descriptor. The next `open()` waits on this barrier before touching
    /// the tty path or asserting DTR/RTS for a new session.
    private var retiringReadabilitySourceSession: ReadabilitySourceSession?
    private var doorbell: ArmedDoorbell?

    /// Pass an exact verified TH-D75 callout path when more than one radio is
    /// attached. Nil is accepted only when discovery finds exactly one radio.
    public init(devicePath: String? = nil) {
        requestedPath = devicePath
        openSystemAccess = Self.productionOpenSystemAccess
        eventQueue = DispatchQueue(label: "org.swiftraccoon.azimuth.posix-usb")
        eventQueue.setSpecific(key: eventQueueKey, value: true)
    }

    init(
        devicePath: String? = nil,
        openSystemAccess: OpenSystemAccess,
        eventQueue: DispatchQueue = DispatchQueue(
            label: "org.swiftraccoon.azimuth.posix-usb.test"
        )
    ) {
        requestedPath = devicePath
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
        availableDevicePaths(
            directory: directory,
            identityProvider: registeredUSBIdentity(for:)
        )
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
        access: OpenSystemAccess
    ) throws -> BoundUSBDevice {
        let path = try selectDevicePath(
            requestedPath: requestedPath,
            verifiedPaths: access.availableDevicePaths()
        )
        guard let identityBeforeOpen = access.registeredUSBDevice(path) else {
            throw AzimuthUSBLinkError.openedDeviceIdentityUnstable(path)
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
              identityAfterOpen == identityBeforeOpen else {
            throw AzimuthUSBLinkError.openedDeviceIdentityUnstable(path)
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
            identity: identityAfterOpen.identity
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
                let identity = registeredUSBIdentity(for: service)
                var registryEntryID: UInt64 = 0
                let registryResult = IORegistryEntryGetRegistryEntryID(
                    service,
                    &registryEntryID
                )
                IOObjectRelease(service)
                guard let identity, registryResult == KERN_SUCCESS else { return nil }
                return RegisteredUSBDevice(
                    identity: identity,
                    registryEntryID: registryEntryID
                )
            }
            IOObjectRelease(service)
        }
    }

    private static func registeredUSBIdentity(for service: io_service_t) -> USBIdentity? {
        let options = IOOptionBits(
            kIORegistryIterateRecursively | kIORegistryIterateParents
        )
        guard let vendor = IORegistryEntrySearchCFProperty(
            service,
            kIOServicePlane,
            "idVendor" as CFString,
            kCFAllocatorDefault,
            options
        ) as? NSNumber,
        let product = IORegistryEntrySearchCFProperty(
            service,
            kIOServicePlane,
            "idProduct" as CFString,
            kCFAllocatorDefault,
            options
        ) as? NSNumber else {
            return nil
        }
        let serialNumber = ["USB Serial Number", "kUSBSerialNumberString"]
            .lazy
            .compactMap { key in
                IORegistryEntrySearchCFProperty(
                    service,
                    kIOServicePlane,
                    key as CFString,
                    kCFAllocatorDefault,
                    options
                ) as? String
            }
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .first { !$0.isEmpty }
        return USBIdentity(
            vendorID: vendor.uint16Value,
            productID: product.uint16Value,
            serialNumber: serialNumber
        )
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
        device: UInt64,
        inode: UInt64,
        rawDevice: UInt64,
        mode: UInt16
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

    private static func openedDeviceNode(_ status: stat) -> OpenedDeviceNode {
        openedDeviceNode(
            device: UInt64(status.st_dev),
            inode: UInt64(status.st_ino),
            rawDevice: UInt64(status.st_rdev),
            mode: UInt16(status.st_mode)
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
        devicePath: String? = nil
    ) -> AzimuthUSBSerialTransport {
        AzimuthUSBSerialTransport(
            link: POSIXAzimuthUSBSerialLink(devicePath: devicePath)
        )
    }

    nonisolated static func availableDevices() -> [AzimuthRadioDevice] {
        POSIXAzimuthUSBSerialLink.availableDevicePaths().map { path in
            AzimuthRadioDevice(
                id: "tty:\(path)",
                name: "Kenwood TH-D75",
                connection: path
            )
        }
    }
}

#endif
