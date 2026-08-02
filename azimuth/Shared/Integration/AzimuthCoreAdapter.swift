// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Dispatch
import Foundation
import OSLog

private let azimuthCoreIOLog = Logger(
    subsystem: "org.swiftraccoon.azimuth",
    category: "core-byte-transport"
)

private let azimuthVerboseCoreIOTracing =
    ProcessInfo.processInfo.environment["AZIMUTH_VERBOSE_USB_TRACE"] == "1"

private func azimuthCoreIOTrace(_ message: String) {
    guard azimuthVerboseCoreIOTracing else { return }
    azimuthCoreIOLog.debug("\(message, privacy: .public)")
}

/// The sole callback object handed across the UniFFI boundary. Rust owns the
/// radio protocol while this object keeps platform USB details in Swift.
final class AzimuthCoreByteTransport: ByteTransport, @unchecked Sendable {
    let radioTransport: any AzimuthRadioTransport
    private let operationLock = NSLock()
    private var nextOperationID: UInt64 = 1

    init(radioTransport: any AzimuthRadioTransport) {
        self.radioTransport = radioTransport
    }

    func write(bytes: Data) async throws {
        let operation = takeOperationID()
        let started = DispatchTime.now().uptimeNanoseconds
        azimuthCoreIOTrace(
            "[Azimuth Core I/O] write#\(operation) started bytes=\(bytes.count)"
        )
        do {
            try await radioTransport.write(Array(bytes))
            azimuthCoreIOTrace(
                "[Azimuth Core I/O] write#\(operation) completed bytes=\(bytes.count) "
                    + "elapsedMs=\(Self.elapsedMilliseconds(since: started))"
            )
        } catch {
            Self.logFailure(operation: operation, kind: "write", started: started, error: error)
            throw Self.platformError(error)
        }
    }

    func read(maxLength: UInt32) async throws -> Data {
        let operation = takeOperationID()
        let started = DispatchTime.now().uptimeNanoseconds
        azimuthCoreIOTrace(
            "[Azimuth Core I/O] read#\(operation) started maxBytes=\(maxLength)"
        )
        do {
            let maximum = Int(exactly: maxLength) ?? Int.max
            let bytes = try await radioTransport.read(maxBytes: maximum)
            azimuthCoreIOTrace(
                "[Azimuth Core I/O] read#\(operation) completed bytes=\(bytes.count) "
                    + "eofOrCancelled=\(bytes.isEmpty) "
                    + "elapsedMs=\(Self.elapsedMilliseconds(since: started))"
            )
            return Data(bytes)
        } catch {
            Self.logFailure(operation: operation, kind: "read", started: started, error: error)
            throw Self.platformError(error)
        }
    }

    func close() async throws {
        let operation = takeOperationID()
        let started = DispatchTime.now().uptimeNanoseconds
        azimuthCoreIOLog.notice("[Azimuth Core I/O] close#\(operation, privacy: .public) started")
        await radioTransport.close()
        azimuthCoreIOLog.notice("[Azimuth Core I/O] close#\(operation, privacy: .public) completed elapsedMs=\(Self.elapsedMilliseconds(since: started), privacy: .public)")
    }

    func reopen() async throws {
        let operation = takeOperationID()
        let started = DispatchTime.now().uptimeNanoseconds
        azimuthCoreIOLog.notice("[Azimuth Core I/O] reopen#\(operation, privacy: .public) started")
        await radioTransport.close()
        do {
            try await radioTransport.open()
            azimuthCoreIOLog.notice("[Azimuth Core I/O] reopen#\(operation, privacy: .public) completed elapsedMs=\(Self.elapsedMilliseconds(since: started), privacy: .public)")
        } catch {
            Self.logFailure(operation: operation, kind: "reopen", started: started, error: error)
            throw Self.platformError(error)
        }
    }

    func setBaudRate(baud: UInt32) throws {
        let operation = takeOperationID()
        let started = DispatchTime.now().uptimeNanoseconds
        azimuthCoreIOLog.notice("[Azimuth Core I/O] baud#\(operation, privacy: .public) started value=\(baud, privacy: .public)")
        do {
            try radioTransport.setBaudRate(baud: baud)
            azimuthCoreIOLog.notice("[Azimuth Core I/O] baud#\(operation, privacy: .public) completed value=\(baud, privacy: .public) elapsedMs=\(Self.elapsedMilliseconds(since: started), privacy: .public)")
        } catch {
            Self.logFailure(operation: operation, kind: "baud", started: started, error: error)
            throw Self.platformError(error)
        }
    }

