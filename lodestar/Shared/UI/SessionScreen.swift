// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

/// Single primary screen. Shows the full chain — radio, reflector,
/// live stream — as one coherent flow. The relay runs automatically
/// when preconditions are met; the user never toggles it.
struct SessionScreen: View {
    let session: SessionCoordinator

    @State private var showPicker = false
    @State private var showDevicePicker = false
    @State private var showHeardHistory = false

    /// Max heard entries shown inline on the dashboard. Rest live
    /// behind the "Show all" sheet to keep this view bounded.
    private let inlineHeardLimit = 5

    private var transport: TransportCoordinator { session.transport }
    private var reflector: ReflectorCoordinator { session.reflector }
    private var relay: RelayCoordinator { session.relay }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                hero
                chainCard
                if shouldShowMcpCard {
                    McpCard(transport: transport)
                }
                if let stream = reflector.currentStream {
                    StreamNowPlayingCard(stream: stream)
                }
                heardHistory
            }
            .padding()
            .frame(maxWidth: 640, alignment: .leading)
        }
        .sheet(isPresented: $showPicker) {
            ReflectorPickerSheet(coordinator: reflector)
        }
        .sheet(isPresented: $showDevicePicker) {
            DevicePickerSheet(coordinator: transport)
        }
        .sheet(isPresented: $showHeardHistory) {
            HeardHistorySheet(coordinator: reflector)
        }
    }

    // MARK: - Sections

    private var hero: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 10) {
                Circle()
                    .fill(heroTint)
                    .frame(width: 12, height: 12)
                Text(heroTitle)
                    .font(.title2.bold())
            }
            Text(session.chainSummary)
                .font(.callout)
                .foregroundStyle(.secondary)
        }
    }

    private var chainCard: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 16) {
                radioRow
                Divider()
                reflectorRow
                Divider()
                relayRow
            }
        }
    }

    private var radioRow: some View {
        HStack(alignment: .top, spacing: 12) {
            stageIcon(
                system: "antenna.radiowaves.left.and.right",
                tint: .blue,
                active: transport.state == .connected
            )
            VStack(alignment: .leading, spacing: 4) {
                Text("Radio").font(.caption.bold()).foregroundStyle(.secondary)
                switch transport.state {
                case .disconnected, .failed:
                    Text("Not connected").font(.headline)
                    if case .failed(let m) = transport.state {
                        Text(m).font(.caption).foregroundStyle(.red)
                    }
                case .connecting:
                    HStack(spacing: 6) {
                        ProgressView().controlSize(.small)
                        Text("Connecting to \(transport.selectedDevice?.name ?? "radio")…")
                            .font(.headline)
                    }
                case .connected:
                    Text(transport.selectedDevice?.name ?? "TH-D75")
                        .font(.headline)
                    radioModeSubtitle
                }
            }
            Spacer()
            radioActions
        }
    }

    @ViewBuilder
    private var radioModeSubtitle: some View {
        if transport.isProbingMode {
            HStack(spacing: 4) {
                ProgressView().controlSize(.mini)
                Text("Probing mode…").font(.caption).foregroundStyle(.secondary)
            }
        } else {
            switch transport.radioMode {
            case .mmdvm:
                Label("MMDVM · ready to relay", systemImage: "waveform.circle.fill")
                    .font(.caption)
                    .foregroundStyle(.green)
            case .cat:
                Label("CAT mode · enable Terminal Mode to relay voice", systemImage: "text.bubble")
                    .font(.caption)
                    .foregroundStyle(.orange)
            case .unknown:
                Text("Mode unknown")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            case .unrecognized(let b):
                Text(String(format: "Unrecognized response (0x%02X)", b))
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
        }
    }

    @ViewBuilder
    private var radioActions: some View {
        switch transport.state {
        case .disconnected, .failed:
            Button {
                showDevicePicker = true
            } label: {
                Label("Connect radio", systemImage: "plus")
            }
            .buttonStyle(.borderedProminent)
        case .connecting:
            EmptyView()
        case .connected:
            Menu {
                Button("Switch radio") { showDevicePicker = true }
                Button("Re-probe mode") { Task { await transport.probeRadioMode() } }
                if transport.radioMode == .cat || transport.radioMode == .unknown {
                    Button {
                        Task { await transport.sendIdentify() }
                    } label: {
                        Text("Send ID test (CAT)")
                    }
                }
                Divider()
                Button(role: .destructive) {
                    Task { await transport.disconnect() }
                } label: {
                    Text("Disconnect radio")
                }
            } label: {
                Image(systemName: "ellipsis.circle")
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
        }
    }

    private var reflectorRow: some View {
        HStack(alignment: .top, spacing: 12) {
            let proto = reflector.connectedReflector?.protocol
            stageIcon(
                system: proto?.sfSymbol ?? "dot.radiowaves.left.and.right",
                tint: proto?.accentColor ?? .blue,
                active: reflector.state == .connected
            )
            VStack(alignment: .leading, spacing: 4) {
                Text("Reflector").font(.caption.bold()).foregroundStyle(.secondary)
                switch reflector.state {
                case .disconnected:
                    Text("Not linked").font(.headline)
                case .connecting:
                    HStack(spacing: 6) {
                        ProgressView().controlSize(.small)
                        Text("Linking…").font(.headline)
                    }
                case .failed(let m):
                    Text("Link failed").font(.headline).foregroundStyle(.red)
                    Text(m).font(.caption).foregroundStyle(.secondary).lineLimit(3)
                case .connected:
                    if let r = reflector.connectedReflector {
                        Text("\(r.name)\(reflector.reflectorModule)")
                            .font(.headline)
                        Text("\(r.host):\(String(r.port))")
                            .font(.caption.monospaced())
                            .foregroundStyle(.secondary)
                    } else {
                        Text("Linked").font(.headline)
                    }
                }
            }
            Spacer()
            reflectorActions
        }
    }

    @ViewBuilder
    private var reflectorActions: some View {
        switch reflector.state {
        case .disconnected, .failed:
            Button {
                showPicker = true
            } label: {
                Label("Choose reflector", systemImage: "plus")
            }
            .buttonStyle(.borderedProminent)
        case .connecting:
            EmptyView()
        case .connected:
            Menu {
                Button("Switch reflector") { showPicker = true }
                Button(role: .destructive) {
                    Task { await reflector.disconnect() }
                } label: {
                    Text("Disconnect reflector")
                }
            } label: {
                Image(systemName: "ellipsis.circle")
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
        }
    }

    private var relayRow: some View {
        HStack(alignment: .top, spacing: 12) {
            stageIcon(
                system: "arrow.left.arrow.right.circle.fill",
                tint: relayTint,
                active: relay.state == .running
            )
            VStack(alignment: .leading, spacing: 4) {
                Text("Relay").font(.caption.bold()).foregroundStyle(.secondary)
                Text(relayTitle).font(.headline)
                if relay.state == .running {
                    HStack(spacing: 18) {
                        Label("\(relay.framesFromRadio)", systemImage: "arrow.up.right")
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(.secondary)
                            .labelStyle(.titleAndIcon)
                        Label("\(relay.framesFromReflector)", systemImage: "arrow.down.left")
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(.secondary)
                            .labelStyle(.titleAndIcon)
                    }
                } else if let msg = relayExplainer {
                    Text(msg).font(.caption).foregroundStyle(.secondary)
                }
            }
            Spacer()
        }
    }

    @ViewBuilder
    private var heardHistory: some View {
        if reflector.state == .connected || !reflector.recentlyHeard.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Text("Recently heard").font(.headline)
                    Spacer()
                    if reflector.recentlyHeard.count > inlineHeardLimit {
                        Button {
                            showHeardHistory = true
                        } label: {
                            HStack(spacing: 2) {
                                Text("Show all \(reflector.recentlyHeard.count)")
                                Image(systemName: "chevron.forward").font(.caption2)
                            }
                        }
                        .buttonStyle(.borderless)
                        .font(.caption)
                    }
                }

                if reflector.recentlyHeard.isEmpty {
                    Text("Stations who transmit through this reflector will appear here.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                } else {
                    ForEach(reflector.recentlyHeard.prefix(inlineHeardLimit)) { entry in
                        HeardRow(entry: entry)
                    }
                }
            }
        }
    }

    // MARK: - Helpers

    private func stageIcon(system: String, tint: Color, active: Bool) -> some View {
        let bg = active ? tint.opacity(0.22) : Color.gray.opacity(0.12)
        let fg = active ? tint : Color.secondary
        return RoundedRectangle(cornerRadius: 10, style: .continuous)
            .fill(bg)
            .frame(width: 44, height: 44)
            .overlay(Image(systemName: system).foregroundStyle(fg))
            .accessibilityHidden(true)
    }

    private var shouldShowMcpCard: Bool {
        guard case .connected = transport.state else { return false }
        if transport.mcpStatus != .idle { return true }
        switch transport.radioMode {
        case .cat, .unrecognized: return true
        case .mmdvm, .unknown:    return false
        }
    }

    private var heroTint: Color {
        if relay.state == .running { return .green }
        if reflector.state == .connected { return .yellow }
        if transport.state == .connected { return .blue }
        return .gray
    }

    private var heroTitle: String {
        if relay.state == .running { return "On the air" }
        if reflector.state == .connected { return "Linked" }
        if transport.state == .connected { return "Radio ready" }
        return "Lodestar"
    }

    private var relayTitle: String {
        switch relay.state {
        case .stopped:        return "Idle"
        case .starting:       return "Starting…"
        case .running:        return "Running"
        case .failed:         return "Failed"
        }
    }

    private var relayTint: Color {
        switch relay.state {
        case .stopped:  return .gray
        case .starting: return .yellow
        case .running:  return .green
        case .failed:   return .red
        }
    }

    private var relayExplainer: String? {
        switch relay.state {
        case .running, .starting:
            return nil
        case .failed(let msg):
            return msg
        case .stopped:
            if transport.state != .connected {
                return "Connect a radio to start."
            }
            if transport.radioMode != .mmdvm {
                return "Radio needs MMDVM (Reflector Terminal) mode. See below."
            }
            if reflector.state != .connected {
                return "Choose a reflector to start."
            }
            return "Preparing…"
        }
    }
}

