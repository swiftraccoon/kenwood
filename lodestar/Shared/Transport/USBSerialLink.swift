// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// External-method selectors of the LodestarDriver user client.
/// MUST match the `LodestarSelector` enum in
/// `Driver/LodestarUserClient.cpp` — the dext side is the other half of
/// this contract.
public enum USBSerialSelector: UInt32 {
    /// structureInput 1..4096 bytes → bulk OUT queue.
    case write = 0
    /// structureOutput 0..4096 bytes; zero bytes means "dext buffer empty".
    case read = 1
    /// Async completion armed as the one-shot data-available doorbell.
    case armDoorbell = 2
    /// scalarOutput ×4: rxBuffered, rxOverflowBytes, linkUp, doorbellArmed.
    case status = 3
    /// structureOutput: `USBDextLogEntry` records, oldest first.
    case copyLog = 4
}

/// Snapshot of the dext's counters (selector 3).
public struct USBDextStatus: Sendable {
    public let rxBuffered: UInt64
    public let rxOverflowBytes: UInt64
    public let linkUp: Bool
    public let doorbellArmed: Bool

    public var text: String {
        "dext: rxBuffered=\(rxBuffered) overflow=\(rxOverflowBytes) "
            + "linkUp=\(linkUp) doorbellArmed=\(doorbellArmed)"
    }
}

/// One diagnostic event from the dext's ring (selector 4). Mirrors
/// `LodestarLogEntry` + `LodestarEvent` in
/// `Driver/LodestarUSBSerialDriver.cpp` — keep in sync.
public struct USBDextLogEntry: Sendable {
    public let seq: UInt32
    public let event: UInt32
    public let code: Int64
    public let a: UInt64
    public let b: UInt64

    /// 32-byte wire layout: u32 seq, u32 event, i64 code, u64 a, u64 b.
    public static let wireSize = 32

    public init?(bytes: ArraySlice<UInt8>) {
        guard bytes.count >= Self.wireSize else { return nil }
        let base = bytes.startIndex
        func u32(_ off: Int) -> UInt32 {
            (0..<4).reduce(UInt32(0)) { $0 | UInt32(bytes[base + off + $1]) << (8 * UInt32($1)) }
        }
        func u64(_ off: Int) -> UInt64 {
            (0..<8).reduce(UInt64(0)) { $0 | UInt64(bytes[base + off + $1]) << (8 * UInt64($1)) }
        }
        seq = u32(0)
        event = u32(4)
        code = Int64(bitPattern: u64(8))
        a = u64(16)
        b = u64(24)
    }

    public var text: String {
        let kern = kernReturnString(Int32(truncatingIfNeeded: code))
        switch event {
        case 1: return "#\(seq) endpoint addr=0x\(String(a, radix: 16)) type=\(b)"
        case 2: return "#\(seq) buffers mapped in=\(a == 1) out=\(b == 1)"
        case 3: return "#\(seq) SET_CONTROL_LINE_STATE → \(kern)"
        case 4: return "#\(seq) Start ok bulkIn=0x\(String(a, radix: 16)) bulkOut=0x\(String(b, radix: 16))"
        case 5: return "#\(seq) Start FAILED stage=\(a) \(kern)"
        case 6:
            let outcome = ["armed", "fired-immediately (data pending)", "aborted (link down)"]
            return "#\(seq) armDoorbell → \(a < outcome.count ? outcome[Int(a)] : "?\(a)")"
        case 7: return "#\(seq) doorbell fired \(kern)"
        case 8: return "#\(seq) bulk-IN error \(kern) bytes=\(a) streak=\(b)"
        case 9: return "#\(seq) RX data edge, \(a) bytes"
        case 10: return "#\(seq) enqueueWrite \(kern) len=\(a) txLenAfter=\(b)"
        case 11: return "#\(seq) TX submit \(kern) n=\(a)"
        case 12:
            let reasons = ["?", "Stop", "bulk-IN error streak", "bulk-IN re-arm after stall",
                           "bulk-IN re-arm", "bulk-OUT submit", "bulk-OUT buffer unmapped"]
            return "#\(seq) LINK FAILED: \(a < reasons.count ? reasons[Int(a)] : "?\(a)")"
        case 13: return "#\(seq) read copied \(b)/\(a)"
        case 14: return "#\(seq) SET_LINE_CODING → \(kern)"
        default: return "#\(seq) event \(event) code=\(code) a=\(a) b=\(b)"
        }
    }
}

