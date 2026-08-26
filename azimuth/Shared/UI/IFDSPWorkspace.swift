// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

/// A settings-backed view of the TH-D75 receive/audio path.
///
/// Azimuth's control connection does not carry the radio's USB audio stream,
/// so this surface deliberately shows configuration and snapshot state rather
/// than inventing a spectrum, level meter, or recording state.
private struct IFDSPSettingsDashboard: View {
    @Environment(AzimuthSceneModel.self) private var model

    private var setupReadCount: Int {
        IFDSPSettingMap.setupSettingIDs.reduce(into: 0) { count, id in
            if model.radioState.settingValues[id] != nil { count += 1 }
        }
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: AzimuthLayout.pageSpacing) {
                overviewPanel
                setupPanel
                architecturePanel

                ViewThatFits(in: .horizontal) {
                    HStack(alignment: .top, spacing: AzimuthLayout.pageSpacing) {
                        VStack(spacing: AzimuthLayout.pageSpacing) {
                            filterPanel
                            utilityPanel
                        }
                        .frame(maxWidth: .infinity, alignment: .topLeading)

                        VStack(spacing: AzimuthLayout.pageSpacing) {
                            equalizerPanel(.receive)
                            equalizerPanel(.transmit)
                        }
                        .frame(maxWidth: .infinity, alignment: .topLeading)
                    }

                    VStack(spacing: AzimuthLayout.pageSpacing) {
                        filterPanel
                        equalizerPanel(.receive)
                        equalizerPanel(.transmit)
                        utilityPanel
                    }
                }
            }
            .azimuthContentColumn(maxWidth: AzimuthLayout.workspaceWidth)
        }
        .azimuthPage()
        .accessibilityIdentifier("azimuth.page.if-dsp")
        .radioSettingNavigationDestination()
        .toolbar { refreshToolbar }
        .alert(
            "IF / DSP operation",
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

    private var overviewPanel: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 14) {
                HStack(alignment: .top, spacing: 14) {
                    VStack(alignment: .leading, spacing: 6) {
                        AzimuthEyebrow("IF / DSP signal path")
                        Text("Know exactly what the radio is routing")
                            .font(.title2.bold())
                        Text(
                            "Live values below come from the TH-D75 settings snapshot. "
                                + "The live IF-DSP workspace separately proves the exact USB device "
                                + "and analyzes its physical USB audio stream."
                        )
                        .font(.callout)
                        .foregroundStyle(.secondary)
                    }

                    Spacer(minLength: 12)

                    snapshotPill
                }

                Divider()

                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 135), spacing: 12)],
                    alignment: .leading,
                    spacing: 12
                ) {
                    AzimuthMetric(
                        label: "Control link",
                        value: connectionSummary,
                        tint: model.radioState.connection.isConnected
                            ? AzimuthPalette.signal : .secondary
                    )
                    AzimuthMetric(
                        label: "Settings read",
                        value: "\(model.radioState.settingValues.count)"
                    )
                    AzimuthMetric(
                        label: "Saved output",
                        value: outputModeSummary
                    )
                    AzimuthMetric(
                        label: "Setup read",
                        value: "\(setupReadCount) / \(IFDSPSettingMap.setupSettingIDs.count)",
                        tint: setupReadCount == IFDSPSettingMap.setupSettingIDs.count
                            ? AzimuthPalette.signal : .secondary
                    )
                }
            }
        }
    }

    @ViewBuilder
    private var snapshotPill: some View {
        let hasLiveSettingValues = !model.radioState.settingValues.isEmpty
        switch model.radioState.connection {
        case .connected:
            AzimuthStatusPill(
                title: hasLiveSettingValues ? "SNAPSHOT LIVE" : "READ PENDING",
                symbol: hasLiveSettingValues
                    ? "checkmark.circle.fill" : "clock.arrow.circlepath",
                color: hasLiveSettingValues
                    ? AzimuthPalette.signal : AzimuthPalette.caution
            )
        case .connecting:
            AzimuthStatusPill(
                title: "CONNECTING",
                symbol: "clock.arrow.circlepath",
                color: AzimuthPalette.caution
            )
        case .disconnected, .failed:
            AzimuthStatusPill(
                title: "NO LIVE READ",
                symbol: "cable.connector.slash",
                color: .secondary
            )
        }
    }

    private var setupPanel: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 14) {
                HStack {
                    VStack(alignment: .leading, spacing: 3) {
                        AzimuthEyebrow("USB IF setup")
                        Text("The four settings that explain the output path")
                            .font(.headline)
                    }
                    Spacer()
                    Text("TAP TO INSPECT CONFIGURATION")
                        .font(.caption2.bold().monospaced())
                        .foregroundStyle(.secondary)
                }

                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 220), spacing: 10)],
                    alignment: .leading,
                    spacing: 10
                ) {
                    ForEach(IFDSPSettingMap.setupItems) { item in
                        setupCard(item)
                    }
                }

                Label(
                    "Menu 102 is saved configuration, not runtime IO readback. Menu 904 only configures what a single-band screen shows. Neither value proves that IF/Detect is presently engaged.",
                    systemImage: "info.circle"
                )
                .font(.caption)
                .foregroundStyle(.secondary)

                Label(
                    "Changing Menu 980 replaces the radio's active USB interface. Mass Storage disables normal CDC/audio and RX/TX operation, disrupts this control session, and may require a reconnect.",
                    systemImage: "exclamationmark.triangle.fill"
                )
                .font(.caption)
                .foregroundStyle(AzimuthPalette.caution)
            }
        }
    }

    @ViewBuilder
    private func setupCard(_ item: IFDSPSetupItem) -> some View {
        if let definition = model.catalog.definition(id: item.settingID) {
            NavigationLink(value: RadioSettingDestination(id: item.settingID)) {
                VStack(alignment: .leading, spacing: 10) {
                    HStack {
                        THD75MenuBadge(definition.menuNumberLabel ?? item.fallbackMenuLabel)
                        Spacer()
                        liveReadStatus(for: item.settingID)
                    }

                    Text(item.title)
                        .font(.subheadline.weight(.semibold))

                    Text(displayValue(for: definition))
                        .font(.title3.weight(.bold).monospaced())
                        .foregroundStyle(
                            model.radioState.settingValues[item.settingID] == nil
                                ? Color.secondary : item.tint
                        )
                        .lineLimit(2)
                        .minimumScaleFactor(0.72)

                    Text(item.detail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)

                    HStack {
                        Spacer()
                        Image(systemName: "chevron.right")
                            .font(.caption.bold())
                            .foregroundStyle(.tertiary)
                    }
                }
                .frame(maxWidth: .infinity, minHeight: 142, alignment: .topLeading)
                .padding(12)
                .background(.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 12))
                .overlay {
                    RoundedRectangle(cornerRadius: 12)
                        .strokeBorder(.primary.opacity(0.08))
                }
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("azimuth.ifdsp.setting.\(item.settingID)")
        } else {
            missingSettingCard(item)
        }
    }

    private func missingSettingCard(_ item: IFDSPSetupItem) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            THD75MenuBadge(item.fallbackMenuLabel)
            Text(item.title)
                .font(.subheadline.weight(.semibold))
            Text("UNAVAILABLE")
                .font(.title3.bold().monospaced())
                .foregroundStyle(.secondary)
            Text("This setting is absent from the loaded catalog.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, minHeight: 142, alignment: .topLeading)
        .padding(12)
        .background(.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 12))
    }

    private func liveReadStatus(for settingID: String) -> some View {
        let status: (title: String, symbol: String, color: Color)
        if let value = model.radioState.settingValues[settingID] {
            if settingID == "radio.UsbFunction", value != .choice(rawValue: 0) {
                status = ("CHANGE", "exclamationmark.triangle.fill", AzimuthPalette.caution)
            } else if settingID == "radio.UsbFunction" {
                status = ("READY", "checkmark.circle.fill", AzimuthPalette.signal)
            } else {
                status = ("READ", "checkmark.circle.fill", AzimuthPalette.signal)
            }
        } else {
            status = ("NOT READ", "minus.circle", Color.secondary)
        }

        return Label(status.title, systemImage: status.symbol)
            .font(.caption2.bold().monospaced())
            .foregroundStyle(status.color)
    }

    private var architecturePanel: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 16) {
                HStack(alignment: .firstTextBaseline) {
                    VStack(alignment: .leading, spacing: 3) {
                        AzimuthEyebrow("Documented architecture")
                        Text("IF output path, when engaged")
                            .font(.headline)
                    }
                    Spacer()
                    AzimuthStatusPill(
                        title: "CONFIGURATION ONLY",
                        symbol: "waveform.slash",
                        color: .secondary
                    )
                }

                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 8) {
                        architectureStage(
                            title: "REQUIRED STATE",
                            value: "Band B · single-band",
                            symbol: "antenna.radiowaves.left.and.right"
                        )
                        architectureArrow
                        architectureStage(
                            title: "SAVED MENU 102",
                            value: "Configured: \(outputModeSummary)",
                            symbol: "dial.medium"
                        )
                        architectureArrow
                        architectureStage(
                            title: "IF PATH",
                            value: "12 kHz center · 15 kHz BW",
                            symbol: "waveform.path.ecg"
                        )
                        architectureArrow
                        architectureStage(
                            title: "USB AUDIO",
                            value: "48 kHz · mono",
                            symbol: "cable.connector"
                        )
                    }

                    VStack(spacing: 8) {
                        architectureStage(
                            title: "REQUIRED STATE",
                            value: "Band B · single-band",
                            symbol: "antenna.radiowaves.left.and.right"
                        )
                        architectureDownArrow
                        architectureStage(
                            title: "SAVED MENU 102",
                            value: "Configured: \(outputModeSummary)",
                            symbol: "dial.medium"
                        )
                        architectureDownArrow
                        architectureStage(
                            title: "IF PATH",
                            value: "12 kHz center · 15 kHz BW",
                            symbol: "waveform.path.ecg"
                        )
                        architectureDownArrow
                        architectureStage(
                            title: "USB AUDIO",
                            value: "48 kHz · mono",
                            symbol: "cable.connector"
                        )
                    }
                }

                Label(
                    "IF-DSP supports AM, LSB, USB, and CW. Before capture, Azimuth proves the USB CAT and audio interfaces share the same physical TH-D75, reserves and verifies the radio state, and then analyzes live USB audio samples.",
                    systemImage: "checkmark.shield"
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
        }
    }

    private func architectureStage(title: String, value: String, symbol: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(title, systemImage: symbol)
                .font(.caption2.bold().monospaced())
                .foregroundStyle(AzimuthPalette.bearing)
            Text(value)
                .font(.subheadline.weight(.semibold).monospaced())
                .lineLimit(2)
                .minimumScaleFactor(0.72)
        }
        .frame(maxWidth: .infinity, minHeight: 72, alignment: .topLeading)
        .padding(12)
        .background(.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 12))
    }

    private var architectureArrow: some View {
        Image(systemName: "arrow.right")
            .font(.caption.bold())
            .foregroundStyle(.secondary)
    }

    private var architectureDownArrow: some View {
        Image(systemName: "arrow.down")
            .font(.caption.bold())
            .foregroundStyle(.secondary)
    }

    private var filterPanel: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 14) {
                panelHeading(
                    eyebrow: "IF receive filters",
                    title: "Mode-specific bandwidth",
                    detail: "The radio applies one stored width per demodulation family."
                )

                ForEach(IFDSPSettingMap.filters) { item in
                    settingRow(item, valueOverride: filterValue(for: item.settingID))
                }
            }
        }
    }

    private func equalizerPanel(_ kind: IFDSPEqualizerKind) -> some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 14) {
                panelHeading(
                    eyebrow: "\(kind.title) equalizer",
                    title: kind == .receive ? "Five-band receive curve" : "Four-band transmit curve",
                    detail: kind.detail
                )

                HStack(spacing: 8) {
                    ForEach(IFDSPSettingMap.equalizerBands(for: kind)) { band in
                        equalizerBand(band, kind: kind)
                    }
                }
                .frame(minHeight: 148)

                Divider()

                ForEach(IFDSPSettingMap.equalizerEnables(for: kind)) { item in
                    settingRow(item)
                }
            }
        }
    }

    @ViewBuilder
    private func equalizerBand(_ band: IFDSPBand, kind: IFDSPEqualizerKind) -> some View {
        let definition = model.catalog.definition(id: band.settingID)
        let value = model.radioState.settingValues[band.settingID]
        let decibels = IFDSPValueFormatter.equalizerDecibels(value, kind: kind)

        if definition != nil {
            NavigationLink(value: RadioSettingDestination(id: band.settingID)) {
                VStack(spacing: 6) {
                    Text(decibels.map(IFDSPValueFormatter.decibelLabel) ?? "–")
                        .font(.caption.bold().monospaced())
                        .foregroundStyle(decibels == nil ? .secondary : kind.tint)

                    ZStack(alignment: .bottom) {
                        Capsule()
                            .fill(.primary.opacity(0.06))
                            .frame(width: 22, height: 92)
                        Capsule()
                            .fill(kind.tint.gradient)
                            .frame(
                                width: 22,
                                height: equalizerBarHeight(decibels, kind: kind)
                            )
                    }

                    Text(band.frequency)
                        .font(.caption2.bold().monospaced())
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(
                "\(kind.title) equalizer \(band.frequency), "
                    + (decibels.map(IFDSPValueFormatter.decibelLabel) ?? "not read")
            )
        }
    }

    private func equalizerBarHeight(
        _ decibels: Int?,
        kind: IFDSPEqualizerKind
    ) -> CGFloat {
        guard let decibels else { return 3 }
        let clamped = min(max(decibels, kind.minimumDB), kind.maximumDB)
        let fraction = Double(clamped - kind.minimumDB)
            / Double(kind.maximumDB - kind.minimumDB)
        return 8 + CGFloat(fraction) * 84
    }

    private var utilityPanel: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 14) {
                panelHeading(
                    eyebrow: "Audio path",
                    title: "Gain, balance, and recording",
                    detail: "Related controls stored in the same authoritative radio snapshot."
                )

                ForEach(IFDSPSettingMap.utilities) { item in
                    settingRow(item)
                }
            }
        }
    }

    private func panelHeading(eyebrow: String, title: String, detail: String) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            AzimuthEyebrow(eyebrow)
            Text(title)
                .font(.headline)
            Text(detail)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    @ViewBuilder
    private func settingRow(
        _ item: IFDSPSettingItem,
        valueOverride: String? = nil
    ) -> some View {
        if let definition = model.catalog.definition(id: item.settingID) {
            NavigationLink(value: RadioSettingDestination(id: item.settingID)) {
                HStack(spacing: 11) {
                    Image(systemName: item.symbol)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(item.tint)
                        .frame(width: 26)

                    VStack(alignment: .leading, spacing: 3) {
                        HStack(spacing: 6) {
                            Text(item.title)
                                .font(.subheadline.weight(.semibold))
                            if let menu = definition.menuNumberLabel {
                                THD75MenuBadge(menu)
                            }
                        }
                        Text(item.detail)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }

                    Spacer(minLength: 8)

                    Text(valueOverride ?? displayValue(for: definition))
                        .font(.caption.weight(.semibold).monospaced())
                        .foregroundStyle(
                            model.radioState.settingValues[item.settingID] == nil
                                ? Color.secondary : item.tint
                        )
                        .lineLimit(2)
                        .multilineTextAlignment(.trailing)

                    Image(systemName: "chevron.right")
                        .font(.caption2.bold())
                        .foregroundStyle(.tertiary)
                }
                .padding(.vertical, 3)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
        }
    }

    private func displayValue(for definition: RadioSettingDefinition) -> String {
        guard let value = model.radioState.settingValues[definition.id] else {
            return "NOT READ"
        }
        return definition.domain.displayText(for: value) ?? value.displayText
    }

    private func filterValue(for settingID: String) -> String? {
        guard let definition = model.catalog.definition(id: settingID),
              model.radioState.settingValues[settingID] != nil else { return nil }
        return "\(displayValue(for: definition)) kHz"
    }

    private var outputModeSummary: String {
        guard let definition = model.catalog.definition(id: IFDSPSettingMap.outputModeID) else {
            return "Menu unavailable"
        }
        return displayValue(for: definition)
    }

    private var connectionSummary: String {
        switch model.radioState.connection {
        case .connected: return "Connected"
        case .connecting: return "Connecting"
        case .disconnected: return "Offline"
        case .failed: return "Failed"
        }
    }

    @ToolbarContentBuilder
    private var refreshToolbar: some ToolbarContent {
        #if os(iOS)
        ToolbarItem(placement: .topBarTrailing) { refreshButton }
        #else
        ToolbarItem(placement: .primaryAction) { refreshButton }
        #endif
    }

    private var refreshButton: some View {
        Button {
            Task { await model.refreshRadioSettings() }
        } label: {
            Label("Refresh DSP settings", systemImage: "arrow.clockwise")
        }
        .disabled(
            !model.radioState.capabilities.settingRead.isAvailable
                || model.isRadioOperationInFlight
        )
        .help(
            "Reads the complete settings snapshot through MCP programming mode. "
                + "Exiting MCP restarts the TH-D75; Azimuth then reconnects. "
                + "This read is intentionally deferred on initial connection."
        )
        .accessibilityIdentifier("azimuth.ifdsp.refresh")
    }
}

