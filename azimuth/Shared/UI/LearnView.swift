// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

struct LearnView: View {
    @Environment(AzimuthSceneModel.self) private var model
    @State private var searchText = ""

    private var filteredChapters: [LearningChapter] {
        AzimuthLearningLibrary.chapters.filter { $0.matches(searchText) }
    }

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: AzimuthLayout.pageSpacing) {
                learnHeader
                capabilityCenter

                ForEach(LearningCollection.allCases) { collection in
                    let chapters = filteredChapters.filter { $0.collection == collection }
                    if !chapters.isEmpty {
                        chapterCollection(collection, chapters: chapters)
                    }
                }

                if filteredChapters.isEmpty {
                    ContentUnavailableView.search(text: searchText)
                        .frame(maxWidth: .infinity, minHeight: 260)
                }
            }
            .azimuthContentColumn(maxWidth: AzimuthLayout.browseWidth)
        }
        .azimuthPage()
        .accessibilityIdentifier("azimuth.page.learn")
        .searchable(text: $searchText, prompt: "Search D75 capabilities")
        .navigationDestination(for: LearningChapter.self) { chapter in
            LearningChapterView(chapter: chapter)
        }
        .radioSettingNavigationDestination()
    }

    private var learnHeader: some View {
        VStack(alignment: .leading, spacing: 6) {
            AzimuthEyebrow("Capability center")
            Text("Practical guides for USB-C, digital voice, APRS, remote operation, and verified configuration.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .frame(maxWidth: 720, alignment: .leading)
        }
        .padding(.horizontal, 4)
    }

    private var capabilityCenter: some View {
        VStack(alignment: .leading, spacing: 11) {
            HStack {
                AzimuthEyebrow("Your current control surface")
                Spacer()
                Text(model.radioState.connection.isConnected ? "RADIO LIVE" : "RADIO OFFLINE")
                    .font(.caption2.bold().monospaced())
                    .foregroundStyle(
                        model.radioState.connection.isConnected ? AzimuthPalette.signal : .secondary
                    )
            }

            LazyVGrid(
                columns: [GridItem(.adaptive(minimum: 205), spacing: AzimuthLayout.cardSpacing)],
                spacing: AzimuthLayout.cardSpacing
            ) {
                learningCapability(
                    title: "Color remote screen",
                    detail: "240 × 180 RGBA frames",
                    symbol: "rectangle.on.rectangle",
                    state: model.radioState.capabilities.screenStreaming
                )
                learningCapability(
                    title: "Complete front panel",
                    detail: "All 25 automated keys",
                    symbol: "circle.grid.3x3.fill",
                    state: model.radioState.capabilities.frontPanelControl
                )
                learningCapability(
                    title: "Settings model",
                    detail: "\(model.catalog.definitions.count) loaded definitions",
                    symbol: "slider.horizontal.3",
                    state: model.radioState.capabilities.settingRead
                )
                learningCapability(
                    title: "Accepted proposals",
                    detail: "Validated batch apply",
                    symbol: "checkmark.shield.fill",
                    state: model.radioState.capabilities.settingWrite
                )
            }
        }
    }

    private func learningCapability(
        title: String,
        detail: String,
        symbol: String,
        state: RadioCapabilityState
    ) -> some View {
        InstrumentPanel(padding: 14) {
            VStack(alignment: .leading, spacing: 9) {
                HStack {
                    Image(systemName: symbol)
                        .font(.title3)
                        .foregroundStyle(AzimuthPalette.bearing)
                    Spacer()
                    Circle()
                        .fill(state.isAvailable ? AzimuthPalette.signal : .secondary.opacity(0.35))
                        .frame(width: 7, height: 7)
                }
                Text(title).font(.headline)
                Text(detail).font(.caption).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, minHeight: 98, alignment: .topLeading)
        }
    }

    private func chapterCollection(
        _ collection: LearningCollection,
        chapters: [LearningChapter]
    ) -> some View {
        VStack(alignment: .leading, spacing: 11) {
            AzimuthEyebrow(collection.title)
            LazyVGrid(
                columns: [GridItem(.adaptive(minimum: 280), spacing: AzimuthLayout.cardSpacing)],
                spacing: AzimuthLayout.cardSpacing
            ) {
                ForEach(chapters) { chapter in
                    NavigationLink(value: chapter) {
                        LearningChapterCard(chapter: chapter)
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }
}

private struct LearningChapterCard: View {
    let chapter: LearningChapter

    var body: some View {
        InstrumentPanel(padding: 15) {
            VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .top) {
                    Image(systemName: chapter.symbol)
                        .font(.title2)
                        .foregroundStyle(AzimuthPalette.bearing)
                    Spacer()
                    Image(systemName: "arrow.up.right")
                        .font(.caption.bold())
                        .foregroundStyle(.tertiary)
                }
                Text(chapter.eyebrow.uppercased())
                    .font(.caption2.bold().monospaced())
                    .foregroundStyle(AzimuthPalette.signal)
                Text(chapter.title)
                    .font(.title3.bold())
                Text(chapter.summary)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(3)
                HStack(spacing: 5) {
                    ForEach(chapter.relatedGroups.prefix(4)) { group in
                        Image(systemName: group.symbol)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .frame(maxWidth: .infinity, minHeight: 176, alignment: .topLeading)
        }
    }
}

private struct LearningChapterView: View {
    @Environment(AzimuthSceneModel.self) private var model
    let chapter: LearningChapter

    private var relatedDefinitions: [RadioSettingDefinition] {
        model.catalog.definitions
            .filter { chapter.relatedGroups.contains($0.group) }
            .prefix(8)
            .map { $0 }
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                chapterHeader

                ForEach(Array(chapter.sections.enumerated()), id: \.offset) { index, section in
                    InstrumentPanel {
                        VStack(alignment: .leading, spacing: 12) {
                            HStack(alignment: .firstTextBaseline) {
                                Text(String(format: "%02d", index + 1))
                                    .font(.caption.bold().monospacedDigit())
                                    .foregroundStyle(AzimuthPalette.bearing)
                                Text(section.heading)
                                    .font(.title2.bold())
                            }
                            Text(section.body)
                                .font(.body)
                                .foregroundStyle(.secondary)
                                .lineSpacing(4)
                            if !section.points.isEmpty {
                                Divider()
                                ForEach(section.points, id: \.self) { point in
                                    HStack(alignment: .top, spacing: 10) {
                                        Image(systemName: "arrow.right.circle.fill")
                                            .foregroundStyle(AzimuthPalette.signal)
                                            .padding(.top, 2)
                                        Text(point)
                                    }
                                }
                            }
                        }
                    }
                }

                if !relatedDefinitions.isEmpty { relatedSettings }
            }
            .frame(maxWidth: 820, alignment: .leading)
            .padding()
        }
        .azimuthPage()
        .navigationTitle(chapter.title)
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
    }

    private var chapterHeader: some View {
        InstrumentPanel {
            HStack(alignment: .top, spacing: 16) {
                Image(systemName: chapter.symbol)
                    .font(.system(size: 34))
                    .foregroundStyle(AzimuthPalette.bearing)
                    .frame(width: 52)
                VStack(alignment: .leading, spacing: 7) {
                    AzimuthEyebrow(chapter.eyebrow)
                    Text(chapter.title)
                        .font(.largeTitle.bold())
                    Text(chapter.summary)
                        .font(.title3)
                        .foregroundStyle(.secondary)
                }
            }
        }
    }

    private var relatedSettings: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 12) {
                AzimuthEyebrow("Related settings in the loaded catalog")
                ForEach(relatedDefinitions) { definition in
                    NavigationLink(value: RadioSettingDestination(id: definition.id)) {
                        HStack {
                            Image(systemName: definition.group.symbol)
                                .foregroundStyle(AzimuthPalette.bearing)
                                .frame(width: 24)
                            VStack(alignment: .leading, spacing: 2) {
                                HStack(spacing: 7) {
                                    Text(definition.title).font(.subheadline.weight(.semibold))
                                    if let menuNumberLabel = definition.menuNumberLabel {
                                        THD75MenuBadge(menuNumberLabel)
                                    }
                                }
                                Text(definition.domain.summary)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                            Text(model.radioState.settingValues[definition.id] == nil ? "NOT READ" : "LIVE")
                                .font(.caption2.bold().monospaced())
                                .foregroundStyle(
                                    model.radioState.settingValues[definition.id] == nil
                                        ? .secondary : AzimuthPalette.signal
                                )
                            Image(systemName: "chevron.right")
                                .font(.caption.bold())
                                .foregroundStyle(.tertiary)
                        }
                        .padding(.vertical, 4)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }
}
