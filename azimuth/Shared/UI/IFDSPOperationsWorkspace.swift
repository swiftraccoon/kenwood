// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import SwiftUI

/// Live IF operating console. Every trace and meter originates in the radio's
/// selected USB audio input; an absent stream stays visibly absent.
struct IFDSPWorkspace: View {
    @Environment(AzimuthSceneModel.self) private var model
    @State private var waterfallRows: [[Float]] = []
    @State private var lastWaterfallInputSampleCount: UInt64?
    @State private var selectedMode: IFDSPMode = .usb
    @State private var filterHz = IFDSPMode.usb.defaultFilterHz
    @State private var isEditingFilter = false
    @State private var frequencyMHz = ""

    private var frame: IFDSPLiveFrame? { model.ifDSPState.latestFrame }
    private var spectrum: IFDSPSpectrum? { frame?.spectrum }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: AzimuthLayout.pageSpacing) {
                captureStrip
                operatorLayout
                setupPanel
            }
            .azimuthContentColumn(maxWidth: AzimuthLayout.workspaceWidth)
        }
        .azimuthPage()
        .accessibilityIdentifier("azimuth.page.if-dsp")
        .radioSettingNavigationDestination()
        .task {
            synchronizeControls()
            synchronizeFrequencyEntry()
        }
        .onChange(of: model.ifDSPConfiguration) { _, _ in
            guard !isEditingFilter else { return }
            synchronizeControls()
        }
        .onChange(of: model.ifDSPState) { _, state in
            updateWaterfall(for: state)
        }
        .onChange(of: model.ifDSPModeState) { _, state in
            if case .active = state { synchronizeFrequencyEntry() }
        }
        .alert(
            "IF-DSP operation",
            isPresented: Binding(
                get: { model.operationError != nil },
                set: { if !$0 { model.dismissOperationError() } }
            )
        ) {
            Button("OK") { model.dismissOperationError() }
        } message: {
            Text(model.operationError ?? "Unknown IF-DSP error")
        }
    }

    private var captureStrip: some View {
        InstrumentPanel {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 18) {
                    captureIdentity
                    Divider().frame(height: 42)
                    routeMetrics
                    Spacer(minLength: 12)
                    captureButton
                }

                VStack(alignment: .leading, spacing: 14) {
                    HStack(alignment: .top) {
                        captureIdentity
                        Spacer()
                        captureButton
                    }
                    routeMetrics
                }
            }
        }
    }

    private var captureIdentity: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 8) {
                AzimuthEyebrow("USB IF receiver")
                captureStatusPill
            }
            Text("Live 12 kHz low-IF analysis")
                .font(.title2.bold())
            Text(captureDetail)
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var routeMetrics: some View {
        LazyVGrid(
            columns: Array(repeating: GridItem(.flexible(), alignment: .leading), count: 3),
            alignment: .leading,
            spacing: 10
        ) {
            AzimuthMetric(label: "RF center", value: radioCenterLabel)
            AzimuthMetric(label: "USB IF", value: ifCenterLabel)
            AzimuthMetric(label: "Input", value: routeName)
            AzimuthMetric(label: "Format", value: routeFormat)
            AzimuthMetric(
                label: "DSP samples",
                value: frame.map { compactCount($0.inputSampleCount) } ?? "–"
            )
            AzimuthMetric(
                label: "Capture loss",
                value: frame.map(captureLossLabel) ?? "–",
                tint: (frame?.droppedSampleCount ?? 0) > 0 ? AzimuthPalette.caution : .primary
            )
        }
        .frame(maxWidth: 620, alignment: .leading)
    }

    private var captureButton: some View {
        Button {
            if captureCanStop {
                Task { await model.stopIFDSP() }
            } else {
                Task { await model.startIFDSP() }
            }
            waterfallRows.removeAll(keepingCapacity: true)
            lastWaterfallInputSampleCount = nil
        } label: {
            Label(
                captureButtonTitle,
                systemImage: captureCanStop ? "stop.fill" : "cable.connector"
            )
            .frame(minWidth: 118)
        }
        .buttonStyle(.borderedProminent)
        .tint(captureCanStop ? .red : AzimuthPalette.bearing)
        .disabled(captureTransitioning)
        .accessibilityIdentifier("azimuth.ifdsp.capture")
    }

    private var operatorLayout: some View {
        ViewThatFits(in: .horizontal) {
            HStack(alignment: .top, spacing: AzimuthLayout.pageSpacing) {
                visualizationPanel
                    .frame(maxWidth: .infinity)
                controlsPanel
                    .frame(width: 320)
            }

            VStack(spacing: AzimuthLayout.pageSpacing) {
                visualizationPanel
                controlsPanel
            }
        }
    }

    private var visualizationPanel: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 12) {
                HStack(alignment: .firstTextBaseline) {
                    VStack(alignment: .leading, spacing: 3) {
                        AzimuthEyebrow("Measured USB IF")
                        Text("Spectrum and waterfall")
                            .font(.headline)
                    }
                    Spacer()
                    if let spectrum {
                        Text(peakLabel(spectrum))
                            .font(.caption.bold().monospaced())
                            .foregroundStyle(AzimuthPalette.signal)
                    }
                }

                IFDSPSpectrumPlot(
                    levelsDBFS: spectrum?.levelsDBFS ?? [],
                    firstBinOffsetHz: Float(spectrum?.firstBinOffsetHz ?? -12_000),
                    binWidthHz: Float(spectrum?.binWidthHz ?? (48_000 / 1_024)),
                    passband: passband,
                    height: 230
                )

                IFDSPWaterfallPlot(
                    rowsDBFS: waterfallRows,
                    firstBinOffsetHz: Float(spectrum?.firstBinOffsetHz ?? -12_000),
                    binWidthHz: Float(spectrum?.binWidthHz ?? (48_000 / 1_024)),
                    passband: passband,
                    maximumRows: 120,
                    height: 250
                )

                HStack {
                    Label("0–24 kHz physical IF input · center 12 kHz", systemImage: "scope")
                    Spacer()
                    Text("\(waterfallRows.count) / 120 ROWS")
                }
                .font(.caption2.bold().monospaced())
                .foregroundStyle(.secondary)
            }
        }
    }

    private var controlsPanel: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 18) {
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        AzimuthEyebrow("Band B center")
                        Spacer()
                        Text("5 kHz STEP")
                            .font(.caption2.bold().monospaced())
                            .foregroundStyle(.secondary)
                    }
                    HStack(spacing: 8) {
                        TextField("145.500", text: $frequencyMHz)
                            .textFieldStyle(.roundedBorder)
                            .font(.body.monospaced())
                            .accessibilityLabel("Band B frequency in megahertz")
                        Text("MHz")
                            .font(.caption.bold().monospaced())
                            .foregroundStyle(.secondary)
                        Button("Tune") {
                            guard let frequencyHz = parsedFrequencyHz else { return }
                            Task { await model.retuneIFDSP(to: frequencyHz) }
                        }
                        .buttonStyle(.borderedProminent)
                        .disabled(!canRetune)
                    }
                    if let frequencyEntryError {
                        Text(frequencyEntryError)
                            .font(.caption)
                            .foregroundStyle(AzimuthPalette.caution)
                    } else {
                        Text("The radio briefly disables IF output, tunes Band B, then verifies IF output again.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }

                Divider()

                VStack(alignment: .leading, spacing: 8) {
                    AzimuthEyebrow("Demodulator")
                    Picker("Mode", selection: selectedModeValue) {
                        ForEach(IFDSPMode.allCases) { mode in
                            Text(mode.title).tag(mode.rawValue)
                        }
                    }
                    .pickerStyle(.segmented)
                    .onChange(of: selectedMode) { _, mode in
                        guard mode != model.ifDSPConfiguration.mode else { return }
                        filterHz = mode.defaultFilterHz
                        Task {
                            await model.configureIFDSP(
                                IFDSPConfiguration(mode: mode, filterHz: nil)
                            )
                        }
                    }
                }

                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text("Passband")
                            .font(.subheadline.weight(.semibold))
                        Spacer()
                        Text(formatBandwidth(filterHz))
                            .font(.subheadline.bold().monospaced())
                            .foregroundStyle(AzimuthPalette.signal)
                    }
                    Slider(
                        value: $filterHz,
                        in: filterRange,
                        step: selectedMode == .cw ? 50 : 100,
                        onEditingChanged: { editing in
                            isEditingFilter = editing
                            if !editing { commitFilter() }
                        }
                    )
                    HStack {
                        Text(formatBandwidth(filterRange.lowerBound))
                        Spacer()
                        Button("Mode default") {
                            filterHz = selectedMode.defaultFilterHz
                            Task {
                                await model.configureIFDSP(
                                    IFDSPConfiguration(mode: selectedMode, filterHz: nil)
                                )
                            }
                        }
                        .buttonStyle(.borderless)
                        Spacer()
                        Text(formatBandwidth(filterRange.upperBound))
                    }
                    .font(.caption2.monospaced())
                    .foregroundStyle(.secondary)
                }

                Divider()

                IFDSPLevelMeter(
                    label: "IF input RMS",
                    valueDBFS: frame.map { Float($0.inputLevelDBFS) },
                    floorDBFS: -120
                )
                IFDSPLevelMeter(
                    label: "Demod output RMS",
                    valueDBFS: frame.map { Float($0.outputLevelDBFS) },
                    floorDBFS: -120
                )

                HStack {
                    AzimuthMetric(
                        label: "Clipped samples",
                        value: frame.map { String($0.clippedSampleCount) } ?? "–",
                        tint: (frame?.clippedSampleCount ?? 0) > 0 ? .red : .primary
                    )
                    AzimuthMetric(
                        label: "Source samples",
                        value: frame.map { compactCount($0.sourceSampleCount) } ?? "–"
                    )
                }

                monitoringNotice
            }
        }
    }

    private var monitoringNotice: some View {
        Group {
            switch model.ifDSPMonitoringState {
            case .unavailable(let reason):
                Label(reason, systemImage: "speaker.slash.fill")
            case .disabled:
                Label("Demodulated audio monitoring is off.", systemImage: "speaker.slash")
            case .active(let output):
                Label("Monitoring on \(output)", systemImage: "speaker.wave.2.fill")
            case .failed(let message):
                Label(message, systemImage: "exclamationmark.triangle.fill")
            }
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .fixedSize(horizontal: false, vertical: true)
    }

    private var setupPanel: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 14) {
                HStack {
                    VStack(alignment: .leading, spacing: 3) {
                        AzimuthEyebrow("Radio setup")
                        Text("Saved setup and live IF override")
                            .font(.headline)
                    }
                    Spacer()
                    if model.radioState.connection.isConnected {
                        Button {
                            Task { await model.refreshRadioSettings() }
                        } label: {
                            Label("Read Radio", systemImage: "arrow.clockwise")
                        }
                        .buttonStyle(.bordered)
                        .disabled(
                            model.isRadioOperationInFlight
                                || model.ifDSPModeState.reservesRadioState
                                || !model.radioState.capabilities.settingRead.isAvailable
                        )
                    }
                }

                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 220), spacing: 10)],
                    alignment: .leading,
                    spacing: 10
                ) {
                    ForEach(IFDSPSettingMap.setupItems) { item in
                        setupLink(item)
                    }
                }

                Label(
                    "Starting IF-DSP saves Band B, dual-band, squelch, mode, tuning step, "
                        + "frequency, and USB output; configures and verifies live IF before "
                        + "capture. Stopping restores and verifies every saved field.",
                    systemImage: "info.circle"
                )
                .font(.caption)
                .foregroundStyle(.secondary)

                Label(
                    "Band B must be in VFO mode. APRS KISS and DV gateway modes must be "
                        + "stopped before IF-DSP can reserve the radio.",
                    systemImage: "exclamationmark.triangle"
                )
                .font(.caption)
                .foregroundStyle(AzimuthPalette.caution)
            }
        }
    }

    private func setupLink(_ item: IFDSPSetupItem) -> some View {
        Group {
            if let definition = model.catalog.definition(id: item.settingID) {
                NavigationLink(value: RadioSettingDestination(id: item.settingID)) {
                    HStack(spacing: 12) {
                        THD75MenuBadge(definition.menuNumberLabel ?? item.fallbackMenuLabel)
                        VStack(alignment: .leading, spacing: 3) {
                            Text(item.title)
                                .font(.subheadline.weight(.semibold))
                            Text(settingValue(definition))
                                .font(.caption.monospaced())
                                .foregroundStyle(item.tint)
                                .lineLimit(1)
                        }
                        Spacer()
                        Image(systemName: "chevron.right")
                            .font(.caption.bold())
                            .foregroundStyle(.tertiary)
                    }
                    .padding(12)
                    .frame(maxWidth: .infinity, minHeight: 68, alignment: .leading)
                    .background(.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 12))
                }
                .buttonStyle(.plain)
            } else {
                Text("\(item.fallbackMenuLabel) · \(item.title)")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder
    private var captureStatusPill: some View {
        switch model.ifDSPModeState {
        case .preparing:
            AzimuthStatusPill(
                title: "CONFIGURING RADIO",
                symbol: "gearshape.2.fill",
                color: AzimuthPalette.caution
            )
        case .tuning:
            AzimuthStatusPill(
                title: "TUNING",
                symbol: "dial.medium.fill",
                color: AzimuthPalette.caution
            )
        case .restoring:
            AzimuthStatusPill(
                title: "RESTORING RADIO",
                symbol: "arrow.uturn.backward.circle.fill",
                color: AzimuthPalette.caution
            )
        case .failed(_, let restorationPending) where restorationPending:
            AzimuthStatusPill(
                title: "RESTORE REQUIRED",
                symbol: "exclamationmark.triangle.fill",
                color: .red
            )
        case .failed:
            AzimuthStatusPill(
                title: "RADIO SETUP FAILED",
                symbol: "exclamationmark.triangle.fill",
                color: .red
            )
        case .inactive, .active:
            audioStatusPill
        }
    }

    @ViewBuilder
    private var audioStatusPill: some View {
        switch model.ifDSPState {
        case .idle:
            AzimuthStatusPill(title: "IDLE", symbol: "pause.circle", color: .secondary)
        case .requestingPermission:
            AzimuthStatusPill(
                title: "PERMISSION",
                symbol: "mic.badge.plus",
                color: AzimuthPalette.caution
            )
        case .waitingForUSBAudio:
            AzimuthStatusPill(
                title: "WAITING FOR USB",
                symbol: "cable.connector",
                color: AzimuthPalette.caution
            )
        case .starting:
            AzimuthStatusPill(
                title: "STARTING AUDIO",
                symbol: "clock.arrow.circlepath",
                color: AzimuthPalette.caution
            )
        case .streaming:
            AzimuthStatusPill(title: "LIVE", symbol: "waveform", color: AzimuthPalette.signal)
        case .paused:
            AzimuthStatusPill(
                title: "PAUSED",
                symbol: "pause.fill",
                color: AzimuthPalette.caution
            )
        case .failed:
            AzimuthStatusPill(title: "AUDIO FAILED", symbol: "exclamationmark.triangle.fill", color: .red)
        }
    }

    private var captureDetail: String {
        switch model.ifDSPModeState {
        case .preparing:
            return "Saving the current radio state and readback-verifying Band B USB IF "
                + "output before audio capture."
        case .active(let status):
            return "Band B is reserved at \(formatFrequencyMHz(status.bandBFrequencyHz)); "
                + "USB IF center is \(formatFrequencyKHz(status.ifCenterHz)). "
                + audioCaptureDetail
        case .tuning(_, let requestedFrequencyHz):
            return "Temporarily disabling IF output and tuning Band B to "
                + "\(formatFrequencyMHz(requestedFrequencyHz)); IF output must pass readback again."
        case .restoring:
            return "Audio is stopped. Restoring and readback-verifying every radio field saved when IF-DSP began."
        case .failed(let message, let restorationPending):
            return restorationPending
                ? "The radio still owns a saved IF-DSP state. Retry Restore before using another radio mode. \(message)"
                : message
        case .inactive:
            return audioCaptureDetail
        }
    }

    private var audioCaptureDetail: String {
        switch model.ifDSPState {
        case .idle:
            return "Azimuth will select the TH-D75 USB audio input and convert its physical "
                + "stream to 48 kHz mono for analysis."
        case .requestingPermission:
            return "Waiting for audio-input permission."
        case .waitingForUSBAudio(let inputs):
            let found = inputs.isEmpty
                ? "No audio inputs are currently visible."
                : "Visible: \(inputs.joined(separator: ", "))."
            return "The TH-D75 USB audio input is not available. \(found)"
        case .starting(let routeName):
            return "Preparing \(routeName) for 48 kHz mono analysis."
        case .streaming:
            return "Charts and meters below are derived from captured PCM, not generated display data."
        case .paused(let reason, _):
            return reason
        case .failed(let message, _):
            return message
        }
    }

    private var routeName: String {
        guard case .streaming(let route, _) = model.ifDSPState else { return "–" }
        return route.name
    }

    private var routeFormat: String {
        guard case .streaming(let route, _) = model.ifDSPState else { return "–" }
        return "\(Int(route.sourceSampleRate / 1_000))k / \(route.sourceChannelCount)ch"
    }

    private var radioCenterLabel: String {
        guard let status = model.ifDSPModeState.activeStatus else { return "–" }
        return formatFrequencyMHz(status.bandBFrequencyHz)
    }

    private var ifCenterLabel: String {
        guard let status = model.ifDSPModeState.activeStatus else { return "–" }
        return formatFrequencyKHz(status.ifCenterHz)
    }

    private var captureCanStop: Bool {
        if model.ifDSPModeState.reservesRadioState { return true }
        return model.ifDSPState.isStreaming
    }

    private var captureButtonTitle: String {
        switch model.ifDSPModeState {
        case .preparing:
            return "Configuring Radio"
        case .tuning:
            return "Tuning"
        case .restoring:
            return "Restoring Radio"
        case .failed(_, let restorationPending) where restorationPending:
            return "Retry Restore"
        case .active:
            return "End IF Mode"
        case .inactive, .failed:
            break
        }
        switch model.ifDSPState {
        case .streaming:
            return "End IF Mode"
        case .paused:
            return "Restart IF Mode"
        case .waitingForUSBAudio, .failed:
            return "Retry IF Mode"
        case .requestingPermission:
            return "Requesting Access"
        case .starting:
            return "Starting IF Mode"
        case .idle:
            return "Begin IF Mode"
        }
    }

    private var captureTransitioning: Bool {
        if model.isIFDSPOperationInFlight { return true }
        switch model.ifDSPModeState {
        case .preparing, .tuning, .restoring: return true
        case .inactive, .active, .failed: break
        }
        switch model.ifDSPState {
        case .requestingPermission, .starting: return true
        default: return false
        }
    }

    private var parsedFrequencyHz: UInt32? {
        IFDSPFrequencyEntry.frequencyHz(fromMHz: frequencyMHz)
    }

    private var frequencyEntryError: String? {
        guard case .active = model.ifDSPModeState else { return nil }
        guard parsedFrequencyHz != nil else {
            return "Enter 0.100–75.995 or 108.000–523.995 MHz on a 5 kHz boundary."
        }
        return nil
    }

    private var canRetune: Bool {
        guard !model.isIFDSPOperationInFlight,
              case .active(let status) = model.ifDSPModeState,
              let requested = parsedFrequencyHz else { return false }
        return requested != status.bandBFrequencyHz
    }

    private var filterRange: ClosedRange<Double> {
        switch selectedMode {
        case .usb, .lsb: return 300...4_000
        case .cw: return 100...2_000
        case .am: return 1_000...5_500
        }
    }

    private var selectedModeValue: Binding<String> {
        Binding(
            get: { selectedMode.rawValue },
            set: { rawValue in
                guard let mode = IFDSPMode(rawValue: rawValue) else { return }
                selectedMode = mode
            }
        )
    }

    private var passband: IFDSPPassband {
        let width = Float(filterHz)
        switch selectedMode {
        case .usb:
            return IFDSPPassband(lowerOffsetHz: 100, upperOffsetHz: 100 + width)
        case .lsb:
            return IFDSPPassband(lowerOffsetHz: -(100 + width), upperOffsetHz: -100)
        case .cw:
            return IFDSPPassband(lowerOffsetHz: 700 - width / 2, upperOffsetHz: 700 + width / 2)
        case .am:
            return IFDSPPassband(lowerOffsetHz: -width / 2, upperOffsetHz: width / 2)
        }
    }

    private func synchronizeControls() {
        selectedMode = model.ifDSPConfiguration.mode
        filterHz = model.ifDSPConfiguration.effectiveFilterHz
    }

    private func synchronizeFrequencyEntry() {
        guard let status = model.ifDSPModeState.activeStatus else { return }
        frequencyMHz = String(format: "%.3f", Double(status.bandBFrequencyHz) / 1_000_000)
    }

    private func commitFilter() {
        let configuration = IFDSPConfiguration(mode: selectedMode, filterHz: filterHz)
        Task { await model.configureIFDSP(configuration) }
    }

    private func updateWaterfall(for state: IFDSPLiveStreamState) {
        switch state {
        case .idle, .requestingPermission, .waitingForUSBAudio, .starting:
            waterfallRows.removeAll(keepingCapacity: true)
            lastWaterfallInputSampleCount = nil
        case .streaming, .paused, .failed:
            retainWaterfallRow(from: state.latestFrame)
        }
    }

    private func retainWaterfallRow(from newFrame: IFDSPLiveFrame?) {
        guard let newFrame, let newSpectrum = newFrame.spectrum else { return }
        if let previous = lastWaterfallInputSampleCount {
            if newFrame.inputSampleCount < previous {
                waterfallRows.removeAll(keepingCapacity: true)
                lastWaterfallInputSampleCount = nil
            } else if newFrame.inputSampleCount - previous < 4_800 {
                return
            }
        }
        lastWaterfallInputSampleCount = newFrame.inputSampleCount
        waterfallRows.append(newSpectrum.levelsDBFS)
        if waterfallRows.count > 120 {
            waterfallRows.removeFirst(waterfallRows.count - 120)
        }
    }

    private func settingValue(_ definition: RadioSettingDefinition) -> String {
        if definition.id == IFDSPSettingMap.outputModeID {
            switch model.ifDSPModeState {
            case .preparing:
                return "CONFIGURING LIVE OVERRIDE"
            case .active:
                return "IF · VERIFIED LIVE OVERRIDE"
            case .tuning:
                return "RETUNING · LIVE OVERRIDE"
            case .restoring:
                return "RESTORING SAVED VALUE"
            case .failed(_, let restorationPending) where restorationPending:
                return "UNKNOWN · RESTORE REQUIRED"
            case .inactive, .failed:
                break
            }
        }
        guard let value = model.radioState.settingValues[definition.id] else { return "NOT READ" }
        return definition.domain.displayText(for: value) ?? value.displayText
    }

    private func peakLabel(_ spectrum: IFDSPSpectrum) -> String {
        let center = Double(model.ifDSPModeState.activeStatus?.ifCenterHz ?? 12_000)
        let frequency = center + spectrum.peakOffsetHz
        return String(format: "PEAK %.3f kHz · %.1f dBFS", frequency / 1_000, spectrum.peakLevelDBFS)
    }

    private func formatFrequencyMHz(_ frequencyHz: UInt32) -> String {
        String(format: "%.3f MHz", Double(frequencyHz) / 1_000_000)
    }

    private func formatFrequencyKHz(_ frequencyHz: UInt32) -> String {
        String(format: "%.3f kHz", Double(frequencyHz) / 1_000)
    }

    private func formatBandwidth(_ value: Double) -> String {
        value >= 1_000
            ? String(format: "%.1f kHz", value / 1_000)
            : "\(Int(value)) Hz"
    }

    private func compactCount(_ value: UInt64) -> String {
        if value >= 1_000_000 { return String(format: "%.1fM", Double(value) / 1_000_000) }
        if value >= 1_000 { return String(format: "%.1fk", Double(value) / 1_000) }
        return String(value)
    }

    private func captureLossLabel(_ frame: IFDSPLiveFrame) -> String {
        guard frame.droppedSampleCount > 0 else { return "0%" }
        let percentage = frame.captureLossFraction * 100
        return String(
            format: "%.1f%% · %@ samples",
            percentage,
            compactCount(frame.droppedSampleCount)
        )
    }
}

enum IFDSPFrequencyEntry {
    private static let lowerBandBRange: ClosedRange<UInt32> = 100_000...75_995_000
    private static let upperBandBRange: ClosedRange<UInt32> = 108_000_000...523_995_000

    static func frequencyHz(fromMHz text: String) -> UInt32? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let megahertz = Double(trimmed),
              megahertz.isFinite,
              (0.100...523.995).contains(megahertz) else { return nil }
        let roundedHz = Int64((megahertz * 1_000_000).rounded())
        guard let frequencyHz = UInt32(exactly: roundedHz),
              frequencyHz.isMultiple(of: 5_000),
              lowerBandBRange.contains(frequencyHz) || upperBandBRange.contains(frequencyHz)
        else { return nil }
        return frequencyHz
    }
}