enum IFDSPEqualizerKind: String, Sendable {
    case receive
    case transmit

    var title: String { rawValue.capitalized }
    var minimumDB: Int { -9 }
    var maximumDB: Int { self == .receive ? 9 : 3 }
    var tint: Color { self == .receive ? AzimuthPalette.bearing : AzimuthPalette.signal }

    var detail: String {
        switch self {
        case .receive:
            return "Menu 913 · raw MCP levels decoded to −9…+9 dB. The 6.4 kHz point is not applied to DV/DR audio."
        case .transmit:
            return "Menu 912 · shared FM/NFM and DV curve decoded to −9…+3 dB."
        }
    }
}

struct IFDSPSetupItem: Identifiable, Sendable {
    let settingID: String
    let fallbackMenuLabel: String
    let title: String
    let detail: String
    let tint: Color

    var id: String { settingID }
}

struct IFDSPSettingItem: Identifiable, Sendable {
    let settingID: String
    let title: String
    let detail: String
    let symbol: String
    let tint: Color

    var id: String { settingID }
}

struct IFDSPBand: Identifiable, Sendable {
    let settingID: String
    let frequency: String

    var id: String { settingID }
}

enum IFDSPSettingMap {
    static let outputModeID = "radio.DetectOutput"

    static let setupItems: [IFDSPSetupItem] = [
        IFDSPSetupItem(
            settingID: "radio.UsbFunction",
            fallbackMenuLabel: "Menu 980",
            title: "USB function",
            detail: "COM + AF/IF Output is required; changing this interface disrupts the live session.",
            tint: AzimuthPalette.signal
        ),
        IFDSPSetupItem(
            settingID: outputModeID,
            fallbackMenuLabel: "Menu 102",
            title: "USB output select",
            detail: "Chooses AF audio, 12 kHz-centered IF, or pre-detection output.",
            tint: AzimuthPalette.bearing
        ),
        IFDSPSetupItem(
            settingID: "radio.SingleBandDisplay",
            fallbackMenuLabel: "Menu 904",
            title: "Single-band display",
            detail: "Chooses the information displayed when the radio is already in single-band mode.",
            tint: AzimuthPalette.bearing
        ),
        IFDSPSetupItem(
            settingID: "radio.UsbAudioOutLevel",
            fallbackMenuLabel: "Menu 91A",
            title: "USB audio level",
            detail: "Sets the level of the radio's output-only USB PCM stream.",
            tint: AzimuthPalette.signal
        ),
    ]

