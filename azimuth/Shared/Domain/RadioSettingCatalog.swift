// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

enum RadioSettingGroup: String, CaseIterable, Identifiable, Hashable, Sendable {
    case radio
    case display
    case audio
    case gps
    case aprs
    case digitalVoice
    case memory
    case connectivity

    var id: String { rawValue }

    var title: String {
        switch self {
        case .radio: return "Radio"
        case .display: return "Display"
        case .audio: return "Audio"
        case .gps: return "GPS"
        case .aprs: return "APRS"
        case .digitalVoice: return "Digital Voice"
        case .memory: return "Memory"
        case .connectivity: return "Connections"
        }
    }

    var symbol: String {
        switch self {
        case .radio: return "antenna.radiowaves.left.and.right"
        case .display: return "sun.max"
        case .audio: return "speaker.wave.2"
        case .gps: return "location"
        case .aprs: return "point.3.connected.trianglepath.dotted"
        case .digitalVoice: return "waveform"
        case .memory: return "square.stack.3d.up"
        case .connectivity: return "cable.connector"
        }
    }
}

struct RadioSettingOption: Identifiable, Hashable, Sendable {
    let rawValue: Int
    let label: String

    var id: Int { rawValue }
}

enum RadioTextEncoding: Hashable, Sendable {
    case utf8
    case ascii

    var title: String {
        switch self {
        case .utf8: return "UTF-8"
        case .ascii: return "ASCII"
        }
    }

    func accepts(_ value: String) -> Bool {
        switch self {
        case .utf8: return true
        case .ascii: return value.utf8.allSatisfy { $0 < 0x80 }
        }
    }
}

/// A lossless bridge between a user-facing decimal value and the integer the
/// D75 actually stores. `ProposedSettingValue.integer` always remains the raw
/// value so snapshots and compare-and-exchange writes never compare rounded
/// display text.
struct RadioScaledIntegerDomain: Hashable, Sendable {
    let rawRange: ClosedRange<Int>
    let inputUnit: String
    let numerator: Int64
    let denominator: Int64
    let displayDecimalPlaces: Int

    var isValid: Bool {
        !inputUnit.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && numerator > 0
            && denominator > 0
            && (0...18).contains(displayDecimalPlaces)
    }

    var summary: String {
        guard let bounds = editableDisplayBounds else {
            return "Invalid display transform"
        }
        let step = Self.fixedDecimal(Self.displayStep(decimalPlaces: displayDecimalPlaces),
                                     decimalPlaces: displayDecimalPlaces)
        return "\(bounds.lower)–\(bounds.upper) \(inputUnit), precision \(step) \(inputUnit)"
    }

    /// Formats a stored integer in the schema-declared display unit and
    /// precision. Formatting never mutates or replaces the raw value.
    func displayText(rawValue: Int, includesUnit: Bool = true) -> String? {
        guard isValid, rawRange.contains(rawValue) else { return nil }
        let display = roundedDisplayValue(rawValue: rawValue)
        let number = Self.fixedDecimal(display, decimalPlaces: displayDecimalPlaces)
        return includesUnit ? "\(number) \(inputUnit)" : number
    }

    /// Parses a display-unit decimal and returns the exact encoded raw value.
    /// Values with more precision than the schema declares, values which do
    /// not round-trip at that precision, and values outside the raw range are
    /// rejected.
    func rawValue(displayText: String) -> Int? {
        guard isValid, let display = parseDisplayDecimal(displayText) else { return nil }
        return encodedRawValue(display: display)
    }

    private var editableDisplayBounds: (lower: String, upper: String)? {
        guard isValid else { return nil }
        let step = Self.displayStep(decimalPlaces: displayDecimalPlaces)
        var lower = roundedDisplayValue(rawValue: rawRange.lowerBound)
        var upper = roundedDisplayValue(rawValue: rawRange.upperBound)

        // A raw endpoint can round to display text that would encode one unit
        // beyond the raw domain (for example raw 9999 displays as 60.0, while
        // entering 60.0 encodes 10000). Advertise only values users can stage.
        for _ in 0..<32 where encodedRawValue(display: lower) == nil { lower += step }
        for _ in 0..<32 where encodedRawValue(display: upper) == nil { upper -= step }
        guard encodedRawValue(display: lower) != nil,
              encodedRawValue(display: upper) != nil,
              lower <= upper else { return nil }
        return (
            Self.fixedDecimal(lower, decimalPlaces: displayDecimalPlaces),
            Self.fixedDecimal(upper, decimalPlaces: displayDecimalPlaces)
        )
    }

