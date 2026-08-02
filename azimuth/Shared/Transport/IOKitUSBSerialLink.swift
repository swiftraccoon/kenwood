// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

#if os(iOS)

import Foundation
import OSLog

private let azimuthIOKitLog = Logger(
    subsystem: "org.swiftraccoon.azimuth",
    category: "usb-user-client"
)

private let azimuthVerboseIOKitTracing =
    ProcessInfo.processInfo.environment["AZIMUTH_VERBOSE_USB_TRACE"] == "1"

private func azimuthIOKitTrace(_ message: String) {
    guard azimuthVerboseIOKitTracing else { return }
    azimuthIOKitLog.debug("\(message, privacy: .public)")
}

private func azimuthIOKitNotice(_ message: String) {
    azimuthIOKitLog.notice("\(message, privacy: .public)")
}

private func azimuthIOKitError(_ message: String) {
    azimuthIOKitLog.error("\(message, privacy: .public)")
}

// Function-like IOKit macros do not import through a bridging header.
private let azimuthIOReturnNoResources = IOReturn(bitPattern: 0xE00002BE)
private let azimuthIOReturnBusy = IOReturn(bitPattern: 0xE00002D5)

private final class AzimuthDoorbellBox {
    let handler: @Sendable (Bool) -> Void
    weak var owner: IOKitAzimuthUSBSerialLink?

    init(handler: @escaping @Sendable (Bool) -> Void) {
        self.handler = handler
    }
}

private func azimuthDoorbellCallback(
    refcon: UnsafeMutableRawPointer?,
    result: IOReturn,
    args: UnsafeMutablePointer<UnsafeMutableRawPointer?>?,
    argumentCount: UInt32
) {
    _ = args
    azimuthIOKitTrace(
        "[Azimuth USB] async doorbell callback: result=\(azimuthKernReturnString(result)) "
            + "argumentCount=\(argumentCount)"
    )
    guard let refcon else { return }
    let box = Unmanaged<AzimuthDoorbellBox>.fromOpaque(refcon).takeRetainedValue()
    box.owner?.doorbellWasConsumed(refcon)
    box.handler(result == kIOReturnSuccess)
}

/// iPadOS DriverKit host connection to `AzimuthUSBSerialDriver`.
public final class IOKitAzimuthUSBSerialLink: AzimuthUSBSerialLink, @unchecked Sendable {
    public static let dataServiceName = "AzimuthUSBSerialDriver"
    public static let controlServiceName = "AzimuthUSBCommDriver"

    /// Prevent close/reopen from invalidating a connection while a synchronous
    /// core callback is changing baud or issuing another user-client call.
    private let operationLock = NSLock()
    private let lock = NSLock()
    private var connection: io_connect_t = IO_OBJECT_NULL
    private var notificationPort: IONotificationPortRef?
    private let callbackQueue = DispatchQueue(
        label: "org.swiftraccoon.azimuth.usb-doorbell"
    )
    /// Owns one retained box while a completion is armed. It is cleared by
    /// either the callback, a failed arm, or `close()` after a queue barrier.
    private var armedRefcon: UnsafeMutableRawPointer?

    public init() {}

    public var connectionDescription: String { "DriverKit USB-C" }

    /// iPadOS starts an approved dext on demand after USB matching. A running
    /// extension may already be registered before the connect action; Azimuth
    /// must also tolerate the initial launch window.
    public var serviceRegistrationWaitNanoseconds: UInt64 { 4_000_000_000 }

    public func servicePresent() -> Bool {
        Self.registered(serviceNamed: Self.dataServiceName)
    }

    public func commServicePresent() -> Bool? {
        Self.registered(serviceNamed: Self.controlServiceName)
    }

    public func open() throws {
        operationLock.lock()
        defer { operationLock.unlock() }
        lock.lock()
        defer { lock.unlock() }
        guard connection == IO_OBJECT_NULL else { return }

        let service = IOServiceGetMatchingService(
            kIOMainPortDefault,
            IOServiceNameMatching(Self.dataServiceName)
        )
        guard service != IO_OBJECT_NULL else {
            azimuthIOKitError(
                "[Azimuth USB] IOServiceOpen skipped: data service not registered"
            )
            throw AzimuthUSBLinkError.serviceNotFound
        }
        defer { IOObjectRelease(service) }

        var opened: io_connect_t = IO_OBJECT_NULL
        let result = IOServiceOpen(service, mach_task_self_, 0, &opened)
        guard result == kIOReturnSuccess, opened != IO_OBJECT_NULL else {
            azimuthIOKitError(
                "[Azimuth USB] IOServiceOpen failed: result=\(azimuthKernReturnString(result))"
            )
            throw AzimuthUSBLinkError.openFailed(code: result)
        }
        guard let port = IONotificationPortCreate(kIOMainPortDefault) else {
            azimuthIOKitError(
                "[Azimuth USB] notification port creation failed after IOServiceOpen"
            )
            IOServiceClose(opened)
            throw AzimuthUSBLinkError.openFailed(code: azimuthIOReturnNoResources)
        }
        IONotificationPortSetDispatchQueue(port, callbackQueue)
        connection = opened
        notificationPort = port
        azimuthIOKitNotice("[Azimuth USB] IOServiceOpen succeeded; notification port ready")
    }