    static let filters: [IFDSPSettingItem] = [
        IFDSPSettingItem(
            settingID: "radio.SsbHighCut",
            title: "SSB high cut",
            detail: "USB and LSB receive filter",
            symbol: "waveform.path",
            tint: AzimuthPalette.bearing
        ),
        IFDSPSettingItem(
            settingID: "radio.CwWidth",
            title: "CW bandwidth",
            detail: "CW receive filter width",
            symbol: "dot.radiowaves.left.and.right",
            tint: AzimuthPalette.bearing
        ),
        IFDSPSettingItem(
            settingID: "radio.AmHighCut",
            title: "AM high cut",
            detail: "AM receive filter",
            symbol: "waveform.path",
            tint: AzimuthPalette.bearing
        ),
    ]

    static let receiveBands: [IFDSPBand] = [
        IFDSPBand(settingID: "radio.RxEqLevel04", frequency: "0.4k"),
        IFDSPBand(settingID: "radio.RxEqLevel08", frequency: "0.8k"),
        IFDSPBand(settingID: "radio.RxEqLevel16", frequency: "1.6k"),
        IFDSPBand(settingID: "radio.RxEqLevel32", frequency: "3.2k"),
        IFDSPBand(settingID: "radio.RxEqLevel64", frequency: "6.4k"),
    ]

