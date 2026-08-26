// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import SwiftUI

/// The APRS tab intentionally uses only capabilities exposed by the current
/// automation ABI: a typed live settings snapshot and guarded front-panel
/// taps. Packet history and APRS mode state remain owned by the radio and are
/// never fabricated here.
private struct APRSSettingsDashboard: View {
    @Environment(AzimuthSceneModel.self) private var model
    @State private var isConfirmingBeacon = false

    private var snapshot: APRSConfigurationSnapshot {
        APRSConfigurationSnapshot(
            catalog: model.catalog,
            values: model.radioState.settingValues
        )
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: AzimuthLayout.pageSpacing) {
                overview
                radioActions
                configurationGrid
            }
            .azimuthContentColumn(maxWidth: AzimuthLayout.browseWidth)
        }
        .azimuthPage()
        .accessibilityIdentifier("azimuth.page.aprs")
        .radioSettingNavigationDestination()
        .toolbar { refreshToolbar }
        .sheet(isPresented: $isConfirmingBeacon) {
            APRSConfirmationSheet(
                title: "Press BCN on the radio?",
                message: "BCN may immediately transmit your position or change beaconing, "
                    + "depending on the radio’s current APRS mode and Menu 510 method. "
                    + "It only has APRS behavior in the appropriate radio context.",
                symbol: "antenna.radiowaves.left.and.right",
                confirmationTitle: "Press BCN",
                confirmationRole: .destructive
            ) {
                pressConfirmedRadioKey(.beacon6)
            }
        }
        .alert(
            "Radio operation",
            isPresented: Binding(
                get: { model.operationError != nil },
                set: { if !$0 { model.dismissOperationError() } }
            )
        ) {
            Button("OK") { model.dismissOperationError() }
        } message: {
            Text(model.operationError ?? "Unknown radio error")
        }
    }

    private var overview: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 16) {
                ViewThatFits(in: .horizontal) {
                    HStack(alignment: .top, spacing: 16) {
                        overviewTitle
                        Spacer(minLength: 20)
                        callsignPill
                    }

                    VStack(alignment: .leading, spacing: 10) {
                        overviewTitle
                        callsignPill
                    }
                }

                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 140), spacing: 12)],
                    alignment: .leading,
                    spacing: 12
                ) {
                    AzimuthMetric(
                        label: "My callsign",
                        value: snapshot.callsignLabel,
                        tint: snapshot.callsignStatus.isConfigured
                            ? AzimuthPalette.signal : AzimuthPalette.caution
                    )
                    AzimuthMetric(label: "Beacon method", value: snapshot.beaconMethodLabel)
                    AzimuthMetric(label: "Data band", value: snapshot.dataBandLabel)
                    AzimuthMetric(label: "Data speed", value: snapshot.dataSpeedLabel)
                    AzimuthMetric(label: "Packet path", value: snapshot.packetPathLabel)
                    AzimuthMetric(label: "Status text", value: snapshot.selectedStatusLabel)
                }

                if snapshot.callsignStatus == .missing {
                    Label(
                        "Set Menu 500 to your callsign and SSID before attempting an APRS transmission.",
                        systemImage: "exclamationmark.triangle.fill"
                    )
                    .font(.callout)
                    .foregroundStyle(AzimuthPalette.caution)
                } else if snapshot.callsignStatus == .notRead {
                    Label(
                        "Connect and read the radio to verify its APRS identity and beacon policy.",
                        systemImage: "arrow.down.to.line.compact"
                    )
                    .font(.callout)
                    .foregroundStyle(.secondary)
                }
            }
        }
    }

    private var overviewTitle: some View {
        VStack(alignment: .leading, spacing: 6) {
            AzimuthEyebrow("APRS station")
            Text("Identity, path, and beacon policy")
                .font(.title2.bold())
            Text(
                "Values below are the radio’s live configuration. Received stations and messages stay on the TH-D75 display until the control core exposes a packet stream."
            )
            .font(.callout)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        }
    }

    @ViewBuilder
    private var callsignPill: some View {
        switch snapshot.callsignStatus {
        case .configured:
            AzimuthStatusPill(
                title: "IDENTITY SET",
                symbol: "checkmark.seal.fill",
                color: AzimuthPalette.signal
            )
        case .missing:
            AzimuthStatusPill(
                title: "CALLSIGN NEEDED",
                symbol: "exclamationmark.triangle.fill",
                color: AzimuthPalette.caution
            )
        case .notRead:
            AzimuthStatusPill(
                title: "NOT READ",
                symbol: "antenna.radiowaves.left.and.right.slash",
                color: .secondary
            )
        }
    }

    private var radioActions: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 14) {
                HStack(alignment: .firstTextBaseline) {
                    VStack(alignment: .leading, spacing: 4) {
                        AzimuthEyebrow("Radio APRS controls")
                        Text("Use APRS keys with visible radio context")
                            .font(.headline)
                    }
                    Spacer()
                    if controlsReady {
                        AzimuthStatusPill(
                            title: "CONTROL READY",
                            symbol: "bolt.horizontal.circle.fill",
                            color: AzimuthPalette.signal
                        )
                    }
                }

                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 190), spacing: 10)],
                    alignment: .leading,
                    spacing: 10
                ) {
                    radioContextButton(
                        title: "Open radio for MSG",
                        detail: "Review the live display, then press MSG",
                        symbol: "message.badge.waveform",
                        key: .msg4
                    )
                    radioContextButton(
                        title: "Open radio for LIST",
                        detail: "Review the live display, then press LIST",
                        symbol: "list.bullet.rectangle",
                        key: .list5
                    )

                    Button {
                        isConfirmingBeacon = true
                    } label: {
                        actionButtonLabel(
                            title: "Press BCN",
                            detail: "Context-sensitive; confirmation required",
                            symbol: "antenna.radiowaves.left.and.right",
                            tint: AzimuthPalette.caution
                        )
                    }
                    .buttonStyle(.plain)
                    .disabled(!beaconControlReady)
                    .accessibilityIdentifier("azimuth.aprs.beacon")
                }

                if !controlsReady {
                    HStack {
                        Label(controlsUnavailableReason, systemImage: "cable.connector.slash")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Spacer()
                        if canConnect {
                            #if targetEnvironment(simulator)
                            Label("Physical iPad required", systemImage: "ipad")
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(.secondary)
                            #else
                            Button("Connect Radio") {
                                Task { await model.connectRadio() }
                            }
                            .buttonStyle(.bordered)
                            .disabled(model.isRadioOperationInFlight)
                            #endif
                        }
                    }
                } else if !snapshot.callsignStatus.isConfigured {
                    Label(
                        "Press BCN is locked until live Menu 500 contains a non-placeholder callsign.",
                        systemImage: "lock.fill"
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                }
            }
        }
    }

    private var configurationGrid: some View {
        LazyVGrid(
            columns: [GridItem(.adaptive(minimum: 330), spacing: AzimuthLayout.pageSpacing)],
            alignment: .leading,
            spacing: AzimuthLayout.pageSpacing
        ) {
            settingPanel(
                eyebrow: "Menus 500–503",
                title: "Station identity",
                symbol: "person.text.rectangle",
                settings: identitySettingLinks
            )
            settingPanel(
                eyebrow: "Menus 504–508",
                title: "Packet channel",
                symbol: "point.3.connected.trianglepath.dotted",
                settings: APRSSettingLinks.packetChannel
            )
            settingPanel(
                eyebrow: "Menus 510–515",
                title: "Beacon transmission",
                symbol: "dot.radiowaves.up.forward",
                settings: APRSSettingLinks.beaconPolicy
            )
            settingPanel(
                eyebrow: "Menus 530–535",
                title: "SmartBeaconing",
                symbol: "location.north.circle",
                settings: APRSSettingLinks.smartBeaconing
            )
        }
    }

    private func settingPanel(
        eyebrow: String,
        title: String,
        symbol: String,
        settings: [APRSSettingLink]
    ) -> some View {
        InstrumentPanel(padding: 14) {
            VStack(alignment: .leading, spacing: 10) {
                AzimuthEyebrow(eyebrow)
                Label(title, systemImage: symbol)
                    .font(.headline)

                Divider()

                let available = availableSettings(settings)
                ForEach(available.indices, id: \.self) { index in
                    let item = available[index]
                    if index > 0 { Divider() }
                    NavigationLink(value: RadioSettingDestination(id: item.link.id)) {
                        settingRow(item.link, definition: item.definition)
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }

    private func settingRow(
        _ link: APRSSettingLink,
        definition: RadioSettingDefinition
    ) -> some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(link.title)
                        .font(.subheadline.weight(.semibold))
                    if let menuNumberLabel = definition.menuNumberLabel {
                        THD75MenuBadge(menuNumberLabel)
                    }
                }
                Text(link.detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }

            Spacer(minLength: 8)

            VStack(alignment: .trailing, spacing: 4) {
                Text(snapshot.label(for: definition.id))
                    .font(.caption.weight(.semibold).monospaced())
                    .foregroundStyle(
                        model.radioState.settingValues[definition.id] == nil
                            ? Color.secondary : AzimuthPalette.signal
                    )
                    .lineLimit(1)
                Image(systemName: "chevron.right")
                    .font(.caption2.bold())
                    .foregroundStyle(.tertiary)
            }
        }
        .contentShape(Rectangle())
        .padding(.vertical, 2)
    }

    private func availableSettings(
        _ settings: [APRSSettingLink]
    ) -> [(link: APRSSettingLink, definition: RadioSettingDefinition)] {
        settings.compactMap { link in
            model.catalog.definition(id: link.id).map { (link, $0) }
        }
    }

    private var identitySettingLinks: [APRSSettingLink] {
        guard let index = snapshot.selectedStatusIndex else {
            return APRSSettingLinks.identity
        }
        return APRSSettingLinks.identity + [
            APRSSettingLink(
                id: "aprs.StatusTextList[\(index)].StatusText",
                title: "Selected status text",
                detail: "Edit the text currently selected in slot \(index + 1)."
            ),
            APRSSettingLink(
                id: "aprs.StatusTextList[\(index)].TxRate",
                title: "Status TX rate",
                detail: "Choose how often slot \(index + 1) accompanies a beacon."
            ),
        ]
    }

    private func radioContextButton(
        title: String,
        detail: String,
        symbol: String,
        key: RadioFrontPanelKey
    ) -> some View {
        Button {
            model.route = .radio
        } label: {
            actionButtonLabel(
                title: title,
                detail: detail,
                symbol: symbol,
                tint: AzimuthPalette.bearing
            )
        }
        .buttonStyle(.plain)
        .disabled(!controlsReady || model.isRadioOperationInFlight)
        .accessibilityIdentifier("azimuth.aprs.\(key.rawValue)")
    }

    private func actionButtonLabel(
        title: String,
        detail: String,
        symbol: String,
        tint: Color
    ) -> some View {
        HStack(spacing: 11) {
            Image(systemName: symbol)
                .font(.title3.weight(.semibold))
                .foregroundStyle(tint)
                .frame(width: 28)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.subheadline.weight(.semibold))
                Text(detail)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 4)
            Image(systemName: "arrow.up.right")
                .font(.caption.bold())
                .foregroundStyle(.tertiary)
        }
        .frame(maxWidth: .infinity, minHeight: 46, alignment: .leading)
        .padding(.horizontal, 12)
        .background(.primary.opacity(0.045), in: RoundedRectangle(cornerRadius: 10))
        .overlay {
            RoundedRectangle(cornerRadius: 10)
                .strokeBorder(.primary.opacity(0.08))
        }
    }

    @ToolbarContentBuilder
    private var refreshToolbar: some ToolbarContent {
        ToolbarItem {
            Button {
                if model.radioState.connection.isConnected {
                    Task { await model.refreshRadioSettings() }
                } else {
                    model.reloadCatalog()
                }
            } label: {
                Label(
                    model.radioState.connection.isConnected
                        ? "Read APRS settings from radio" : "Reload settings catalog",
                    systemImage: "arrow.clockwise"
                )
            }
            .disabled(model.catalogLoadState == .loading || model.isRadioOperationInFlight)
            .help(
                model.radioState.connection.isConnected
                    ? "Reads the complete settings snapshot through MCP programming mode. "
                        + "Exiting MCP restarts the TH-D75; Azimuth then reconnects. "
                        + "The initial connection intentionally defers this read."
                    : "Reloads the reviewed settings catalog without contacting the radio."
            )
        }
    }

    private var controlsReady: Bool {
        model.radioState.capabilities.frontPanelControl.isAvailable
    }

    private var beaconControlReady: Bool {
        controlsReady
            && snapshot.callsignStatus.isConfigured
            && !model.isRadioOperationInFlight
    }

    private var canConnect: Bool {
        switch model.radioState.connection {
        case .disconnected, .failed: return true
        case .connecting, .connected: return false
        }
    }

    private var controlsUnavailableReason: String {
        switch model.radioState.capabilities.frontPanelControl {
        case .available: return "Radio controls are ready."
        case .preparing: return "Radio controls are preparing."
        case .unavailable(let reason): return reason
        }
    }

    private func pressConfirmedRadioKey(_ key: RadioFrontPanelKey) {
        Task {
            await model.press(key)
            // The BCN result belongs on the radio display, so surface the
            // existing live display immediately after the confirmed key tap.
            model.route = .radio
        }
    }
}

