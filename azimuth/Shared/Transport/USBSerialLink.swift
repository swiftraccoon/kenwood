// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// Version 1 of the Azimuth app ↔ DriverKit external-method ABI.
///
/// Raw values and call shapes are append-only. Never reorder or repurpose an
/// existing selector: released apps and dexts may be upgraded independently.
public enum AzimuthUSBSelectorV1: UInt32, CaseIterable {
    /// structureInput 1...4096 bytes -> bulk OUT queue.
    case write = 0
    /// structureOutput 0...4096 bytes; zero means the RX ring is empty.
    case read = 1
    /// Async, zero-payload, one-shot data/link-state doorbell.
    case armDoorbell = 2
    /// Four scalar outputs: buffered, overflow, link-up, doorbell-armed.
    case status = 3
    /// structureOutput of 32-byte `AzimuthUSBDextLogEntry` records.
    case copyLog = 4
}

/// ABI v2 appends one operation; all v1 raw values and call shapes stay fixed.
public enum AzimuthUSBSelectorV2: UInt32, CaseIterable {
    /// One scalar input, restricted to 9600 or 115200 baud.
    case setBaudRate = 5
}

public enum AzimuthUSBABIV1 {
    public static let version: UInt32 = 1
    public static let maximumTransferBytes = 4096
    public static let statusScalarCount = 4
    public static let logEntryBytes = 32
}

public enum AzimuthUSBABIV2 {
    public static let version: UInt32 = 2
    public static let supportedBaudRates: Set<UInt32> = [9_600, 115_200]
}

public struct AzimuthUSBDextStatus: Sendable, Equatable {
    public let rxBuffered: UInt64
    public let rxOverflowBytes: UInt64
    public let linkUp: Bool
    public let doorbellArmed: Bool

    public var text: String {
        "dext: rxBuffered=\(rxBuffered) overflow=\(rxOverflowBytes) "
            + "linkUp=\(linkUp) doorbellArmed=\(doorbellArmed)"
    }
}

/// A diagnostic event copied from the dext. Its explicit little-endian wire
/// decoding avoids depending on Swift struct layout or host alignment.
public struct AzimuthUSBDextLogEntry: Sendable, Equatable {
    public let sequence: UInt32
    public let event: UInt32
    public let code: Int64
    public let a: UInt64
    public let b: UInt64

    public static let wireSize = AzimuthUSBABIV1.logEntryBytes

    public init?(bytes: ArraySlice<UInt8>) {
        guard bytes.count >= Self.wireSize else { return nil }
        let base = bytes.startIndex
        func u32(_ offset: Int) -> UInt32 {
            (0..<4).reduce(0) {
                $0 | UInt32(bytes[base + offset + $1]) << (8 * UInt32($1))
            }
        }
        func u64(_ offset: Int) -> UInt64 {
            (0..<8).reduce(0) {
                $0 | UInt64(bytes[base + offset + $1]) << (8 * UInt64($1))
            }
        }
        sequence = u32(0)
        event = u32(4)
        code = Int64(bitPattern: u64(8))
        a = u64(16)
        b = u64(24)
    }

    public var text: String {
        let result = azimuthKernReturnString(Int32(truncatingIfNeeded: code))
        switch event {
        case 1: return "#\(sequence) endpoint 0x\(String(a, radix: 16)) type=\(b)"
        case 2: return "#\(sequence) buffers mapped in=\(a == 1) out=\(b == 1)"
        case 3:
            let state = a == 0 ? "none" : "0x\(String(a, radix: 16))"
            return "#\(sequence) SET_CONTROL_LINE_STATE \(state) -> \(result)"
        case 4:
            return "#\(sequence) started IN=0x\(String(a, radix: 16)) "
                + "OUT=0x\(String(b, radix: 16))"
        case 5: return "#\(sequence) start failed stage=\(a) \(result)"
        case 6:
            let values = ["armed", "fired immediately", "aborted: link down"]
            let outcome = a < UInt64(values.count) ? values[Int(a)] : "unknown \(a)"
            return "#\(sequence) doorbell \(outcome)"
        case 7: return "#\(sequence) doorbell fired \(result)"
        case 8: return "#\(sequence) bulk-IN error \(result) bytes=\(a) streak=\(b)"
        case 9: return "#\(sequence) RX edge bytes=\(a)"
        case 10: return "#\(sequence) enqueue \(result) bytes=\(a) queued=\(b)"
        case 11:
            let timeout = b == 0 ? "unbounded" : "\(b)ms"
            return "#\(sequence) TX submit \(result) bytes=\(a) timeout=\(timeout)"
        case 12:
            let reasons = ["unknown", "stop", "IN errors", "IN stall re-arm",
                           "IN re-arm", "OUT submit", "OUT buffer", "OUT completion",
                           "OUT short completion", "OUT session reset",
                           "OUT valid length", "client session setup"]
            let reason = a < UInt64(reasons.count) ? reasons[Int(a)] : "unknown \(a)"
            return "#\(sequence) link failed: \(reason)"
        case 13: return "#\(sequence) read copied \(b)/\(a)"
        case 14: return "#\(sequence) SET_LINE_CODING -> \(result)"
        case 15: return "#\(sequence) set baud \(a) -> \(result)"
        case 16:
            return "#\(sequence) TX complete \(result) bytes=\(a)/\(b)"
        case 17:
            return "#\(sequence) client attached; TX reset \(result) "
                + "active=\(a) queued=\(b)"
        case 18:
            return "#\(sequence) client detached; TX reset \(result) "
                + "active=\(a) queued=\(b)"
        case 19:
            return "#\(sequence) session SET_LINE_CODING baud=\(a) -> \(result)"
        case 20:
            return "#\(sequence) session SET_CONTROL_LINE_STATE DTR|RTS -> \(result)"
        case 21:
            let stages = ["initial", "re-arm", "stall re-arm"]
            let stage = a < UInt64(stages.count) ? stages[Int(a)] : "unknown \(a)"
            return "#\(sequence) bulk-IN submit \(stage) \(result) bytes=\(b)"
        case 22:
            return "#\(sequence) bulk-IN complete \(result) bytes=\(a) priorStreak=\(b)"
        case 23:
            return "#\(sequence) session SET_CONTROL_LINE_STATE none -> \(result)"
        default: return "#\(sequence) event=\(event) code=\(code) a=\(a) b=\(b)"
        }
    }
}

