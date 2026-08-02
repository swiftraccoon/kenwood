// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

struct AssistantView: View {
    @Environment(AzimuthSceneModel.self) private var model
    @State private var request = ""
#if os(iOS)
    @State private var speechInput = AssistantSpeechInput()
#endif

    private let examples = [
        "Make the radio quiet for a meeting",
        "Reduce display brightness for night use",
        "Set up conservative APRS beaconing",
    ]

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: AzimuthLayout.pageSpacing) {
                assistantHeader

                switch model.assistantWorkflow {
                case .idle:
                    composer
                    assistantEmptyState
                case .proposing(let submittedRequest):
                    proposingState(request: submittedRequest)
                case .review(let plan):
                    planReview(plan)
                case .applying(let plan, let progress):
                    applyingState(plan: plan, progress: progress)
                case .applied(let plan, let report):
                    appliedState(plan: plan, report: report)
                case .failed(let plan, let report, let message):
                    failedState(plan: plan, report: report, message: message)
                }
            }
            .azimuthContentColumn(maxWidth: AzimuthLayout.standardWidth)
        }
        .azimuthPage()
        .accessibilityIdentifier("azimuth.page.assistant")
        .onDisappear {
#if os(iOS)
            Task { await speechInput.cancel() }
#endif
        }
    }

    private var assistantHeader: some View {
        ViewThatFits(in: .horizontal) {
            HStack(alignment: .firstTextBaseline, spacing: AzimuthLayout.pageSpacing) {
                VStack(alignment: .leading, spacing: 6) {
                    AzimuthEyebrow("On-device intelligence")
                    Text("Describe an outcome. Azimuth grounds a concrete proposal in the loaded catalog, validates every target, and waits for your Accept or Decline.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                assistantAvailabilityBadge
            }

            VStack(alignment: .leading, spacing: 8) {
                AzimuthEyebrow("On-device intelligence")
                Text("Describe an outcome. Azimuth builds a concrete, validated proposal and waits for your approval.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                assistantAvailabilityBadge
            }
        }
        .padding(.horizontal, 4)
    }

    @ViewBuilder
    private var assistantAvailabilityBadge: some View {
        switch model.assistantAvailability {
        case .available:
            AzimuthStatusPill(
                title: "ON DEVICE",
                symbol: "checkmark.circle.fill",
                color: AzimuthPalette.signal
            )
        case .unavailable:
            AzimuthStatusPill(
                title: "UNAVAILABLE",
                symbol: "exclamationmark.circle",
                color: AzimuthPalette.caution
            )
        }
    }

    private var composer: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 14) {
                Text("What should your D75 do?")
                    .font(.title3.bold())
                TextField(
                    "Describe the result you want…",
                    text: $request,
                    axis: .vertical
                )
                .lineLimit(3...7)
                .textFieldStyle(.roundedBorder)
                .onSubmit { submitRequest() }
                .disabled(assistantSpeechIsActive)
                .accessibilityIdentifier("azimuth.assistant.request")

#if os(iOS)
                assistantSpeechControls
#endif

                ScrollView(.horizontal, showsIndicators: false) {
                    HStack {
                        ForEach(examples, id: \.self) { example in
                            Button(example) { request = example }
                                .buttonStyle(.bordered)
                                .controlSize(.small)
                        }
                    }
                }
                .disabled(assistantSpeechIsActive)

                ViewThatFits(in: .horizontal) {
                    HStack {
                        assistantComposerMetadata
                        Spacer()
                        generateChangesButton
                    }

                    VStack(alignment: .leading, spacing: 3) {
                        assistantComposerMetadata
                        generateChangesButton
                            .frame(maxWidth: .infinity, alignment: .trailing)
                    }
                }
            }
        }
    }

    private var assistantComposerMetadata: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text("\(model.catalog.definitions.count) catalog definitions available")
                .font(.caption.weight(.semibold))
            if case .unavailable(let reason) = model.assistantAvailability {
                Text(reason)
                    .font(.caption)
                    .foregroundStyle(AzimuthPalette.caution)
            }
        }
    }

    private var generateChangesButton: some View {
        Button {
            submitRequest()
        } label: {
            Label("Generate Changes", systemImage: "sparkles")
        }
        .buttonStyle(.borderedProminent)
        .disabled(
            request.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                || !model.assistantAvailability.isAvailable
                || assistantSpeechIsActive
        )
    }

    private var assistantSpeechIsActive: Bool {
#if os(iOS)
        speechInput.isActive
#else
        false
#endif
    }