    private func parseDisplayDecimal(_ input: String) -> Decimal? {
        var value = input.trimmingCharacters(in: .whitespacesAndNewlines)
        let units = [inputUnit, inputUnit.hasSuffix("s") ? String(inputUnit.dropLast()) : inputUnit]
        for unit in units.sorted(by: { $0.count > $1.count }) where !unit.isEmpty {
            if value.lowercased().hasSuffix(unit.lowercased()) {
                value.removeLast(unit.count)
                value = value.trimmingCharacters(in: .whitespacesAndNewlines)
                break
            }
        }

        if let separator = Locale.current.decimalSeparator, separator != "." {
            value = value.replacingOccurrences(of: separator, with: ".")
        }
        guard !value.isEmpty,
              value.contains(where: \.isNumber),
              value.allSatisfy({ $0.isNumber || $0 == "." || $0 == "+" || $0 == "-" }),
              value.filter({ $0 == "." }).count <= 1,
              value.dropFirst().allSatisfy({ $0 != "+" && $0 != "-" }),
              let decimal = Decimal(
                string: value,
                locale: Locale(identifier: "en_US_POSIX")
              ) else { return nil }

        let declared = Self.round(decimal, decimalPlaces: displayDecimalPlaces)
        guard decimal == declared else { return nil }
        return decimal
    }

    private func encodedRawValue(display: Decimal) -> Int? {
        let scaled = display * Decimal(numerator) / Decimal(denominator)
        let rounded = Self.round(scaled, decimalPlaces: 0)
        guard rounded >= Decimal(rawRange.lowerBound),
              rounded <= Decimal(rawRange.upperBound),
              let raw = Int(NSDecimalNumber(decimal: rounded).stringValue),
              rawRange.contains(raw),
              roundedDisplayValue(rawValue: raw) == display else { return nil }
        return raw
    }

    private func roundedDisplayValue(rawValue: Int) -> Decimal {
        let decoded = Decimal(rawValue) * Decimal(denominator) / Decimal(numerator)
        return Self.round(decoded, decimalPlaces: displayDecimalPlaces)
    }

    private static func displayStep(decimalPlaces: Int) -> Decimal {
        var divisor = Decimal(1)
        for _ in 0..<decimalPlaces { divisor *= 10 }
        return Decimal(1) / divisor
    }

    private static func round(_ value: Decimal, decimalPlaces: Int) -> Decimal {
        var value = value
        var result = Decimal()
        NSDecimalRound(&result, &value, decimalPlaces, .plain)
        return result
    }

    private static func fixedDecimal(_ value: Decimal, decimalPlaces: Int) -> String {
        var text = NSDecimalNumber(decimal: value).stringValue
        guard decimalPlaces > 0 else {
            return text.components(separatedBy: ".").first ?? text
        }
        if let separator = text.firstIndex(of: ".") {
            let existing = text.distance(from: text.index(after: separator), to: text.endIndex)
            if existing < decimalPlaces {
                text.append(String(repeating: "0", count: decimalPlaces - existing))
            }
        } else {
            text += "." + String(repeating: "0", count: decimalPlaces)
        }
        return text
    }
}

enum RadioSettingDomain: Hashable, Sendable {
    case boolean
    case choice([RadioSettingOption])
    case integer(range: ClosedRange<Int>, step: Int, unit: String?)
    case scaledInteger(RadioScaledIntegerDomain)
    case text(maxLength: Int, encoding: RadioTextEncoding)
    case data(description: String)

    var kindTitle: String {
        switch self {
        case .boolean: return "On / Off"
        case .choice: return "Choice"
        case .integer: return "Number"
        case .scaledInteger(let scale): return "Number · \(scale.inputUnit.capitalized)"
        case .text: return "Text"
        case .data: return "Data"
        }
    }

    var symbol: String {
        switch self {
        case .boolean: return "switch.2"
        case .choice: return "list.bullet.circle"
        case .integer, .scaledInteger: return "number.circle"
        case .text: return "textformat"
        case .data: return "shippingbox"
        }
    }

    var summary: String {
        switch self {
        case .boolean:
            return "Off or On"
        case .choice(let options):
            return "\(options.count) choices"
        case .integer(let range, let step, let unit):
            let suffix = unit.map { " \($0)" } ?? ""
            return "\(range.lowerBound)–\(range.upperBound)\(suffix), step \(step)"
        case .scaledInteger(let scale):
            return scale.summary
        case .text(let maxLength, let encoding):
            return "Up to \(maxLength) bytes · \(encoding.title)"
        case .data(let description):
            return description
        }
    }

