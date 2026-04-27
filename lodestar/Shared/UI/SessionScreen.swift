// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// Single primary screen. Shows the full chain — radio, reflector,
/// live stream — as one coherent flow. The relay runs automatically
/// when preconditions are met; the user never toggles it.
struct SessionScreen: View {
    let session: SessionCoordinator

    @State private var showPicker = false
    @State private var showDevicePicker = false
    @State private var showHeardHistory = false

    /// Max heard entries shown inline on the dashboard. User-configurable
    /// via Settings → Diagnostics; rest live behind the "Show all" sheet.
    private var inlineHeardLimit: Int { reflector.inlineHeardLimit }

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
                if reflector.state == .connected {
                    // Always shown once linked, so an idle reflector
                    // still surfaces the chain and reassures that the
                    // link is live. Fields show placeholders while
                    // nobody's transmitting.
                    StreamNowPlayingCard(stream: reflector.currentStream)
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
        .onReceive(NotificationCenter.default.publisher(for: .lodestarShowDevicePicker)) { _ in
            showDevicePicker = true
        }
        .onReceive(NotificationCenter.default.publisher(for: .lodestarShowReflectorPicker)) { _ in
            showPicker = true
        }
        .onReceive(NotificationCenter.default.publisher(for: .lodestarShowHeardHistory)) { _ in
            showHeardHistory = true
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
        .accessibilityElement(children: .combine)
        .accessibilityLabel(a11yRadioLabel)
    }

    private var a11yRadioLabel: String {
        var parts: [String] = ["Radio"]
        switch transport.state {
        case .disconnected: parts.append("not connected")
        case .connecting:   parts.append("connecting")
        case .failed(let m): parts.append("failed: \(m)")
        case .connected:
            parts.append(transport.selectedDevice?.name ?? "TH-D75")
            switch transport.radioMode {
            case .mmdvm:        parts.append("MMDVM mode, ready to relay")
            case .cat:          parts.append("CAT mode")
            case .unknown:      parts.append("mode unknown")
            case .unrecognized: parts.append("mode unrecognized")
            }
        }
        return parts.joined(separator: ", ")
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
                Toggle("Auto-connect on launch", isOn: Binding(
                    get: { transport.autoConnectRadio },
                    set: { transport.autoConnectRadio = $0 }
                ))
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
        .accessibilityElement(children: .combine)
        .accessibilityLabel(a11yReflectorLabel)
    }

    private var a11yReflectorLabel: String {
        var parts: [String] = ["Reflector"]
        switch reflector.state {
        case .disconnected:  parts.append("not linked")
        case .connecting:    parts.append("linking")
        case .failed(let m): parts.append("failed: \(m)")
        case .connected:
            if let r = reflector.connectedReflector {
                parts.append("\(r.name) module \(reflector.reflectorModule)")
            } else {
                parts.append("linked")
            }
        }
        return parts.joined(separator: ", ")
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
                Divider()
                Toggle("Auto-connect on launch", isOn: Binding(
                    get: { reflector.autoConnectReflector },
                    set: { reflector.autoConnectReflector = $0 }
                ))
                Divider()
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
                            .symbolEffect(
                                .bounce.up,
                                options: .nonRepeating,
                                value: relay.framesFromRadio
                            )
                        Label("\(relay.framesFromReflector)", systemImage: "arrow.down.left")
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(.secondary)
                            .labelStyle(.titleAndIcon)
                            .symbolEffect(
                                .bounce.down,
                                options: .nonRepeating,
                                value: relay.framesFromReflector
                            )
                    }
                } else if let msg = relayExplainer {
                    Text(msg).font(.caption).foregroundStyle(.secondary)
                }
            }
            Spacer()
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(a11yRelayLabel)
    }

    private var a11yRelayLabel: String {
        var parts: [String] = ["Relay", relayTitle]
        if relay.state == .running {
            parts.append("radio to reflector frames \(relay.framesFromRadio)")
            parts.append("reflector to radio frames \(relay.framesFromReflector)")
        } else if let msg = relayExplainer {
            parts.append(msg)
        }
        return parts.joined(separator: ", ")
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
                #if os(iOS)
                ipadBody
                #else
                macBody
                #endif
            }
            .navigationTitle("Connect radio")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar { toolbar }
            .onAppear { coordinator.refreshPairedDevices() }
        }
        #if os(macOS)
        .frame(minWidth: 420, minHeight: 360)
        #endif
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .cancellationAction) {
            Button("Close") { dismiss() }
        }
        #if os(macOS)
        ToolbarItem {
            Button {
                coordinator.refreshPairedDevices()
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
        }
        #endif
    }

    #if os(macOS)
    @ViewBuilder
    private var macBody: some View {
        if coordinator.availableDevices.isEmpty {
            ContentUnavailableView {
                Label("No paired radios", systemImage: "antenna.radiowaves.left.and.right.slash")
            } description: {
                Text("Pair your TH-D75 in **System Settings → Bluetooth** (menu 934 on the radio enables pairing).")
            } actions: {
                Button("Refresh") {
                    coordinator.refreshPairedDevices()
                }
                .buttonStyle(.borderedProminent)
            }
        } else {
            List(coordinator.availableDevices) { dev in
                deviceButton(dev)
            }
        }
    }
    #endif

    #if os(iOS)
    /// iPad direct-radio access requires the USB-CDC DriverKit transport
    /// (planned). Until that ships the iPad build is reflector-only —
    /// surface a clear placeholder rather than an empty list.
    @ViewBuilder
    private var ipadBody: some View {
        ContentUnavailableView {
            Label("Direct radio access — coming soon", systemImage: "cable.connector")
        } description: {
            Text("On iPad, connecting directly to a TH-D75 requires a USB-C cable and a Lodestar DriverKit extension that's still in development. In the meantime, use **Reflectors** to TX/RX over IP.")
        } actions: {
            Button("Close") { dismiss() }
                .buttonStyle(.borderedProminent)
        }
    }
    #endif

    private func deviceButton(_ dev: BluetoothDevice) -> some View {
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
                Image(systemName: "chevron.forward")
                    .foregroundStyle(.tertiary)
                    .font(.caption)
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .disabled(coordinator.isBusy)
    }
}