#if os(iOS)
    private var assistantSpeechControls: some View {
        VStack(alignment: .leading, spacing: 8) {
            switch speechInput.phase {
            case .idle, .unavailable, .failed:
                HStack(spacing: 10) {
                    Button {
                        Task {
                            await speechInput.start(currentText: request) { transcript in
                                request = transcript
                            }
                        }
                    } label: {
                        Label("Start Dictation", systemImage: "mic.fill")
                    }
                    .buttonStyle(.bordered)
                    .disabled(ifDSPIsUsingAudioInput)
                    .accessibilityIdentifier("azimuth.assistant.dictation.start")

                    Text(
                        ifDSPIsUsingAudioInput
                            ? "End live IF capture before starting dictation."
                            : "On-device; the microphone is used only while listening."
                    )
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

            case .preparing(let message):
                HStack(spacing: 10) {
                    ProgressView()
                        .controlSize(.small)
                    Text(message)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                    dictationCancelButton
                }

            case .recording:
                HStack(spacing: 10) {
                    Label("Listening…", systemImage: "waveform")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.red)
                    Spacer()
                    Button {
                        Task { await speechInput.stop() }
                    } label: {
                        Label("Stop & Use Text", systemImage: "stop.fill")
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(.red)
                    .accessibilityIdentifier("azimuth.assistant.dictation.stop")
                    dictationCancelButton
                }

            case .finalizing:
                HStack(spacing: 10) {
                    ProgressView()
                        .controlSize(.small)
                    Text("Finishing transcription…")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                    dictationCancelButton
                }
            }

            switch speechInput.phase {
            case .unavailable(let message), .failed(let message):
                Label(message, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(AzimuthPalette.caution)
                    .accessibilityIdentifier("azimuth.assistant.dictation.message")
            case .idle, .preparing, .recording, .finalizing:
                EmptyView()
            }
        }
    }

    private var ifDSPIsUsingAudioInput: Bool {
        if model.isIFDSPOperationInFlight || model.ifDSPModeState.reservesRadioState {
            return true
        }
        switch model.ifDSPState {
        case .requestingPermission, .starting, .streaming:
            return true
        case .idle, .waitingForUSBAudio, .paused, .failed:
            return false
        }
    }

    private var dictationCancelButton: some View {
        Button("Cancel") {
            Task { await speechInput.cancel() }
        }
        .buttonStyle(.bordered)
        .accessibilityIdentifier("azimuth.assistant.dictation.cancel")
    }
#endif

    private var assistantEmptyState: some View {
        HStack(spacing: AzimuthLayout.cardSpacing) {
            Image(systemName: "arrow.triangle.branch")
                .font(.title3)
                .foregroundStyle(AzimuthPalette.bearing)
            VStack(alignment: .leading, spacing: 4) {
                Text("One request, one reviewed transaction")
                    .font(.headline)
                Text("Review live and target values before anything is sent to the radio.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 4)
    }

    private func proposingState(request: String) -> some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 14) {
                HStack(spacing: 12) {
                    ProgressView()
                    VStack(alignment: .leading, spacing: 3) {
                            Text("Generating a validated change list")
                            .font(.headline)
                        Text("Apple Intelligence is working on this device.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                Text("“\(request)”")
                    .font(.title3.weight(.medium))
                    .foregroundStyle(.secondary)
                ProgressView()
                    .progressViewStyle(.linear)
            }
        }
    }

    private func planReview(_ plan: AssistantPlan) -> some View {
        VStack(alignment: .leading, spacing: AzimuthLayout.pageSpacing) {
            InstrumentPanel {
                VStack(alignment: .leading, spacing: 14) {
                    HStack {
                        VStack(alignment: .leading, spacing: 4) {
                            AzimuthEyebrow("Pending review")
                            Text(plan.summary)
                                .font(.title3.bold())
                        }
                        Spacer()
                        AzimuthStatusPill(
                            title: model.assistantCanAccept ? "READY TO APPLY" : "ACTION REQUIRED",
                            symbol: model.assistantCanAccept
                                ? "checkmark.shield.fill" : "exclamationmark.triangle.fill",
                            color: model.assistantCanAccept
                                ? AzimuthPalette.signal : AzimuthPalette.caution
                        )
                    }

                    Text("Request: “\(plan.request)”")
                        .font(.callout)
                        .foregroundStyle(.secondary)

                    if plan.needsClarification {
                        Label(
                            "This proposal needs a clearer request or contains a rejected target. Decline it and revise the request before applying.",
                            systemImage: "questionmark.bubble.fill"
                        )
                        .foregroundStyle(AzimuthPalette.caution)
                    } else if !model.radioState.connection.isConnected {
                        Label(
                            "Connect the TH-D75 to enable Accept.",
                            systemImage: "cable.connector"
                        )
                        .foregroundStyle(AzimuthPalette.caution)
                    } else if !model.radioState.capabilities.settingWrite.isAvailable {
                        Label(
                            "The connected radio has not enabled verified setting writes.",
                            systemImage: "lock.fill"
                        )
                        .foregroundStyle(AzimuthPalette.caution)
                    }
                }
            }

            ForEach(Array(plan.changes.enumerated()), id: \.element.id) { index, change in
                proposalChangeCard(index: index + 1, change: change)
            }

            InstrumentPanel {
                HStack {
                    Button(role: .destructive) {
                        model.declineAssistantPlan()
                    } label: {
                        Label("Decline", systemImage: "xmark")
                    }

                    if !model.radioState.connection.isConnected {
                        Button {
                            model.route = .radio
                        } label: {
                            Label("Go to Radio", systemImage: "cable.connector")
                        }
                    }

                    Spacer()

                    Button {
                        Task { await model.acceptAssistantPlan() }
                    } label: {
                        Label("Accept & Apply Automatically", systemImage: "checkmark.shield.fill")
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(AzimuthPalette.signal)
                    .disabled(!model.assistantCanAccept)
                }
            }
        }
    }

    private func proposalChangeCard(
        index: Int,
        change: AssistantPlan.Change
    ) -> some View {
        InstrumentPanel {
            HStack(alignment: .top, spacing: 14) {
                Text("\(index)")
                    .font(.caption.bold().monospacedDigit())
                    .frame(width: 30, height: 30)
                    .background(.primary.opacity(0.07), in: Circle())

                VStack(alignment: .leading, spacing: 10) {
                    HStack(alignment: .firstTextBaseline) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(change.definition?.title ?? "Rejected setting")
                                .font(.headline)
                            Text(change.requestedSettingID)
                                .font(.caption2.monospaced())
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        changeValidationBadge(change.validation)
                    }

                    HStack(spacing: 10) {
                        assistantValue(
                            label: "BEFORE",
                            value: change.previousValue.map { previous in
                                change.definition.map { definition in
                                    assistantSettingValue(previous, definition: definition)
                                } ?? previous.displayText
                            } ?? "LIVE VALUE NOT READ"
                        )
                        Image(systemName: "arrow.right")
                            .foregroundStyle(AzimuthPalette.bearing)
                        assistantValue(
                            label: "TARGET",
                            value: change.proposedValue.flatMap { proposed in
                                change.definition.map { definition in
                                    assistantSettingValue(proposed, definition: definition)
                                }
                            } ?? change.proposedValueText
                        )
                    }

                    Text(change.rationale)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
            }
        }
    }

    private func assistantValue(label: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(label)
                .font(.caption2.bold().monospaced())
                .foregroundStyle(.secondary)
            Text(value)
                .font(.body.weight(.semibold).monospaced())
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(10)
        .background(.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 9))
    }

    @ViewBuilder
    private func changeValidationBadge(_ validation: AssistantPlan.Validation) -> some View {
        switch validation {
        case .validated:
            AzimuthStatusPill(title: "VALIDATED", symbol: "checkmark.circle.fill", color: AzimuthPalette.signal)
        case .noChange:
            AzimuthStatusPill(title: "NO CHANGE", symbol: "equal.circle.fill", color: .secondary)
        case .unknownSetting:
            AzimuthStatusPill(title: "UNKNOWN ID", symbol: "xmark.octagon.fill", color: .red)
        case .invalidValue:
            AzimuthStatusPill(title: "INVALID VALUE", symbol: "xmark.octagon.fill", color: .red)
        case .specializedEditorRequired:
            AzimuthStatusPill(title: "SPECIALIZED", symbol: "wrench.and.screwdriver.fill", color: AzimuthPalette.caution)
        case .duplicateSetting:
            AzimuthStatusPill(title: "DUPLICATE", symbol: "square.on.square", color: .red)
        case .liveValueUnavailable:
            AzimuthStatusPill(title: "VALUE NOT READ", symbol: "arrow.down.to.line.compact", color: AzimuthPalette.caution)
        }
    }

    private func applyingState(
        plan: AssistantPlan,
        progress: RadioSettingApplyProgress
    ) -> some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 16) {
                HStack(spacing: 12) {
                    ProgressView()
                    VStack(alignment: .leading, spacing: 3) {
                        AzimuthEyebrow("Applying accepted proposal")
                        Text("Writing \(min(progress.completedCount + 1, progress.totalCount)) of \(progress.totalCount)")
                            .font(.title3.bold())
                    }
                }
                ProgressView(value: progress.fractionCompleted)
                if let settingID = progress.currentSettingID,
                   let definition = model.catalog.definition(id: settingID) {
                    Text(definition.title)
                        .font(.callout.monospaced())
                        .foregroundStyle(.secondary)
                }
                Label(
                    "Keep the radio powered and USB connected until the transaction finishes.",
                    systemImage: "bolt.shield.fill"
                )
                .font(.callout)
                .foregroundStyle(AzimuthPalette.caution)
            }
        }
    }

    private func appliedState(
        plan: AssistantPlan,
        report: RadioSettingApplyReport
    ) -> some View {
        VStack(alignment: .leading, spacing: AzimuthLayout.pageSpacing) {
            InstrumentPanel {
                VStack(alignment: .leading, spacing: 12) {
                    Image(systemName: "checkmark.seal.fill")
                        .font(.system(size: 42))
                        .foregroundStyle(AzimuthPalette.signal)
                    AzimuthEyebrow("Applied")
                    Text("\(report.appliedCount) radio changes completed")
                        .font(.largeTitle.bold())
                    Text(plan.summary)
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }

            applyResults(report)

            HStack {
                Spacer()
                Button("New Request") {
                    request = ""
                    model.resetAssistantWorkflow()
                }
                .buttonStyle(.borderedProminent)
            }
        }
    }

    private func failedState(
        plan: AssistantPlan?,
        report: RadioSettingApplyReport?,
        message: String
    ) -> some View {
        VStack(alignment: .leading, spacing: AzimuthLayout.pageSpacing) {
            InstrumentPanel {
                VStack(alignment: .leading, spacing: 10) {
                    Label("Assistant operation stopped", systemImage: "exclamationmark.triangle.fill")
                        .font(.title2.bold())
                        .foregroundStyle(.red)
                    Text(message)
                    if plan != nil {
                        Text("No unreported retry occurs. Build a fresh proposal before another apply attempt.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }

            if let report { applyResults(report) }

            HStack {
                Button("Decline & Discard", role: .destructive) {
                    model.declineAssistantPlan()
                }
                Spacer()
                Button("Start Fresh") {
                    model.resetAssistantWorkflow()
                }
                .buttonStyle(.borderedProminent)
            }
        }
    }

    private func applyResults(_ report: RadioSettingApplyReport) -> some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 12) {
                AzimuthEyebrow("Controller report")
                ForEach(report.results) { result in
                    HStack {
                        Image(systemName: resultSymbol(result.outcome))
                            .foregroundStyle(resultColor(result.outcome))
                        Text(model.catalog.definition(id: result.settingID)?.title ?? result.settingID)
                        Spacer()
                        Text(resultLabel(result.outcome))
                            .font(.caption.bold().monospaced())
                            .foregroundStyle(resultColor(result.outcome))
                    }
                    if result.id != report.results.last?.id { Divider() }
                }
            }
        }
    }

    private func resultSymbol(_ outcome: RadioSettingApplyResult.Outcome) -> String {
        switch outcome {
        case .applied: return "checkmark.circle.fill"
        case .failed: return "xmark.circle.fill"
        case .rolledBack: return "arrow.uturn.backward.circle.fill"
        }
    }

    private func resultColor(_ outcome: RadioSettingApplyResult.Outcome) -> Color {
        switch outcome {
        case .applied: return AzimuthPalette.signal
        case .failed: return .red
        case .rolledBack: return AzimuthPalette.caution
        }
    }

    private func resultLabel(_ outcome: RadioSettingApplyResult.Outcome) -> String {
        switch outcome {
        case .applied: return "APPLIED"
        case .failed(let reason): return "FAILED · \(reason)"
        case .rolledBack(let reason): return "ROLLED BACK · \(reason)"
        }
    }

    private func submitRequest() {
        let trimmed = request.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !assistantSpeechIsActive else { return }
        Task { await model.proposeAssistantPlan(request: trimmed) }
    }
}

private func assistantSettingValue(
    _ value: ProposedSettingValue,
    definition: RadioSettingDefinition
) -> String {
    definition.domain.displayText(for: value) ?? value.displayText
}
