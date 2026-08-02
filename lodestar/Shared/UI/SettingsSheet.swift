// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

/// App settings for platforms without a Settings scene (iPad). Groups
/// the persisted knobs that macOS exposes through the Settings window
/// and the Diagnostics pane.
struct SettingsSheet: View {
    @Environment(TransportCoordinator.self) private var transport
    @Environment(ReflectorCoordinator.self) private var reflector
    @Environment(ReflectorDirectoryStore.self) private var directory
    @Environment(\.dismiss) private var dismiss

    private let moduleLetters =
        (UInt8(ascii: "A")...UInt8(ascii: "Z")).map { String(UnicodeScalar($0)) }

    var body: some View {
        @Bindable var reflector = reflector
        @Bindable var transport = transport
        NavigationStack {
            Form {
                Section("Operator") {
                    TextField("Callsign", text: $reflector.callsign)
                        .autocorrectionDisabled()
                        #if os(iOS)
                        .textInputAutocapitalization(.characters)
                        #endif
                    Picker("Local module", selection: $reflector.localModule) {
                        ForEach(moduleLetters, id: \.self) { Text($0) }
                    }
                    Picker("Reflector module", selection: $reflector.reflectorModule) {
                        ForEach(moduleLetters, id: \.self) { Text($0) }
                    }
                }
                Section("On launch") {
                    Toggle("Auto-connect radio", isOn: $transport.autoConnectRadio)
                    Toggle("Auto-connect reflector", isOn: $reflector.autoConnectReflector)
                }
                Section("Audio") {
                    Toggle("Monitor reflector audio", isOn: $reflector.monitorAudioEnabled)
                }
                Section("Recently heard") {
                    Toggle("Keep history across launches", isOn: $reflector.persistRecentlyHeard)
                    Stepper(
                        "Rows shown inline: \(reflector.inlineHeardLimit)",
                        value: $reflector.inlineHeardLimit, in: 1...50
                    )
                    Button("Clear history", role: .destructive) {
                        reflector.clearHeardHistory()
                    }
                }
                Section {
                    Button {
                        Task {
                            await directory.refreshDPlusDirectory(callsign: reflector.callsign)
                        }
                    } label: {
                        Label("Refresh DPlus directory", systemImage: "arrow.clockwise")
                    }
                    .disabled(directory.isRefreshing)
                } header: {
                    Text("Directory")
                } footer: {
                    Text(directory.statusLine)
                }
                Section("About") {
                    LabeledContent("Lodestar core", value: version())
                }
            }
            .navigationTitle("Settings")
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }
}
