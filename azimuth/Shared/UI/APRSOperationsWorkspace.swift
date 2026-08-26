// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import MapKit
import SwiftUI

/// Environment-bound APRS tab used by the app shell. The large operator view
/// below stays independently testable and contains no generated-core imports.
struct APRSWorkspace: View {
    @Environment(AzimuthSceneModel.self) private var model
    @State private var configuration = APRSSessionConfiguration.receiveOnly

    var body: some View {
        APRSOperationsWorkspace(
            state: model.aprsState,
            configuration: $configuration,
            settingDefinitions: model.catalog.filtered(query: "", group: .aprs),
            settingValues: model.radioState.settingValues,
            controlConnectionName: controlConnectionName,
            isExternalOperationInFlight: model.isAPRSOperationInFlight,
            startSession: { configuration in
                await model.startAPRS(configuration)
            },
            stopSession: {
                await model.stopAPRS()
            },
            sendMessage: { addressee, text, messageID in
                try await model.sendAPRSMessage(
                    addressee: addressee,
                    text: text,
                    messageID: messageID
                )
            },
            sendPosition: { latitude, longitude, comment in
                try await model.sendAPRSPosition(
                    latitude: latitude,
                    longitude: longitude,
                    comment: comment
                )
            }
        )
        .alert(
            model.aprsDVGatewayRecoveryAlert?.title ?? "Inspect DV Gateway?",
            isPresented: Binding(
                get: { model.aprsDVGatewayRecoveryAlert != nil },
                set: {
                    if !$0 {
                        model.hideAPRSDVGatewayRecoveryAlertPresentation()
                    }
                }
            )
        ) {
            if model.aprsDVGatewayRecoveryAlert?.automaticRecoveryAvailable == true {
                Button("Inspect DV Gateway and Retry APRS") {
                    Task { await model.inspectDVGatewayAndRetryAPRS() }
                }
            }
            Button(
                model.aprsDVGatewayRecoveryAlert?.dismissalButtonTitle
                    ?? "Dismiss",
                role: .cancel
            ) {
                model.dismissAPRSDVGatewayRecoveryAlert()
            }
        } message: {
            Text(
                model.aprsDVGatewayRecoveryAlert?.message
                    ?? "Azimuth needs your approval before inspecting Menu 983, Menu 506, and Menu 650 or changing Menu 650."
            )
        }
        .alert(
            "APRS operation",
            isPresented: Binding(
                get: { model.operationError != nil },
                set: { if !$0 { model.dismissOperationError() } }
            )
        ) {
            Button("OK") { model.dismissOperationError() }
        } message: {
            Text(model.operationError ?? "Unknown APRS error")
        }
    }

    private var controlConnectionName: String {
        if case .connected(_, let transport) = model.radioState.connection {
            return transport
        }
        return model.selectedRadioEndpoint?.transport.title ?? "selected control link"
    }
}

/// Operator surface for one host-owned APRS KISS session.
///
/// The parent owns the radio lifecycle. This view never starts a session or
/// transmits from `body`, `task`, or a state change: each side effect originates
/// in a labelled button and transmission gets a second explicit confirmation.
struct APRSOperationsWorkspace: View {
    let state: APRSOperationalState
    @Binding var configuration: APRSSessionConfiguration
    let settingDefinitions: [RadioSettingDefinition]
    let settingValues: [String: ProposedSettingValue]
    let controlConnectionName: String
    let isExternalOperationInFlight: Bool
    let startSession: @MainActor (APRSSessionConfiguration) async throws -> Void
    let stopSession: @MainActor () async throws -> Void
    let sendMessage: @MainActor (String, String, String?) async throws -> APRSActivity
    let sendPosition: @MainActor (Double, Double, String) async throws -> APRSActivity

    @State private var section: APRSWorkspaceSection = .activity
    @State private var activityFilter: APRSActivityFilter = .all
    @State private var searchText = ""
    @State private var selectedActivity: APRSActivity?
    @State private var selectedStation: APRSStation?
    @State private var showsStartConfirmation = false
    @State private var transmitSheet: APRSTransmitSheet?
    @State private var isPerformingLifecycleOperation = false
    @State private var operationError: String?

    private var status: APRSSessionStatus { state.status }

    private var filteredActivities: [APRSActivity] {
        Array(
            APRSActivityQuery(filter: activityFilter, text: searchText)
                .apply(to: state.activities)
                .reversed()
        )
    }

    private var filteredStations: [APRSStation] {
        APRSStationQuery(text: searchText).apply(to: state.stations)
    }

    private var mappedStations: [APRSStation] {
        filteredStations.filter(\.hasPlottablePosition)
    }