    func accepts(_ value: ProposedSettingValue) -> Bool {
        switch (self, value) {
        case (.boolean, .boolean):
            return true
        case (.choice(let options), .choice(let rawValue)):
            return options.contains { $0.rawValue == rawValue }
        case (.integer(let range, let step, _), .integer(let value)):
            return range.contains(value) && (value - range.lowerBound).isMultiple(of: step)
        case (.scaledInteger(let scale), .integer(let rawValue)):
            return scale.isValid && scale.rawRange.contains(rawValue)
        case (.text(let maxLength, let encoding), .text(let value)):
            return value.utf8.count <= maxLength && encoding.accepts(value)
        case (.data, .data):
            // Assistant and generic UI cannot author binary settings. A
            // specialized editor must validate their exact format.
            return false
        default:
            return false
        }
    }

    /// Parse the user-facing representation while retaining raw storage for
    /// executable proposals.
    func parseDisplayValue(_ rawValue: String) -> ProposedSettingValue? {
        let value = rawValue.trimmingCharacters(in: .whitespacesAndNewlines)
        switch self {
        case .boolean:
            switch value.lowercased() {
            case "on", "true", "enabled", "yes": return .boolean(true)
            case "off", "false", "disabled", "no": return .boolean(false)
            default: return nil
            }
        case .choice(let options):
            guard let option = options.first(where: {
                $0.label.compare(value, options: [.caseInsensitive, .diacriticInsensitive])
                    == .orderedSame
                    || String($0.rawValue) == value
            }) else { return nil }
            return .choice(rawValue: option.rawValue)
        case .integer:
            guard let integer = Int(value) else { return nil }
            return .integer(integer)
        case .scaledInteger(let scale):
            guard let raw = scale.rawValue(displayText: value) else { return nil }
            return .integer(raw)
        case .text:
            // Text is data, not a token. Preserve exactly what the review UI
            // shows, including leading/trailing whitespace and an empty value.
            return .text(rawValue)
        case .data:
            return nil
        }
    }

    /// Format a typed raw value for review without weakening its identity in
    /// the snapshot or write path.
    func displayText(for value: ProposedSettingValue) -> String? {
        switch (self, value) {
        case (.choice(let options), .choice(let rawValue)):
            return options.first(where: { $0.rawValue == rawValue })?.label ?? String(rawValue)
        case (.integer(_, _, let unit), .integer(let value)):
            return "\(value)\(unit.map { " \($0)" } ?? "")"
        case (.scaledInteger(let scale), .integer(let rawValue)):
            return scale.displayText(rawValue: rawValue)
        default:
            return accepts(value) ? value.displayText : nil
        }
    }
}

struct RadioSettingDefinition: Identifiable, Hashable, Sendable {
    let id: String
    let group: RadioSettingGroup
    let title: String
    let summary: String
    let domain: RadioSettingDomain
    let menuNumbers: [String]
    let schemaReference: String?
    let requiresRestart: Bool
    let isSpecializedEditor: Bool

    var menuNumberLabel: String? {
        guard !menuNumbers.isEmpty else { return nil }
        let prefix = menuNumbers.count == 1 ? "Menu" : "Menus"
        return "\(prefix) \(menuNumbers.joined(separator: " / "))"
    }
}

enum ProposedSettingValue: Hashable, Sendable {
    case boolean(Bool)
    case choice(rawValue: Int)
    case integer(Int)
    case text(String)
    case data(Data)

    var displayText: String {
        switch self {
        case .boolean(let enabled): return enabled ? "On" : "Off"
        case .choice(let rawValue): return "Choice \(rawValue)"
        case .integer(let value): return String(value)
        case .text(let value): return value
        case .data(let data): return "\(data.count) bytes"
        }
    }
}

enum RadioSettingCatalogSource: Equatable, Sendable {
    case designPreview
    case reviewedSchema(version: String)
    case radioSnapshot(device: String, capturedAt: Date)

    var isLive: Bool {
        if case .radioSnapshot = self { return true }
        return false
    }
}

struct RadioSettingCatalog: Equatable, Sendable {
    let source: RadioSettingCatalogSource
    let definitions: [RadioSettingDefinition]

    var groups: [RadioSettingGroup] {
        let present = Set(definitions.map(\.group))
        return RadioSettingGroup.allCases.filter(present.contains)
    }

