import SwiftUI

struct ProbeView: View {
    let model: ProbeModel

    var body: some View {
        NavigationStack {
            List {
                Section("Session") {
                    LabeledContent("Mic permission", value: model.permission)
                    if let error = model.activationError {
                        Text("activation failed: \(error)")
                            .foregroundStyle(.red)
                    }
                    if let info = model.sessionInfo {
                        LabeledContent("Sample rate", value: String(format: "%.0f Hz", info.sampleRate))
                        LabeledContent(
                            "Input channels",
                            value: "\(info.inputChannels) (max \(info.maxInputChannels))"
                        )
                        LabeledContent(
                            "Input latency",
                            value: String(format: "%.1f ms", info.inputLatencyMs)
                        )
                        LabeledContent(
                            "IO buffer",
                            value: String(format: "%.1f ms", info.ioBufferMs)
                        )
                        LabeledContent("Derived app format", value: info.appFormat)
                    }
                    LabeledContent("Preferred input", value: model.preferredInputResult)
                }

                Section("Available Inputs") {
                    if model.availableInputs.isEmpty {
                        if model.permission == "denied" {
                            ContentUnavailableView(
                                "Microphone access denied",
                                systemImage: "mic.slash",
                                description: Text(
                                    "Enable microphone access for USB Probe in Settings; without it iOS reports no inputs at all."
                                )
                            )
                        } else {
                            ContentUnavailableView(
                                "No audio inputs",
                                systemImage: "mic.slash",
                                description: Text(
                                    "Plug the radio in with a data-capable USB-C cable (Menu 980 = COM + AF/IF Output)."
                                )
                            )
                        }
                    } else {
                        ForEach(model.availableInputs) { PortRow(port: $0) }
                    }
                }

                Section("Active Route: Inputs") {
                    ForEach(model.routeInputs) { PortRow(port: $0) }
                }

                Section("Active Route: Outputs") {
                    ForEach(model.routeOutputs) { PortRow(port: $0) }
                }

                Section {
                    if model.accessories.isEmpty {
                        Text("None; expected: the CDC serial interface is invisible to iPhone apps (no MFi chip).")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(model.accessories) { accessory in
                            VStack(alignment: .leading, spacing: 2) {
                                Text(accessory.name).font(.headline)
                                Text("\(accessory.manufacturer) \(accessory.modelNumber)")
                                    .font(.caption)
                                Text(accessory.protocols.joined(separator: ", "))
                                    .font(.caption.monospaced())
                            }
                        }
                    }
                } header: {
                    Text("External Accessories (MFi)")
                } footer: {
                    Text("Last change: \(model.lastChange)")
                }

                Section("Bidirectional Channels (no MFi)") {
                    if model.dataChannels.lines.isEmpty {
                        Text("running…").foregroundStyle(.secondary)
                    } else {
                        ForEach(Array(model.dataChannels.lines.enumerated()), id: \.offset) { _, line in
                            Text(line)
                                .font(.caption.monospaced())
                                .textSelection(.enabled)
                        }
                    }
                }

                Section("Raw-USB Control Attempt") {
                    if model.controlProbe.isEmpty {
                        Text("running…").foregroundStyle(.secondary)
                    } else {
                        ForEach(Array(model.controlProbe.enumerated()), id: \.offset) { _, line in
                            Text(line)
                                .font(.caption.monospaced())
                                .textSelection(.enabled)
                        }
                    }
                }
            }
            .navigationTitle("USB Probe")
            .toolbar {
                Button("Refresh", systemImage: "arrow.clockwise") {
                    model.refreshAndRoute()
                }
            }
            .task { await model.start() }
        }
    }
}

private struct PortRow: View {
    let port: PortInfo

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 6) {
                Text(port.name).font(.headline)
                if port.isUSB {
                    Text("USB")
                        .font(.caption2.bold())
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(.blue.opacity(0.2), in: Capsule())
                }
            }
            Text(port.type)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(port.id)
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
            if !port.channels.isEmpty {
                Text("channels: " + port.channels.map(\.name).joined(separator: ", "))
                    .font(.caption)
            }
            if !port.dataSources.isEmpty {
                Text("sources: " + port.dataSources.joined(separator: ", "))
                    .font(.caption)
            }
        }
    }
}