/// "Enable Reflector Terminal Mode" card. Only surfaced when the
/// radio is connected in CAT (or unrecognized) mode — if it's already
/// in MMDVM we skip it entirely.
struct McpCard: View {
    let transport: TransportCoordinator

    var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 10) {
                Label("Enable Reflector Terminal Mode", systemImage: "gearshape.2")
                    .font(.headline)
                Text("Flips radio menu 650 via an MCP write. The radio reboots into MMDVM binary framing on Bluetooth — that's the only mode in which Lodestar can relay voice between the radio and a reflector.")
                    .font(.callout)
                    .foregroundStyle(.secondary)

                Button {
                    Task { await transport.enableReflectorTerminalMode() }
                } label: {
                    Label("Enable Reflector Terminal Mode", systemImage: "arrow.up.right.circle.fill")
                }
                .buttonStyle(.borderedProminent)
                .disabled(transport.isBusy)

                mcpStatus
            }
        }
    }

    @ViewBuilder
    private var mcpStatus: some View {
        switch transport.mcpStatus {
        case .idle:
            EmptyView()
        case .running(let msg):
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text(msg).font(.caption.monospaced())
            }
        case .succeededRebooting:
            VStack(alignment: .leading, spacing: 4) {
                Label("Reflector Terminal Mode enabled", systemImage: "checkmark.seal.fill")
                    .foregroundStyle(.green)
                    .font(.callout)
                Text("Radio is rebooting. Reconnect once it's back to use the new mode.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button("OK") { transport.acknowledgeMcpStatus() }
                    .buttonStyle(.borderless)
            }
        case .failed(let msg):
            VStack(alignment: .leading, spacing: 4) {
                Label("MCP failed", systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
                    .font(.callout)
                Text(msg)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                Button("Dismiss") { transport.acknowledgeMcpStatus() }
                    .buttonStyle(.borderless)
            }
        }
    }
}

