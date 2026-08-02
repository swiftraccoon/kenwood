// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import FoundationModels

enum AssistantAvailability: Equatable, Sendable {
    case available
    case unavailable(reason: String)

    var isAvailable: Bool {
        if case .available = self { return true }
        return false
    }
}

struct AssistantPlanDraft: Equatable, Sendable {
    let summary: String
    let needsClarification: Bool
    let changes: [Change]

    struct Change: Equatable, Sendable {
        let settingID: String
        let proposedValue: String
        let rationale: String
    }
}

struct AssistantPlan: Equatable, Sendable {
    let request: String
    let summary: String
    let needsClarification: Bool
    let changes: [Change]

    var isFullyValidated: Bool {
        !needsClarification
            && changes.contains { $0.validation == .validated }
            && changes.allSatisfy {
                $0.validation == .validated || $0.validation == .noChange
            }
    }

    struct Change: Identifiable, Equatable, Sendable {
        let id: UUID
        let requestedSettingID: String
        let definition: RadioSettingDefinition?
        let previousValue: ProposedSettingValue?
        let proposedValueText: String
        let proposedValue: ProposedSettingValue?
        let rationale: String
        let validation: Validation
    }

    enum Validation: Equatable, Sendable {
        case validated
        case unknownSetting
        case invalidValue(reason: String)
        case specializedEditorRequired
        case duplicateSetting
        case liveValueUnavailable
        case noChange
    }
}

enum AssistantPlanValidator {
    /// Converts generated strings into domain values and rejects anything not
    /// represented by the authoritative catalog. This function has no radio
    /// reference and cannot execute a change.
    static func validate(
        request: String,
        draft: AssistantPlanDraft,
        catalog: RadioSettingCatalog,
        currentValues: [String: ProposedSettingValue] = [:]
    ) -> AssistantPlan {
        let duplicateIDs = Set(
            Dictionary(grouping: draft.changes, by: \.settingID)
                .filter { $0.value.count > 1 }
                .keys
        )
        let changes = draft.changes.map { proposed in
            guard let definition = catalog.definition(id: proposed.settingID) else {
                return AssistantPlan.Change(
                    id: UUID(),
                    requestedSettingID: proposed.settingID,
                    definition: nil,
                    previousValue: nil,
                    proposedValueText: proposed.proposedValue,
                    proposedValue: nil,
                    rationale: proposed.rationale,
                    validation: .unknownSetting
                )
            }

            if duplicateIDs.contains(proposed.settingID) {
                return AssistantPlan.Change(
                    id: UUID(),
                    requestedSettingID: proposed.settingID,
                    definition: definition,
                    previousValue: currentValues[definition.id],
                    proposedValueText: proposed.proposedValue,
                    proposedValue: nil,
                    rationale: proposed.rationale,
                    validation: .duplicateSetting
                )
            }

            if definition.isSpecializedEditor {
                return AssistantPlan.Change(
                    id: UUID(),
                    requestedSettingID: proposed.settingID,
                    definition: definition,
                    previousValue: currentValues[definition.id],
                    proposedValueText: proposed.proposedValue,
                    proposedValue: nil,
                    rationale: proposed.rationale,
                    validation: .specializedEditorRequired
                )
            }

            guard let value = parse(proposed.proposedValue, domain: definition.domain) else {
                return AssistantPlan.Change(
                    id: UUID(),
                    requestedSettingID: proposed.settingID,
                    definition: definition,
                    previousValue: currentValues[definition.id],
                    proposedValueText: proposed.proposedValue,
                    proposedValue: nil,
                    rationale: proposed.rationale,
                    validation: .invalidValue(reason: "Value is outside \(definition.domain.summary).")
                )
            }

            let validation: AssistantPlan.Validation
            if !definition.domain.accepts(value) {
                validation = .invalidValue(reason: "Value is outside \(definition.domain.summary).")
            } else if let current = currentValues[definition.id] {
                validation = current == value ? .noChange : .validated
            } else {
                validation = .liveValueUnavailable
            }

            return AssistantPlan.Change(
                id: UUID(),
                requestedSettingID: proposed.settingID,
                definition: definition,
                previousValue: currentValues[definition.id],
                proposedValueText: proposed.proposedValue,
                proposedValue: value,
                rationale: proposed.rationale,
                validation: validation
            )
        }

        return AssistantPlan(
            request: request,
            summary: draft.summary,
            needsClarification: draft.needsClarification
                || changes.contains {
                    $0.validation != .validated && $0.validation != .noChange
                },
            changes: changes
        )
    }

    private static func parse(
        _ rawValue: String,
        domain: RadioSettingDomain
    ) -> ProposedSettingValue? {
        domain.parseDisplayValue(rawValue)
    }
}

