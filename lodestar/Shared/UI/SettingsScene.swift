// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

/// macOS `Settings` scene — the window shown for `App → Settings…` /
/// ⌘,. Tabbed per Apple HIG: General, Connections, Diagnostics.
///
/// All controls write to the coordinators via `@Bindable`; coordinators
/// already persist to `UserDefaults` on `didSet`.
struct SettingsScene: View {
    @Bindable var transport: TransportCoordinator
    @Bindable var reflector: ReflectorCoordinator

    var body: some View {
        TabView {
            GeneralTab(reflector: reflector)
                .tabItem { Label("General", systemImage: "gearshape") }

            ConnectionsTab(transport: transport, reflector: reflector)
                .tabItem { Label("Connections", systemImage: "antenna.radiowaves.left.and.right") }

            DiagnosticsTab(reflector: reflector)
                .tabItem { Label("Diagnostics", systemImage: "stethoscope") }
        }
        .frame(width: 480, height: 340)
    }
}

// MARK: - General

private struct GeneralTab: View {
    @Bindable var reflector: ReflectorCoordinator

    private let modules = ["A", "B", "C", "D", "E"]

    var body: some View {
        Form {
            Section {
                LabeledContent("Callsign") {
                    TextField("", text: $reflector.callsign, prompt: Text("W1AW"))
                        .textFieldStyle(.roundedBorder)
                        .frame(maxWidth: 160)
                        .autocorrectionDisabled()
                }
                LabeledContent("Local module") {
                    Picker("", selection: $reflector.localModule) {
                        ForEach(modules, id: \.self) { Text($0).tag($0) }
                    }
                    .labelsHidden()
                    .frame(maxWidth: 80)
                }
                LabeledContent("Reflector module") {
                    Picker("", selection: $reflector.reflectorModule) {
                        ForEach(modules, id: \.self) { Text($0).tag($0) }
                    }
                    .labelsHidden()
                    .frame(maxWidth: 80)
                }
            } header: {
                Text("Operator")
            } footer: {
                Text("Callsign and module letters are used when identifying yourself to a D-STAR reflector.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
    }
}

// MARK: - Connections

private struct ConnectionsTab: View {
    @Bindable var transport: TransportCoordinator
    @Bindable var reflector: ReflectorCoordinator

    var body: some View {
        Form {
            Section("Auto-connect on launch") {
                Toggle("Radio", isOn: $transport.autoConnectRadio)
                if let name = transport.rememberedRadioName {
                    LabeledContent("Last used") {
                        Text(name).foregroundStyle(.secondary)
                    }
                }
                Toggle("Reflector", isOn: $reflector.autoConnectReflector)
                if let name = reflector.rememberedReflectorName {
                    LabeledContent("Last used") {
                        Text(name).foregroundStyle(.secondary)
                    }
                }
            }
        }
        .formStyle(.grouped)
    }
}

// MARK: - Diagnostics

private struct DiagnosticsTab: View {
    @Bindable var reflector: ReflectorCoordinator

    private let inlineLimitOptions = [5, 10, 15, 25, 50]

    var body: some View {
        Form {
            Section {
                Toggle("Keep history across quits", isOn: $reflector.persistRecentlyHeard)
                LabeledContent("Entries") {
                    Text("\(reflector.recentlyHeard.count)")
                        .foregroundStyle(.secondary)
                }
                LabeledContent("Shown on main screen") {
                    Picker("", selection: $reflector.inlineHeardLimit) {
                        ForEach(inlineLimitOptions, id: \.self) { n in
                            Text("\(n)").tag(n)
                        }
                    }
                    .labelsHidden()
                    .frame(maxWidth: 80)
                }
                Button(role: .destructive) {
                    reflector.clearHeardHistory()
                } label: {
                    Label("Clear history", systemImage: "trash")
                }
                .disabled(reflector.recentlyHeard.isEmpty)
            } header: {
                Text("Recently heard")
            } footer: {
                Text("Entries above the limit remain accessible via the \"Show all\" sheet on the main screen.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Section("Logs") {
                Text("Open the live log viewer from **Window → Lodestar Log** (⌘⇧L).")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                Text("Full system log is also available in **Console.app** under subsystems `org.swiftraccoon.lodestar` (Swift) and `org.swiftraccoon.lodestar.rust` (Rust).")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
    }
}