    private var filteredSettings: [RadioSettingDefinition] {
        APRSSettingQuery(text: searchText).apply(to: settingDefinitions)
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: AzimuthLayout.pageSpacing) {
                sessionHeader
                sectionPicker

                switch section {
                case .activity:
                    activitySection
                case .stations:
                    stationsSection
                case .configuration:
                    configurationSection
                }
            }
            .azimuthContentColumn(maxWidth: AzimuthLayout.workspaceWidth)
        }
        .azimuthPage()
        .accessibilityIdentifier("azimuth.page.aprs")
        .searchable(text: $searchText, prompt: searchPrompt)
        .radioSettingNavigationDestination()
        .sheet(isPresented: $showsStartConfirmation) {
            APRSConfirmationSheet(
                title: configuration.isReceiveOnly
                    ? "Start receive-only KISS?" : "Start APRS KISS?",
                message: "Azimuth will give the \(controlConnectionName) control session to the KISS TNC. "
                    + "The Radio screen, CAT controls, and persistent settings are paused "
                    + "until you stop APRS and CAT is restored.",
                symbol: "antenna.radiowaves.left.and.right",
                confirmationTitle: configuration.isReceiveOnly
                    ? "Start Receive-Only" : "Start KISS Session"
            ) {
                Task { await performStart() }
            }
        }
        .sheet(item: $selectedActivity) { activity in
            NavigationStack { APRSActivityDetail(activity: activity) }
                .presentationDetents([.medium, .large])
        }
        .sheet(item: $selectedStation) { station in
            NavigationStack { APRSStationDetail(station: station) }
                .presentationDetents([.medium, .large])
        }
        .sheet(item: $transmitSheet) { sheet in
            NavigationStack {
                switch sheet {
                case .message:
                    APRSMessageTransmitView(send: sendMessage)
                case .position:
                    APRSPositionTransmitView(send: sendPosition)
                }
            }
        }
        .alert("APRS operation failed", isPresented: operationErrorIsPresented) {
            Button("OK") { operationError = nil }
        } message: {
            Text(operationError ?? "Unknown APRS error")
        }
    }

    private var sessionHeader: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 16) {
                ViewThatFits(in: .horizontal) {
                    HStack(alignment: .top, spacing: 18) {
                        sessionIdentity
                        Spacer(minLength: 16)
                        sessionAction
                    }

                    VStack(alignment: .leading, spacing: 14) {
                        HStack(alignment: .top) {
                            sessionIdentity
                            Spacer(minLength: 12)
                            sessionAction
                        }
                    }
                }

                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 135), spacing: 12)],
                    alignment: .leading,
                    spacing: 12
                ) {
                    AzimuthMetric(label: "Received", value: String(status.receivedPackets))
                    AzimuthMetric(label: "Transmitted", value: String(status.transmittedPackets))
                    AzimuthMetric(label: "Stations", value: String(state.stations.count))
                    AzimuthMetric(
                        label: "Decode failures",
                        value: String(status.decodeFailures),
                        tint: status.decodeFailures == 0 ? .primary : AzimuthPalette.caution
                    )
                    AzimuthMetric(
                        label: "Journal drops",
                        value: String(status.droppedActivities),
                        tint: status.droppedActivities == 0 ? .primary : AzimuthPalette.caution
                    )
                    AzimuthMetric(
                        label: "Station mode",
                        value: activeConfiguration?.isReceiveOnly == false ? "RX + MANUAL TX" : "RX ONLY"
                    )
                }

                modeOwnershipNotice

                if let error = status.lastError, !error.isEmpty {
                    Label(error, systemImage: "exclamationmark.triangle.fill")
                        .font(.callout)
                        .foregroundStyle(AzimuthPalette.caution)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

    private var sessionIdentity: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 9) {
                AzimuthEyebrow("Host KISS TNC")
                sessionStatusPill
            }
            Text("APRS operations")
                .font(.title2.bold())
            Text(sessionDetail)
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    @ViewBuilder
    private var sessionStatusPill: some View {
        switch status.phase {
        case .unavailable:
            AzimuthStatusPill(
                title: "UNAVAILABLE",
                symbol: "cable.connector.slash",
                color: .secondary
            )
        case .inactive:
            AzimuthStatusPill(title: "STOPPED", symbol: "stop.circle", color: .secondary)
        case .starting:
            AzimuthStatusPill(
                title: "ENTERING KISS",
                symbol: "arrow.triangle.2.circlepath",
                color: AzimuthPalette.caution
            )
        case .active:
            AzimuthStatusPill(
                title: "KISS LIVE",
                symbol: "dot.radiowaves.left.and.right",
                color: AzimuthPalette.signal
            )
        case .restoring:
            AzimuthStatusPill(
                title: "RESTORING CAT",
                symbol: "arrow.triangle.2.circlepath",
                color: AzimuthPalette.caution
            )
        case .failed:
            AzimuthStatusPill(
                title: "SESSION FAILED",
                symbol: "exclamationmark.triangle.fill",
                color: .red
            )
        }
    }

    @ViewBuilder
    private var sessionAction: some View {
        switch status.phase {
        case .unavailable:
            Button("Start KISS") {}
                .buttonStyle(.borderedProminent)
                .disabled(true)
        case .inactive, .failed:
            Button {
                showsStartConfirmation = true
            } label: {
                Label(
                    configuration.isReceiveOnly ? "Start RX" : "Start KISS",
                    systemImage: "play.fill"
                )
                .frame(minWidth: 100)
            }
            .buttonStyle(.borderedProminent)
            .tint(AzimuthPalette.bearing)
            .disabled(
                isPerformingLifecycleOperation
                    || isExternalOperationInFlight
                    || configurationValidationError != nil
            )
            .accessibilityIdentifier("azimuth.aprs.start")
        case .starting, .restoring:
            ProgressView()
                .controlSize(.small)
                .frame(minWidth: 120, minHeight: 34)
        case .active:
            Button {
                Task { await performStop() }
            } label: {
                Label("Stop KISS", systemImage: "stop.fill")
                    .frame(minWidth: 100)
            }
            .buttonStyle(.borderedProminent)
            .tint(.red)
            .disabled(isPerformingLifecycleOperation || isExternalOperationInFlight)
            .accessibilityIdentifier("azimuth.aprs.stop")
        }
    }

    private var modeOwnershipNotice: some View {
        Label {
            Text(modeOwnershipText)
                .fixedSize(horizontal: false, vertical: true)
        } icon: {
            Image(systemName: catIsPaused ? "pause.rectangle.fill" : "info.circle.fill")
        }
        .font(.caption)
        .foregroundStyle(catIsPaused ? AzimuthPalette.caution : .secondary)
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            (catIsPaused ? AzimuthPalette.caution : Color.secondary).opacity(0.09),
            in: RoundedRectangle(cornerRadius: 9)
        )
    }

    private var sectionPicker: some View {
        Picker("APRS workspace", selection: $section) {
            ForEach(APRSWorkspaceSection.allCases) { item in
                Label(item.title, systemImage: item.symbol).tag(item)
            }
        }
        .pickerStyle(.segmented)
        .accessibilityIdentifier("azimuth.aprs.section")
    }

    private var activitySection: some View {
        InstrumentPanel(padding: 14) {
            VStack(alignment: .leading, spacing: 12) {
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    VStack(alignment: .leading, spacing: 3) {
                        AzimuthEyebrow("Timestamped journal")
                        Text("Live packet activity")
                            .font(.headline)
                    }
                    Spacer()
                    Picker("Filter", selection: $activityFilter) {
                        ForEach(APRSActivityFilter.allCases) { filter in
                            Label(filter.title, systemImage: filter.symbol).tag(filter)
                        }
                    }
                    .pickerStyle(.menu)
                }

                if state.historyTruncated {
                    Label(
                        "Earlier activity has rolled out of the bounded journal.",
                        systemImage: "clock.badge.exclamationmark"
                    )
                    .font(.caption)
                    .foregroundStyle(AzimuthPalette.caution)
                }

                transmitActions

                if filteredActivities.isEmpty {
                    activityEmptyState
                } else {
                    LazyVStack(spacing: 0) {
                        ForEach(filteredActivities) { activity in
                            Button {
                                selectedActivity = activity
                            } label: {
                                APRSActivityRow(activity: activity)
                            }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier("azimuth.aprs.activity.\(activity.sequence)")

                            if activity.id != filteredActivities.last?.id {
                                Divider().padding(.leading, 42)
                            }
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var transmitActions: some View {
        if status.phase.isActive {
            HStack(spacing: 10) {
                Button {
                    transmitSheet = .message
                } label: {
                    Label("One-shot message", systemImage: "message")
                }
                .buttonStyle(.bordered)
                .disabled(transmitIsUnavailable || isExternalOperationInFlight)
                .accessibilityIdentifier("azimuth.aprs.compose-message")

                Button {
                    transmitSheet = .position
                } label: {
                    Label("One-shot position", systemImage: "location")
                }
                .buttonStyle(.bordered)
                .disabled(transmitIsUnavailable || isExternalOperationInFlight)
                .accessibilityIdentifier("azimuth.aprs.compose-position")

                Spacer()

                if transmitIsUnavailable {
                    Label("RX-only session", systemImage: "lock.fill")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
    }

    @ViewBuilder
    private var activityEmptyState: some View {
        if !searchText.isEmpty || activityFilter != .all {
            ContentUnavailableView(
                "No matching activity",
                systemImage: "line.3.horizontal.decrease.circle",
                description: Text("Change the activity filter or search text.")
            )
        } else {
            switch status.phase {
            case .unavailable(let reason):
                ContentUnavailableView(
                    "APRS unavailable",
                    systemImage: "cable.connector.slash",
                    description: Text(reason)
                )
            case .active:
                ContentUnavailableView(
                    "Listening: no packets yet",
                    systemImage: "dot.radiowaves.left.and.right",
                    description: Text("The KISS receiver is active. This journal stays empty until real RF or session activity arrives.")
                )
            case .starting:
                ContentUnavailableView(
                    "Entering KISS",
                    systemImage: "arrow.triangle.2.circlepath",
                    description: Text("Waiting for the radio to hand the serial session to its TNC.")
                )
            case .restoring:
                ContentUnavailableView(
                    "Restoring CAT",
                    systemImage: "arrow.triangle.2.circlepath",
                    description: Text("Packet reception has stopped while regular radio control is restored.")
                )
            case .inactive, .failed:
                ContentUnavailableView(
                    "APRS session stopped",
                    systemImage: "stop.circle",
                    description: Text("Configure the session, then explicitly start KISS to receive packets.")
                )
            }
        }
    }

    private var stationsSection: some View {
        InstrumentPanel(padding: 14) {
            VStack(alignment: .leading, spacing: 12) {
                HStack(alignment: .firstTextBaseline) {
                    VStack(alignment: .leading, spacing: 3) {
                        AzimuthEyebrow("Derived from received RF")
                        Text("Heard stations")
                            .font(.headline)
                    }
                    Spacer()
                    Text("\(filteredStations.count) STATIONS")
                        .font(.caption2.bold().monospaced())
                        .foregroundStyle(.secondary)
                }

                if !mappedStations.isEmpty {
                    APRSStationMap(stations: mappedStations) { station in
                        selectedStation = station
                    }
                }

                if filteredStations.isEmpty {
                    stationEmptyState
                } else {
                    LazyVStack(spacing: 0) {
                        ForEach(filteredStations) { station in
                            Button {
                                selectedStation = station
                            } label: {
                                APRSStationRow(station: station)
                            }
                            .buttonStyle(.plain)

                            if station.id != filteredStations.last?.id {
                                Divider().padding(.leading, 42)
                            }
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var stationEmptyState: some View {
        if !searchText.isEmpty {
            ContentUnavailableView.search(text: searchText)
        } else if status.phase.isActive {
            ContentUnavailableView(
                "No stations heard yet",
                systemImage: "antenna.radiowaves.left.and.right",
                description: Text("Stations appear only after a real received AX.25 packet identifies its source.")
            )
        } else {
            ContentUnavailableView(
                "No retained stations",
                systemImage: "person.2.slash",
                description: Text("Start a KISS session to build the station list from received RF.")
            )
        }
    }

    private var configurationSection: some View {
        VStack(spacing: AzimuthLayout.pageSpacing) {
            sessionConfigurationPanel
            persistentSettingsPanel
        }
    }

    private var sessionConfigurationPanel: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 16) {
                HStack(alignment: .firstTextBaseline) {
                    VStack(alignment: .leading, spacing: 3) {
                        AzimuthEyebrow("Applied when KISS starts")
                        Text("Host session configuration")
                            .font(.headline)
                    }
                    Spacer()
                    Text(configuration.isReceiveOnly ? "RX ONLY" : "TX ARMED ON CONFIRMATION")
                        .font(.caption2.bold().monospaced())
                        .foregroundStyle(
                            configuration.isReceiveOnly ? Color.secondary : AzimuthPalette.caution
                        )
                }

                Text(
                    "These values configure the host-owned KISS session. They do not silently rewrite the radio’s persistent APRS menus."
                )
                .font(.caption)
                .foregroundStyle(.secondary)

                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 280), spacing: 14)],
                    alignment: .leading,
                    spacing: 14
                ) {
                    configurationTextField(
                        title: "Station callsign",
                        prompt: "Blank for receive-only",
                        text: $configuration.stationCallsign,
                        detail: "CALL or CALL-SSID. Blank prevents all app transmissions."
                    )
                    configurationTextField(
                        title: "Digipeater path",
                        prompt: "Direct path when blank",
                        text: $configuration.path,
                        detail: "Comma-separated AX.25 path, for example WIDE1-1,WIDE2-1."
                    )

                    VStack(alignment: .leading, spacing: 6) {
                        Text("Packet data rate")
                            .font(.subheadline.weight(.semibold))
                        Picker("Packet data rate", selection: $configuration.dataRate) {
                            ForEach(APRSPacketDataRate.allCases) { dataRate in
                                Text(dataRate.title).tag(dataRate)
                            }
                        }
                        .labelsHidden()
                        .pickerStyle(.segmented)
                    }

                    HStack(spacing: 12) {
                        configurationTextField(
                            title: "Symbol table",
                            prompt: "/",
                            text: $configuration.symbolTable,
                            detail: "One printable ASCII character."
                        )
                        configurationTextField(
                            title: "Symbol code",
                            prompt: ">",
                            text: $configuration.symbolCode,
                            detail: "One printable ASCII character."
                        )
                    }

                    configurationStepper(
                        title: "TX delay",
                        value: configurationUInt8Binding(\APRSSessionConfiguration.txDelay10ms),
                        range: 0...120,
                        suffix: " × 10 ms"
                    )
                    configurationStepper(
                        title: "Persistence",
                        value: configurationUInt8Binding(\APRSSessionConfiguration.persistence),
                        range: 0...255,
                        suffix: " · \(persistencePercent)%"
                    )
                    configurationStepper(
                        title: "Slot time",
                        value: configurationUInt8Binding(\APRSSessionConfiguration.slotTime10ms),
                        range: 0...250,
                        suffix: " × 10 ms"
                    )
                    configurationStepper(
                        title: "TX tail",
                        value: configurationUInt8Binding(\APRSSessionConfiguration.txTail10ms),
                        range: 0...255,
                        suffix: " × 10 ms"
                    )

                    Toggle("Full duplex KISS", isOn: $configuration.fullDuplex)
                        .font(.subheadline.weight(.semibold))
                }
                .disabled(configurationIsLocked)

                if let validationError = configurationValidationError {
                    Label(validationError, systemImage: "exclamationmark.triangle.fill")
                        .font(.caption)
                        .foregroundStyle(AzimuthPalette.caution)
                } else if configuration.isReceiveOnly {
                    Label(
                        "Receive-only is deliberate: message and position transmit controls remain locked.",
                        systemImage: "shield.checkered"
                    )
                    .font(.caption)
                    .foregroundStyle(AzimuthPalette.signal)
                }

                if configurationIsLocked {
                    Label(
                        "Stop KISS and wait for CAT restoration before changing the next session’s configuration.",
                        systemImage: "lock.fill"
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                }
            }
        }
    }

    private var persistentSettingsPanel: some View {
        InstrumentPanel(padding: 14) {
            VStack(alignment: .leading, spacing: 12) {
                HStack(alignment: .firstTextBaseline) {
                    VStack(alignment: .leading, spacing: 3) {
                        AzimuthEyebrow("Complete radio configuration")
                        Text("Every APRS setting")
                            .font(.headline)
                    }
                    Spacer()
                    Text("\(filteredSettings.count) SETTINGS")
                        .font(.caption2.bold().monospaced())
                        .foregroundStyle(.secondary)
                }

                if catIsPaused {
                    Label(
                        "Persistent setting reads and writes are unavailable while KISS owns the \(controlConnectionName) control link. Stop the session to edit these menus.",
                        systemImage: "pause.rectangle.fill"
                    )
                    .font(.caption)
                    .foregroundStyle(AzimuthPalette.caution)
                }

                if filteredSettings.isEmpty {
                    if searchText.isEmpty {
                        ContentUnavailableView(
                            "APRS settings unavailable",
                            systemImage: "slider.horizontal.3",
                            description: Text("The active radio catalog does not contain APRS definitions.")
                        )
                    } else {
                        ContentUnavailableView.search(text: searchText)
                    }
                } else {
                    LazyVStack(spacing: 0) {
                        ForEach(filteredSettings) { definition in
                            NavigationLink(value: RadioSettingDestination(id: definition.id)) {
                                APRSSettingLinkRow(
                                    definition: definition,
                                    value: settingValues[definition.id]
                                )
                            }
                            .buttonStyle(.plain)
                            .disabled(catIsPaused)

                            if definition.id != filteredSettings.last?.id {
                                Divider().padding(.leading, 38)
                            }
                        }
                    }
                }
            }
        }
    }

    private func configurationTextField(
        title: String,
        prompt: String,
        text: Binding<String>,
        detail: String
    ) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(title)
                .font(.subheadline.weight(.semibold))
            TextField(prompt, text: text)
                .textFieldStyle(.roundedBorder)
            Text(detail)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func configurationStepper(
        title: String,
        value: Binding<Int>,
        range: ClosedRange<Int>,
        suffix: String
    ) -> some View {
        Stepper(value: value, in: range) {
            HStack {
                Text(title)
                    .font(.subheadline.weight(.semibold))
                Spacer()
                Text("\(value.wrappedValue)\(suffix)")
                    .font(.caption.bold().monospaced())
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func configurationUInt8Binding(
        _ keyPath: WritableKeyPath<APRSSessionConfiguration, UInt8>
    ) -> Binding<Int> {
        Binding(
            get: { Int(configuration[keyPath: keyPath]) },
            set: { configuration[keyPath: keyPath] = UInt8(clamping: $0) }
        )
    }

    private var persistencePercent: Int {
        Int((Double(configuration.persistence) + 1) / 256 * 100)
    }

    private var activeConfiguration: APRSSessionConfiguration? {
        status.configuration ?? (status.phase.isActive ? configuration : nil)
    }

    private var transmitIsUnavailable: Bool {
        activeConfiguration?.isReceiveOnly != false
    }

    private var configurationIsLocked: Bool {
        switch status.phase {
        case .starting, .active, .restoring: return true
        case .unavailable, .inactive, .failed: return false
        }
    }

    private var catIsPaused: Bool {
        switch status.phase {
        case .starting, .active, .restoring: return true
        case .unavailable, .inactive, .failed: return false
        }
    }

    private var sessionDetail: String {
        switch status.phase {
        case .unavailable(let reason): return reason
        case .inactive: return "Stopped. No packet stream is active and normal radio control remains available."
        case .starting: return "The radio is leaving CAT and entering host-controlled KISS mode."
        case .active:
            if let startedAt = status.startedAt {
                return "Receiving real KISS traffic since \(startedAt.formatted(date: .abbreviated, time: .standard))."
            }
            return "Receiving real KISS traffic from the radio."
        case .restoring: return "KISS has stopped; Azimuth is requalifying regular CAT control."
        case .failed: return "The last APRS lifecycle transition failed. Review the journal before retrying."
        }
    }

    private var modeOwnershipText: String {
        if catIsPaused {
            return "KISS owns the \(controlConnectionName) control session. Radio screen streaming, front-panel CAT control, and persistent settings are paused; stopping APRS restores them."
        }
        return "KISS and CAT cannot own the TH-D75 \(controlConnectionName) control session at the same time. Starting APRS visibly pauses the Radio and Settings surfaces until restoration completes."
    }

    private var searchPrompt: String {
        switch section {
        case .activity: return "Search source, summary, path, or raw packet"
        case .stations: return "Search heard stations"
        case .configuration: return "Search every APRS setting"
        }
    }

    private var configurationValidationError: String? {
        APRSSessionConfigurationValidator.firstError(in: configuration)
    }

    private var operationErrorIsPresented: Binding<Bool> {
        Binding(
            get: { operationError != nil },
            set: { if !$0 { operationError = nil } }
        )
    }

    @MainActor
    private func performStart() async {
        guard !isPerformingLifecycleOperation,
              configurationValidationError == nil else { return }
        isPerformingLifecycleOperation = true
        defer { isPerformingLifecycleOperation = false }
        do {
            try await startSession(configuration)
        } catch {
            operationError = error.localizedDescription
        }
    }

    @MainActor
    private func performStop() async {
        guard !isPerformingLifecycleOperation else { return }
        isPerformingLifecycleOperation = true
        defer { isPerformingLifecycleOperation = false }
        do {
            try await stopSession()
        } catch {
            operationError = error.localizedDescription
        }
    }
}

enum APRSWorkspaceSection: String, CaseIterable, Identifiable, Equatable, Sendable {
    case activity
    case stations
    case configuration

    var id: String { rawValue }

    var title: String {
        switch self {
        case .activity: return "Activity"
        case .stations: return "Stations"
        case .configuration: return "Configuration"
        }
    }

    var symbol: String {
        switch self {
        case .activity: return "waveform.path.ecg"
        case .stations: return "person.2.wave.2"
        case .configuration: return "slider.horizontal.3"
        }
    }
}

enum APRSActivityFilter: String, CaseIterable, Identifiable, Equatable, Sendable {
    case all
    case received
    case transmitted
    case messages
    case positions
    case weather
    case problems
    case system

    var id: String { rawValue }

    var title: String {
        switch self {
        case .all: return "All activity"
        case .received: return "Received"
        case .transmitted: return "Transmitted"
        case .messages: return "Messages"
        case .positions: return "Positions"
        case .weather: return "Weather"
        case .problems: return "Problems"
        case .system: return "Session"
        }
    }

    var symbol: String {
        switch self {
        case .all: return "line.3.horizontal.decrease.circle"
        case .received: return "arrow.down"
        case .transmitted: return "arrow.up"
        case .messages: return "message"
        case .positions: return "location"
        case .weather: return "cloud.sun"
        case .problems: return "exclamationmark.triangle"
        case .system: return "gearshape"
        }
    }

    func includes(_ activity: APRSActivity) -> Bool {
        switch self {
        case .all: return true
        case .received: return activity.direction == .rx
        case .transmitted: return activity.direction == .tx
        case .messages: return activity.kind == .message
        case .positions: return activity.kind == .position
        case .weather: return activity.kind == .weather || activity.kind == .rawWeather
        case .problems: return activity.kind == .decodeError || activity.kind == .error
        case .system:
            return activity.direction == .system
                || activity.kind == .session
                || activity.kind == .kissControl
        }
    }
}

struct APRSActivityQuery: Equatable, Sendable {
    var filter: APRSActivityFilter = .all
    var text = ""

    func apply(to activities: [APRSActivity]) -> [APRSActivity] {
        let needle = normalized(text)
        return activities.filter { activity in
            guard filter.includes(activity) else { return false }
            guard !needle.isEmpty else { return true }
            let searchable = [
                activity.source,
                activity.destination,
                activity.summary,
                activity.rawPacket,
                activity.path.joined(separator: ","),
                activity.kind.rawValue,
                activity.direction.rawValue,
            ]
                .compactMap { $0 }
                .joined(separator: " ")
                .folding(options: [.caseInsensitive, .diacriticInsensitive], locale: .current)
                .lowercased()
            return searchable.contains(needle)
        }
    }

    private func normalized(_ value: String) -> String {
        value
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .folding(options: [.caseInsensitive, .diacriticInsensitive], locale: .current)
            .lowercased()
    }
}

struct APRSStationQuery: Equatable, Sendable {
    var text = ""

    func apply(to stations: [APRSStation]) -> [APRSStation] {
        let needle = text.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !needle.isEmpty else { return stations }
        return stations.filter { station in
            [
                station.callsign,
                station.latestSummary,
                station.path.joined(separator: ","),
            ]
                .joined(separator: " ")
                .lowercased()
                .contains(needle)
        }
    }
}

struct APRSSettingQuery: Equatable, Sendable {
    var text = ""

    func apply(to definitions: [RadioSettingDefinition]) -> [RadioSettingDefinition] {
        let needle = text.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return definitions
            .filter { definition in
                guard definition.group == .aprs else { return false }
                guard !needle.isEmpty else { return true }
                return [
                    definition.id,
                    definition.title,
                    definition.summary,
                    definition.menuNumberLabel ?? "",
                ]
                    .joined(separator: " ")
                    .lowercased()
                    .contains(needle)
            }
            .sorted(by: Self.settingOrder)
    }

    private static func settingOrder(
        _ left: RadioSettingDefinition,
        _ right: RadioSettingDefinition
    ) -> Bool {
        let leftMenu = left.menuNumbers.compactMap(Int.init).min() ?? Int.max
        let rightMenu = right.menuNumbers.compactMap(Int.init).min() ?? Int.max
        if leftMenu != rightMenu { return leftMenu < rightMenu }
        let titleOrder = left.title.localizedStandardCompare(right.title)
        if titleOrder != .orderedSame { return titleOrder == .orderedAscending }
        return left.id < right.id
    }
}

/// APRS actions deliberately avoid `confirmationDialog` on iPad. In regular
/// width that API is backed by a `UIAlertController` popover; the iPadOS 27
/// glass host can impose a different size than the alert controller and emit
/// unsatisfiable Auto Layout constraints. A normal SwiftUI form sheet keeps
/// the confirmation modal and adaptive without depending on that UIKit path.
struct APRSConfirmationSheet: View {
    @Environment(\.dismiss) private var dismiss

    let title: String
    let message: String
    let symbol: String
    let confirmationTitle: String
    var confirmationRole: ButtonRole?
    let confirm: @MainActor () -> Void

    init(
        title: String,
        message: String,
        symbol: String,
        confirmationTitle: String,
        confirmationRole: ButtonRole? = nil,
        confirm: @escaping @MainActor () -> Void
    ) {
        self.title = title
        self.message = message
        self.symbol = symbol
        self.confirmationTitle = confirmationTitle
        self.confirmationRole = confirmationRole
        self.confirm = confirm
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                HStack(alignment: .top, spacing: 14) {
                    Image(systemName: symbol)
                        .font(.title2.weight(.semibold))
                        .foregroundStyle(AzimuthPalette.bearing)
                        .frame(width: 44, height: 44)
                        .background(AzimuthPalette.bearing.opacity(0.12), in: Circle())
                        .accessibilityHidden(true)

                    VStack(alignment: .leading, spacing: 6) {
                        AzimuthEyebrow("Confirm APRS action")
                        Text(title)
                            .font(.title2.bold())
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }

                Text(message)
                    .font(.body)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                Divider()

                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 12) {
                        cancelButton
                        confirmationButton
                    }

                    VStack(spacing: 10) {
                        confirmationButton
                        cancelButton
                    }
                }
            }
            .frame(maxWidth: 520, alignment: .leading)
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .top)
        }
        .presentationSizing(.form)
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }

    private var cancelButton: some View {
        Button("Cancel", role: .cancel) {
            dismiss()
        }
        .buttonStyle(.bordered)
        .controlSize(.large)
        .frame(maxWidth: .infinity)
    }

    private var confirmationButton: some View {
        Button(role: confirmationRole) {
            dismiss()
            confirm()
        } label: {
            Text(confirmationTitle)
                .frame(maxWidth: .infinity)
        }
        .buttonStyle(.borderedProminent)
        .controlSize(.large)
        .tint(confirmationRole == .destructive ? .red : AzimuthPalette.bearing)
    }
}

enum APRSSessionConfigurationValidator {
    static func firstError(in configuration: APRSSessionConfiguration) -> String? {
        let station = configuration.stationCallsign
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .uppercased()
        if !station.isEmpty, !isValidStation(station) {
            return "Station callsign must be 1–6 A–Z/0–9 characters with an optional SSID from 0 through 15."
        }
        if !isPrintableASCIICharacter(configuration.symbolTable) {
            return "Symbol table must contain exactly one printable ASCII character."
        }
        if !isPrintableASCIICharacter(configuration.symbolCode) {
            return "Symbol code must contain exactly one printable ASCII character."
        }
        if configuration.txDelay10ms > 120 {
            return "TX delay must be between 0 and 120 (0–1200 ms)."
        }
        if configuration.slotTime10ms > 250 {
            return "Slot time must be between 0 and 250 (0–2500 ms)."
        }
        return nil
    }

    private static func isValidStation(_ value: String) -> Bool {
        let parts = value.split(separator: "-", omittingEmptySubsequences: false)
        guard (1...2).contains(parts.count),
              (1...6).contains(parts[0].count),
              parts[0].allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber) }) else {
            return false
        }
        guard parts.count == 2 else { return true }
        guard let ssid = UInt8(parts[1]), ssid <= 15 else { return false }
        return !parts[1].isEmpty
    }

    private static func isPrintableASCIICharacter(_ value: String) -> Bool {
        guard value.utf8.count == 1, let byte = value.utf8.first else { return false }
        return (0x21...0x7E).contains(byte)
    }
}

private enum APRSTransmitSheet: String, Identifiable {
    case message
    case position

    var id: String { rawValue }
}

private struct APRSActivityRow: View {
    let activity: APRSActivity

    var body: some View {
        HStack(alignment: .top, spacing: 11) {
            Image(systemName: activity.kind.symbol)
                .font(.body.weight(.semibold))
                .foregroundStyle(activity.direction.color)
                .frame(width: 30, height: 30)
                .background(activity.direction.color.opacity(0.11), in: Circle())

            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 7) {
                    Text(activity.direction.label)
                        .font(.caption2.bold().monospaced())
                        .foregroundStyle(activity.direction.color)
                    if let source = activity.source {
                        Text(source)
                            .font(.subheadline.bold().monospaced())
                    }
                    if let destination = activity.destination {
                        Image(systemName: "arrow.right")
                            .font(.caption2)
                            .foregroundStyle(.tertiary)
                        Text(destination)
                            .font(.caption.monospaced())
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    Text(activity.timestamp, format: .dateTime.hour().minute().second())
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                }

                Text(activity.summary)
                    .font(.callout)
                    .foregroundStyle(activity.kind == .error ? Color.red : .primary)
                    .lineLimit(3)

                if !activity.path.isEmpty {
                    Text("VIA \(activity.path.joined(separator: ", "))")
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }

            Image(systemName: "chevron.right")
                .font(.caption2.bold())
                .foregroundStyle(.tertiary)
                .padding(.top, 7)
        }
        .contentShape(Rectangle())
        .padding(.vertical, 10)
    }
}

/// A map of positions decoded from real received packets. There is no user
/// location request and no placeholder annotation; an empty station set means
/// this view is not created.
private struct APRSStationMap: View {
    let stations: [APRSStation]
    let select: (APRSStation) -> Void

    var body: some View {
        Map {
            ForEach(stations) { station in
                if let coordinate = station.coordinate {
                    Annotation(station.callsign, coordinate: coordinate, anchor: .bottom) {
                        Button {
                            select(station)
                        } label: {
                            VStack(spacing: 3) {
                                Text(station.callsign)
                                    .font(.caption2.bold().monospaced())
                                    .padding(.horizontal, 6)
                                    .padding(.vertical, 3)
                                    .foregroundStyle(.primary)
                                    .background(.regularMaterial, in: Capsule())
                                Image(systemName: "antenna.radiowaves.left.and.right.circle.fill")
                                    .font(.title2)
                                    .symbolRenderingMode(.palette)
                                    .foregroundStyle(AzimuthPalette.signal, .background)
                            }
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Open \(station.callsign) station details")
                    }
                }
            }
        }
        .mapStyle(.standard(elevation: .flat))
        .frame(height: 330)
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(.primary.opacity(0.10))
        }
        .accessibilityLabel("Map of \(stations.count) APRS stations with decoded positions")
    }
}

private struct APRSStationRow: View {
    let station: APRSStation

    var body: some View {
        HStack(alignment: .top, spacing: 11) {
            Image(systemName: "antenna.radiowaves.left.and.right")
                .foregroundStyle(AzimuthPalette.signal)
                .frame(width: 30, height: 30)
                .background(AzimuthPalette.signal.opacity(0.11), in: Circle())

            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text(station.callsign)
                        .font(.subheadline.bold().monospaced())
                    Text("\(station.packetCount) pkt")
                        .font(.caption2.bold().monospaced())
                        .foregroundStyle(.secondary)
                    Spacer()
                    Text(station.lastHeard, style: .relative)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Text(station.latestSummary)
                    .font(.callout)
                    .lineLimit(2)
                HStack(spacing: 12) {
                    if let latitude = station.latitude, let longitude = station.longitude {
                        Label(
                            String(format: "%.5f, %.5f", latitude, longitude),
                            systemImage: "location.fill"
                        )
                    }
                    if !station.path.isEmpty {
                        Label(station.path.joined(separator: ","), systemImage: "point.3.filled.connected.trianglepath.dotted")
                    }
                }
                .font(.caption2.monospaced())
                .foregroundStyle(.secondary)
                .lineLimit(1)
            }

            Image(systemName: "chevron.right")
                .font(.caption2.bold())
                .foregroundStyle(.tertiary)
                .padding(.top, 7)
        }
        .contentShape(Rectangle())
        .padding(.vertical, 10)
    }
}

private struct APRSSettingLinkRow: View {
    let definition: RadioSettingDefinition
    let value: ProposedSettingValue?

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: definition.group.symbol)
                .foregroundStyle(AzimuthPalette.bearing)
                .frame(width: 28)

            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 7) {
                    Text(definition.title)
                        .font(.subheadline.weight(.semibold))
                    if let menu = definition.menuNumberLabel {
                        THD75MenuBadge(menu)
                    }
                }
                Text(definition.summary)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }

            Spacer(minLength: 8)

            VStack(alignment: .trailing, spacing: 4) {
                if let value {
                    Text(definition.domain.displayText(for: value) ?? value.displayText)
                        .font(.caption.bold().monospaced())
                        .foregroundStyle(AzimuthPalette.signal)
                        .lineLimit(1)
                } else {
                    Text("NOT READ")
                        .font(.caption2.bold().monospaced())
                        .foregroundStyle(.secondary)
                }
                Image(systemName: "chevron.right")
                    .font(.caption2.bold())
                    .foregroundStyle(.tertiary)
            }
        }
        .contentShape(Rectangle())
        .padding(.vertical, 9)
    }
}