    func definition(id: String) -> RadioSettingDefinition? {
        definitions.first { $0.id == id }
    }

    func filtered(query: String, group: RadioSettingGroup?) -> [RadioSettingDefinition] {
        let terms = query.searchTerms
        return definitions
            .filter { definition in
                guard group == nil || definition.group == group else { return false }
                guard !terms.isEmpty else { return true }
                let haystack = definition.searchableText.normalizedForSearch
                return terms.allSatisfy(haystack.contains)
            }
            .sorted { lhs, rhs in
                let order = lhs.title.localizedStandardCompare(rhs.title)
                return order == .orderedSame ? lhs.id < rhs.id : order == .orderedAscending
            }
    }

    func sections(query: String, group: RadioSettingGroup?) -> [RadioSettingSection] {
        let matches = filtered(query: query, group: group)
        let grouped = Dictionary(grouping: matches, by: \.group)
        return groups.compactMap { settingGroup in
            guard let definitions = grouped[settingGroup], !definitions.isEmpty else { return nil }
            return RadioSettingSection(group: settingGroup, definitions: definitions)
        }
    }

    /// Lexical retrieval for the on-device planner. Generated IDs are still
    /// validated against the entire catalog after generation.
    func assistantCandidates(for request: String, limit: Int = 48) -> [RadioSettingDefinition] {
        let expanded = request + " " + Self.aliasTerms(for: request)
        let terms = expanded.searchTerms
        let scored = definitions.compactMap { definition -> (RadioSettingDefinition, Int)? in
            let haystack = definition.searchableText.normalizedForSearch
            let score = terms.reduce(into: 0) { result, term in
                if definition.id.normalizedForSearch.contains(term) { result += 10 }
                if definition.title.normalizedForSearch.contains(term) { result += 8 }
                if definition.group.title.normalizedForSearch.contains(term) { result += 4 }
                if haystack.contains(term) { result += 2 }
            }
            return score > 0 ? (definition, score) : nil
        }
        .sorted { lhs, rhs in lhs.1 == rhs.1 ? lhs.0.id < rhs.0.id : lhs.1 > rhs.1 }
        .prefix(limit)
        .map(\.0)

        return scored.isEmpty ? Array(definitions.prefix(min(limit, definitions.count))) : scored
    }

    private static func aliasTerms(for request: String) -> String {
        let value = request.normalizedForSearch
        var terms: [String] = []
        let aliases: [(words: [String], expansion: String)] = [
            (["quiet", "silent", "mute"], "beep audio volume"),
            (["night", "bright", "dark"], "display brightness backlight"),
            (["position", "location"], "gps"),
            (["beacon", "packet"], "aprs"),
            (["digital voice", "d star", "dstar"], "digital voice dv gateway"),
        ]
        for alias in aliases where alias.words.contains(where: value.contains) {
            terms.append(alias.expansion)
        }
        return terms.joined(separator: " ")
    }
}

struct RadioSettingSection: Identifiable, Sendable {
    let group: RadioSettingGroup
    let definitions: [RadioSettingDefinition]

    var id: RadioSettingGroup { group }
}

protocol RadioSettingCatalogProviding: Sendable {
    func catalog() async throws -> RadioSettingCatalog
}

struct PreviewRadioSettingCatalogProvider: RadioSettingCatalogProviding {
    func catalog() async throws -> RadioSettingCatalog { .designPreview }
}