@MainActor
protocol AssistantPlanning: AnyObject {
    var availability: AssistantAvailability { get }

    func propose(
        request: String,
        catalog: RadioSettingCatalog,
        currentValues: [String: ProposedSettingValue]
    ) async throws -> AssistantPlan
}

/// Apple's on-device language model only generates a typed proposal.
/// Validation is deterministic, and this object has no transport or
/// controller property. Execution is a separate user-accepted scene action.
@MainActor
final class OnDeviceAssistantPlanner: AssistantPlanning {
    private let model = SystemLanguageModel.default

    var availability: AssistantAvailability {
        switch model.availability {
        case .available:
            return .available
        case .unavailable(.deviceNotEligible):
            return .unavailable(reason: "Apple Intelligence is not supported on this device.")
        case .unavailable(.appleIntelligenceNotEnabled):
            return .unavailable(reason: "Turn on Apple Intelligence to build plans on this device.")
        case .unavailable(.modelNotReady):
            return .unavailable(reason: "The on-device model is still downloading or preparing.")
        @unknown default:
            return .unavailable(reason: "The on-device model is unavailable for an unknown reason.")
        }
    }

    func propose(
        request: String,
        catalog: RadioSettingCatalog,
        currentValues: [String: ProposedSettingValue]
    ) async throws -> AssistantPlan {
        guard availability.isAvailable else { throw AssistantPlannerError.modelUnavailable }

        let candidates = catalog.assistantCandidates(for: request)
        let schema = candidates.map { definition in
            let acceptedValues: String
            switch definition.domain {
            case .choice(let options):
                acceptedValues = options.map(\.label).joined(separator: ", ")
            case .scaledInteger:
                acceptedValues = "display-unit decimal only: \(definition.domain.summary); never use the raw stored integer"
            default:
                acceptedValues = definition.domain.summary
            }
            let current = currentValues[definition.id]
                .map { Self.display($0, for: definition) }
                ?? "not read"
            return "\(definition.id) | \(definition.title) | current: \(current) | allowed: \(acceptedValues)"
        }
        .joined(separator: "\n")

        let session = LanguageModelSession(instructions: """
            You are Azimuth's Kenwood TH-D75 planning assistant. You have no tools and
            no radio access. Produce a PROPOSAL FOR EXPLICIT USER REVIEW and never claim
            that a setting was read, changed, saved, transmitted, or applied. Use only exact setting IDs
            and accepted values supplied in the prompt. Do not give the operator manual
            menu instructions: every requested action must be represented by an executable
            setting change that Azimuth can apply automatically after Accept. If the request
            cannot be fulfilled from those candidates, propose no speculative change and
            mark the plan as needing clarification.
            """)

        let response = try await session.respond(
            to: """
                Operator request: \(request)

                Candidate settings (exact ID | title | live current value | accepted domain):
                \(schema)

                Return a concise preview with at most five independently reviewable changes.
                """,
            generating: GeneratedAssistantPlan.self
        )
        let draft = AssistantPlanDraft(
            summary: response.content.summary,
            needsClarification: response.content.needsClarification,
            changes: response.content.changes.map {
                AssistantPlanDraft.Change(
                    settingID: $0.settingID,
                    proposedValue: $0.proposedValue,
                    rationale: $0.rationale
                )
            }
        )
        return AssistantPlanValidator.validate(
            request: request,
            draft: draft,
            catalog: catalog,
            currentValues: currentValues
        )
    }

    private static func display(
        _ value: ProposedSettingValue,
        for definition: RadioSettingDefinition
    ) -> String {
        definition.domain.displayText(for: value) ?? value.displayText
    }
}

enum AssistantPlannerError: LocalizedError {
    case modelUnavailable

    var errorDescription: String? {
        switch self {
        case .modelUnavailable: return "The on-device language model is not available."
        }
    }
}

@Generable(description: "A pending-review radio configuration proposal that has not been applied")
private struct GeneratedAssistantPlan {
    @Guide(description: "A concise summary that explicitly describes this as a preview")
    var summary: String

    @Guide(description: "True when the operator must clarify the request")
    var needsClarification: Bool

    @Guide(description: "Zero to five proposed setting changes", .maximumCount(5))
    var changes: [GeneratedAssistantChange]
}

@Generable(description: "One unexecuted setting change in a preview plan")
private struct GeneratedAssistantChange {
    @Guide(description: "An exact setting ID copied from the candidate list")
    var settingID: String

    @Guide(description: "An exact accepted value for that setting")
    var proposedValue: String

    @Guide(description: "Why the proposed change helps with the operator's request")
    var rationale: String
}