/// Device-picker sheet — mirrors the reflector sheet pattern.
private struct DevicePickerSheet: View {
    let coordinator: TransportCoordinator
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Group {
                if coordinator.availableDevices.isEmpty {
                    ContentUnavailableView {
                        Label("No paired radios", systemImage: "antenna.radiowaves.left.and.right.slash")
                    } description: {
                        #if os(macOS)
                        Text("Pair your TH-D75 in **System Settings → Bluetooth** (menu 934 on the radio enables pairing).")
                        #else
                        Text("Bluetooth Classic SPP isn't available on iOS / iPadOS. Run the macOS build to pair with a TH-D75.")
                        #endif
                    } actions: {
                        Button("Refresh") {
                            coordinator.refreshPairedDevices()
                        }
                        .buttonStyle(.borderedProminent)
                    }
                } else {
                    List(coordinator.availableDevices) { dev in
                        Button {
                            coordinator.select(dev)
                            Task {
                                await coordinator.connect()
                                dismiss()
                            }
                        } label: {
                            HStack(spacing: 12) {
                                Image(systemName: "antenna.radiowaves.left.and.right")
                                    .foregroundStyle(.blue)
                                VStack(alignment: .leading) {
                                    Text(dev.name).font(.headline)
                                    Text(dev.address).font(.caption.monospaced()).foregroundStyle(.secondary)
                                }
                                Spacer()
                                Image(systemName: "chevron.forward").foregroundStyle(.tertiary).font(.caption)
                            }
                            .contentShape(.rect)
                        }
                        .buttonStyle(.plain)
                        .disabled(coordinator.isBusy)
                    }
                }
            }
            .navigationTitle("Connect radio")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
                ToolbarItem {
                    Button {
                        coordinator.refreshPairedDevices()
                    } label: {
                        Label("Refresh", systemImage: "arrow.clockwise")
                    }
                }
            }
            .onAppear { coordinator.refreshPairedDevices() }
        }
        #if os(macOS)
        .frame(minWidth: 420, minHeight: 360)
        #endif
    }
}