private struct APRSActivityDetail: View {
    @Environment(\.dismiss) private var dismiss
    let activity: APRSActivity

    var body: some View {
        List {
            Section("Decoded") {
                LabeledContent("Direction", value: activity.direction.label)
                LabeledContent("Category", value: activity.kind.label)
                LabeledContent("Time") {
                    Text(activity.timestamp, format: .dateTime.year().month().day().hour().minute().second())
                }
                if let source = activity.source { LabeledContent("Source", value: source) }
                if let destination = activity.destination { LabeledContent("Destination", value: destination) }
                if !activity.path.isEmpty {
                    LabeledContent("Path", value: activity.path.joined(separator: ", "))
                }
                Text(activity.summary)
                    .font(.callout)
                    .textSelection(.enabled)
            }

            if let latitude = activity.latitude, let longitude = activity.longitude {
                Section("Position") {
                    LabeledContent("Latitude", value: String(format: "%.6f°", latitude))
                    LabeledContent("Longitude", value: String(format: "%.6f°", longitude))
                    if let speed = activity.speedKnots {
                        LabeledContent("Speed", value: "\(speed) kn")
                    }
                    if let course = activity.courseDegrees {
                        LabeledContent("Course", value: "\(course)°")
                    }
                }
            }

            Section("Raw packet") {
                if activity.rawPacket.isEmpty {
                    Text("No raw packet for this session event.")
                        .foregroundStyle(.secondary)
                } else {
                    Text(activity.rawPacket)
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                }
            }

            if !activity.rawAX25.isEmpty {
                Section("AX.25 bytes") {
                    Text(activity.rawAX25.hexDump)
                        .font(.caption2.monospaced())
                        .textSelection(.enabled)
                }
            }

            Section("Journal") {
                LabeledContent("Sequence", value: String(activity.sequence))
                LabeledContent("Session", value: String(activity.sessionID))
            }
        }
        .navigationTitle(activity.kind.label)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("Done") { dismiss() }
            }
        }
    }
}