    static let transmitBands: [IFDSPBand] = [
        IFDSPBand(settingID: "radio.TxEqLevel04", frequency: "0.4k"),
        IFDSPBand(settingID: "radio.TxEqLevel08", frequency: "0.8k"),
        IFDSPBand(settingID: "radio.TxEqLevel16", frequency: "1.6k"),
        IFDSPBand(settingID: "radio.TxEqLevel32", frequency: "3.2k"),
    ]

    static let receiveEnables: [IFDSPSettingItem] = [
        IFDSPSettingItem(
            settingID: "radio.RxEqualizer",
            title: "Receive EQ",
            detail: "Apply the five-band receive curve",
            symbol: "slider.vertical.3",
            tint: AzimuthPalette.bearing
        ),
    ]

    static let transmitEnables: [IFDSPSettingItem] = [
        IFDSPSettingItem(
            settingID: "radio.TxEqualizerFmNfm",
            title: "FM / NFM transmit EQ",
            detail: "Apply the transmit curve to analog voice",
            symbol: "waveform",
            tint: AzimuthPalette.signal
        ),
        IFDSPSettingItem(
            settingID: "radio.TxEqualizerDv",
            title: "DV transmit EQ",
            detail: "Apply the transmit curve to digital voice",
            symbol: "waveform.badge.mic",
            tint: AzimuthPalette.signal
        ),
    ]

