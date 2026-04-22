// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

/// Full-history sheet for the reflector's "Recently heard" log.
///
/// Keeps the main dashboard bounded — Apple's own apps use the same
/// "summary inline, full list behind a tap" pattern (Photos' Recently
/// Viewed, Music's Recently Played, Health's daily summaries).
struct HeardHistorySheet: View {
    let coordinator: ReflectorCoordinator
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Group {
                if coordinator.recentlyHeard.isEmpty {
                    ContentUnavailableView(
                        "Nothing heard yet",
                        systemImage: "waveform.slash",
                        description: Text("Stations who transmit through this reflector will appear here.")
                    )
                } else {
                    List {
                        ForEach(coordinator.recentlyHeard) { entry in
                            HeardDetailRow(entry: entry)
                        }
                    }
                    #if os(macOS)
                    .listStyle(.inset)
                    #else
                    .listStyle(.insetGrouped)
                    #endif
                }
            }
            .navigationTitle("Recently heard")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
                if !coordinator.recentlyHeard.isEmpty {
                    ToolbarItem {
                        Button(role: .destructive) {
                            coordinator.clearHeardHistory()
                        } label: {
                            Label("Clear", systemImage: "trash")
                        }
                    }
                }
            }
        }
        #if os(macOS)
        .frame(minWidth: 460, idealWidth: 520, minHeight: 480, idealHeight: 600)
        #endif
    }
}

/// Expanded row used only in the full history sheet. Shows a few more
/// details than the inline preview (end reason, frame count).
struct HeardDetailRow: View {
    let entry: ReflectorCoordinator.HeardEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
                Image(systemName: "waveform.path")
                    .foregroundStyle(.secondary)
                    .font(.caption)
                Text("\(entry.mycall)/\(entry.suffix)")
                    .font(.body.monospaced())
                Text("→ \(entry.urcall)")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                Spacer()
                Text(entry.endedAt, style: .time)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            if let text = entry.text, !text.isEmpty {
                Label(text, systemImage: "text.bubble")
                    .font(.callout)
                    .foregroundStyle(.primary.opacity(0.9))
                    .padding(.leading, 20)
                    .accessibilityLabel("Message: \(text)")
            }
            if let pos = entry.position {
                VStack(alignment: .leading, spacing: 2) {
                    Label(GpsFormat.coordinate(pos), systemImage: "location.fill")
                        .font(.callout.monospacedDigit())
                        .foregroundStyle(.primary.opacity(0.9))
                        .textSelection(.enabled)
                    if let comment = pos.comment, !comment.isEmpty {
                        Text(comment)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .padding(.leading, 22)
                    }
                }
                .padding(.leading, 20)
                .accessibilityLabel("Position: \(GpsFormat.summary(pos))")
            }
            HStack(spacing: 10) {
                Label(durationString(entry.duration), systemImage: "clock")
                    .font(.caption2.monospacedDigit())
                Label("\(entry.frames) frames", systemImage: "waveform")
                    .font(.caption2.monospacedDigit())
                Text("·").font(.caption2).foregroundStyle(.tertiary)
                Text(entry.endReason)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            .foregroundStyle(.secondary)
            .padding(.leading, 20)
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(accessibilityLabel)
    }

    private var accessibilityLabel: String {
        var parts: [String] = ["\(entry.mycall) \(entry.suffix)", "to \(entry.urcall)"]
        if let text = entry.text, !text.isEmpty { parts.append("message: \(text)") }
        if let pos = entry.position { parts.append("position \(GpsFormat.coordinate(pos))") }
        parts.append("\(Int(entry.duration)) seconds")
        return parts.joined(separator: ", ")
    }

    private func durationString(_ seconds: TimeInterval) -> String {
        let s = Int(seconds.rounded())
        return String(format: "%d:%02d", s / 60, s % 60)
    }
}