enum APRSCallsignStatus: Equatable, Sendable {
    case notRead
    case missing
    case configured(String)

    var isConfigured: Bool {
        if case .configured = self { return true }
        return false
    }
}

/// Pure projection of the typed settings snapshot. Keeping this independent
/// from SwiftUI makes the safety gate and displayed labels unit-testable.
struct APRSConfigurationSnapshot: Equatable, Sendable {
    let callsignStatus: APRSCallsignStatus
    let beaconMethodLabel: String
    let dataBandLabel: String
    let dataSpeedLabel: String
    let packetPathLabel: String
    let selectedStatusLabel: String
    let selectedStatusIndex: Int?

    private let catalog: RadioSettingCatalog
    private let values: [String: ProposedSettingValue]

    init(
        catalog: RadioSettingCatalog,
        values: [String: ProposedSettingValue]
    ) {
        self.catalog = catalog
        self.values = values
        callsignStatus = Self.callsignStatus(values[APRSSettingID.myCallsign])
        beaconMethodLabel = Self.label(
            id: APRSSettingID.beaconMethod,
            catalog: catalog,
            values: values
        )
        dataBandLabel = Self.label(
            id: APRSSettingID.dataBand,
            catalog: catalog,
            values: values
        )
        dataSpeedLabel = Self.label(
            id: APRSSettingID.dataSpeed,
            catalog: catalog,
            values: values
        )
        packetPathLabel = Self.label(
            id: APRSSettingID.packetPath,
            catalog: catalog,
            values: values
        )
        let selectedStatusIndex = Self.selectedStatusIndex(values: values)
        self.selectedStatusIndex = selectedStatusIndex
        selectedStatusLabel = Self.selectedStatusLabel(
            index: selectedStatusIndex,
            catalog: catalog,
            values: values
        )
    }