    static let utilities: [IFDSPSettingItem] = [
        IFDSPSettingItem(
            settingID: "radio.Balance",
            title: "A / B audio balance",
            detail: "Mix the two receive bands",
            symbol: "circle.lefthalf.filled",
            tint: AzimuthPalette.bearing
        ),
        IFDSPSettingItem(
            settingID: "radio.MicSensitivity",
            title: "Microphone sensitivity",
            detail: "Transmit input gain profile",
            symbol: "mic",
            tint: AzimuthPalette.signal
        ),
        IFDSPSettingItem(
            settingID: "radio.RecordingBand",
            title: "Recording band",
            detail: "Band captured by the radio's microSD recorder in AF mode",
            symbol: "record.circle",
            tint: AzimuthPalette.caution
        ),
    ]

    static let setupSettingIDs = setupItems.map(\.settingID)

    static let allSettingIDs: [String] = {
        setupSettingIDs
            + filters.map(\.settingID)
            + receiveBands.map(\.settingID)
            + transmitBands.map(\.settingID)
            + receiveEnables.map(\.settingID)
            + transmitEnables.map(\.settingID)
            + utilities.map(\.settingID)
    }()

    static func equalizerBands(for kind: IFDSPEqualizerKind) -> [IFDSPBand] {
        kind == .receive ? receiveBands : transmitBands
    }

    static func equalizerEnables(for kind: IFDSPEqualizerKind) -> [IFDSPSettingItem] {
        kind == .receive ? receiveEnables : transmitEnables
    }
}

enum IFDSPValueFormatter {
    /// MCP stores both EQ domains with raw zero representing -9 dB.
    static func equalizerDecibels(
        _ value: ProposedSettingValue?,
        kind: IFDSPEqualizerKind
    ) -> Int? {
        guard case .integer(let rawValue) = value else { return nil }
        let decibels = rawValue - 9
        guard (kind.minimumDB...kind.maximumDB).contains(decibels) else { return nil }
        return decibels
    }

    static func decibelLabel(_ decibels: Int) -> String {
        if decibels > 0 { return "+\(decibels) dB" }
        if decibels == 0 { return "0 dB" }
        return "\(decibels) dB"
    }
}