    private func takeOperationID() -> UInt64 {
        operationLock.withLock {
            let operation = nextOperationID
            nextOperationID &+= 1
            return operation
        }
    }

    private static func elapsedMilliseconds(since started: UInt64) -> UInt64 {
        (DispatchTime.now().uptimeNanoseconds &- started) / 1_000_000
    }

    private static func logFailure(
        operation: UInt64,
        kind: String,
        started: UInt64,
        error: Error
    ) {
        let errorType = String(reflecting: type(of: error))
        azimuthCoreIOLog.error("[Azimuth Core I/O] \(kind, privacy: .public)#\(operation, privacy: .public) failed type=\(errorType, privacy: .public) elapsedMs=\(elapsedMilliseconds(since: started), privacy: .public) detail=\(error.localizedDescription, privacy: .private)")
    }

    private static func platformError(_ error: Error) -> ByteTransportError {
        if let transportError = error as? ByteTransportError { return transportError }
        return .Platform(message: String(describing: error))
    }
}

enum AzimuthCoreIntegrationError: LocalizedError, Equatable {
    case catalogCount(expected: Int, actual: Int)
    case duplicateSettingID(String)
    case unsupportedSetting(String, reason: String)
    case invalidScreen(String)

    var errorDescription: String? {
        switch self {
        case .catalogCount(let expected, let actual):
            return "AzimuthCore supplied \(actual) settings; schema 3 requires \(expected)."
        case .duplicateSettingID(let id):
            return "AzimuthCore supplied duplicate setting ID \(id)."
        case .unsupportedSetting(let id, let reason):
            return "Setting \(id) could not be represented safely: \(reason)"
        case .invalidScreen(let reason):
            return "AzimuthCore supplied an invalid screen frame: \(reason)"
        }
    }
}

/// Immutable, authoritative projection of the generated MCP-D75 schema.
struct AzimuthCoreSettingSchema: Sendable {
    static let expectedSettingCount = 400

    let records: [SettingRecord]
    let recordsByID: [String: SettingRecord]
    let productCatalog: RadioSettingCatalog

    var deferredBlobCount: Int {
        records.lazy.filter(\.isBlob).count
    }

    var scalarSettingCount: Int {
        records.count - deferredBlobCount
    }

    init(records: [SettingRecord] = settingCatalog()) throws {
        guard records.count == Self.expectedSettingCount else {
            throw AzimuthCoreIntegrationError.catalogCount(
                expected: Self.expectedSettingCount,
                actual: records.count
            )
        }

        var indexed: [String: SettingRecord] = [:]
        var definitions: [RadioSettingDefinition] = []
        indexed.reserveCapacity(records.count)
        definitions.reserveCapacity(records.count)
        for record in records {
            guard indexed.updateValue(record, forKey: record.id) == nil else {
                throw AzimuthCoreIntegrationError.duplicateSettingID(record.id)
            }
            definitions.append(try Self.definition(for: record))
        }

        self.records = records
        recordsByID = indexed
        productCatalog = RadioSettingCatalog(
            source: .reviewedSchema(version: "MCP-D75 schema 3"),
            definitions: definitions
        )
    }

    private static func definition(for record: SettingRecord) throws -> RadioSettingDefinition {
        let mapped = try domain(for: record)
        return RadioSettingDefinition(
            id: record.id,
            group: group(for: record),
            title: record.displayName,
            summary: summary(for: record, specialized: mapped.specialized),
            domain: mapped.domain,
            menuNumbers: THD75MenuNumberIndex.numbers(for: record.id),
            schemaReference: "MCP-D75 \(menuName(record.menu)) • 0x\(String(record.offset, radix: 16, uppercase: true))",
            requiresRestart: false,
            isSpecializedEditor: mapped.specialized
        )
    }

