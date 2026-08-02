// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

/// Stable navigation value shared by every surface that links into the radio
/// settings catalog. The destination resolves the definition from the current
/// catalog when it is presented, so callers never carry a stale definition.
struct RadioSettingDestination: Hashable, Sendable {
    let id: String
}

extension View {
    func radioSettingNavigationDestination() -> some View {
        navigationDestination(for: RadioSettingDestination.self) { destination in
            RadioSettingDetailView(id: destination.id)
        }
    }
}

private struct RadioSettingDetailView: View {
    @Environment(AzimuthSceneModel.self) private var model
    let id: String

    var body: some View {
        if let definition = model.catalog.definition(id: id) {
            SettingEditorView(definition: definition)
        } else {
            ContentUnavailableView(
                "Setting unavailable",
                systemImage: "exclamationmark.triangle",
                description: Text("The active catalog no longer contains \(id).")
            )
        }
    }
}

struct SettingsCatalogView: View {
    @Environment(AzimuthSceneModel.self) private var model
    @State private var searchText = ""
    @State private var selectedGroup: RadioSettingGroup?

    private var sections: [RadioSettingSection] {
        model.catalog.sections(query: searchText, group: selectedGroup)
    }

    private var resultCount: Int {
        sections.reduce(0) { $0 + $1.definitions.count }
    }

    var body: some View {
        List {
            Section { catalogHeader }
                .listRowInsets(
                    EdgeInsets(
                        top: 8,
                        leading: AzimuthLayout.pageGutter,
                        bottom: 8,
                        trailing: AzimuthLayout.pageGutter
                    )
                )
                .listRowBackground(Color.clear)
                .listRowSeparator(.hidden)

            if sections.isEmpty {
                ContentUnavailableView.search(text: searchText)
                    .listRowBackground(Color.clear)
            } else {
                ForEach(sections) { section in
                    Section {
                        ForEach(section.definitions) { definition in
                            NavigationLink(value: RadioSettingDestination(id: definition.id)) {
                                SettingCatalogRow(
                                    definition: definition,
                                    liveValue: model.radioState.settingValues[definition.id]
                                )
                            }
                        }
                    } header: {
                        Label(section.group.title, systemImage: section.group.symbol)
                            .font(.caption.bold())
                    }
                }
            }
        }
        #if os(macOS)
        .listStyle(.inset)
        #else
        .listStyle(.insetGrouped)
        #endif
        .frame(maxWidth: AzimuthLayout.browseWidth)
        .frame(maxWidth: .infinity)
        .scrollContentBackground(.hidden)
        .azimuthPage()
        .accessibilityIdentifier("azimuth.page.settings")
        .searchable(text: $searchText, prompt: "Search every D75 setting")
        .radioSettingNavigationDestination()
        .toolbar {
            ToolbarItem { groupMenu }
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
                            ? "Read settings from radio" : "Reload catalog",
                        systemImage: "arrow.clockwise"
                    )
                }
                .disabled(
                    model.catalogLoadState == .loading
                        || model.isRadioOperationInFlight
                )
            }
        }
    }

    private var catalogHeader: some View {
        VStack(alignment: .leading, spacing: AzimuthLayout.cardSpacing) {
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .top) {
                    VStack(alignment: .leading, spacing: 6) {
                        AzimuthEyebrow("Complete configuration")
                        Text("Search, inspect, and apply every field in the reviewed TH-D75 schema.")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    sourceBadge
                }

                VStack(alignment: .leading, spacing: 8) {
                    AzimuthEyebrow("Complete configuration")
                    Text("Search, inspect, and apply every field in the reviewed TH-D75 schema.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                    sourceBadge
                }
            }

            LazyVGrid(
                columns: [GridItem(.adaptive(minimum: 135), spacing: AzimuthLayout.cardSpacing)],
                alignment: .leading,
                spacing: AzimuthLayout.cardSpacing
            ) {
                AzimuthMetric(label: "Visible", value: "\(resultCount)")
                AzimuthMetric(label: "Catalog", value: "\(model.catalog.definitions.count)")
                AzimuthMetric(
                    label: "Live scalar",
                    value: "\(model.radioState.settingValues.count)"
                )
                AzimuthMetric(
                    label: "Write path",
                    value: model.radioState.capabilities.settingWrite.isAvailable ? "READY" : "OFFLINE",
                    tint: model.radioState.capabilities.settingWrite.isAvailable
                        ? AzimuthPalette.signal : .secondary
                )
            }

            if case .designPreview = model.catalog.source {
                Label(
                    "Design-time sample definitions are clearly prefixed `preview`. The core adapter replaces them with the complete reviewed D75 schema.",
                    systemImage: "hammer.fill"
                )
                .font(.caption)
                .foregroundStyle(AzimuthPalette.caution)
            }
        }
        .padding(.vertical, 8)
    }

    @ViewBuilder
    private var sourceBadge: some View {
        switch model.catalog.source {
        case .designPreview:
            AzimuthStatusPill(title: "DESIGN DATA", symbol: "hammer.fill", color: AzimuthPalette.caution)
        case .reviewedSchema(let version):
            AzimuthStatusPill(title: version.uppercased(), symbol: "checkmark.seal.fill", color: AzimuthPalette.bearing)
        case .radioSnapshot:
            AzimuthStatusPill(title: "LIVE SNAPSHOT", symbol: "bolt.horizontal.circle.fill", color: AzimuthPalette.signal)
        }
    }

    private var groupMenu: some View {
        Menu {
            Button {
                selectedGroup = nil
            } label: {
                if selectedGroup == nil {
                    Label("All groups", systemImage: "checkmark")
                } else {
                    Text("All groups")
                }
            }
            Divider()
            ForEach(model.catalog.groups) { group in
                Button {
                    selectedGroup = group
                } label: {
                    if selectedGroup == group {
                        Label(group.title, systemImage: "checkmark")
                    } else {
                        Text(group.title)
                    }
                }
            }
        } label: {
            Label(
                selectedGroup?.title ?? "All groups",
                systemImage: "line.3.horizontal.decrease.circle"
            )
        }
    }
}

