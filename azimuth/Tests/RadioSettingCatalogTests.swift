// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import CoreGraphics
import Foundation
import XCTest
@testable import Azimuth

final class RadioSettingCatalogTests: XCTestCase {
    private let catalog = RadioSettingCatalog.designPreview

    func testSearchMatchesAcrossTitleSummaryAndChoiceLabels() throws {
        XCTAssertEqual(
            catalog.filtered(query: "brightness", group: nil).map(\.title),
            ["Display Brightness"]
        )
        XCTAssertTrue(
            catalog.filtered(query: "SmartBeaconing", group: nil)
                .contains { $0.title == "Beacon Method" }
        )
        XCTAssertTrue(
            catalog.filtered(query: "callsign", group: .digitalVoice)
                .allSatisfy { $0.group == .digitalVoice }
        )
    }

    func testAssistantAliasRetrievalFindsQuietControls() {
        let titles = Set(catalog.assistantCandidates(for: "Make it quiet").map(\.title))
        XCTAssertTrue(titles.contains("Key Beep"))
        XCTAssertTrue(titles.contains("Beep Volume"))
    }

    func testValidatorRequiresLiveBeforeValue() throws {
        let definition = try XCTUnwrap(catalog.definitions.first { $0.title == "Key Beep" })
        let draft = AssistantPlanDraft(
            summary: "Turn off confirmation beeps.",
            needsClarification: false,
            changes: [.init(settingID: definition.id, proposedValue: "Off", rationale: "Quiet operation")]
        )
        let plan = AssistantPlanValidator.validate(
            request: "Make it quiet",
            draft: draft,
            catalog: catalog
        )
        XCTAssertEqual(plan.changes.first?.validation, .liveValueUnavailable)
        XCTAssertTrue(plan.needsClarification)
        XCTAssertFalse(plan.isFullyValidated)
    }

    func testValidatorBuildsActualBeforeAfterDiff() throws {
        let definition = try XCTUnwrap(catalog.definitions.first { $0.title == "Key Beep" })
        let draft = AssistantPlanDraft(
            summary: "Turn off confirmation beeps.",
            needsClarification: false,
            changes: [.init(settingID: definition.id, proposedValue: "Off", rationale: "Quiet operation")]
        )
        let plan = AssistantPlanValidator.validate(
            request: "Make it quiet",
            draft: draft,
            catalog: catalog,
            currentValues: [definition.id: .boolean(true)]
        )
        XCTAssertEqual(plan.changes.first?.previousValue, .boolean(true))
        XCTAssertEqual(plan.changes.first?.proposedValue, .boolean(false))
        XCTAssertEqual(plan.changes.first?.validation, .validated)
        XCTAssertTrue(plan.isFullyValidated)
    }

    func testValidatorRejectsDuplicateSettingIDs() throws {
        let definition = try XCTUnwrap(catalog.definitions.first { $0.title == "Key Beep" })
        let draft = AssistantPlanDraft(
            summary: "Conflicting duplicate",
            needsClarification: false,
            changes: [
                .init(settingID: definition.id, proposedValue: "On", rationale: "First"),
                .init(settingID: definition.id, proposedValue: "Off", rationale: "Second"),
            ]
        )
        let plan = AssistantPlanValidator.validate(
            request: "conflict",
            draft: draft,
            catalog: catalog,
            currentValues: [definition.id: .boolean(true)]
        )
        XCTAssertTrue(plan.changes.allSatisfy { $0.validation == .duplicateSetting })
        XCTAssertFalse(plan.isFullyValidated)
    }

    func testValidatorMarksNoOpWithoutMakingItExecutable() throws {
        let definition = try XCTUnwrap(catalog.definitions.first { $0.title == "Key Beep" })
        let draft = AssistantPlanDraft(
            summary: "Already quiet",
            needsClarification: false,
            changes: [.init(settingID: definition.id, proposedValue: "Off", rationale: "Quiet")]
        )
        let plan = AssistantPlanValidator.validate(
            request: "quiet",
            draft: draft,
            catalog: catalog,
            currentValues: [definition.id: .boolean(false)]
        )
        XCTAssertEqual(plan.changes.first?.validation, .noChange)
        XCTAssertFalse(plan.isFullyValidated)
    }

    func testRGBAFramePreservesChannelOrder() throws {
        let bytes = Data([0x11, 0x44, 0xAA, 0xFF])
        let frame = RadioScreenFrame(
            width: 1,
            height: 1,
            rgba8888: bytes,
            capturedAt: Date(timeIntervalSince1970: 0)
        )
        let image = try XCTUnwrap(frame.cgImage)
        XCTAssertEqual(image.alphaInfo, .last)
        XCTAssertTrue(image.bitmapInfo.contains(.byteOrder32Big))
        XCTAssertEqual(image.dataProvider?.data as Data?, bytes)
    }

    func testRemotePanelExposesAllCoreKeys() {
        XCTAssertEqual(RadioFrontPanelKey.allCases.count, 25)
        XCTAssertEqual(
            Set(RadioFrontPanelKey.allCases),
            Set([
                .mode, .menu, .ab, .function, .monitor,
                .up, .down, .left, .right, .enter,
                .mark0, .vfo1, .mr2, .call3, .msg4, .list5,
                .beacon6, .reverse7, .tone8, .pf1_9, .mhzStar, .pf2Hash,
                .micPf1, .micPf2, .micPf3,
            ])
        )
    }

    func testFixedStringAllowsEmptyAndMeasuresUTF8Bytes() {
        let domain = RadioSettingDomain.text(maxLength: 4, encoding: .utf8)
        XCTAssertTrue(domain.accepts(.text("")))
        XCTAssertTrue(domain.accepts(.text("test")))
        XCTAssertTrue(domain.accepts(.text("éé")))
        XCTAssertFalse(domain.accepts(.text("ééé")))
    }

    func testTextProposalPreservesExactWhitespaceAndEmptyValue() {
        let domain = RadioSettingDomain.text(maxLength: 16, encoding: .ascii)

        XCTAssertEqual(domain.parseDisplayValue("  CQ  "), .text("  CQ  "))
        XCTAssertEqual(domain.parseDisplayValue(""), .text(""))
        XCTAssertTrue(domain.accepts(.text("  CQ  ")))
    }

    func testASCIITextDomainRejectsNonASCIIBeforeReview() {
        let domain = RadioSettingDomain.text(maxLength: 16, encoding: .ascii)

        XCTAssertTrue(domain.accepts(.text("CQ K1ABC")))
        XCTAssertFalse(domain.accepts(.text("café")))
    }
}