    var callsignLabel: String {
        switch callsignStatus {
        case .configured(let value): return value
        case .missing: return "NOT SET"
        case .notRead: return "NOT READ"
        }
    }

    func label(for id: String) -> String {
        if id == APRSSettingID.statusTextSelect,
           case .integer(let index) = values[id],
           (0...4).contains(index) {
            return "Text \(index + 1)"
        }
        return Self.label(id: id, catalog: catalog, values: values)
    }

    private static func callsignStatus(
        _ value: ProposedSettingValue?
    ) -> APRSCallsignStatus {
        guard let value else { return .notRead }
        guard case .text(let raw) = value else { return .missing }
        let callsign = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        let baseCallsign = callsign
            .split(separator: "-", maxSplits: 1, omittingEmptySubsequences: false)
            .first?
            .uppercased()
        guard !callsign.isEmpty, baseCallsign != "NOCALL" else {
            return .missing
        }
        return .configured(callsign)
    }

    private static func selectedStatusIndex(
        values: [String: ProposedSettingValue]
    ) -> Int? {
        guard case .integer(let index) = values[APRSSettingID.statusTextSelect],
              (0...4).contains(index) else { return nil }
        return index
    }

    private static func selectedStatusLabel(
        index: Int?,
        catalog: RadioSettingCatalog,
        values: [String: ProposedSettingValue]
    ) -> String {
        guard let index else {
            return label(id: APRSSettingID.statusTextSelect, catalog: catalog, values: values)
        }
        let textID = "aprs.StatusTextList[\(index)].StatusText"
        guard case .text(let value) = values[textID] else { return "Text \(index + 1)" }
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? "Text \(index + 1) · EMPTY" : trimmed
    }