private struct StreamNowPlayingCard: View {
    let stream: ReflectorCoordinator.StreamSnapshot?

    private var isLive: Bool { stream != nil }

    var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    if isLive {
                        Label("Now transmitting", systemImage: "waveform")
                            .foregroundStyle(.green)
                            .font(.headline)
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
                Divider()
                row("MY",   mycall)
                row("UR",   stream?.urcall ?? "")
                row("RPT1", stream?.rpt1 ?? "")
                row("RPT2", stream?.rpt2 ?? "")
                Divider()
                // Slow-data fields are always rendered so the card's
                // footprint is stable across the transmit / idle /
                // text-arrives-first / position-arrives-first sequence.
                // An empty value renders as a tertiary `—`, matching
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
    }

    private var mycall: String {
        guard let s = stream else { return "" }
        return "\(s.mycall)/\(s.suffix)"
    }

    private func row(_ label: String, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(label)
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
                .frame(width: 40, alignment: .leading)
            Text(value.isEmpty ? "—" : value)
                .font(.body.monospaced())
                .foregroundStyle(value.isEmpty ? .tertiary : .primary)
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
            Text(isEmpty ? "—" : value)
                .font(monospaced ? .callout.monospaced() : .callout)
                .foregroundStyle(isEmpty ? .tertiary : .primary)
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
        .accessibilityLabel(a11yLabel)
        .contextMenu {
            HeardEntryContextMenu(entry: entry)
        }
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

/// Context-menu actions for a `HeardEntry`. Apple-HIG style: verbs
/// ordered from most to least expected; destructive at the bottom
/// if any. Copy + look-up are both idempotent and don't require
/// confirmation.
private struct HeardEntryContextMenu: View {
    let entry: ReflectorCoordinator.HeardEntry

    var body: some View {
        Button {
            copyToPasteboard(entry.mycall)
        } label: {
            Label("Copy Callsign", systemImage: "doc.on.doc")
        }

        Button {
            if let url = URL(string: "https://www.qrz.com/db/\(entry.mycall)") {
                openURL(url)
            }
        } label: {
            Label("Look Up on QRZ.com", systemImage: "person.text.rectangle")
        }

        if let text = entry.text, !text.isEmpty {
            Divider()
            Button {
                copyToPasteboard(text)
            } label: {
                Label("Copy TX Message", systemImage: "text.bubble")
            }
        }

        if let pos = entry.position {
            Divider()
            Button {
                copyToPasteboard(GpsFormat.coordinate(pos))
            } label: {
                Label("Copy Coordinates", systemImage: "location")
            }
            Button {
                let lat = pos.latitude
                let lon = pos.longitude
                if let url = URL(string: "https://maps.apple.com/?ll=\(lat),\(lon)&q=\(entry.mycall)") {
                    openURL(url)
                }
            } label: {
                Label("Show on Map", systemImage: "map")
            }
        }
    }

    private func copyToPasteboard(_ s: String) {
        #if os(macOS)
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(s, forType: .string)
        #else
        UIPasteboard.general.string = s
        #endif
    }

    private func openURL(_ url: URL) {
        #if os(macOS)
        NSWorkspace.shared.open(url)
        #else
        // SwiftUI's openURL action would need @Environment injection;
        // at context-menu invocation time UIApplication.open works.
        UIApplication.shared.open(url)
        #endif
    }
}

