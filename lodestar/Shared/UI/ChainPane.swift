// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

/// The Radio → Reflector → Relay chain. Two presentations:
/// `.expanded` is the full card (stage rows, connect buttons, inline
/// errors — the classic dashboard card unchanged); `.strip` is a
/// one-line summary for the wide rail once everything is healthy.
struct ChainPane: View {
    let session: SessionCoordinator
    let display: RailState.ChainDisplay
    let onConnectRadio: () -> Void
    let onChooseReflector: () -> Void
    /// Strip tapped (expand) or expanded-card header tapped (collapse).
    let onToggleExpand: () -> Void

    private var transport: TransportCoordinator { session.transport }
    private var reflector: ReflectorCoordinator { session.reflector }
    private var relay: RelayCoordinator { session.relay }

    var body: some View {
        switch display {
        case .strip:
            strip
        case .expanded:
            expandedCard
        }
    }

    // MARK: - Strip

    private var strip: some View {
        Button(action: onToggleExpand) {
            HStack(spacing: 10) {
                Circle()
                    .fill(Self.heroTint(transport: transport, reflector: reflector, relay: relay))
                    .frame(width: 10, height: 10)
                Text(Self.heroTitle(transport: transport, reflector: reflector, relay: relay))
                    .font(.headline)
                Text(session.chainSummary)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                Spacer()
                Image(systemName: "chevron.down")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(stripA11yLabel)
        .accessibilityHint("Expands the connection details")
    }

    private var stripA11yLabel: String {
        "\(Self.heroTitle(transport: transport, reflector: reflector, relay: relay)), \(session.chainSummary)"
    }

    // MARK: - Expanded

    private var expandedCard: some View {
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

    // MARK: - Radio stage

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
                // Prominent action on its own full-width line — a
                // trailing placement gets crushed against the status
                // text at rail width (380 pt).
                if showsRadioConnectButton {
                    Button {
                        onConnectRadio()
                    } label: {
                        Label("Connect radio", systemImage: "plus")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.borderedProminent)
                    .padding(.top, 6)
                }
            }
            Spacer()
            if transport.state == .connected {
                radioMenu
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(a11yRadioLabel)
    }

    private var showsRadioConnectButton: Bool {
        switch transport.state {
        case .disconnected, .failed: return true
        case .connecting, .connected: return false
        }
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
                Label("CAT mode · tap “Set up radio for USB relay” below to enable voice relay",
                      systemImage: "text.bubble")
                    .font(.caption)
                    .foregroundStyle(.orange)
            case .unknown:
                Text("Mode unknown")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if let probeError = transport.lastProbeErrorText {
                    Text(probeError)
                        .font(.caption)
                        .foregroundStyle(.red)
                }
            case .unrecognized(let b):
                Text(String(format: "Unrecognized response (0x%02X)", b))
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
        }
    }

    private var radioMenu: some View {
        Menu {
            Button("Switch radio") { onConnectRadio() }
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

    // MARK: - Reflector stage

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
                if showsReflectorChooseButton {
                    Button {
                        onChooseReflector()
                    } label: {
                        Label("Choose reflector", systemImage: "plus")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.borderedProminent)
                    .padding(.top, 6)
                }
            }
            Spacer()
            if reflector.state == .connected {
                reflectorMenu
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(a11yReflectorLabel)
    }

    private var showsReflectorChooseButton: Bool {
        switch reflector.state {
        case .disconnected, .failed: return true
        case .connecting, .connected: return false
        }
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

    private var reflectorMenu: some View {
        Menu {
            Button("Switch reflector") { onChooseReflector() }
            Divider()
            Toggle("Auto-connect on launch", isOn: Binding(
                get: { reflector.autoConnectReflector },
                set: { reflector.autoConnectReflector = $0 }
            ))
            Toggle("Monitor audio on this device", isOn: Binding(
                get: { reflector.monitorAudioEnabled },
                set: { reflector.monitorAudioEnabled = $0 }
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

    // MARK: - Relay stage

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
                    if let err = relay.lastError {
                        Label(err, systemImage: "exclamationmark.triangle.fill")
                            .font(.caption)
                            .foregroundStyle(.orange)
                            .lineLimit(2)
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
            if let err = relay.lastError {
                parts.append("relay error: \(err)")
            }
        } else if let msg = relayExplainer {
            parts.append(msg)
        }
        return parts.joined(separator: ", ")
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

    // MARK: - Shared status helpers (also used by the narrow hero)

    static func heroTint(
        transport: TransportCoordinator,
        reflector: ReflectorCoordinator,
        relay: RelayCoordinator
    ) -> Color {
        if relay.state == .running { return .green }
        if reflector.state == .connected { return .yellow }
        if transport.state == .connected { return .blue }
        return .gray
    }

    static func heroTitle(
        transport: TransportCoordinator,
        reflector: ReflectorCoordinator,
        relay: RelayCoordinator
    ) -> String {
        if relay.state == .running { return "On the air" }
        if reflector.state == .connected { return "Linked" }
        if transport.state == .connected { return "Radio ready" }
        return "Lodestar"
    }
}
