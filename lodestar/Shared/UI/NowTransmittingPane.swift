// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

/// Live-stream card. `.regular` (narrow column) keeps the stable
/// footprint across transmit/idle so the layout never jumps; `.hero`
/// (wide rail) renders across-the-desk callsign type and only exists
/// while a stream is live. The callsign is tappable → StationPopover,
/// so the live speaker is fully actionable mid-stream.
struct NowTransmittingPane: View {
    enum Size {
        case regular
        case hero
    }

    let stream: ReflectorCoordinator.StreamSnapshot?
    let size: Size

    @State private var poppedStation: StationRef?

    private var isLive: Bool { stream != nil }

    var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                header
                Divider()
                if size == .hero, let s = stream {
                    heroCallsign(s)
                } else {
                    row("MY", mycall, tappable: isLive)
                }
                row("UR",   stream?.urcall ?? "", tappable: false)
                row("RPT1", stream?.rpt1 ?? "", tappable: false)
                row("RPT2", stream?.rpt2 ?? "", tappable: false)
                Divider()
                // Slow-data fields are always rendered so the card's
                // footprint is stable across the transmit / idle /
                // text-arrives-first / position-arrives-first sequence.
                // An empty value renders as a tertiary "None", matching
                // the callsign rows above.
                slowDataRow(
                    icon: "text.bubble",
                    label: "TX",
                    value: stream?.latestText ?? "",
                    monospaced: false
                )
                slowDataRow(
                    icon: "location.fill",
                    label: "GPS",
                    value: stream?.latestPosition.map(GpsFormat.coordinate) ?? "",
                    monospaced: true
                )
            }
        }
        .contextMenu {
            if let s = stream {
                StationActionMenuItems(station: StationRef(stream: s))
            }
        }
        .popover(item: $poppedStation) { station in
            StationPopover(station: station)
        }
    }

    private var header: some View {
        HStack {
            if isLive {
                Label("Now transmitting", systemImage: "waveform")
                    .foregroundStyle(.green)
                    .font(size == .hero ? .title3.bold() : .headline)
                Spacer()
                if let s = stream {
                    Text("\(s.framesReceived) frames")
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                }
            } else {
                Label("Reflector quiet", systemImage: "waveform.slash")
                    .foregroundStyle(.secondary)
                    .font(.headline)
                Spacer()
                Text("Waiting")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }
        }
    }

    private func heroCallsign(_ s: ReflectorCoordinator.StreamSnapshot) -> some View {
        Button {
            poppedStation = StationRef(stream: s)
        } label: {
            Text(StationRef(stream: s).displayCallsign)
                .font(.system(size: 34, weight: .bold, design: .monospaced))
                .lineLimit(1)
                .minimumScaleFactor(0.5)
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Now transmitting: \(StationRef(stream: s).displayCallsign)")
        .accessibilityHint("Shows station details and actions")
    }

    private var mycall: String {
        guard let s = stream else { return "" }
        return "\(s.mycall)/\(s.suffix)"
    }

    private func row(_ label: String, _ value: String, tappable: Bool) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(label)
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
                .frame(width: 40, alignment: .leading)
            if tappable, let s = stream {
                Button {
                    poppedStation = StationRef(stream: s)
                } label: {
                    Text(value.isEmpty ? "None" : value)
                        .font(.body.monospaced())
                        .underline(pattern: .dot)
                        .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .accessibilityHint("Shows station details and actions")
            } else {
                Text(value.isEmpty ? "None" : value)
                    .font(.body.monospaced())
                    .foregroundStyle(value.isEmpty ? .tertiary : .primary)
            }
        }
    }

    private func slowDataRow(
        icon: String,
        label: String,
        value: String,
        monospaced: Bool
    ) -> some View {
        let isEmpty = value.isEmpty
        return HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: icon)
                .foregroundStyle(isEmpty ? .tertiary : .secondary)
                .font(.caption)
                .frame(width: 14)
            if !label.isEmpty {
                Text(label)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .frame(width: 30, alignment: .leading)
            } else {
                Color.clear.frame(width: 30)
            }
            Text(isEmpty ? "None" : value)
                .font(monospaced ? .callout.monospaced() : .callout)
                .foregroundStyle(isEmpty ? .tertiary : .primary)
                .textSelection(.enabled)
            Spacer(minLength: 0)
        }
    }
}