    private static func label(
        id: String,
        catalog: RadioSettingCatalog,
        values: [String: ProposedSettingValue]
    ) -> String {
        guard let value = values[id] else { return "NOT READ" }
        guard let definition = catalog.definition(id: id) else { return value.displayText }
        return definition.domain.displayText(for: value) ?? value.displayText
    }
}

enum APRSSettingID {
    static let myCallsign = "aprs.MyCallsign"
    static let beaconMethod = "aprs.BeaconTxMethod"
    static let dataBand = "aprs.TncDataBand"
    static let dataSpeed = "aprs.TncDataSpeed"
    static let packetPath = "aprs.PacketPathType"
    static let statusTextSelect = "aprs.StatusTextSelect"
}

struct APRSSettingLink: Identifiable, Hashable, Sendable {
    let id: String
    let title: String
    let detail: String
}

enum APRSSettingLinks {
    static let identity = [
        APRSSettingLink(
            id: APRSSettingID.myCallsign,
            title: "My callsign",
            detail: "Station callsign and SSID used on transmitted packets."
        ),
        APRSSettingLink(
            id: "aprs.IconSymbol",
            title: "Station icon",
            detail: "Symbol sent to other APRS stations."
        ),
        APRSSettingLink(
            id: "aprs.PositionComment",
            title: "Position comment",
            detail: "Operational state attached to position reports."
        ),
        APRSSettingLink(
            id: APRSSettingID.statusTextSelect,
            title: "Status text",
            detail: "Select one of the radio’s five status-text slots."
        ),
    ]