private struct StreamNowPlayingCard: View {
    let stream: ReflectorCoordinator.StreamSnapshot

    var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Label("Now transmitting", systemImage: "waveform")
                        .foregroundStyle(.green)
                        .font(.headline)
                    Spacer()
                    Text("\(stream.framesReceived) frames")
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                }
                Divider()
                row("MY",   "\(stream.mycall)/\(stream.suffix)")
                row("UR",   stream.urcall)
                row("RPT1", stream.rpt1)
                row("RPT2", stream.rpt2)
                if stream.latestText != nil || stream.latestPosition != nil {
                    Divider()
                }
                if let text = stream.latestText, !text.isEmpty {
                    slowDataRow(
                        icon: "text.bubble",
                        label: "TX",
                        value: text,
                        monospaced: false
                    )
                }
                if let pos = stream.latestPosition {
                    slowDataRow(
                        icon: "location.fill",
                        label: "GPS",
                        value: GpsFormat.coordinate(pos),
                        monospaced: true
                    )
                    if let comment = pos.comment, !comment.isEmpty {
                        slowDataRow(
                            icon: "quote.bubble",
                            label: "",
                            value: comment,
                            monospaced: false
                        )
                    }
                }
            }
        }
    }

    private func row(_ label: String, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(label)
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
                .frame(width: 40, alignment: .leading)
            Text(value.isEmpty ? "—" : value)
                .font(.body.monospaced())
        }
    }

    private func slowDataRow(
        icon: String,
        label: String,
        value: String,
        monospaced: Bool
    ) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: icon)
                .foregroundStyle(.secondary)
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
            Text(value)
                .font(monospaced ? .callout.monospaced() : .callout)
                .foregroundStyle(.primary)
                .textSelection(.enabled)
            Spacer(minLength: 0)
        }
    }
}

private struct HeardRow: View {
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
    }

    private func durationString(_ seconds: TimeInterval) -> String {
        let s = Int(seconds.rounded())
        return String(format: "%d:%02d", s / 60, s % 60)
    }
}