private struct APRSStationDetail: View {
    @Environment(\.dismiss) private var dismiss
    let station: APRSStation

    var body: some View {
        List {
            Section("Station") {
                LabeledContent("Callsign", value: station.callsign)
                LabeledContent("Packets heard", value: String(station.packetCount))
                LabeledContent("Last heard") {
                    Text(station.lastHeard, format: .dateTime.year().month().day().hour().minute().second())
                }
                if !station.path.isEmpty {
                    LabeledContent("Latest path", value: station.path.joined(separator: ", "))
                }
            }

            Section("Latest activity") {
                Text(station.latestSummary)
                    .textSelection(.enabled)
            }

            if let latitude = station.latitude, let longitude = station.longitude {
                Section("Latest position") {
                    LabeledContent("Latitude", value: String(format: "%.6f°", latitude))
                    LabeledContent("Longitude", value: String(format: "%.6f°", longitude))
                    if let speed = station.speedKnots {
                        LabeledContent("Speed", value: "\(speed) kn")
                    }
                    if let course = station.courseDegrees {
                        LabeledContent("Course", value: "\(course)°")
                    }
                }
            }
        }
        .navigationTitle(station.callsign)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("Done") { dismiss() }
            }
        }
    }
}

private struct APRSMessageTransmitView: View {
    @Environment(\.dismiss) private var dismiss
    let send: @MainActor (String, String, String?) async throws -> APRSActivity

