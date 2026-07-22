// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// One station as the UI acts on it, regardless of where it appeared
/// (heard row, live stream, map pin). Single source of truth for which
/// actions the popover and context menus offer, so the surfaces can
/// never drift apart.
struct StationRef: Equatable, Identifiable {
    let mycall: String
    let suffix: String
    let urcall: String?
    let text: String?
    let position: GpsPosition?
    let heardAt: Date?
    let duration: TimeInterval?
    let isLive: Bool

    /// Stable enough for popover/selection identity: a live station
    /// and a heard row for the same call are distinct presentations.
    var id: String {
        let epoch = heardAt.map { "\($0.timeIntervalSince1970)" } ?? "-"
        return "\(mycall)/\(suffix)#\(isLive ? "live" : epoch)"
    }

    /// `MYCALL/SUFFIX`, or bare `MYCALL` when the suffix is empty.
    var displayCallsign: String {
        suffix.isEmpty ? mycall : "\(mycall)/\(suffix)"
    }

    init(entry: ReflectorCoordinator.HeardEntry) {
        self.mycall = entry.mycall
        self.suffix = entry.suffix
        self.urcall = entry.urcall
        self.text = entry.text
        self.position = entry.position
        self.heardAt = entry.endedAt
        self.duration = entry.duration
        self.isLive = false
    }

    init(stream: ReflectorCoordinator.StreamSnapshot) {
        self.mycall = stream.mycall
        self.suffix = stream.suffix
        self.urcall = stream.urcall
        self.text = stream.latestText
        self.position = stream.latestPosition
        self.heardAt = nil
        self.duration = nil
        self.isLive = true
    }

    /// Actions with no backing data are omitted entirely (hidden,
    /// never disabled). Order is fixed display order.
    var availableActions: [StationAction] {
        var actions: [StationAction] = [.lookUpQrz, .copyCallsign]
        if let text, !text.isEmpty {
            actions.append(.copyMessage)
        }
        if position != nil {
            actions.append(.copyCoordinates)
            actions.append(.openInMaps)
        }
        return actions
    }
}

/// The station action vocabulary. Adding a case here automatically
/// surfaces it in the popover and every context menu.
enum StationAction: Identifiable, Equatable {
    case lookUpQrz
    case copyCallsign
    case copyMessage
    case copyCoordinates
    case openInMaps

    var id: Self { self }

    var title: String {
        switch self {
        case .lookUpQrz:       return "Look Up on QRZ.com"
        case .copyCallsign:    return "Copy Callsign"
        case .copyMessage:     return "Copy TX Message"
        case .copyCoordinates: return "Copy Coordinates"
        case .openInMaps:      return "Open in Maps"
        }
    }

    var systemImage: String {
        switch self {
        case .lookUpQrz:       return "person.text.rectangle"
        case .copyCallsign:    return "doc.on.doc"
        case .copyMessage:     return "text.bubble"
        case .copyCoordinates: return "location"
        case .openInMaps:      return "map"
        }
    }
}

/// Executes a station action. Pasteboard + URL side effects live here
/// so the popover and menus stay declarative.
@MainActor
enum StationActionRunner {
    static func run(_ action: StationAction, on station: StationRef) {
        switch action {
        case .lookUpQrz:
            if let url = URL(string: "https://www.qrz.com/db/\(station.mycall)") {
                open(url)
            }
        case .copyCallsign:
            copy(station.mycall)
        case .copyMessage:
            if let text = station.text { copy(text) }
        case .copyCoordinates:
            if let pos = station.position { copy(GpsFormat.coordinate(pos)) }
        case .openInMaps:
            if let pos = station.position,
               let url = URL(string: "https://maps.apple.com/?ll=\(pos.latitude),\(pos.longitude)&q=\(station.mycall)") {
                open(url)
            }
        }
    }

    private static func copy(_ s: String) {
        #if os(macOS)
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(s, forType: .string)
        #else
        UIPasteboard.general.string = s
        #endif
    }

    private static func open(_ url: URL) {
        #if os(macOS)
        NSWorkspace.shared.open(url)
        #else
        UIApplication.shared.open(url)
        #endif
    }
}

/// Context-menu items for a station, shared by heard rows, the NOW TX
/// card, and map pins. The popover is the discoverable path; this is
/// the right-click / long-press fast path over the same action set.
struct StationActionMenuItems: View {
    let station: StationRef

    var body: some View {
        ForEach(station.availableActions) { action in
            Button {
                StationActionRunner.run(action, on: station)
            } label: {
                Label(action.title, systemImage: action.systemImage)
            }
        }
    }
}

/// Tap-anchored station card: identity + details + visible action
/// buttons (the Maps place-card pattern).
struct StationPopover: View {
    let station: StationRef

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Text(station.displayCallsign)
                    .font(.title3.bold().monospaced())
                if station.isLive {
                    Label("On air", systemImage: "waveform")
                        .font(.caption.bold())
                        .foregroundStyle(.green)
                }
            }
            if let ur = station.urcall, !ur.isEmpty {
                Text("→ \(ur)")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
            }
            if let text = station.text, !text.isEmpty {
                Text(text)
                    .font(.callout)
                    .textSelection(.enabled)
            }
            if let pos = station.position {
                Label(GpsFormat.coordinate(pos), systemImage: "location.fill")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
            }
            if let heardAt = station.heardAt, let duration = station.duration {
                Text("\(durationString(duration)) · \(heardAt.formatted(date: .omitted, time: .shortened))")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
            Divider()
            ForEach(station.availableActions) { action in
                Button {
                    StationActionRunner.run(action, on: station)
                } label: {
                    Label(action.title, systemImage: action.systemImage)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .buttonStyle(.borderless)
            }
        }
        .padding()
        .frame(minWidth: 260, alignment: .leading)
        .presentationCompactAdaptation(.popover)
    }

    private func durationString(_ seconds: TimeInterval) -> String {
        let s = Int(seconds.rounded())
        return String(format: "%d:%02d", s / 60, s % 60)
    }
}
