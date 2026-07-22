// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

#if os(iOS)

// IOKit's C API arrives via the target bridging header (iPad/IOKitShim.h):
// the iOS SDK has the headers but no `import IOKit` Swift module.
import Foundation
import OSLog

private let log = Logger(subsystem: "org.swiftraccoon.lodestar", category: "usb-link")

/// `kIOReturnNoResources`: the header defines it via the
/// `iokit_common_err()` function-like macro, which Swift cannot import
/// through a bridging header. Same numeric value the dext returns for a
/// full write queue.
private let ioReturnNoResources = IOReturn(bitPattern: 0xE00002BE)

/// Box passed as the async-callback refcon so the C callback can reach
/// the handler closure. Retained while a doorbell is armed; the retain
/// is consumed by exactly one of: the callback firing, an arm failure,
/// or `close()` reclaiming a never-fired doorbell.
private final class DoorbellBox {
    let onFire: @Sendable (Bool) -> Void
    weak var owner: IOKitUSBSerialLink?
    init(_ onFire: @escaping @Sendable (Bool) -> Void) { self.onFire = onFire }
}

/// C-convention trampoline for the armed async completion. `result` is
/// the IOReturn the dext passed to `AsyncCompletion`: success = data
/// available, `kIOReturnAborted`/anything else = teardown.
private func doorbellCallback(
    refcon: UnsafeMutableRawPointer?,
    result: IOReturn,
    args: UnsafeMutablePointer<UnsafeMutableRawPointer?>?,
    numArgs: UInt32
) {
    guard let refcon else { return }
    let box = Unmanaged<DoorbellBox>.fromOpaque(refcon).takeRetainedValue()
    box.owner?.doorbellConsumed(refcon)
    box.onFire(result == kIOReturnSuccess)
}

/// Real `USBSerialLink` over the IOKit user-client C API.
///
/// Thread-safety: all mutable state behind `lock`; the notification
/// port delivers callbacks on a private serial dispatch queue.
public final class IOKitUSBSerialLink: USBSerialLink, @unchecked Sendable {
    /// The dext's IOUserClass, which is what the registered service is named.
    static let serviceName = "LodestarUSBSerialDriver"

    private let lock = NSLock()
    private var connection: io_connect_t = IO_OBJECT_NULL
    private var notifyPort: IONotificationPortRef?
    private let queue = DispatchQueue(label: "org.swiftraccoon.lodestar.usb-doorbell")
    /// Opaque pointer of the currently armed (retained, unfired)
    /// `DoorbellBox`, so `close()` can reclaim the retain when the
    /// callback will never run. Cleared by the callback on delivery.
    private var armedRefcon: UnsafeMutableRawPointer?

    public init() {}

    public func servicePresent() -> Bool {
        Self.registered(Self.serviceName)
    }

    /// Whether the companion control-interface driver is running.
    /// Diagnostics only (it has no user client).
    public func commServicePresent() -> Bool? {
        Self.registered("LodestarUSBCommDriver")
    }

    private static func registered(_ name: String) -> Bool {
        let service = IOServiceGetMatchingService(
            kIOMainPortDefault,
            IOServiceNameMatching(name)
        )
        guard service != IO_OBJECT_NULL else { return false }
        IOObjectRelease(service)
        return true
    }

    public func open() throws {
        lock.lock(); defer { lock.unlock() }
        guard connection == IO_OBJECT_NULL else { return }
        let service = IOServiceGetMatchingService(
            kIOMainPortDefault,
            IOServiceNameMatching(Self.serviceName)
        )
        guard service != IO_OBJECT_NULL else { throw USBLinkError.serviceNotFound }
        defer { IOObjectRelease(service) }

        var connect: io_connect_t = IO_OBJECT_NULL
        let kr = IOServiceOpen(service, mach_task_self_, 0, &connect)
        guard kr == kIOReturnSuccess, connect != IO_OBJECT_NULL else {
            throw USBLinkError.openFailed(kern: kr)
        }
        guard let port = IONotificationPortCreate(kIOMainPortDefault) else {
            IOServiceClose(connect)
            throw USBLinkError.openFailed(kern: ioReturnNoResources)
        }
        IONotificationPortSetDispatchQueue(port, queue)
        connection = connect
        notifyPort = port
        log.info("user client opened")
    }

    public func close() {
        lock.lock()
        let connect = connection
        let port = notifyPort
        connection = IO_OBJECT_NULL
        notifyPort = nil
        lock.unlock()
        if let port { IONotificationPortDestroy(port) }
        // Barrier: any doorbell callout already dispatched has now run
        // (and consumed its refcon); after the destroy above no new one
        // can be scheduled. Whatever is still recorded as armed will
        // never fire, so reclaim its retain.
        queue.sync {}
        lock.lock()
        let leftover = armedRefcon
        armedRefcon = nil
        lock.unlock()
        if let leftover { Unmanaged<DoorbellBox>.fromOpaque(leftover).release() }
        if connect != IO_OBJECT_NULL { IOServiceClose(connect) }
    }