private struct SettingCatalogRow: View {
    let definition: RadioSettingDefinition
    let liveValue: ProposedSettingValue?

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: definition.domain.symbol)
                .font(.body.weight(.semibold))
                .foregroundStyle(AzimuthPalette.bearing)
                .frame(width: 28)

            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 7) {
                    Text(definition.title)
                        .font(.body.weight(.medium))
                    if let menuNumberLabel = definition.menuNumberLabel {
                        THD75MenuBadge(menuNumberLabel)
                    }
                }
                Text(definition.summary)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }

            Spacer(minLength: 10)

            VStack(alignment: .trailing, spacing: 3) {
                Text(liveValue.map { settingValueLabel($0, definition: definition) } ?? "NOT READ")
                    .font(.caption.weight(.semibold).monospaced())
                    .foregroundStyle(liveValue == nil ? .secondary : AzimuthPalette.signal)
                Text(definition.domain.kindTitle)
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(.vertical, 4)
    }
}

private struct SettingEditorView: View {
    @Environment(AzimuthSceneModel.self) private var model
    let definition: RadioSettingDefinition

    @State private var draft: ProposedSettingValue?
    @State private var scaledDraftText = ""
    @State private var scaledDraftError: String?

    private var currentValue: ProposedSettingValue? {
        model.radioState.settingValues[definition.id]
    }

    private var isDirty: Bool {
        guard let draft, let currentValue else { return false }
        return draft != currentValue
    }

    private var hasDraftEdits: Bool {
        guard case .scaledInteger(let scale) = definition.domain,
              case .integer(let rawValue) = currentValue else { return isDirty }
        return isDirty
            || scale.displayText(rawValue: rawValue, includesUnit: false) != scaledDraftText
    }

    private var isApplying: Bool {
        if case .applying = model.manualSettingApplyState { return true }
        return false
    }

    private var canEdit: Bool {
        !definition.isSpecializedEditor
            && currentValue != nil
            && model.radioState.connection.isConnected
            && model.radioState.capabilities.settingWrite.isAvailable
            && !isApplying
    }