    private static func domain(
        for record: SettingRecord
    ) throws -> (domain: RadioSettingDomain, specialized: Bool) {
        if record.presentation == .scaledInteger {
            guard record.valueKind == .unsignedInteger else {
                throw AzimuthCoreIntegrationError.unsupportedSetting(
                    record.id,
                    reason: "a scaled field must use unsigned integer storage"
                )
            }
            guard let minimum = record.unsignedMin,
                  let maximum = record.unsignedMax,
                  let lower = Int(exactly: minimum),
                  let upper = Int(exactly: maximum),
                  lower <= upper else {
                throw AzimuthCoreIntegrationError.unsupportedSetting(
                    record.id,
                    reason: "its scaled raw range cannot be represented on this platform"
                )
            }
            guard let transform = record.storageTransform else {
                throw AzimuthCoreIntegrationError.unsupportedSetting(
                    record.id,
                    reason: "scaled presentation is missing its storage transform"
                )
            }
            let scaled = RadioScaledIntegerDomain(
                rawRange: lower...upper,
                inputUnit: transform.inputUnit,
                numerator: transform.numerator,
                denominator: transform.denominator,
                displayDecimalPlaces: Int(transform.displayDecimalPlaces)
            )
            guard scaled.isValid else {
                throw AzimuthCoreIntegrationError.unsupportedSetting(
                    record.id,
                    reason: "its generated display transform is invalid"
                )
            }
            return (.scaledInteger(scaled), false)
        }

        let presentationNeedsSpecializedEditor = record.presentation == .blob
        switch record.valueKind {
        case .boolean:
            return (.boolean, presentationNeedsSpecializedEditor)
        case .choice:
            var choices: [RadioSettingOption] = []
            var seen: Set<UInt64> = []
            for option in record.options where seen.insert(option.rawValue).inserted {
                guard let raw = Int(exactly: option.rawValue) else {
                    return (
                        .data(description: "Choice values exceed this platform's integer range."),
                        true
                    )
                }
                let label = option.label ?? humanize(option.member)
                choices.append(RadioSettingOption(rawValue: raw, label: label))
            }
            for allowed in record.allowedValues where seen.insert(allowed).inserted {
                guard let raw = Int(exactly: allowed) else {
                    return (
                        .data(description: "Choice values exceed this platform's integer range."),
                        true
                    )
                }
                choices.append(RadioSettingOption(rawValue: raw, label: String(raw)))
            }
            guard !choices.isEmpty else {
                throw AzimuthCoreIntegrationError.unsupportedSetting(
                    record.id,
                    reason: "the schema declared a choice without any allowed values"
                )
            }
            return (.choice(choices), presentationNeedsSpecializedEditor)
        case .unsignedInteger:
            guard let minimum = record.unsignedMin,
                  let maximum = record.unsignedMax,
                  let lower = Int(exactly: minimum),
                  let upper = Int(exactly: maximum),
                  lower <= upper else {
                return (
                    .data(description: "Unsigned range requires a specialized numeric editor."),
                    true
                )
            }
            return (
                .integer(range: lower...upper, step: 1, unit: nil),
                presentationNeedsSpecializedEditor
            )
        case .signedInteger:
            guard let minimum = record.signedMin,
                  let maximum = record.signedMax,
                  let lower = Int(exactly: minimum),
                  let upper = Int(exactly: maximum),
                  lower <= upper else {
                return (
                    .data(description: "Signed range requires a specialized numeric editor."),
                    true
                )
            }
            return (
                .integer(range: lower...upper, step: 1, unit: nil),
                presentationNeedsSpecializedEditor
            )
        case .text:
            guard let length = Int(exactly: record.byteLength) else {
                throw AzimuthCoreIntegrationError.unsupportedSetting(
                    record.id,
                    reason: "text width exceeds this platform's integer range"
                )
            }
            guard let textEncoding = record.textEncoding else {
                throw AzimuthCoreIntegrationError.unsupportedSetting(
                    record.id,
                    reason: "a text field is missing its generated encoding constraint"
                )
            }
            let encoding: RadioTextEncoding = switch textEncoding {
            case .utf8: .utf8
            case .memoryMapAscii: .ascii
            }
            return (
                .text(maxLength: length, encoding: encoding),
                presentationNeedsSpecializedEditor
            )
        case .bytes:
            return (
                .data(description: "Exact \(record.byteLength)-byte radio data."),
                true
            )
        }
    }

    private static func summary(for record: SettingRecord, specialized: Bool) -> String {
        switch record.presentation {
        case .scaledInteger:
            if let transform = record.storageTransform {
                return "Set this authoritative \(menuName(record.menu)) coordinate component in \(transform.inputUnit); Azimuth preserves the exact encoded D75 value."
            }
            return "Set this authoritative scaled MCP-D75 value in its declared display unit."
        case .blob:
            return "Authoritative \(record.byteLength)-byte MCP-D75 content; use its specialized editor."
        case .direct:
            if specialized {
                return "Authoritative MCP-D75 value requiring a format-specific editor."
            }
            switch record.valueKind {
            case .boolean: return "Enable or disable this authoritative MCP-D75 option."
            case .choice: return "Select an authoritative MCP-D75 value."
            case .unsignedInteger, .signedInteger: return "Set the authoritative MCP-D75 numeric value."
            case .text: return "Set the authoritative MCP-D75 text value."
            case .bytes: return "Edit this authoritative MCP-D75 data value."
            }
        }
    }