    /// Called from the doorbell callback: this refcon's retain was just
    /// consumed by delivery, so `close()` must not release it again.
    fileprivate func doorbellConsumed(_ refcon: UnsafeMutableRawPointer) {
        lock.lock()
        if armedRefcon == refcon { armedRefcon = nil }
        lock.unlock()
    }

    public func write(_ bytes: [UInt8]) throws {
        let connect = try currentConnection()
        let kr = bytes.withUnsafeBytes { raw in
            IOConnectCallStructMethod(
                connect, USBSerialSelector.write.rawValue,
                raw.baseAddress, raw.count, nil, nil
            )
        }
        switch kr {
        case kIOReturnSuccess: return
        case ioReturnNoResources: throw USBLinkError.backpressure
        default: throw USBLinkError.callFailed(kern: kr)
        }
    }

    public func drain(maxBytes: Int) throws -> [UInt8] {
        let connect = try currentConnection()
        var out = [UInt8](repeating: 0, count: min(maxBytes, 4096))
        var outLen = out.count
        let kr = out.withUnsafeMutableBytes { raw in
            IOConnectCallStructMethod(
                connect, USBSerialSelector.read.rawValue,
                nil, 0, raw.baseAddress, &outLen
            )
        }
        guard kr == kIOReturnSuccess else { throw USBLinkError.callFailed(kern: kr) }
        out.removeLast(out.count - outLen)
        return out
    }

    public func armDoorbell(onFire: @escaping @Sendable (Bool) -> Void) throws {
        let connect = try currentConnection()
        lock.lock()
        guard let port = notifyPort else {
            lock.unlock()
            throw USBLinkError.notOpen
        }
        let machPort = IONotificationPortGetMachPort(port)
        lock.unlock()

        let box = DoorbellBox(onFire)
        box.owner = self
        let refcon = Unmanaged.passRetained(box).toOpaque()
        // io_async_ref64_t layout per OSMessageNotification.h:
        // slot 0 = kIOAsyncReservedIndex (kernel-owned; the wake port
        // lands here), slot 1 = kIOAsyncCalloutFuncIndex (callback fn
        // ptr), slot 2 = kIOAsyncCalloutRefconIndex; the ref count is
        // kIOAsyncCalloutCount = 3. Putting the callback in slot 0 is
        // the classic off-by-one that silently never fires.
        var asyncRef: [UInt64] = Array(repeating: 0, count: 8)
        asyncRef[1] = UInt64(UInt(bitPattern: unsafeBitCast(
            doorbellCallback as IOAsyncCallback, to: Int.self)))
        asyncRef[2] = UInt64(UInt(bitPattern: refcon))

        let kr = IOConnectCallAsyncStructMethod(
            connect, USBSerialSelector.armDoorbell.rawValue,
            machPort, &asyncRef, 3, nil, 0, nil, nil
        )
        guard kr == kIOReturnSuccess else {
            Unmanaged<DoorbellBox>.fromOpaque(refcon).release()
            throw USBLinkError.callFailed(kern: kr)
        }
        lock.lock()
        armedRefcon = refcon
        lock.unlock()
    }

    public func status() throws -> USBDextStatus? {
        let connect = try currentConnection()
        var out = [UInt64](repeating: 0, count: 4)
        var outCnt: UInt32 = 4
        let kr = IOConnectCallScalarMethod(
            connect, USBSerialSelector.status.rawValue, nil, 0, &out, &outCnt
        )
        guard kr == kIOReturnSuccess, outCnt == 4 else {
            throw USBLinkError.callFailed(kern: kr)
        }
        return USBDextStatus(
            rxBuffered: out[0], rxOverflowBytes: out[1],
            linkUp: out[2] == 1, doorbellArmed: out[3] == 1
        )
    }

    public func dextLog() throws -> [USBDextLogEntry] {
        let connect = try currentConnection()
        var raw = [UInt8](repeating: 0, count: 4096)
        var rawLen = raw.count
        let kr = raw.withUnsafeMutableBytes { buf in
            IOConnectCallStructMethod(
                connect, USBSerialSelector.copyLog.rawValue,
                nil, 0, buf.baseAddress, &rawLen
            )
        }
        guard kr == kIOReturnSuccess else { throw USBLinkError.callFailed(kern: kr) }
        var entries: [USBDextLogEntry] = []
        var offset = 0
        while offset + USBDextLogEntry.wireSize <= rawLen {
            if let e = USBDextLogEntry(bytes: raw[offset..<(offset + USBDextLogEntry.wireSize)]) {
                entries.append(e)
            }
            offset += USBDextLogEntry.wireSize
        }
        return entries
    }

    private func currentConnection() throws -> io_connect_t {
        lock.lock(); defer { lock.unlock() }
        guard connection != IO_OBJECT_NULL else { throw USBLinkError.notOpen }
        return connection
    }
}

public extension USBSerialTransport {
    /// Probe for a connected TH-D75 over USB. Returns the synthetic
    /// descriptor iff the dext service is registered.
    nonisolated static func availableDevices() -> [BluetoothDevice] {
        IOKitUSBSerialLink().servicePresent() ? [.usbSynthetic] : []
    }
}

#endif
