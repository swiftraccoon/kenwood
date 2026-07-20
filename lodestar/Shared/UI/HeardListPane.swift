// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

/// Heard-history list. On the wide canvas (`limit == nil`) it owns the
/// rail's remaining height and the "Show all" sheet retires; on narrow
/// (`limit == n`) it renders the classic inline preview. Tapping a row
/// selects the station (syncing the map pin highlight) and opens the
/// station popover — the touch-first path to QRZ/copy/maps actions.
struct HeardListPane: View {
    let reflector: ReflectorCoordinator
    let limit: Int?
    @Binding var selectedStationID: String?
    let onShowAll: (() -> Void)?

    @State private var poppedStation: StationRef?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            header
            if reflector.recentlyHeard.isEmpty {
                Text("Stations who transmit through this reflector will appear here.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else if let limit {
                ForEach(reflector.recentlyHeard.prefix(limit)) { entry in
                    rowButton(entry)
                }
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 4) {
                        ForEach(reflector.recentlyHeard) { entry in
                            rowButton(entry)
                        }
                    }
                }
            }
        }
        .popover(item: $poppedStation) { station in
            StationPopover(station: station)
        }
    }

    private var header: some View {
        HStack {
            Text("Recently heard").font(.headline)
            Spacer()
            if let limit, let onShowAll, reflector.recentlyHeard.count > limit {
                Button(action: onShowAll) {
                    HStack(spacing: 2) {
                        Text("Show all \(reflector.recentlyHeard.count)")
                        Image(systemName: "chevron.forward").font(.caption2)
                    }
                }
                .buttonStyle(.borderless)
                .font(.caption)
            }
        }
    }

    private func rowButton(_ entry: ReflectorCoordinator.HeardEntry) -> some View {
        let station = StationRef(entry: entry)
        return Button {
            selectedStationID = station.id
            poppedStation = station
        } label: {
            HeardRow(entry: entry)
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .background(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(selectedStationID == station.id
                      ? Color.accentColor.opacity(0.15)
                      : Color.clear)
        )
        .contextMenu {
            StationActionMenuItems(station: station)
        }
        .accessibilityHint("Shows station details and actions")
    }
}

/// One heard-history row. Rendering only — tap handling, selection,
/// and menus live in `HeardListPane.rowButton`.
struct HeardRow: View {
    let entry: ReflectorCoordinator.HeardEntry

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "waveform.path")
                .foregroundStyle(.secondary)
                .font(.caption)
                .padding(.top, 2)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text("\(entry.mycall)/\(entry.suffix)")
                        .font(.body.monospaced())
                    Text("→ \(entry.urcall)")
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                    if entry.position != nil {
                        Image(systemName: "location.fill")
                            .foregroundStyle(.blue)
                            .font(.caption2)
                            .accessibilityLabel("Position reported")
                    }
                }
                if let text = entry.text, !text.isEmpty {
                    Text(text)
                        .font(.caption)
                        .foregroundStyle(.primary.opacity(0.85))
                        .lineLimit(2)
                        .accessibilityLabel("Message: \(text)")
                }
                if let pos = entry.position {
                    Text(GpsFormat.coordinate(pos))
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                        .accessibilityLabel("Position: \(GpsFormat.coordinate(pos))")
                }
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 1) {
                Text(durationString(entry.duration))
                    .font(.caption.monospaced())
                Text(entry.endedAt, style: .time)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(a11yLabel)
    }

    private var a11yLabel: String {
        var parts: [String] = ["\(entry.mycall) \(entry.suffix)", "to \(entry.urcall)"]
        if let text = entry.text, !text.isEmpty { parts.append("message: \(text)") }
        if let pos = entry.position { parts.append("position \(GpsFormat.coordinate(pos))") }
        parts.append(durationString(entry.duration))
        return parts.joined(separator: ", ")
    }

    private func durationString(_ seconds: TimeInterval) -> String {
        let s = Int(seconds.rounded())
        return String(format: "%d:%02d", s / 60, s % 60)
    }
}
