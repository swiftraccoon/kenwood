// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

#if os(macOS)

import Darwin
import Dispatch
import Foundation

/// Native macOS transport through Apple's public CDC ACM tty service.
/// No dext or private framework is needed on macOS: the TH-D75 appears as a
/// `/dev/cu.usbmodem*` device owned by the system USB serial driver.
public final class POSIXAzimuthUSBSerialLink: AzimuthUSBSerialLink, @unchecked Sendable {
    public static let deviceDirectory = "/dev"
    public static let calloutPrefix = "cu.usbmodem"

    private let requestedPath: String?
    private let operationLock = NSLock()
    private let lock = NSLock()
    private let eventQueue = DispatchQueue(
        label: "org.swiftraccoon.azimuth.posix-usb"
    )
    private var fileDescriptor: Int32 = -1
    private var openedPath: String?
    private var readabilitySource: DispatchSourceRead?
    private var doorbell: (@Sendable (Bool) -> Void)?

    /// Pass an exact callout path when a future picker offers more than one
    /// attached CDC radio. Nil deterministically selects the first path.
    public init(devicePath: String? = nil) {
        requestedPath = devicePath
    }

    public var connectionDescription: String {
        lock.lock()
        defer { lock.unlock() }
        return openedPath ?? requestedPath ?? "macOS USB CDC"
    }

    public func servicePresent() -> Bool {
        if let requestedPath {
            return FileManager.default.fileExists(atPath: requestedPath)
        }
        return !Self.availableDevicePaths().isEmpty
    }

    public func open() throws {
        operationLock.lock()
        defer { operationLock.unlock() }
        lock.lock()
        defer { lock.unlock() }
        guard fileDescriptor < 0 else { return }

        let path = requestedPath ?? Self.availableDevicePaths().first
        guard let path else { throw AzimuthUSBLinkError.serviceNotFound }

        let opened = Darwin.open(path, O_RDWR | O_NOCTTY | O_NONBLOCK | O_CLOEXEC)
        guard opened >= 0 else {
            throw AzimuthUSBLinkError.systemCall(operation: "open \(path)", code: errno)
        }
        do {
            try Self.configure(fd: opened)
        } catch {
            Darwin.close(opened)
            throw error
        }

        let source = DispatchSource.makeReadSource(
            fileDescriptor: opened,
            queue: eventQueue
        )
        source.setEventHandler { [weak self] in self?.readabilityChanged() }
        fileDescriptor = opened
        openedPath = path
        readabilitySource = source
        source.resume()
    }

    public func close() {
        operationLock.lock()
        defer { operationLock.unlock() }
        lock.lock()
        let opened = fileDescriptor
        let source = readabilitySource
        let pendingDoorbell = doorbell
        fileDescriptor = -1
        openedPath = nil
        readabilitySource = nil
        doorbell = nil
        lock.unlock()

        source?.cancel()
        if opened >= 0 { Darwin.close(opened) }
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
        doorbell = onFire
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
            consumeDoorbell(dataAvailable: true)
        }
    }

    public static func availableDevicePaths(
        directory: String = deviceDirectory
    ) -> [String] {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: directory)) ?? []
        return names
            .filter { $0.hasPrefix(calloutPrefix) }
            .sorted()
            .map { (directory as NSString).appendingPathComponent($0) }
    }

    private func readabilityChanged() {
        consumeDoorbell(dataAvailable: true)
    }

    private func consumeDoorbell(dataAvailable: Bool) {
        lock.lock()
        let callback = doorbell
        doorbell = nil
        lock.unlock()
        callback?(dataAvailable)
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