    static let packetChannel = [
        APRSSettingLink(
            id: APRSSettingID.packetPath,
            title: "Packet path",
            detail: "Digipeater path family used for outgoing packets."
        ),
        APRSSettingLink(
            id: APRSSettingID.dataSpeed,
            title: "Data speed",
            detail: "1200 or 9600 bps internal TNC rate."
        ),
        APRSSettingLink(
            id: APRSSettingID.dataBand,
            title: "Data band",
            detail: "Radio band carrying APRS data."
        ),
        APRSSettingLink(
            id: "aprs.TncDcdSense",
            title: "DCD sense",
            detail: "Channel-busy policy before packet transmission."
        ),
        APRSSettingLink(
            id: "aprs.TncTxDelay",
            title: "TX delay",
            detail: "Preamble delay before packet data."
        ),
    ]

    static let beaconPolicy = [
        APRSSettingLink(
            id: APRSSettingID.beaconMethod,
            title: "Method",
            detail: "Manual, PTT, automatic, or SmartBeaconing trigger."
        ),
        APRSSettingLink(
            id: "aprs.BeaconTxInterval",
            title: "Initial interval",
            detail: "Starting interval for automatic beacons."
        ),
        APRSSettingLink(
            id: "aprs.BeaconTxDecay",
            title: "Decay algorithm",
            detail: "Lengthen the interval when position is unchanged."
        ),
        APRSSettingLink(
            id: "aprs.BeaconTxProportion",
            title: "Proportional pathing",
            detail: "Vary path use across periodic beacons."
        ),
        APRSSettingLink(
            id: "aprs.BeaconSpeedOff",
            title: "Suppress speed",
            detail: "Omit speed information from position reports."
        ),
        APRSSettingLink(
            id: "aprs.BeaconAltitudeOff",
            title: "Suppress altitude",
            detail: "Omit altitude information from position reports."
        ),
    ]

    static let smartBeaconing = [
        APRSSettingLink(
            id: "aprs.LowSpeedSpeed",
            title: "Low speed",
            detail: "Threshold for slow-rate beaconing."
        ),
        APRSSettingLink(
            id: "aprs.HiSpeedSpeed",
            title: "High speed",
            detail: "Threshold for fast-rate beaconing."
        ),
        APRSSettingLink(
            id: "aprs.SlowRateTime",
            title: "Slow rate",
            detail: "Beacon interval at or below low speed."
        ),
        APRSSettingLink(
            id: "aprs.FastRateTime",
            title: "Fast rate",
            detail: "Beacon interval at or above high speed."
        ),
        APRSSettingLink(
            id: "aprs.TurnAngleDeg",
            title: "Turn angle",
            detail: "Minimum heading change for corner pegging."
        ),
        APRSSettingLink(
            id: "aprs.TurnSlopeDegSpeed",
            title: "Turn slope",
            detail: "Speed-sensitive addition to the turn threshold."
        ),
        APRSSettingLink(
            id: "aprs.TurnTimeTime",
            title: "Turn time",
            detail: "Minimum time between corner-peg beacons."
        ),
    ]

    static let all = identity + packetChannel + beaconPolicy + smartBeaconing
}