    private static func menuName(_ menu: SettingMenu) -> String {
        switch menu {
        case .radio: return "Radio"
        case .gps: return "GPS"
        case .aprs: return "APRS"
        case .dv: return "D-STAR"
        }
    }

    private static func group(for record: SettingRecord) -> RadioSettingGroup {
        switch record.menu {
        case .gps: return .gps
        case .aprs: return .aprs
        case .dv: return .digitalVoice
        case .radio:
            let local = record.id.split(separator: ".", maxSplits: 1).last
                .map(String.init)?.lowercased() ?? record.id.lowercased()
            if containsAny(local, [
                "display", "backlight", "brightness", "background", "lcd", "meter",
            ]) {
                return .display
            }
            if containsAny(local, [
                "audio", "beep", "volume", "equalizer", "eqlevel", "voice", "mic",
                "vox", "earphone", "cw", "ssb", "amhighcut",
            ]) {
                return .audio
            }
            if containsAny(local, [
                "bluetooth", "usb", "interface", "remotecontorol", "kissmode",
            ]) {
                return .connectivity
            }
            if containsAny(local, [
                "scan", "recall", "recording", "qsolog", "grouplink",
            ]) {
                return .memory
            }
            return .radio
        }
    }

    private static func containsAny(_ value: String, _ terms: [String]) -> Bool {
        terms.contains(where: value.contains)
    }

    private static func humanize(_ identifier: String) -> String {
        var result = ""
        var previous: Character?
        for character in identifier {
            if character == "_" || character == "." {
                if !result.hasSuffix(" ") { result.append(" ") }
            } else {
                if character.isUppercase,
                   let previous,
                   previous.isLowercase || previous.isNumber,
                   !result.hasSuffix(" ") {
                    result.append(" ")
                }
                result.append(character)
            }
            previous = character
        }
        return result
    }
}

struct AzimuthCoreCatalogProvider: RadioSettingCatalogProviding {
    private let schema: AzimuthCoreSettingSchema

    init(records: [SettingRecord] = settingCatalog()) throws {
        schema = try AzimuthCoreSettingSchema(records: records)
    }

    func catalog() async throws -> RadioSettingCatalog {
        schema.productCatalog
    }

    /// Synchronous startup snapshot so the shipping app never flashes or
    /// plans against the design-preview catalog while its first task starts.
    var initialCatalog: RadioSettingCatalog {
        schema.productCatalog
    }
}

enum AzimuthCoreSettingValueBridge {
    static func productValue(
        _ value: SettingValue,
        record: SettingRecord
    ) throws -> ProposedSettingValue {
        switch (record.valueKind, value) {
        case (.boolean, .boolean(let value)):
            return .boolean(value)
        case (.choice, .unsigned(let value)):
            guard let value = Int(exactly: value) else { throw overflow(record.id) }
            return .choice(rawValue: value)
        case (.unsignedInteger, .unsigned(let value)):
            guard let value = Int(exactly: value) else { throw overflow(record.id) }
            return .integer(value)
        case (.signedInteger, .signed(let value)):
            guard let value = Int(exactly: value) else { throw overflow(record.id) }
            return .integer(value)
        case (.text, .text(let value)):
            return .text(value)
        case (.bytes, .bytes(let value)):
            return .data(value)
        default:
            throw AzimuthCoreIntegrationError.unsupportedSetting(
                record.id,
                reason: "the live value type does not match the generated schema"
            )
        }
    }

    static func coreValue(
        _ value: ProposedSettingValue,
        record: SettingRecord
    ) throws -> SettingValue {
        switch (record.valueKind, value) {
        case (.boolean, .boolean(let value)):
            return .boolean(value: value)
        case (.choice, .choice(let rawValue)):
            guard rawValue >= 0 else { throw typeMismatch(record.id) }
            return .unsigned(value: UInt64(rawValue))
        case (.unsignedInteger, .integer(let value)):
            guard value >= 0 else { throw typeMismatch(record.id) }
            return .unsigned(value: UInt64(value))
        case (.signedInteger, .integer(let value)):
            return .signed(value: Int64(value))
        case (.text, .text(let value)):
            return .text(value: value)
        case (.bytes, .data(let value)):
            return .bytes(value: value)
        default:
            throw typeMismatch(record.id)
        }
    }

    private static func overflow(_ id: String) -> AzimuthCoreIntegrationError {
        .unsupportedSetting(id, reason: "the radio value exceeds this platform's integer range")
    }

    private static func typeMismatch(_ id: String) -> AzimuthCoreIntegrationError {
        .unsupportedSetting(id, reason: "the proposed value type does not match the generated schema")
    }
}