/// Render a `kern_return_t` legibly: named constant where known, else
/// unsigned hex (never Swift's signed-decimal-with-0x-prefix mangling
/// that turned MIG_SERVER_DIED into "0x-134").
public func kernReturnString(_ kern: Int32) -> String {
    switch kern {
    case 0: return "OK"
    case -308: return "MIG_SERVER_DIED (driver process crashed)"
    case -305: return "MIG_NO_REPLY"
    case -304: return "MIG_BAD_ARGUMENTS"
    case Int32(bitPattern: 0xE00002BE): return "kIOReturnNoResources"
    case Int32(bitPattern: 0xE00002C2): return "kIOReturnBadArgument"
    case Int32(bitPattern: 0xE00002C9): return "kIOReturnNotAttached"
    case Int32(bitPattern: 0xE00002CD): return "kIOReturnNotPermitted"
    case Int32(bitPattern: 0xE00002D9): return "kIOReturnAborted"
    case Int32(bitPattern: 0xE00002E2): return "kIOReturnNoDevice"
    case Int32(bitPattern: 0xE00002EB): return "kIOReturnTimeout"
    default:
        return "0x\(String(UInt32(bitPattern: kern), radix: 16, uppercase: true)) (\(kern))"
    }
}

/// Errors surfaced by a `USBSerialLink` implementation.
public enum USBLinkError: Error, Equatable {
    /// The dext service is not registered (no radio, or driver disabled).
    case serviceNotFound
    /// `IOServiceOpen` failed (e.g. driver not enabled in Settings).
    case openFailed(kern: Int32)
    /// Operation attempted before `open()` / after `close()`.
    case notOpen
    /// Dext write queue full (`kIOReturnNoResources`) — retryable.
    case backpressure
    /// Any other failed user-client call.
    case callFailed(kern: Int32)
}

/// Seam between `USBSerialTransport` and the IOKit user-client calls.
///
/// One instance represents one potential connection to the dext.
/// `IOKitUSBSerialLink` (iOS) is the real implementation; tests use a
/// scriptable mock. All methods are synchronous and non-blocking: the
/// doorbell + drain protocol lives in `USBSerialTransport`, not here.
public protocol USBSerialLink: Sendable {
    /// `true` when the dext's IOService is registered — radio plugged
    /// in AND the driver enabled in Settings → General → Drivers.
    func servicePresent() -> Bool

    /// Open the user-client connection.
    func open() throws

    /// Close the connection. Safe to call repeatedly.
    func close()

    /// Selector 0. Throws `USBLinkError.backpressure` when the dext
    /// write queue is full.
    func write(_ bytes: [UInt8]) throws

    /// Selector 1. Returns whatever the dext has buffered, up to
    /// `maxBytes` (≤4096); empty array when the dext buffer is empty.
    func drain(maxBytes: Int) throws -> [UInt8]

    /// Selector 2. Arm the one-shot doorbell. `onFire(true)` = data
    /// available; `onFire(false)` = link torn down (unplug/close).
    /// The handler may be invoked on any thread/queue.
    func armDoorbell(onFire: @escaping @Sendable (Bool) -> Void) throws

    /// Selector 3. Dext counters, or nil when unsupported (mocks).
    func status() throws -> USBDextStatus?

    /// Selector 4. Dext diagnostic event ring, oldest first.
    func dextLog() throws -> [USBDextLogEntry]

    /// Whether the companion control-interface driver is registered
    /// (diagnostics only; nil = concept doesn't apply to this link).
    func commServicePresent() -> Bool?
}

public extension USBSerialLink {
    func status() throws -> USBDextStatus? { nil }
    func dextLog() throws -> [USBDextLogEntry] { [] }
    func commServicePresent() -> Bool? { nil }
}