    @State private var addressee = ""
    @State private var text = ""
    @State private var messageID = ""
    @State private var showsConfirmation = false
    @State private var isSending = false
    @State private var errorMessage: String?

    var body: some View {
        Form {
            Section("One-shot APRS message") {
                TextField("Destination callsign", text: $addressee)
                TextField("Message", text: $text, axis: .vertical)
                    .lineLimit(2...5)
                TextField("Message ID (optional)", text: $messageID)
            }

            Section {
                Label(
                    "Azimuth sends exactly one packet. It does not retry, wait for an acknowledgement, or claim delivery.",
                    systemImage: "exclamationmark.triangle.fill"
                )
                .foregroundStyle(AzimuthPalette.caution)
            }
        }
        .navigationTitle("Transmit message")
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Cancel") { dismiss() }
                    .disabled(isSending)
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Review & Transmit") { showsConfirmation = true }
                    .disabled(validationError != nil || isSending)
            }
        }
        .sheet(isPresented: $showsConfirmation) {
            APRSConfirmationSheet(
                title: "Transmit one APRS message?",
                message: "To \(normalizedAddressee): \(text). "
                    + "This is an RF transmission with no automatic retry or delivery guarantee.",
                symbol: "message.badge.waveform.fill",
                confirmationTitle: "Transmit Once",
                confirmationRole: .destructive
            ) {
                Task { await performSend() }
            }
        }
        .alert("Message not transmitted", isPresented: errorIsPresented) {
            Button("OK") { errorMessage = nil }
        } message: {
            Text(errorMessage ?? "Unknown APRS error")
        }
    }

    private var normalizedAddressee: String {
        addressee.trimmingCharacters(in: .whitespacesAndNewlines).uppercased()
    }

    private var normalizedMessageID: String? {
        let value = messageID.trimmingCharacters(in: .whitespacesAndNewlines).uppercased()
        return value.isEmpty ? nil : value
    }

    private var validationError: String? {
        APRSTransmitValidator.messageError(
            addressee: normalizedAddressee,
            text: text,
            messageID: normalizedMessageID
        )
    }

    private var errorIsPresented: Binding<Bool> {
        Binding(
            get: { errorMessage != nil },
            set: { if !$0 { errorMessage = nil } }
        )
    }

    @MainActor
    private func performSend() async {
        guard !isSending, validationError == nil else { return }
        isSending = true
        defer { isSending = false }
        do {
            _ = try await send(normalizedAddressee, text, normalizedMessageID)
            dismiss()
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

private struct APRSPositionTransmitView: View {
    @Environment(\.dismiss) private var dismiss
    let send: @MainActor (Double, Double, String) async throws -> APRSActivity

    @State private var latitude = ""
    @State private var longitude = ""
    @State private var comment = ""
    @State private var showsConfirmation = false
    @State private var isSending = false
    @State private var errorMessage: String?

    var body: some View {
        Form {
            Section("Manual position") {
                TextField("Latitude (−90…90)", text: $latitude)
                TextField("Longitude (−180…180)", text: $longitude)
                TextField("Comment (optional)", text: $comment, axis: .vertical)
                    .lineLimit(2...4)
                if let commentError = APRSTransmitValidator.positionTextError(comment) {
                    Text(commentError)
                        .font(.caption)
                        .foregroundStyle(AzimuthPalette.caution)
                }
            }

            Section {
                Label(
                    "Coordinates are transmitted exactly as entered. Azimuth does not substitute a device or radio GPS fix here.",
                    systemImage: "location.slash.fill"
                )
                Label(
                    "This sends one RF packet with no automatic repeat or delivery guarantee.",
                    systemImage: "exclamationmark.triangle.fill"
                )
                .foregroundStyle(AzimuthPalette.caution)
            }
        }
        .navigationTitle("Transmit position")
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Cancel") { dismiss() }
                    .disabled(isSending)
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Review & Transmit") { showsConfirmation = true }
                    .disabled(
                        parsedPosition == nil
                            || APRSTransmitValidator.positionTextError(comment) != nil
                            || isSending
                    )
            }
        }
        .sheet(isPresented: $showsConfirmation) {
            APRSConfirmationSheet(
                title: "Transmit one position packet?",
                message: positionConfirmationText,
                symbol: "location.fill",
                confirmationTitle: "Transmit Once",
                confirmationRole: .destructive
            ) {
                Task { await performSend() }
            }
        }
        .alert("Position not transmitted", isPresented: errorIsPresented) {
            Button("OK") { errorMessage = nil }
        } message: {
            Text(errorMessage ?? "Unknown APRS error")
        }
    }

    private var parsedPosition: (latitude: Double, longitude: Double)? {
        APRSTransmitValidator.position(latitude: latitude, longitude: longitude)
    }

    private var positionConfirmationText: String {
        guard let parsedPosition else { return "The coordinates are invalid." }
        return String(
            format: "Transmit %.6f°, %.6f° once? Verify these manual coordinates before keying RF.",
            parsedPosition.latitude,
            parsedPosition.longitude
        )
    }

    private var errorIsPresented: Binding<Bool> {
        Binding(
            get: { errorMessage != nil },
            set: { if !$0 { errorMessage = nil } }
        )
    }

    @MainActor
    private func performSend() async {
        guard !isSending,
              let position = parsedPosition,
              APRSTransmitValidator.positionTextError(comment) == nil else { return }
        isSending = true
        defer { isSending = false }
        do {
            _ = try await send(position.latitude, position.longitude, comment)
            dismiss()
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

enum APRSTransmitValidator {
    /// APRS message text is at most 67 bytes in the protocol implementation.
    static let maximumMessageBytes = 67
    /// Uncompressed position text has 43 bytes after the symbol code.
    static let maximumPositionTextBytes = 43

    static func messageError(
        addressee: String,
        text: String,
        messageID: String?
    ) -> String? {
        guard !addressee.isEmpty,
              addressee.utf8.count <= 9,
              addressee.utf8.allSatisfy({ $0 < 0x80 }) else {
            return "Destination must contain 1–9 ASCII characters."
        }
        guard !text.isEmpty, text.utf8.count <= maximumMessageBytes else {
            return "Message must contain 1–\(maximumMessageBytes) UTF-8 bytes."
        }
        if let messageID {
            guard (1...5).contains(messageID.utf8.count),
                  messageID.utf8.allSatisfy({ byte in
                      (0x30...0x39).contains(byte)
                          || (0x41...0x5A).contains(byte)
                          || (0x61...0x7A).contains(byte)
                  }) else {
                return "Message ID must contain 1–5 ASCII letters or digits."
            }
        }
        return nil
    }

    static func positionTextError(_ text: String) -> String? {
        let bytes = Array(text.utf8)
        guard bytes.count <= maximumPositionTextBytes else {
            return "Position comment must contain at most \(maximumPositionTextBytes) ASCII bytes."
        }
        guard bytes.allSatisfy({ (0x20...0x7E).contains($0) }) else {
            return "Position comment must contain only printable ASCII characters."
        }
        guard !bytes.contains(0x7C), !bytes.contains(0x7E) else {
            return "Position comment cannot contain | or ~."
        }
        return nil
    }

    static func position(
        latitude: String,
        longitude: String
    ) -> (latitude: Double, longitude: Double)? {
        guard let latitude = decimal(latitude),
              let longitude = decimal(longitude),
              latitude.isFinite,
              longitude.isFinite,
              (-90...90).contains(latitude),
              (-180...180).contains(longitude) else { return nil }
        return (latitude, longitude)
    }

    private static func decimal(_ value: String) -> Double? {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        if let direct = Double(trimmed) { return direct }
        guard let separator = Locale.current.decimalSeparator, separator != "." else { return nil }
        return Double(trimmed.replacingOccurrences(of: separator, with: "."))
    }
}

private extension APRSActivityDirection {
    var label: String {
        switch self {
        case .rx: return "RX"
        case .tx: return "TX"
        case .system: return "SYS"
        }
    }

    var color: Color {
        switch self {
        case .rx: return AzimuthPalette.signal
        case .tx: return AzimuthPalette.caution
        case .system: return AzimuthPalette.bearing
        }
    }
}

private extension APRSActivityKind {
    var label: String {
        switch self {
        case .session: return "Session"
        case .position: return "Position"
        case .message: return "Message"
        case .status: return "Status"
        case .object: return "Object"
        case .item: return "Item"
        case .weather: return "Weather"
        case .telemetry: return "Telemetry"
        case .query: return "Query"
        case .thirdParty: return "Third-party"
        case .grid: return "Grid"
        case .rawGPS: return "Raw GPS"
        case .capabilities: return "Capabilities"
        case .directionFinding: return "Direction finding"
        case .userDefined: return "User-defined"
        case .test: return "Test/invalid"
        case .rawWeather: return "Raw weather"
        case .ax25: return "AX.25"
        case .kissControl: return "KISS control"
        case .decodeError: return "Decode error"
        case .error: return "Error"
        }
    }

    var symbol: String {
        switch self {
        case .session, .kissControl: return "gearshape.2"
        case .position, .grid: return "location"
        case .message: return "message"
        case .status: return "text.bubble"
        case .object, .item: return "mappin.and.ellipse"
        case .weather, .rawWeather: return "cloud.sun"
        case .telemetry: return "gauge.with.dots.needle.50percent"
        case .query: return "questionmark.bubble"
        case .thirdParty: return "arrow.triangle.branch"
        case .rawGPS: return "location.north.line"
        case .capabilities: return "list.bullet.clipboard"
        case .directionFinding: return "scope"
        case .userDefined: return "curlybraces"
        case .test: return "testtube.2"
        case .ax25: return "waveform"
        case .decodeError, .error: return "exclamationmark.triangle.fill"
        }
    }
}

private extension APRSStation {
    var hasPlottablePosition: Bool { coordinate != nil }

    var coordinate: CLLocationCoordinate2D? {
        guard let latitude,
              let longitude,
              latitude.isFinite,
              longitude.isFinite,
              (-90...90).contains(latitude),
              (-180...180).contains(longitude) else { return nil }
        return CLLocationCoordinate2D(latitude: latitude, longitude: longitude)
    }
}

private extension Data {
    var hexDump: String {
        enumerated()
            .map { index, byte in
                (index > 0 && index.isMultiple(of: 16) ? "\n" : "")
                    + String(format: "%02X", byte)
            }
            .joined(separator: " ")
    }
}