    public func close() {
        operationLock.lock()
        defer { operationLock.unlock() }
        lock.lock()
        let opened = connection
        let port = notificationPort
        connection = IO_OBJECT_NULL
        notificationPort = nil
        lock.unlock()

        azimuthIOKitTrace(
            "[Azimuth USB] closing IOKit link: userClientOpen=\(opened != IO_OBJECT_NULL) "
                + "notificationPortOpen=\(port != nil)"
        )
        if let port { IONotificationPortDestroy(port) }
        // All already-enqueued completions have consumed their retain after
        // this barrier. A still-recorded box can no longer be called back.
        callbackQueue.sync {}
        lock.lock()
        let orphan = armedRefcon
        armedRefcon = nil
        lock.unlock()
        if let orphan {
            Unmanaged<AzimuthDoorbellBox>.fromOpaque(orphan).release()
        }
        if opened != IO_OBJECT_NULL { IOServiceClose(opened) }
    }

    public func write(_ bytes: [UInt8]) throws {
        operationLock.lock()
        defer { operationLock.unlock() }
        guard (1...AzimuthUSBABIV1.maximumTransferBytes).contains(bytes.count) else {
            throw AzimuthUSBLinkError.invalidTransferLength(bytes.count)
        }
        let opened = try currentConnection()
        let result = bytes.withUnsafeBytes { rawBuffer in
            IOConnectCallStructMethod(
                opened,
                AzimuthUSBSelectorV1.write.rawValue,
                rawBuffer.baseAddress,
                rawBuffer.count,
                nil,
                nil
            )
        }
        switch result {
        case kIOReturnSuccess: return
        case azimuthIOReturnNoResources: throw AzimuthUSBLinkError.backpressure
        default: throw AzimuthUSBLinkError.callFailed(code: result)
        }
    }

    public func setBaudRate(baud: UInt32) throws {
        operationLock.lock()
        defer { operationLock.unlock() }
        guard AzimuthUSBABIV2.supportedBaudRates.contains(baud) else {
            throw AzimuthUSBLinkError.unsupportedBaudRate(baud)
        }
        let opened = try currentConnection()
        var input = [UInt64(baud)]
        let result = IOConnectCallScalarMethod(
            opened,
            AzimuthUSBSelectorV2.setBaudRate.rawValue,
            &input,
            1,
            nil,
            nil
        )
        guard result == kIOReturnSuccess else {
            throw AzimuthUSBLinkError.callFailed(code: result)
        }
    }

    public func drain(maxBytes: Int) throws -> [UInt8] {
        operationLock.lock()
        defer { operationLock.unlock() }
        guard maxBytes > 0 else { throw AzimuthUSBLinkError.invalidTransferLength(maxBytes) }
        let opened = try currentConnection()
        var bytes = [UInt8](
            repeating: 0,
            count: min(maxBytes, AzimuthUSBABIV1.maximumTransferBytes)
        )
        var count = bytes.count
        let result = bytes.withUnsafeMutableBytes { rawBuffer in
            IOConnectCallStructMethod(
                opened,
                AzimuthUSBSelectorV1.read.rawValue,
                nil,
                0,
                rawBuffer.baseAddress,
                &count
            )
        }
        guard result == kIOReturnSuccess else {
            throw AzimuthUSBLinkError.callFailed(code: result)
        }
        guard count <= bytes.count else {
            throw AzimuthUSBLinkError.callFailed(code: IOReturn(bitPattern: 0xE00002C2))
        }
        bytes.removeLast(bytes.count - count)
        return bytes
    }