public func azimuthKernReturnString(_ value: Int32) -> String {
    switch value {
    case 0: return "OK"
    case -308: return "MIG_SERVER_DIED"
    case -305: return "MIG_NO_REPLY"
    case -304: return "MIG_BAD_ARGUMENTS"
    case Int32(bitPattern: 0xE00002BE): return "kIOReturnNoResources"
    case Int32(bitPattern: 0xE00002C0): return "kIOReturnNoDevice"
    case Int32(bitPattern: 0xE00002C1): return "kIOReturnNotPrivileged"
    case Int32(bitPattern: 0xE00002C2): return "kIOReturnBadArgument"
    case Int32(bitPattern: 0xE00002C5): return "kIOReturnExclusiveAccess"
    case Int32(bitPattern: 0xE00002CD): return "kIOReturnNotOpen"
    case Int32(bitPattern: 0xE00002D5): return "kIOReturnBusy"
    case Int32(bitPattern: 0xE00002D6): return "kIOReturnTimeout"
    case Int32(bitPattern: 0xE00002D7): return "kIOReturnOffline"
    case Int32(bitPattern: 0xE00002D8): return "kIOReturnNotReady"
    case Int32(bitPattern: 0xE00002D9): return "kIOReturnNotAttached"
    case Int32(bitPattern: 0xE00002E2): return "kIOReturnNotPermitted"
    case Int32(bitPattern: 0xE00002E7): return "kIOReturnUnderrun"
    case Int32(bitPattern: 0xE00002E8): return "kIOReturnOverrun"
    case Int32(bitPattern: 0xE00002E9): return "kIOReturnDeviceError"
    case Int32(bitPattern: 0xE00002EB): return "kIOReturnAborted"
    case Int32(bitPattern: 0xE00002ED): return "kIOReturnNotResponding"
    default:
        return "0x\(String(UInt32(bitPattern: value), radix: 16, uppercase: true)) (\(value))"
    }
}

public enum AzimuthUSBLinkError: Error, Sendable, Equatable {
    case unsupportedEnvironment(String)
    case serviceNotFound
    case openFailed(code: Int32)
    case notOpen
    case invalidTransferLength(Int)
    case unsupportedBaudRate(UInt32)
    case backpressure
    case callFailed(code: Int32)
    case systemCall(operation: String, code: Int32)
}

/// Synchronous, non-blocking seam used by `AzimuthUSBSerialTransport`.
/// The iPad implementation talks to a DriverKit user client; macOS talks to
/// the system CDC tty. Both expose the same one-shot doorbell/drain contract.
public protocol AzimuthUSBSerialLink: Sendable {
    func servicePresent() -> Bool
    /// Time allowed for an approved iPadOS dext to publish its data and
    /// companion control services after attach. Native tty discovery does not
    /// expose a separate control service.
    var serviceRegistrationWaitNanoseconds: UInt64 { get }
    func open() throws
    func close()
    func setBaudRate(baud: UInt32) throws
    func write(_ bytes: [UInt8]) throws
    func drain(maxBytes: Int) throws -> [UInt8]
    func armDoorbell(onFire: @escaping @Sendable (Bool) -> Void) throws
    func status() throws -> AzimuthUSBDextStatus?
    func dextLog() throws -> [AzimuthUSBDextLogEntry]
    func commServicePresent() -> Bool?
    var connectionDescription: String { get }
}

public extension AzimuthUSBSerialLink {
    var serviceRegistrationWaitNanoseconds: UInt64 { 0 }
    func status() throws -> AzimuthUSBDextStatus? { nil }
    func dextLog() throws -> [AzimuthUSBDextLogEntry] { [] }
    func commServicePresent() -> Bool? { nil }
    var connectionDescription: String { "USB-C" }
}