extension RadioSettingCatalog {
    /// Curated design data. IDs are intentionally prefixed with `preview` so
    /// they cannot be mistaken for authoritative MCP identifiers or written
    /// back to hardware. The core adapter replaces this catalog wholesale.
    static let designPreview = RadioSettingCatalog(
        source: .designPreview,
        definitions: [
            preview("beep", .audio, "Key Beep", "Audible confirmation for key presses.", .boolean),
            preview("beepVolume", .audio, "Beep Volume", "Level of interface confirmation tones.", .integer(range: 0...10, step: 1, unit: nil)),
            preview("receiveEq", .audio, "Receive Equalizer", "Tone profile applied to received audio.", choices("Flat", "High Boost", "Low Boost")),
            preview("vox", .audio, "VOX", "Voice-operated transmit behavior.", .boolean),
            preview("brightness", .display, "Display Brightness", "Backlight intensity for the radio display.", .integer(range: 1...5, step: 1, unit: nil)),
            preview("theme", .display, "Display Color", "Color treatment used by the radio interface.", choices("White", "Amber", "Green")),
            preview("timeout", .display, "Backlight Timeout", "How long the backlight remains active.", choices("Always", "2 seconds", "5 seconds", "10 seconds")),
            preview("batterySaver", .radio, "Battery Saver", "Receiver sleep interval used to reduce power consumption.", choices("Off", "Short", "Medium", "Long")),
            preview("autoPowerOff", .radio, "Auto Power Off", "Automatically powers down after inactivity.", choices("Off", "30 minutes", "60 minutes", "120 minutes")),
            preview("txPower", .radio, "Transmit Power", "Default transmitter power level.", choices("Low", "Medium", "High")),
            preview("keyLock", .radio, "Key Lock", "Controls which physical keys remain available while locked.", choices("Key", "Frequency", "All")),
            preview("gpsEnabled", .gps, "Built-in GPS", "Enables the receiver used for position features.", .boolean),
            preview("gpsDatum", .gps, "GPS Datum", "Coordinate reference used for reported positions.", choices("WGS-84", "Tokyo")),
            preview("gpsSentence", .gps, "NMEA Sentence Output", "Selects position sentences sent to an external client.", choices("RMC", "GGA", "GLL")),
            preview("aprsCallsign", .aprs, "APRS Callsign", "Station callsign and SSID used for APRS.", .text(maxLength: 9, encoding: .ascii)),
            preview("aprsBeacon", .aprs, "Beacon Method", "Determines when an APRS position beacon is sent.", choices("Manual", "Interval", "SmartBeaconing")),
            preview("aprsInterval", .aprs, "Beacon Interval", "Time between interval-based APRS beacons.", choices("30 seconds", "1 minute", "3 minutes", "5 minutes", "10 minutes")),
            preview("aprsComment", .aprs, "Status Text", "Short text included with APRS reports.", .text(maxLength: 42, encoding: .utf8)),
            preview("myCallsign", .digitalVoice, "My Callsign", "Callsign placed in D-STAR digital voice headers.", .text(maxLength: 8, encoding: .ascii)),
            preview("gatewayMode", .digitalVoice, "DV Gateway Mode", "Selects local digital voice gateway behavior.", choices("Off", "Access Point", "Terminal")),
            preview("digitalSquelch", .digitalVoice, "Digital Squelch", "Filters received digital voice by code or callsign.", choices("Off", "Code", "Callsign")),
            preview("memoryName", .memory, "Memory Name", "Label shown for a stored channel.", .text(maxLength: 16, encoding: .utf8), specialized: true),
            preview("scanResume", .memory, "Scan Resume", "Behavior after scan activity is detected.", choices("Time", "Carrier", "Seek")),
            preview("usbFunction", .connectivity, "USB Function", "Function presented by the radio's USB connection.", choices("COM + Audio", "Mass Storage"), restart: true),
            preview("bluetooth", .connectivity, "Bluetooth", "Enables the radio's Bluetooth subsystem.", .boolean),
        ]
    )

    private static func preview(
        _ id: String,
        _ group: RadioSettingGroup,
        _ title: String,
        _ summary: String,
        _ domain: RadioSettingDomain,
        specialized: Bool = false,
        restart: Bool = false
    ) -> RadioSettingDefinition {
        RadioSettingDefinition(
            id: "preview.\(group.rawValue).\(id)",
            group: group,
            title: title,
            summary: summary,
            domain: domain,
            menuNumbers: [],
            schemaReference: nil,
            requiresRestart: restart,
            isSpecializedEditor: specialized
        )
    }

    private static func choices(_ labels: String...) -> RadioSettingDomain {
        .choice(labels.enumerated().map { RadioSettingOption(rawValue: $0.offset, label: $0.element) })
    }
}

private extension RadioSettingDefinition {
    var searchableText: String {
        var fields = [id, group.title, title, summary, domain.kindTitle, domain.summary]
        if let menuNumberLabel { fields.append(menuNumberLabel) }
        if let schemaReference { fields.append(schemaReference) }
        if case .choice(let options) = domain {
            fields.append(contentsOf: options.map(\.label))
        }
        return fields.joined(separator: " ")
    }
}

private extension String {
    var normalizedForSearch: String {
        folding(options: [.caseInsensitive, .diacriticInsensitive], locale: .current)
            .replacingOccurrences(of: "_", with: " ")
            .replacingOccurrences(of: ".", with: " ")
            .lowercased()
    }

    var searchTerms: [String] {
        normalizedForSearch
            .split { !$0.isLetter && !$0.isNumber }
            .map(String.init)
            .filter { $0.count > 1 }
    }
}