    public func armDoorbell(onFire: @escaping @Sendable (Bool) -> Void) throws {
        operationLock.lock()
        defer { operationLock.unlock() }
        let opened = try currentConnection()
        let box = AzimuthDoorbellBox(handler: onFire)
        box.owner = self
        let refcon = Unmanaged.passRetained(box).toOpaque()

        lock.lock()
        guard let port = notificationPort else {
            lock.unlock()
            Unmanaged<AzimuthDoorbellBox>.fromOpaque(refcon).release()
            throw AzimuthUSBLinkError.notOpen
        }
        guard armedRefcon == nil else {
            lock.unlock()
            Unmanaged<AzimuthDoorbellBox>.fromOpaque(refcon).release()
            throw AzimuthUSBLinkError.callFailed(code: azimuthIOReturnBusy)
        }
        let machPort = IONotificationPortGetMachPort(port)
        // Record ownership before the call. An immediate completion may run
        // before IOConnectCallAsyncStructMethod returns.
        armedRefcon = refcon
        lock.unlock()

        // io_async_ref64_t: reserved wake-port slot, callback, refcon.
        var asyncReference = [UInt64](repeating: 0, count: 8)
        asyncReference[1] = UInt64(UInt(bitPattern: unsafeBitCast(
            azimuthDoorbellCallback as IOAsyncCallback,
            to: Int.self
        )))
        asyncReference[2] = UInt64(UInt(bitPattern: refcon))

        let result = IOConnectCallAsyncStructMethod(
            opened,
            AzimuthUSBSelectorV1.armDoorbell.rawValue,
            machPort,
            &asyncReference,
            3,
            nil,
            0,
            nil,
            nil
        )
        guard result == kIOReturnSuccess else {
            lock.lock()
            let stillOwned = armedRefcon == refcon
            if stillOwned { armedRefcon = nil }
            lock.unlock()
            if stillOwned {
                Unmanaged<AzimuthDoorbellBox>.fromOpaque(refcon).release()
            }
            throw AzimuthUSBLinkError.callFailed(code: result)
        }
    }

    public func status() throws -> AzimuthUSBDextStatus? {
        operationLock.lock()
        defer { operationLock.unlock() }
        let opened = try currentConnection()
        var output = [UInt64](repeating: 0, count: AzimuthUSBABIV1.statusScalarCount)
        var count = UInt32(output.count)
        let result = IOConnectCallScalarMethod(
            opened,
            AzimuthUSBSelectorV1.status.rawValue,
            nil,
            0,
            &output,
            &count
        )
        guard result == kIOReturnSuccess,
              count == UInt32(AzimuthUSBABIV1.statusScalarCount) else {
            throw AzimuthUSBLinkError.callFailed(code: result)
        }
        return AzimuthUSBDextStatus(
            rxBuffered: output[0],
            rxOverflowBytes: output[1],
            linkUp: output[2] == 1,
            doorbellArmed: output[3] == 1
        )
    }

    public func dextLog() throws -> [AzimuthUSBDextLogEntry] {
        operationLock.lock()
        defer { operationLock.unlock() }
        let opened = try currentConnection()
        var bytes = [UInt8](
            repeating: 0,
            count: AzimuthUSBABIV1.maximumTransferBytes
        )
        var count = bytes.count
        let result = bytes.withUnsafeMutableBytes { rawBuffer in
            IOConnectCallStructMethod(
                opened,
                AzimuthUSBSelectorV1.copyLog.rawValue,
                nil,
                0,
                rawBuffer.baseAddress,
                &count
            )
        }
        guard result == kIOReturnSuccess, count <= bytes.count else {
            throw AzimuthUSBLinkError.callFailed(code: result)
        }
        var entries: [AzimuthUSBDextLogEntry] = []
        var offset = 0
        while offset + AzimuthUSBDextLogEntry.wireSize <= count {
            if let entry = AzimuthUSBDextLogEntry(
                bytes: bytes[offset..<(offset + AzimuthUSBDextLogEntry.wireSize)]
            ) {
                entries.append(entry)
            }
            offset += AzimuthUSBDextLogEntry.wireSize
        }
        return entries
    }

    fileprivate func doorbellWasConsumed(_ refcon: UnsafeMutableRawPointer) {
        lock.lock()
        if armedRefcon == refcon { armedRefcon = nil }
        lock.unlock()
    }

    private func currentConnection() throws -> io_connect_t {
        lock.lock()
        defer { lock.unlock() }
        guard connection != IO_OBJECT_NULL else { throw AzimuthUSBLinkError.notOpen }
        return connection
    }

    private static func registered(serviceNamed name: String) -> Bool {
        let service = IOServiceGetMatchingService(
            kIOMainPortDefault,
            IOServiceNameMatching(name)
        )
        guard service != IO_OBJECT_NULL else { return false }
        IOObjectRelease(service)
        return true
    }
}

public extension AzimuthUSBSerialTransport {
    nonisolated static func platformDefault() -> AzimuthUSBSerialTransport {
        #if targetEnvironment(simulator)
        return AzimuthUSBSerialTransport(link: AzimuthUnavailableUSBSerialLink(
            reason: "USBDriverKit is unavailable in Simulator. Run Azimuth on a physical M-series iPad."
        ))
        #else
        AzimuthUSBSerialTransport(link: IOKitAzimuthUSBSerialLink())
        #endif
    }

    nonisolated static func availableDevices() -> [AzimuthRadioDevice] {
        #if targetEnvironment(simulator)
        return []
        #else
        let link = IOKitAzimuthUSBSerialLink()
        return link.servicePresent() && link.commServicePresent() == true ? [.thD75USBC] : []
        #endif
    }
}

#endif