    private var draftIsValid: Bool {
        guard let draft, definition.domain.accepts(draft) else { return false }
        if case .scaledInteger = definition.domain {
            return scaledDraftError == nil
        }
        return true
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                editorHeader
                valueComparison
                editorPanel
                applyStatus
                schemaPanel
            }
            .frame(maxWidth: 760, alignment: .leading)
            .padding()
        }
        .azimuthPage()
        .navigationTitle(definition.title)
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .onAppear {
            resetDraft(to: currentValue)
            model.resetManualSettingApplyState()
        }
        .onChange(of: currentValue) { oldValue, newValue in
            if draft == nil || draft == oldValue { resetDraft(to: newValue) }
        }
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Cancel") { resetDraft(to: currentValue) }
                    .disabled(!hasDraftEdits || isApplying)
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Apply") {
                    guard let draft else { return }
                    Task { await model.applyManualSetting(id: definition.id, targetValue: draft) }
                }
                .buttonStyle(.borderedProminent)
                .disabled(!isDirty || !canEdit || !draftIsValid)
            }
        }
    }

    private var editorHeader: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Label(definition.group.title, systemImage: definition.group.symbol)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(AzimuthPalette.bearing)
                    if let menuNumberLabel = definition.menuNumberLabel {
                        THD75MenuBadge(menuNumberLabel)
                    }
                    Spacer()
                    Text(definition.id)
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                }
                Text(definition.title)
                    .font(.largeTitle.bold())
                Text(definition.summary)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var valueComparison: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 12) {
                AzimuthEyebrow("Change review")
                HStack(spacing: 14) {
                    comparisonValue(
                        title: "LIVE BEFORE",
                        value: currentValue.map { settingValueLabel($0, definition: definition) } ?? "NOT READ",
                        color: currentValue == nil ? .secondary : .primary
                    )
                    Image(systemName: "arrow.right")
                        .foregroundStyle(isDirty ? AzimuthPalette.bearing : .secondary)
                    comparisonValue(
                        title: "STAGED AFTER",
                        value: draft.map { settingValueLabel($0, definition: definition) } ?? "NO STAGE",
                        color: isDirty ? AzimuthPalette.signal : .secondary
                    )
                }
            }
        }
    }

    private func comparisonValue(title: String, value: String, color: Color) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(title)
                .font(.caption2.bold().monospaced())
                .foregroundStyle(.secondary)
            Text(value)
                .font(.title3.weight(.semibold).monospaced())
                .foregroundStyle(color)
                .lineLimit(2)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(12)
        .background(.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 10))
    }

    private var editorPanel: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 14) {
                HStack {
                    AzimuthEyebrow("Editor · \(definition.domain.kindTitle)")
                    Spacer()
                    if !canEdit {
                        AzimuthStatusPill(title: "READ ONLY", symbol: "lock.fill", color: .secondary)
                    }
                }

                if definition.isSpecializedEditor {
                    Label(
                        "This setting requires a specialized editor and remains read-only here.",
                        systemImage: "wrench.and.screwdriver"
                    )
                    .foregroundStyle(AzimuthPalette.caution)
                } else if currentValue == nil {
                    Label(
                        "Connect and read the radio to load the live before value and unlock this editor.",
                        systemImage: "arrow.down.to.line.compact"
                    )
                    .foregroundStyle(.secondary)
                }

                settingControl
                    .disabled(!canEdit)
            }
        }
    }

    @ViewBuilder
    private var settingControl: some View {
        switch definition.domain {
        case .boolean:
            Toggle("Enabled", isOn: Binding(
                get: {
                    guard case .boolean(let value) = draft else { return false }
                    return value
                },
                set: { draft = .boolean($0) }
            ))
            .toggleStyle(.switch)

        case .choice(let options):
            Picker("Value", selection: Binding(
                get: {
                    guard case .choice(let rawValue) = draft else {
                        return options.first?.rawValue ?? 0
                    }
                    return rawValue
                },
                set: { draft = .choice(rawValue: $0) }
            )) {
                ForEach(options) { option in
                    Text(option.label).tag(option.rawValue)
                }
            }
            .pickerStyle(.menu)

        case .integer(let range, let step, let unit):
            Stepper(
                value: Binding(
                    get: {
                        guard case .integer(let value) = draft else { return range.lowerBound }
                        return value
                    },
                    set: { draft = .integer($0) }
                ),
                in: range,
                step: step
            ) {
                HStack {
                    Text("Value")
                    Spacer()
                    Text(integerDraftLabel(unit: unit))
                        .font(.body.monospacedDigit())
                }
            }

        case .scaledInteger(let scale):
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    TextField("Value", text: $scaledDraftText)
                        .textFieldStyle(.roundedBorder)
                        #if os(iOS)
                        .keyboardType(.decimalPad)
                        #endif
                        .onChange(of: scaledDraftText) { _, newValue in
                            updateScaledDraft(newValue, scale: scale)
                        }
                    Text(scale.inputUnit)
                        .foregroundStyle(.secondary)
                }
                Text(scale.summary)
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
                if let scaledDraftError {
                    Label(scaledDraftError, systemImage: "exclamationmark.triangle.fill")
                        .font(.caption)
                        .foregroundStyle(.red)
                }
            }

        case .text(let maxLength, let encoding):
            TextField(
                "Value",
                text: Binding(
                    get: {
                        guard case .text(let value) = draft else { return "" }
                        return value
                    },
                    set: { draft = .text(utf8Prefix($0, maxBytes: maxLength)) }
                )
            )
            .textFieldStyle(.roundedBorder)
            Text("\(textDraftCount) / \(maxLength) \(encoding.title) bytes")
                .font(.caption.monospacedDigit())
                .foregroundStyle(
                    textDraftIsValid(maxLength: maxLength, encoding: encoding)
                        ? Color.secondary : Color.red
                )
            if encoding == .ascii {
                Text("This radio field accepts ASCII characters only.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

        case .data(let description):
            Label(description, systemImage: "shippingbox")
                .foregroundStyle(.secondary)
        }
    }

    @ViewBuilder
    private var applyStatus: some View {
        switch model.manualSettingApplyState {
        case .idle:
            EmptyView()
        case .applying(let progress):
            InstrumentPanel {
                VStack(alignment: .leading, spacing: 10) {
                    Label("Applying to TH-D75…", systemImage: "arrow.triangle.2.circlepath")
                        .font(.headline)
                    ProgressView(value: progress.fractionCompleted)
                }
            }
        case .applied:
            InstrumentPanel {
                Label("Change applied and radio state refreshed.", systemImage: "checkmark.seal.fill")
                    .font(.headline)
                    .foregroundStyle(AzimuthPalette.signal)
            }
        case .failed(_, let message):
            InstrumentPanel {
                Label(message, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
            }
        }
    }

    private var schemaPanel: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 10) {
                AzimuthEyebrow("Accepted domain")
                LabeledContent("Type", value: definition.domain.kindTitle)
                LabeledContent("Range", value: definition.domain.summary)
                if let menuNumberLabel = definition.menuNumberLabel {
                    LabeledContent("TH-D75 menu", value: menuNumberLabel)
                }
                if let schemaReference = definition.schemaReference {
                    LabeledContent("Schema location", value: schemaReference)
                }
                LabeledContent("Restart required", value: definition.requiresRestart ? "Yes" : "No")
            }
        }
    }

    private func integerDraftLabel(unit: String?) -> String {
        guard case .integer(let value) = draft else { return "–" }
        return "\(value)\(unit.map { " \($0)" } ?? "")"
    }

    private var textDraftCount: Int {
        guard case .text(let value) = draft else { return 0 }
        return value.utf8.count
    }

    private func textDraftIsValid(
        maxLength: Int,
        encoding: RadioTextEncoding
    ) -> Bool {
        guard case .text(let value) = draft else { return false }
        return value.utf8.count <= maxLength && encoding.accepts(value)
    }

    private func resetDraft(to value: ProposedSettingValue?) {
        draft = value
        guard case .scaledInteger(let scale) = definition.domain else {
            scaledDraftText = ""
            scaledDraftError = nil
            return
        }
        guard case .integer(let rawValue) = value,
              let text = scale.displayText(rawValue: rawValue, includesUnit: false) else {
            scaledDraftText = ""
            scaledDraftError = value == nil ? nil : "The live raw value cannot be displayed safely."
            return
        }
        scaledDraftText = text
        scaledDraftError = nil
    }

    private func updateScaledDraft(_ value: String, scale: RadioScaledIntegerDomain) {
        if let parsed = definition.domain.parseDisplayValue(value),
           definition.domain.accepts(parsed) {
            draft = parsed
            scaledDraftError = nil
            return
        }

        // Some legal raw endpoints round to display text that cannot be
        // re-encoded (9999 -> 60.0 seconds). Showing that live value is safe;
        // it remains unchanged until the operator enters an authorable value.
        if case .integer(let currentRaw) = currentValue,
           scale.displayText(rawValue: currentRaw, includesUnit: false) == value {
            draft = currentValue
            scaledDraftError = nil
            return
        }
        scaledDraftError = "Enter a value in \(scale.summary)."
    }
}

struct THD75MenuBadge: View {
    let label: String

    init(_ label: String) {
        self.label = label
    }

    var body: some View {
        Text(label.uppercased())
            .font(.caption2.bold().monospaced())
            .foregroundStyle(AzimuthPalette.bearing)
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .background(AzimuthPalette.bearing.opacity(0.10), in: Capsule())
            .accessibilityLabel(label)
    }
}

private func settingValueLabel(
    _ value: ProposedSettingValue,
    definition: RadioSettingDefinition
) -> String {
    definition.domain.displayText(for: value) ?? value.displayText
}

private func utf8Prefix(_ value: String, maxBytes: Int) -> String {
    var result = ""
    for character in value {
        let candidate = result + String(character)
        guard candidate.utf8.count <= maxBytes else { break }
        result = candidate
    }
    return result
}
